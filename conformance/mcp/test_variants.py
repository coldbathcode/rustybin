"""Gateway-demo variants with the official Python SDK:
- /mcp/protected: 401 challenge, RFC 9728 metadata, bearer token from the built-in IdP,
  403 insufficient_scope step-up for destructive tools
- /mcp/apikey: X-API-Key
- /mcp/servers/{weather,crm,devtools}: tool subsets
"""

import base64
import json

import httpx2
from mcp import Client
from mcp.client.streamable_http import streamable_http_client
from mcp.shared._httpx_utils import create_mcp_http_client

from common import BASE, check, run, section, text_of


def jwt_claims(token: str) -> dict:
    payload = token.split(".")[1]
    payload += "=" * (-len(payload) % 4)
    return json.loads(base64.urlsafe_b64decode(payload))


def client_for(path: str, headers: dict | None = None, mode: str = "auto") -> Client:
    http = create_mcp_http_client(headers=headers or {})
    return Client(streamable_http_client(f"{BASE}{path}", http_client=http), mode=mode)


async def protected() -> None:
    section("/mcp/protected: discovery")
    async with httpx2.AsyncClient() as http:
        r = await http.post(
            f"{BASE}/mcp/protected",
            json={"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
            headers={"accept": "application/json, text/event-stream"},
        )
        check(r.status_code == 401, f"unauthenticated -> 401 ({r.status_code})")
        challenge = r.headers.get("www-authenticate", "")
        check(challenge.startswith("Bearer ") and "resource_metadata=" in challenge, f"challenge: {challenge}")
        prm_url = challenge.split('resource_metadata="')[1].split('"')[0]
        prm = (await http.get(prm_url)).json()
        resource = prm["resource"]
        check(resource == f"{BASE}/mcp/protected", f"PRM resource {resource}")
        issuer = prm["authorization_servers"][0]
        meta = (await http.get(f"{issuer}/.well-known/openid-configuration")).json()
        token_endpoint = meta["token_endpoint"]
        check(token_endpoint.endswith("/oauth/token"), "authorization server metadata")

        async def token(scope: str) -> str:
            resp = await http.post(
                token_endpoint,
                data={
                    "grant_type": "client_credentials",
                    "client_id": "rustybin",
                    "client_secret": "rustybin",
                    "scope": scope,
                    "resource": resource,
                },
            )
            return resp.json()["access_token"]

        reader = await token("mcp:tools")
        writer = await token("mcp:tools mcp:tools:write")
        aud = jwt_claims(reader).get("aud")
        if resource not in (aud if isinstance(aud, list) else [aud]):
            print(f"  note: IdP token aud is {aud!r} (no RFC 8707 binding yet); server must accept it via RUSTYBIN_MCP_ACCEPTED_AUDIENCES")

    section("/mcp/protected: SDK with bearer token")
    async with client_for("/mcp/protected", {"Authorization": f"Bearer {reader}"}) as c:
        check(c.protocol_version == "2026-07-28", "modern session over bearer auth")
        tools = await c.list_tools()
        check(any(t.name == "cancel_order" for t in tools.tools), "tools listed")
        r = await c.call_tool("inspect_request", {})
        check(r.structured_content["tokenClaims"]["scope"] == "mcp:tools", "token claims visible to tools")
        try:
            await c.call_tool("cancel_order", {"order_id": "o5"})
            check(False, "cancel_order without write scope is rejected")
        except Exception as e:  # noqa: BLE001 - the SDK surfaces the 403 as an error
            check("403" in str(e) or "scope" in str(e).lower() or "Forbidden" in str(e), f"403 insufficient_scope surfaced: {type(e).__name__}")
    async with client_for("/mcp/protected", {"Authorization": f"Bearer {writer}"}, mode="legacy") as c:
        r = await c.call_tool("cancel_order", {"order_id": "o5"})
        check(r.structured_content["status"] == "cancelled", "cancel_order with mcp:tools:write (legacy session)")

    async with httpx2.AsyncClient() as http:
        r = await http.post(
            f"{BASE}/mcp/protected",
            json={"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "cancel_order", "arguments": {"order_id": "o5"},
                "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28",
                          "io.modelcontextprotocol/clientCapabilities": {}}}},
            headers={
                "accept": "application/json, text/event-stream",
                "authorization": f"Bearer {reader}",
                "mcp-protocol-version": "2026-07-28",
                "mcp-method": "tools/call",
                "mcp-name": "cancel_order",
            },
        )
        check(r.status_code == 403, "raw 403")
        check('error="insufficient_scope"' in r.headers.get("www-authenticate", ""), "insufficient_scope challenge")


async def apikey() -> None:
    section("/mcp/apikey")
    async with client_for("/mcp/apikey", {"X-API-Key": "conformance"}) as c:
        r = await c.call_tool("echo", {"message": "keyed"})
        check(text_of(r) == "keyed", "call with X-API-Key")
        r = await c.call_tool("inspect_request", {})
        check("masked" in r.structured_content["headers"]["x-api-key"], "API key masked in inspect_request")
    try:
        async with client_for("/mcp/apikey") as c:
            await c.list_tools()
        check(False, "missing key rejected")
    except Exception:  # noqa: BLE001
        check(True, "missing key rejected")


async def named() -> None:
    expected = {
        "weather": {"get_weather", "get_time", "inspect_request"},
        "crm": {"lookup_customer", "search_orders", "cancel_order", "inspect_request"},
    }
    for name, tools in expected.items():
        section(f"/mcp/servers/{name}")
        async with client_for(f"/mcp/servers/{name}") as c:
            listed = {t.name for t in (await c.list_tools()).tools}
            check(listed == tools, f"{name} tools {sorted(listed)}")
            check(c.server_info.name == f"rustybin-mcp-{name}", "server name")
    section("/mcp/servers/devtools")
    async with client_for("/mcp/servers/devtools", mode="legacy") as c:
        listed = {t.name for t in (await c.list_tools()).tools}
        check("slow_task" in listed and "get_weather" not in listed, "devtools subset")


async def main() -> None:
    await protected()
    await apikey()
    await named()


if __name__ == "__main__":
    run(main)
