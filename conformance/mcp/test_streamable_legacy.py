"""Handshake-era MCP (initialize + Mcp-Session-Id) over Streamable HTTP with the
official Python SDK, for protocol versions 2025-11-25, 2025-06-18 and 2025-03-26.

The SDK always offers its newest handshake version; to exercise older ones the
script patches the version the client offers in `initialize`.
"""

import anyio
import mcp.client.session as sdk_session
import mcp_types as types
from mcp import Client
from mcp.shared.exceptions import MCPError

from common import BASE, check, run, section, text_of

VERSIONS = ["2025-11-25", "2025-06-18", "2025-03-26"]


async def elicitation_cb(context, params):
    return types.ElicitResult(action="accept", content={"confirm": True})


async def sampling_cb(context, params):
    return types.CreateMessageResult(
        role="assistant",
        content=types.TextContent(type="text", text="legacy sampling ok"),
        model="conformance-model",
        stop_reason="endTurn",
    )


async def exercise(version: str) -> None:
    sdk_session.LATEST_HANDSHAKE_VERSION = version
    logs: list = []
    updates: list = []

    async def logging_cb(params):
        logs.append(params)

    async def message_handler(msg):
        if isinstance(msg, types.ResourceUpdatedNotification):
            updates.append(msg.params.uri)

    async with Client(
        f"{BASE}/mcp",
        mode="legacy",
        elicitation_callback=elicitation_cb,
        sampling_callback=sampling_cb,
        logging_callback=logging_cb,
        message_handler=message_handler,
    ) as c:
        section(f"{version}: initialize")
        check(c.protocol_version == version, f"negotiated {version} (got {c.protocol_version})")
        check(c.server_info.name == "rustybin-mcp", "serverInfo")
        await c.send_ping()
        check(True, "ping")

        section(f"{version}: lists")
        tools = await c.list_tools()
        names = [t.name for t in tools.tools]
        check(len(names) >= 18, f"{len(names)} tools")
        resources = await c.list_resources()
        check(len(resources.resources) == 4, "4 resources")
        prompts = await c.list_prompts()
        check(len(prompts.prompts) == 3, "3 prompts")

        section(f"{version}: tools")
        r = await c.call_tool("get_weather", {"city": "Lisbon"})
        check("Lisbon" in text_of(r), "get_weather")
        if version >= "2025-06-18":
            check(r.structured_content is not None and r.structured_content["city"] == "Lisbon", "structuredContent")
        await c.set_logging_level("info")
        progress: list = []

        async def on_progress(p, total, message):
            progress.append(p)

        r = await c.call_tool("slow_task", {"duration_ms": 300, "steps": 3}, progress_callback=on_progress)
        check(progress == [1, 2, 3], f"progress notifications {progress}")
        check(len(logs) >= 3, f"log notifications after logging/setLevel ({len(logs)})")
        r = await c.call_tool("fail", {})
        check(r.is_error, "fail -> isError")
        try:
            await c.call_tool("throw", {})
            check(False, "throw raises")
        except MCPError as e:
            check(e.code == -32603, "throw -> -32603")
        r = await c.call_tool("sample_llm", {"prompt": "hi"})
        check("legacy sampling ok" in text_of(r), f"sampling/createMessage server request: {text_of(r)!r}")
        r = await c.call_tool("elicit_confirmation", {"action": "restart"})
        if version >= "2025-06-18":
            check("User confirmed" in text_of(r), f"elicitation/create server request: {text_of(r)!r}")
        else:
            check("did not declare" in text_of(r), "elicitation explained before 2025-06-18")
        r = await c.call_tool("fetch_resource_link", {})
        if version >= "2025-06-18":
            check(any(isinstance(x, types.ResourceLink) for x in r.content), "resource_link content")
        else:
            check(any(isinstance(x, types.EmbeddedResource) for x in r.content), "embedded resource fallback")
        r = await c.call_tool("inspect_request", {})
        check(text_of(r).find("mcp-session-id") >= 0, "inspect_request sees Mcp-Session-Id")

        section(f"{version}: resources + prompts")
        rr = await c.read_resource("rustybin://data/customers.json")
        check("alice@example.com" in rr.contents[0].text, "read customers.json")
        try:
            await c.read_resource("rustybin://missing")
            check(False, "missing resource errors")
        except MCPError as e:
            check(e.code == -32002, f"missing resource -> -32002 ({e.code})")
        p = await c.get_prompt("summarize", {"text": "Rust is fast.", "style": "tldr"})
        check("tldr" in p.messages[0].content.text, "summarize prompt")
        comp = await c.complete(types.PromptReference(type="ref/prompt", name="incident_report"), {"name": "severity", "value": "sev"})
        check(len(comp.completion.values) == 4, "completion")

        section(f"{version}: resources/subscribe + GET stream")
        await c.subscribe_resource("rustybin://clock")
        with anyio.fail_after(12):
            while not updates:
                await anyio.sleep(0.1)
        check(updates[0] == "rustybin://clock", "notifications/resources/updated on the GET stream")
        await c.unsubscribe_resource("rustybin://clock")


async def main() -> None:
    for v in VERSIONS:
        await exercise(v)


if __name__ == "__main__":
    run(main)
