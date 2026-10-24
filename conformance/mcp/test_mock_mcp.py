"""The MCP server of an OpenAPI mock (/mock/{name}/_rustybin/mcp) with the
official Python SDK.

Uses the built-in petstore sample: tools/list with annotations, input and
output schemas (the SDK compiles and checks every output schema), a read
and a write call whose structured content the SDK validates, a 404 as a
tool error, mocked security with the API key header sent the way
`claude mcp add --header` sends it, the spec resource, and a 2025-11-25
session.
"""

import json
import os
import urllib.request

from mcp import Client
from mcp.client.streamable_http import streamable_http_client
from mcp.shared._httpx_utils import create_mcp_http_client

from common import BASE, check, run, section, text_of

PATH = "/mock/petstore/_rustybin/mcp"
TOKEN = os.environ.get("MANAGE_ADMIN_TOKEN", "conformance-admin")


def client(headers: dict | None = None, mode: str = "auto") -> Client:
    http = create_mcp_http_client(headers=headers or {})
    return Client(streamable_http_client(f"{BASE}{PATH}", http_client=http), mode=mode)


def patch_petstore(body: dict) -> None:
    req = urllib.request.Request(
        f"{BASE}/_rustybin/mocks/petstore",
        method="PATCH",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {TOKEN}"},
    )
    with urllib.request.urlopen(req, timeout=10) as r:
        assert r.status == 200, r.status


async def modern() -> None:
    section("2026-07-28: petstore tools")
    async with client() as c:
        check(c.protocol_version == "2026-07-28", f"negotiated 2026-07-28 (got {c.protocol_version})")
        tools = {t.name: t for t in (await c.list_tools()).tools}
        check(
            list(tools) == ["listPets", "createPet", "getPetById", "updatePet", "deletePet", "placeOrder"],
            f"one tool per operation: {list(tools)}",
        )
        get = tools["getPetById"]
        check(get.annotations is not None and get.annotations.read_only_hint is True, "getPetById is read only")
        check(tools["deletePet"].annotations.destructive_hint is True, "deletePet is destructive")
        check(get.input_schema.get("required") == ["petId"], "petId is required")
        check(get.output_schema is not None, "getPetById declares an output schema")

        r = await c.call_tool("getPetById", {"petId": 3})
        check(not r.is_error, f"getPetById 3 answers: {text_of(r)[:80]!r}")
        sc = r.structured_content or {}
        check(sc.get("status") == 200 and sc.get("body", {}).get("id") == 3, f"structured {{status, body}}: {sc}")

        r = await c.call_tool("getPetById", {"petId": 0})
        check(r.is_error, "pet 0 is a tool error (the sample's 404 rule)")
        check((r.structured_content or {}).get("status") == 404, "the error carries the 404")

        r = await c.call_tool("listPets", {"limit": 2})
        check(not r.is_error and isinstance((r.structured_content or {}).get("body"), list), "listPets returns a list")

        r = await c.call_tool("placeOrder", {"body": {"petId": 1, "quantity": 2}})
        check(not r.is_error and (r.structured_content or {}).get("status") == 201, f"placeOrder 201: {text_of(r)[:80]!r}")

        resources = [str(x.uri) for x in (await c.list_resources()).resources]
        check(resources == ["rustybin://mock/petstore/openapi.json"], f"the spec resource: {resources}")
        rr = await c.read_resource("rustybin://mock/petstore/openapi.json")
        doc = json.loads(rr.contents[0].text)
        check(doc["info"]["title"] == "Petstore", "the spec reads back")
        check(doc.get("x-rustybin-mcp", {}).get("url", "").endswith(PATH), "the spec names the MCP URL")


async def secured() -> None:
    section("mocked security: credentials from the MCP request")
    patch_petstore({"security": {"enabled": True}, "deploy": True})
    try:
        async with client() as c:
            r = await c.call_tool("createPet", {"body": {"name": "Rex"}})
            check(r.is_error and (r.structured_content or {}).get("status") == 401, "createPet without a key is a 401")
        async with client({"X-API-Key": "demo-key"}) as c:
            r = await c.call_tool("createPet", {"body": {"name": "Rex"}})
            check(not r.is_error and (r.structured_content or {}).get("status") == 201, f"createPet with the key: {text_of(r)[:80]!r}")
    finally:
        patch_petstore({"security": {"enabled": False}, "deploy": True})


async def legacy() -> None:
    section("2025-11-25 session")
    async with client(mode="legacy") as c:
        check(c.protocol_version == "2025-11-25", f"negotiated 2025-11-25 (got {c.protocol_version})")
        tools = [t.name for t in (await c.list_tools()).tools]
        check("getPetById" in tools, "tools listed in the session")
        r = await c.call_tool("getPetById", {"petId": 9})
        check(not r.is_error and (r.structured_content or {}).get("body", {}).get("id") == 9, "call in the session")


async def main() -> None:
    await modern()
    await secured()
    await legacy()


run(main)
