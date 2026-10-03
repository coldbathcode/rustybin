"""MCP 2026-07-28 (stateless) over Streamable HTTP with the official Python SDK.

The SDK's default `mode="auto"` probes `server/discover`, adopts 2026-07-28 and
then sends every request with the `_meta` envelope plus MCP-Protocol-Version,
Mcp-Method, Mcp-Name and Mcp-Param-* headers (validated by the server).
"""

import anyio
import mcp_types as types
from mcp import Client
from mcp.shared.exceptions import MCPError

from common import BASE, check, run, section, text_of


async def elicitation_cb(context, params):
    return types.ElicitResult(action="accept", content={"confirm": True, "comment": "from sdk"})


async def sampling_cb(context, params):
    return types.CreateMessageResult(
        role="assistant",
        content=types.TextContent(type="text", text="sampled by the conformance client"),
        model="conformance-model",
        stop_reason="endTurn",
    )


async def main() -> None:
    logs: list = []

    async def logging_cb(params):
        logs.append(params)

    async with Client(
        f"{BASE}/mcp",
        elicitation_callback=elicitation_cb,
        sampling_callback=sampling_cb,
        logging_callback=logging_cb,
        log_level="info",
    ) as c:
        section("discover")
        check(c.protocol_version == "2026-07-28", f"negotiated 2026-07-28 (got {c.protocol_version})")
        check(c.server_info is not None and c.server_info.name == "rustybin-mcp", "serverInfo name")
        check(c.server_capabilities.tools is not None, "tools capability")

        section("lists")
        tools = await c.list_tools()
        names = [t.name for t in tools.tools]
        check(len(names) >= 18, f"{len(names)} tools listed")
        weather = next(t for t in tools.tools if t.name == "get_weather")
        check(weather.output_schema is not None and weather.annotations.read_only_hint, "get_weather schema + annotations")
        resources = await c.list_resources()
        check(any(r.uri == "rustybin://docs/readme" for r in resources.resources), "resources/list has readme")
        templates = await c.list_resource_templates()
        check(len(templates.resource_templates) == 2, "two resource templates")
        prompts = await c.list_prompts()
        check({p.name for p in prompts.prompts} == {"summarize", "code_review", "incident_report"}, "three prompts")

        section("tools")
        r = await c.call_tool("echo", {"message": "hello"})
        check(text_of(r) == "hello" and r.structured_content["length"] == 5, "echo")
        r = await c.call_tool("add", {"a": 2, "b": 40})
        check(r.structured_content["sum"] == 42, "add")
        r = await c.call_tool("calculate", {"expression": "(2 + 3) * 4"})
        check(r.structured_content["result"] == 20, "calculate")
        r = await c.call_tool("get_weather", {"city": "Paris", "units": "imperial"})
        check(r.structured_content["unit"] == "F", "get_weather (Mcp-Param-City header)")
        r = await c.call_tool("lookup_customer", {"customer_id": "u1"})
        check(r.structured_content["customer"]["name"] == "Alice Chen", "lookup_customer (Mcp-Param-Customer-Id)")
        r = await c.call_tool("search_orders", {"status": "shipped"})
        check(r.structured_content["count"] == 2, "search_orders")
        r = await c.call_tool("get_time", {"timezone": "Asia/Tokyo"})
        check(r.structured_content["utcOffset"] == "+09:00", "get_time")

        progress: list = []

        async def on_progress(p, total, message):
            progress.append((p, total, message))

        r = await c.call_tool("slow_task", {"duration_ms": 400, "steps": 4}, progress_callback=on_progress)
        check(r.structured_content["completed"] is True, "slow_task result")
        check([p[0] for p in progress] == [1, 2, 3, 4], f"slow_task progress {progress}")
        check(any(getattr(l, "level", "") == "info" for l in logs), f"log messages via _meta logLevel ({len(logs)})")

        r = await c.call_tool("fail", {})
        check(r.is_error, "fail returns isError")
        try:
            await c.call_tool("throw", {"code": -32050, "message": "boom"})
            check(False, "throw raises")
        except MCPError as e:
            check(e.code == -32050, f"throw raises JSON-RPC error {e.code}")
        r = await c.call_tool("large_output", {"kb": 32})
        check(len(text_of(r)) == 32 * 1024, "large_output 32 KB")
        r = await c.call_tool("generate_image", {"prompt": "a red fox", "size": 8})
        check(isinstance(r.content[0], types.ImageContent) and r.content[0].mime_type == "image/png", "generate_image")
        r = await c.call_tool("fetch_resource_link", {"uri": "rustybin://data/customers.json"})
        check(any(isinstance(x, types.ResourceLink) for x in r.content), "fetch_resource_link")
        r = await c.call_tool("prompt_injection_demo", {})
        check("TEST DATA" in text_of(r), "prompt_injection_demo is labelled")
        r = await c.call_tool("inspect_request", {})
        hdrs = r.structured_content["headers"]
        check(hdrs.get("mcp-method") == "tools/call" and hdrs.get("mcp-name") == "inspect_request", "inspect_request sees routing headers")
        r = await c.call_tool("elicit_confirmation", {"action": "ship it"})
        check("User confirmed" in text_of(r), f"elicitation via multi round-trip: {text_of(r)!r}")
        r = await c.call_tool("sample_llm", {"prompt": "say hi"})
        check("sampled by the conformance client" in text_of(r), "sampling via multi round-trip")

        section("resources")
        rr = await c.read_resource("rustybin://docs/readme")
        check("Rustybin MCP" in rr.contents[0].text, "read readme")
        rr = await c.read_resource("rustybin://images/logo.png")
        check(isinstance(rr.contents[0], types.BlobResourceContents), "read blob")
        rr = await c.read_resource("rustybin://customers/u3")
        check("Carol" in rr.contents[0].text, "read template customer")
        rr = await c.read_resource("rustybin://weather/New%20York")
        check("New York" in rr.contents[0].text, "read template weather")
        try:
            await c.read_resource("rustybin://missing")
            check(False, "missing resource errors")
        except MCPError as e:
            check(e.code == -32602, f"missing resource -> -32602 ({e.code})")

        section("prompts + completion")
        p = await c.get_prompt("code_review", {"code": "print(1)", "language": "python"})
        check(len(p.messages) == 2, "code_review prompt")
        p = await c.get_prompt("incident_report", {"service": "payments"})
        check(any(isinstance(m.content, types.EmbeddedResource) for m in p.messages), "incident_report embeds resource")
        comp = await c.complete(types.PromptReference(type="ref/prompt", name="code_review"), {"name": "language", "value": "ru"})
        check(comp.completion.values == ["ruby", "rust"], f"prompt completion {comp.completion.values}")
        comp = await c.complete(
            types.ResourceTemplateReference(type="ref/resource", uri="rustybin://customers/{id}"),
            {"name": "id", "value": "u"},
        )
        check(len(comp.completion.values) == 5, "template completion")

        section("subscriptions/listen")
        async with c.listen(resource_subscriptions=["rustybin://clock"]) as sub:
            check(list(sub.honored.resource_subscriptions or []) == ["rustybin://clock"], "listen acknowledged")
            with anyio.fail_after(12):
                event = await sub.__anext__()
            check(getattr(event, "uri", None) == "rustybin://clock", f"resource updated event {event}")

    section("pagination (?page_size=4)")
    async with Client(f"{BASE}/mcp?page_size=4") as c:
        seen, cursor = [], None
        for _ in range(20):
            page = await c.list_tools(cursor=cursor)
            seen += [t.name for t in page.tools]
            cursor = page.next_cursor
            if not cursor:
                break
        check(len(seen) == len(names) and len(set(seen)) == len(seen), f"paginated {len(seen)} tools")


if __name__ == "__main__":
    run(main)
