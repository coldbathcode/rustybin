"""The management MCP server at /_rustybin/mcp with the official Python SDK.

run.sh starts the server with RUSTYBIN_ADMIN_TOKEN and the mock AI provider:
without the token the caller only sees the read tools, with it
(`Authorization: Bearer`, as in `claude mcp add --header`) every tool. Covers
discovery, role-filtered tools/list with annotations, the prompt set, the
resources, a create, deploy, try_request and delete round trip, destructive
tools confirmed through elicitation (2026-07-28 multi round-trip and a
2025-11-25 session's elicitation/create) or `confirm: true` when the client
cannot ask, and the build_instance walk: a spec from a description, the
examples, the portal, the version 2 export, git and the agents.
"""

import os

import mcp.client.session as sdk_session
import mcp_types as types
from mcp import Client
from mcp.client.streamable_http import streamable_http_client
from mcp.shared._httpx_utils import create_mcp_http_client

from common import BASE, check, run, section, text_of

PATH = "/_rustybin/mcp"
TOKEN = os.environ.get("MANAGE_ADMIN_TOKEN", "conformance-admin")
ADMIN = {"Authorization": f"Bearer {TOKEN}"}

ORDERS = """openapi: 3.0.3
info: {title: Orders, version: "1"}
paths:
  /orders/{id}:
    get:
      operationId: getOrder
      parameters:
        - {name: id, in: path, required: true, schema: {type: integer}}
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                type: object
                required: [id]
                properties: {id: {type: integer}, total: {type: number}}
              example: {id: 1, total: 9.5}
"""


def client(headers: dict | None = None, mode: str = "auto", **kw) -> Client:
    http = create_mcp_http_client(headers=headers or {})
    return Client(streamable_http_client(f"{BASE}{PATH}", http_client=http), mode=mode, **kw)


def answer(action: str):
    async def cb(context, params):
        if action == "accept":
            return types.ElicitResult(action="accept", content={"confirm": True})
        return types.ElicitResult(action=action)

    return cb


async def mock_exists(c: Client, name: str) -> bool:
    r = await c.call_tool("get_mock", {"name": name})
    return not r.is_error


async def create(c: Client, name: str) -> None:
    r = await c.call_tool("create_mock", {"spec": ORDERS, "name": name})
    check(not r.is_error, f"create_mock {name}: {text_of(r)[:80]!r}")


async def anonymous() -> None:
    section("no token: read tools only")
    async with client() as c:
        tools = {t.name for t in (await c.list_tools()).tools}
        check("get_instance" in tools and "list_mocks" in tools, "read tools listed")
        check("create_mock" not in tools and "delete_mock" not in tools, "mock changes hidden")
        r = await c.call_tool("get_instance", {})
        check(r.structured_content["caller"]["admin"] is False, "caller is not admin")
        r = await c.call_tool("create_mock", {"spec": ORDERS, "name": "nope"})
        check(r.is_error, f"create_mock refused: {text_of(r)[:80]!r}")


async def admin() -> None:
    async with client(ADMIN) as c:
        section("discover (2026-07-28)")
        check(c.protocol_version == "2026-07-28", f"negotiated 2026-07-28 (got {c.protocol_version})")
        check(c.server_info is not None and c.server_info.name == "rustybin-manage", "serverInfo name")
        check("untrusted_*" in (c.instructions or ""), "instructions warn about untrusted_* content")

        section("tools/list")
        tools = {t.name: t for t in (await c.list_tools()).tools}
        for name in ["get_instance", "find_endpoints", "create_mock", "deploy_mock", "delete_mock",
                     "try_request", "issue_token", "wait_for_requests", "apply_config", "export_config"]:
            check(name in tools, f"{name} listed")
        check("get_visit" not in tools, "no visit tools without self-service")
        d = tools["delete_mock"]
        check(d.annotations.destructive_hint is True and d.annotations.read_only_hint is False,
              "delete_mock destructive annotations")
        check("confirm" in d.input_schema["properties"], "delete_mock takes confirm")
        check(tools["get_mock"].annotations.read_only_hint is True, "get_mock readOnlyHint")
        r = await c.call_tool("get_instance", {})
        check(r.structured_content["caller"]["admin"] is True, "bearer admin token recognised")

        section("prompts")
        prompts = {p.name for p in (await c.list_prompts()).prompts}
        check(prompts == {"build_instance", "mock_an_api", "publish_portal", "backup_to_git", "test_mcp_server",
                          "onboard_agent", "debug_gateway_call", "secure_a_mock", "load_test_gateway"},
              f"nine prompts {prompts}")
        p = await c.get_prompt("secure_a_mock", {"name": "petstore", "mode": "jwt"})
        check("issue_token" in p.messages[0].content.text, "secure_a_mock mentions issue_token")
        p = await c.get_prompt("build_instance", {"brief": "an orders API for the checkout team"})
        text = p.messages[0].content.text
        check("checkout team" in text and "generate_spec" in text and "git_push" in text, "build_instance renders the brief")

        section("resources")
        resources = {str(r.uri) for r in (await c.list_resources()).resources}
        check(resources == {"rustybin://config.schema.json", "rustybin://catalogue", "rustybin://harness.schema.json",
                            "rustybin://portal/catalog", "rustybin://cli.md"}, f"five resources {resources}")
        rr = await c.read_resource("rustybin://config.schema.json")
        check("mocks" in rr.contents[0].text, "read config.schema.json")
        rr = await c.read_resource("rustybin://cli.md")
        check("rustybin login" in rr.contents[0].text, "read cli.md")

        section("round trip: create, deploy, try_request, delete")
        await create(c, "conf-orders")
        r = await c.call_tool("deploy_mock", {"name": "conf-orders"})
        check(not r.is_error, f"deploy_mock: {text_of(r)[:80]!r}")
        r = await c.call_tool("try_request", {"path": "/mock/conf-orders/orders/7"})
        s = r.structured_content
        check(not r.is_error and s["status"] == 200, f"try_request status {s.get('status')}")
        check(s.get("untrusted_body_json") == {"id": 1, "total": 9.5}, "spec example under untrusted_body_json")
        check('"total"' not in text_of(r), "mocked body kept out of the text")
        r = await c.call_tool("try_request", {"path": "/_rustybin/config"})
        check(r.is_error, "try_request refuses the control plane")

        section("delete without a way to ask: confirm needed")
        r = await c.call_tool("delete_mock", {"name": "conf-orders"})
        check(r.is_error and "confirm" in text_of(r), "refused without confirmation")
        check(await mock_exists(c, "conf-orders"), "mock still there")
        r = await c.call_tool("delete_mock", {"name": "conf-orders", "confirm": True})
        check(not r.is_error, "deleted with confirm: true")
        check(not await mock_exists(c, "conf-orders"), "mock gone")


async def build_instance() -> None:
    """The build_instance walk: from a brief to a published, exported instance."""
    async with client(ADMIN) as c:
        section("build_instance: the AI provider")
        r = await c.call_tool("assist_status", {})
        s = r.structured_content
        check(not r.is_error and s["enabled"] is True and s["provider"] == "mock", "assist on (mock provider)")
        check(all("value" not in (p.get("key") or {}) for p in s["providers"]), "keys shown as {set, hint} only")

        section("build_instance: describe and create")
        r = await c.call_tool("generate_spec", {"description": "a parcel tracking API with shipments and events"})
        s = r.structured_content
        check(not r.is_error and "openapi" in s["text"] and "mock" not in s, "a spec without creating anything")
        r = await c.call_tool("generate_spec", {"description": "a parcel tracking API", "create": True, "name": "conf-parcels"})
        check(r.is_error and "confirm" in text_of(r), "creating is confirmed")
        r = await c.call_tool("generate_spec", {"description": "a parcel tracking API", "create": True,
                                                "name": "conf-parcels", "confirm": True})
        check(not r.is_error and r.structured_content["mock"]["name"] == "conf-parcels", f"created: {text_of(r)[:80]!r}")

        section("build_instance: coverage and examples")
        r = await c.call_tool("check_mock", {"name": "conf-parcels"})
        check(not r.is_error and "coverage" in r.structured_content, "check_mock carries the coverage")
        r = await c.call_tool("generate_examples", {"name": "conf-parcels", "source": "generated", "apply": True})
        check(r.is_error and "confirm" in text_of(r), "storing examples is confirmed")
        r = await c.call_tool("generate_examples", {"name": "conf-parcels", "source": "generated", "apply": True, "confirm": True})
        check(not r.is_error and r.structured_content["applied"] is True, f"examples stored: {text_of(r)[:80]!r}")
        r = await c.call_tool("edit_spec_with_ai", {"name": "conf-parcels", "instruction": "add a DELETE /shipments/{id} operation"})
        check(not r.is_error and r.structured_content["applied"] is False, "an edit is previewed, not saved")
        r = await c.call_tool("deploy_mock", {"name": "conf-parcels"})
        check(not r.is_error, "deployed")

        section("build_instance: the portal")
        r = await c.call_tool("publish_portal_item", {"id": "mock:conf-parcels", "title": "Parcels", "tags": ["demo"], "confirm": True})
        check(not r.is_error and r.structured_content["published"]["title"] == "Parcels", "published")
        r = await c.call_tool("list_portal_items", {})
        items = {i["id"]: i for i in r.structured_content["items"]}
        check(items.get("mock:conf-parcels", {}).get("published") is not None, "listed as published")

        section("build_instance: export, git, agents, settings")
        r = await c.call_tool("export_config", {})
        s = r.structured_content
        check(not r.is_error and s["version"] == 2 and "conf-parcels" in s["yaml"], "version 2 export with the mock")
        check("portal" in s["sections"], "the portal section is in the export")
        r = await c.call_tool("git_status", {})
        check(not r.is_error and "No repository configured" in text_of(r), "git_status without a remote")
        r = await c.call_tool("git_push", {})
        check(r.is_error and "confirm" in text_of(r), "git_push is confirmed")
        r = await c.call_tool("list_agents", {})
        agents = r.structured_content["agents"]
        check(not r.is_error and len(agents) >= 1, f"{len(agents)} agent(s) listed")
        me = next((a for a in agents if a["credential"].get("kind") == "admin"), agents[0])
        r = await c.call_tool("get_agent", {"id": me["id"]})
        check(not r.is_error and r.structured_content["calls"]["total"] >= 1, "get_agent")
        r = await c.call_tool("revoke_agent", {"id": me["id"], "confirm": True})
        check(r.is_error and "revoke itself" in text_of(r), "an agent cannot revoke itself")
        r = await c.call_tool("update_settings", {"values": {"assist.openai_key": "sk-nope"}})
        check(r.is_error and "secret" in text_of(r), "secrets are refused as inputs")
        r = await c.call_tool("mcp_call", {"target": "/mock/conf-parcels/_rustybin/mcp", "method": "tools/list"})
        s = r.structured_content
        check(not r.is_error and len(s["untrusted_result"]["tools"]) > 0 and "result" not in s, "mcp_call answers under untrusted_result")
        r = await c.call_tool("mcp_call", {"target": "https://mcp.example.com/mcp", "method": "tools/list"})
        check(r.is_error and "confirm" in text_of(r), "a call to another host is confirmed")

        section("build_instance: cleanup")
        r = await c.call_tool("unpublish_portal_item", {"id": "mock:conf-parcels"})
        check(not r.is_error, "unpublished")
        r = await c.call_tool("delete_mock", {"name": "conf-parcels", "confirm": True})
        check(not r.is_error, "deleted")


async def elicitation_modern() -> None:
    section("2026-07-28: delete confirmed through elicitation (input_required)")
    async with client(ADMIN, elicitation_callback=answer("decline")) as c:
        await create(c, "conf-elicit")
        r = await c.call_tool("delete_mock", {"name": "conf-elicit"})
        check(r.is_error and "did not confirm" in text_of(r), "declined: nothing deleted")
        check(await mock_exists(c, "conf-elicit"), "mock still there")
    async with client(ADMIN, elicitation_callback=answer("accept")) as c:
        r = await c.call_tool("delete_mock", {"name": "conf-elicit"})
        check(not r.is_error, f"accepted: {text_of(r)[:80]!r}")
        check(not await mock_exists(c, "conf-elicit"), "mock gone")


async def elicitation_legacy() -> None:
    section("2025-11-25 session: delete confirmed through elicitation/create")
    sdk_session.LATEST_HANDSHAKE_VERSION = "2025-11-25"
    async with client(ADMIN, mode="legacy", elicitation_callback=answer("accept")) as c:
        check(c.protocol_version == "2025-11-25", f"negotiated 2025-11-25 (got {c.protocol_version})")
        check(c.server_info.name == "rustybin-manage", "serverInfo name")
        await create(c, "conf-legacy")
        r = await c.call_tool("delete_mock", {"name": "conf-legacy"})
        check(not r.is_error, f"accepted: {text_of(r)[:80]!r}")
        check(not await mock_exists(c, "conf-legacy"), "mock gone")


async def main() -> None:
    await anonymous()
    await admin()
    await build_instance()
    await elicitation_modern()
    await elicitation_legacy()


if __name__ == "__main__":
    run(main)
