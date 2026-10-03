"""Deprecated HTTP+SSE transport (2024-11-05) with the official Python SDK's
`sse_client`: GET /mcp/sse (endpoint event) + POST /mcp/messages?sessionId=...
"""

import mcp.client.session as sdk_session
import mcp_types as types
from mcp import Client
from mcp.client.sse import sse_client

from common import BASE, check, run, section, text_of


async def sampling_cb(context, params):
    return types.CreateMessageResult(
        role="assistant",
        content=types.TextContent(type="text", text="sse sampling ok"),
        model="conformance-model",
    )


async def exercise(version: str) -> None:
    sdk_session.LATEST_HANDSHAKE_VERSION = version
    async with Client(sse_client(f"{BASE}/mcp/sse"), mode="legacy", sampling_callback=sampling_cb) as c:
        section(f"HTTP+SSE, protocol {version}")
        check(c.protocol_version == version, f"negotiated {version} (got {c.protocol_version})")
        tools = await c.list_tools()
        check(len(tools.tools) >= 18, f"{len(tools.tools)} tools")
        r = await c.call_tool("echo", {"message": "via sse"})
        check(text_of(r) == "via sse", "echo")
        progress: list = []

        async def on_progress(p, total, message):
            progress.append(p)

        r = await c.call_tool("slow_task", {"duration_ms": 200, "steps": 2}, progress_callback=on_progress)
        check(progress == [1, 2], f"progress over the SSE stream {progress}")
        r = await c.call_tool("sample_llm", {"prompt": "hello"})
        check("sse sampling ok" in text_of(r), "sampling request over the SSE stream")
        rr = await c.read_resource("rustybin://docs/readme")
        check("Rustybin" in rr.contents[0].text, "read resource")
        p = await c.get_prompt("summarize", {"text": "abc"})
        check(len(p.messages) == 1, "get prompt")
        r = await c.call_tool("inspect_request", {})
        check('"transport": "http+sse"' in text_of(r), "inspect_request reports http+sse")


async def main() -> None:
    for v in ["2024-11-05", "2025-11-25"]:
        await exercise(v)


if __name__ == "__main__":
    run(main)
