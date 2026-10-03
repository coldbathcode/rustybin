"""A2A conformance checks for rustybin using the official a2a-sdk (1.2.x) client.

Usage:
    python conformance/a2a/a2a_conformance.py [--base http://127.0.0.1:18500]

Drives every demo agent with the official client over:
  - v1.0 JSON-RPC and v1.0 HTTP+JSON (agent card resolved from
    /a2a/{agent}/.well-known/agent-card.json),
  - v0.3 JSON-RPC and v0.3 HTTP+JSON (SDK compat transports, selected by
    resolving the legacy card /a2a/{agent}/.well-known/agent.json).
Checks: card resolution, send message (blocking), streaming, get task,
cancel (running and not cancelable), list tasks, subscribe, input-required
continuation, auth-required + bearer token + extended card, push config CRUD
and delivery to the built-in webhook sink, error mapping.

Exit code 0 when every check passes.
"""

import argparse
import asyncio
import sys
import traceback
import uuid

import httpx

from a2a.client import A2ACardResolver, ClientConfig, ClientFactory
from a2a.client.card_resolver import parse_agent_card
from a2a.types import (
    CancelTaskRequest,
    DeleteTaskPushNotificationConfigRequest,
    GetExtendedAgentCardRequest,
    GetTaskPushNotificationConfigRequest,
    GetTaskRequest,
    ListTaskPushNotificationConfigsRequest,
    ListTasksRequest,
    Message,
    Part,
    Role,
    SendMessageConfiguration,
    SendMessageRequest,
    SubscribeToTaskRequest,
    TaskNotCancelableError,
    TaskNotFoundError,
    TaskPushNotificationConfig,
    TaskState,
)
from a2a.utils.constants import TransportProtocol

JSONRPC = TransportProtocol.JSONRPC
REST = TransportProtocol.HTTP_JSON

EXPECTED_STATE = {
    "echo": TaskState.TASK_STATE_COMPLETED,
    "weather": TaskState.TASK_STATE_COMPLETED,
    "travel-planner": TaskState.TASK_STATE_COMPLETED,
    "approval": TaskState.TASK_STATE_INPUT_REQUIRED,
    "flaky": TaskState.TASK_STATE_FAILED,
    "secure": TaskState.TASK_STATE_AUTH_REQUIRED,
    "reject": TaskState.TASK_STATE_REJECTED,
}
PROMPT = {
    "echo": "hello agent",
    "weather": "What is the weather in Paris?",
    "travel-planner": "Plan a 2 day trip to Kyoto",
    "approval": "Approve my $120 taxi expense",
    "flaky": "do something",
    "secure": "who am I?",
    "reject": "book a flight",
}
TERMINAL_OR_INTERRUPTED = {
    TaskState.TASK_STATE_COMPLETED,
    TaskState.TASK_STATE_FAILED,
    TaskState.TASK_STATE_CANCELED,
    TaskState.TASK_STATE_REJECTED,
    TaskState.TASK_STATE_INPUT_REQUIRED,
    TaskState.TASK_STATE_AUTH_REQUIRED,
}

results: list[tuple[str, bool, str]] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    results.append((name, ok, detail))
    print(f"{'PASS' if ok else 'FAIL'}  {name}{'  ' + detail if detail and not ok else ''}")


def msg(text: str, task_id: str = "", context_id: str = "", fast: bool = True) -> Message:
    m = Message(
        message_id=str(uuid.uuid4()),
        role=Role.ROLE_USER,
        parts=[Part(text=text)],
    )
    if task_id:
        m.task_id = task_id
    if context_id:
        m.context_id = context_id
    if fast:
        m.metadata.update({"stepDelayMs": 20})
    return m


def send_req(m: Message, **cfg) -> SendMessageRequest:
    req = SendMessageRequest(message=m)
    if cfg:
        req.configuration.CopyFrom(SendMessageConfiguration(**cfg))
    return req


async def collect(client, req):
    return [ev async for ev in client.send_message(req)]


def last_task_state(events):
    state = None
    for ev in events:
        if ev.HasField("task"):
            state = ev.task.status.state
        elif ev.HasField("status_update"):
            state = ev.status_update.status.state
    return state


def task_id_of(events) -> str:
    for ev in events:
        if ev.HasField("task"):
            return ev.task.id
        if ev.HasField("status_update"):
            return ev.status_update.task_id
    return ""


async def make_client(http, card, binding, streaming, polling=False):
    cfg = ClientConfig(
        streaming=streaming,
        polling=polling,
        httpx_client=http,
        supported_protocol_bindings=[binding],
    )
    return ClientFactory(cfg).create(card)


async def get_token(http: httpx.AsyncClient, base: str) -> str:
    r = await http.post(
        f"{base}/oauth/token",
        data={"grant_type": "client_credentials", "client_id": "rustybin", "client_secret": "secret"},
    )
    r.raise_for_status()
    return r.json()["access_token"]


async def agent_suite(base: str, agent: str, binding: str, legacy: bool, token: str) -> None:
    label = f"[{'v0.3' if legacy else 'v1.0'} {binding} {agent}]"
    headers = {"X-Rustybin-Session": f"conf-{uuid.uuid4()}"}
    async with httpx.AsyncClient(timeout=30, headers=headers) as http:
        # Card resolution.
        try:
            if legacy:
                raw = (await http.get(f"{base}/a2a/{agent}/.well-known/agent.json")).json()
                card = parse_agent_card(raw)
            else:
                card = await A2ACardResolver(http, f"{base}/a2a/{agent}").get_agent_card()
            versions = {i.protocol_version for i in card.supported_interfaces}
            check(f"{label} resolve card", bool(card.name) and bool(card.skills), str(versions))
            if legacy:
                check(f"{label} legacy card is v0.3", versions == {"0.3.0"}, str(versions))
            else:
                check(f"{label} card lists 1.0 and 0.3", {"1.0", "0.3"} <= versions, str(versions))
        except Exception as e:  # noqa: BLE001
            check(f"{label} resolve card", False, repr(e))
            return

        expected = EXPECTED_STATE[agent]

        # Blocking send.
        try:
            client = await make_client(http, card, binding, streaming=False)
            events = await collect(client, send_req(msg(PROMPT[agent])))
            st = last_task_state(events)
            check(f"{label} send message", st == expected, f"state={TaskState.Name(st) if st is not None else None}")
            tid = task_id_of(events)
            task = await client.get_task(GetTaskRequest(id=tid, history_length=1))
            check(f"{label} get task", task.id == tid and task.status.state == expected and len(task.history) <= 1)
            if agent == "weather":
                datas = [p for a in task.artifacts for p in a.parts if p.HasField("data")]
                check(f"{label} data artifact", bool(datas))
            if agent == "travel-planner":
                kinds = {p.WhichOneof("content") for a in task.artifacts for p in a.parts}
                check(f"{label} text/data/url/raw artifacts", {"text", "data", "url", "raw"} <= kinds, str(kinds))
            # Not cancelable (already terminal or interrupted -> terminal only raise).
            if expected in (TaskState.TASK_STATE_COMPLETED, TaskState.TASK_STATE_FAILED, TaskState.TASK_STATE_REJECTED):
                try:
                    await client.cancel_task(CancelTaskRequest(id=tid))
                    check(f"{label} cancel terminal task -> TaskNotCancelableError", False, "no error")
                except TaskNotCancelableError:
                    check(f"{label} cancel terminal task -> TaskNotCancelableError", True)
                except Exception as e:  # noqa: BLE001
                    check(f"{label} cancel terminal task -> TaskNotCancelableError", False, repr(e))
            else:
                canceled = await client.cancel_task(CancelTaskRequest(id=tid))
                check(f"{label} cancel interrupted task", canceled.status.state == TaskState.TASK_STATE_CANCELED)
            try:
                await client.get_task(GetTaskRequest(id=str(uuid.uuid4())))
                check(f"{label} unknown task -> TaskNotFoundError", False, "no error")
            except TaskNotFoundError:
                check(f"{label} unknown task -> TaskNotFoundError", True)
            except Exception as e:  # noqa: BLE001
                # v0.3 REST maps 404 by error type name.
                check(f"{label} unknown task -> TaskNotFoundError", "not found" in str(e).lower(), repr(e))
        except Exception as e:  # noqa: BLE001
            check(f"{label} send/get/cancel", False, repr(e) + traceback.format_exc(limit=2))

        # Streaming send.
        try:
            sclient = await make_client(http, card, binding, streaming=True)
            events = await collect(sclient, send_req(msg(PROMPT[agent])))
            first_is_task = bool(events) and events[0].HasField("task")
            st = last_task_state(events)
            check(f"{label} stream", first_is_task and st == expected, f"{len(events)} events, state={st}")
            if agent == "travel-planner":
                ups = [e.artifact_update for e in events if e.HasField("artifact_update")]
                check(f"{label} stream artifact chunks", any(u.append for u in ups) and any(u.last_chunk for u in ups))
                statuses = [e.status_update.status.state for e in events if e.HasField("status_update")]
                check(f"{label} stream WORKING updates", TaskState.TASK_STATE_WORKING in statuses)
        except Exception as e:  # noqa: BLE001
            check(f"{label} stream", False, repr(e))

        # Running task: cancel and subscribe (travel planner only).
        if agent == "travel-planner":
            try:
                pclient = await make_client(http, card, binding, streaming=False, polling=True)
                events = await collect(pclient, send_req(msg("Plan a 5 day trip to Oslo", fast=False)))
                tid = task_id_of(events)
                check(f"{label} return immediately", last_task_state(events) not in TERMINAL_OR_INTERRUPTED)
                canceled = await pclient.cancel_task(CancelTaskRequest(id=tid))
                check(f"{label} cancel running task", canceled.status.state == TaskState.TASK_STATE_CANCELED)
                events = await collect(pclient, send_req(msg("Plan a 2 day trip to Rome")))
                tid = task_id_of(events)
                sclient = await make_client(http, card, binding, streaming=True)
                sub = [ev async for ev in sclient.subscribe(SubscribeToTaskRequest(id=tid))]
                check(
                    f"{label} subscribe",
                    bool(sub) and sub[0].HasField("task") and last_task_state(sub) == TaskState.TASK_STATE_COMPLETED,
                    f"{len(sub)} events",
                )
            except Exception as e:  # noqa: BLE001
                check(f"{label} cancel/subscribe running task", False, repr(e))

        # Multi-turn: input-required continuation.
        if agent == "approval":
            try:
                client = await make_client(http, card, binding, streaming=False)
                events = await collect(client, send_req(msg(PROMPT[agent])))
                first = next(e.task for e in events if e.HasField("task"))
                events = await collect(client, send_req(msg("approve", task_id=first.id, context_id=first.context_id)))
                st = last_task_state(events)
                check(f"{label} input-required continuation", st == TaskState.TASK_STATE_COMPLETED, f"state={st}")
            except Exception as e:  # noqa: BLE001
                check(f"{label} input-required continuation", False, repr(e))

        # Auth: token continues the task, extended card.
        if agent == "secure":
            try:
                async with httpx.AsyncClient(
                    timeout=30, headers={**headers, "Authorization": f"Bearer {token}"}
                ) as authed:
                    client = await make_client(authed, card, binding, streaming=False)
                    events = await collect(client, send_req(msg(PROMPT[agent])))
                    check(f"{label} bearer token -> completed", last_task_state(events) == TaskState.TASK_STATE_COMPLETED)
                    ext = await client.get_extended_agent_card(GetExtendedAgentCardRequest())
                    check(f"{label} extended card", any(s.id == "audit-log" for s in ext.skills))
            except Exception as e:  # noqa: BLE001
                check(f"{label} bearer token / extended card", False, repr(e))

        # Direct message reply.
        if agent == "echo":
            try:
                client = await make_client(http, card, binding, streaming=False)
                events = await collect(client, send_req(msg("msg: hi")))
                check(f"{label} direct message reply", len(events) == 1 and events[0].HasField("message"))
            except Exception as e:  # noqa: BLE001
                check(f"{label} direct message reply", False, repr(e))


async def push_and_list_suite(base: str, binding: str) -> None:
    label = f"[v1.0 {binding} push/list]"
    headers = {"X-Rustybin-Session": f"conf-{uuid.uuid4()}"}
    sink = f"conf-{uuid.uuid4().hex[:12]}"
    sink_url = f"{base}/a2a/webhook-sink/{sink}"
    async with httpx.AsyncClient(timeout=30, headers=headers) as http:
        card = await A2ACardResolver(http, f"{base}/a2a/travel-planner").get_agent_card()
        client = await make_client(http, card, binding, streaming=False, polling=True)
        events = await collect(client, send_req(msg("Plan a 2 day trip to Porto", fast=False)))
        tid = task_id_of(events)
        try:
            cfg = TaskPushNotificationConfig(task_id=tid, url=sink_url, token="tok-1")
            cfg.authentication.scheme = "Bearer"
            cfg.authentication.credentials = "sink-secret"
            created = await client.create_task_push_notification_config(cfg)
            check(f"{label} create push config", created.id != "" and created.url == sink_url)
            got = await client.get_task_push_notification_config(
                GetTaskPushNotificationConfigRequest(task_id=tid, id=created.id)
            )
            check(f"{label} get push config", got.id == created.id)
            listed = await client.list_task_push_notification_configs(ListTaskPushNotificationConfigsRequest(task_id=tid))
            check(f"{label} list push configs", len(listed.configs) == 1)
            # Wait for the task to finish, then inspect the sink.
            for _ in range(100):
                t = await client.get_task(GetTaskRequest(id=tid))
                if t.status.state in TERMINAL_OR_INTERRUPTED:
                    break
                await asyncio.sleep(0.1)
            received = (await http.get(sink_url)).json()
            notes = received.get("notifications", [])
            auth_ok = any(n["headers"].get("authorization") == "Bearer sink-secret" for n in notes)
            token_ok = any(n["headers"].get("x-a2a-notification-token") == "tok-1" for n in notes)
            final = any(
                n["body"].get("statusUpdate", {}).get("status", {}).get("state") == "TASK_STATE_COMPLETED" for n in notes
            )
            check(f"{label} push delivered to sink", bool(notes) and auth_ok and token_ok and final, f"{len(notes)} notes")
            await client.delete_task_push_notification_config(
                DeleteTaskPushNotificationConfigRequest(task_id=tid, id=created.id)
            )
            listed = await client.list_task_push_notification_configs(ListTaskPushNotificationConfigsRequest(task_id=tid))
            check(f"{label} delete push config", len(listed.configs) == 0)
            try:
                bad = TaskPushNotificationConfig(task_id=tid, url="http://169.254.169.254/latest/meta-data")
                await client.create_task_push_notification_config(bad)
                check(f"{label} SSRF push URL rejected", False, "accepted")
            except Exception as e:  # noqa: BLE001
                check(f"{label} SSRF push URL rejected", "not allowed" in str(e) or "Invalid" in type(e).__name__, repr(e))
        except Exception as e:  # noqa: BLE001
            check(f"{label} push config CRUD", False, repr(e))
        try:
            lt = await client.list_tasks(ListTasksRequest(page_size=1))
            check(f"{label} list tasks", lt.page_size == 1 and len(lt.tasks) == 1 and lt.total_size >= 1)
            if lt.total_size > 1:
                check(f"{label} list tasks pagination", lt.next_page_token != "")
        except Exception as e:  # noqa: BLE001
            check(f"{label} list tasks", False, repr(e))


async def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:18500")
    args = ap.parse_args()
    base = args.base.rstrip("/")
    async with httpx.AsyncClient(timeout=30) as http:
        token = await get_token(http, base)
        root = await A2ACardResolver(http, base).get_agent_card()
        check("[root] /.well-known/agent-card.json resolves", root.name == "Echo Agent")
        legacy_root = parse_agent_card((await http.get(f"{base}/.well-known/agent.json")).json())
        check("[root] /.well-known/agent.json resolves (v0.3)", legacy_root.supported_interfaces[0].protocol_version == "0.3.0")
    for legacy in (False, True):
        for binding in (JSONRPC, REST):
            for agent in EXPECTED_STATE:
                await agent_suite(base, agent, binding, legacy, token)
    for binding in (JSONRPC, REST):
        await push_and_list_suite(base, binding)
    failed = [r for r in results if not r[1]]
    print(f"\n{len(results) - len(failed)}/{len(results)} checks passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
