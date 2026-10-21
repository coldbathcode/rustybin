"""OAuth sign-in to the management MCP server with the official Python SDK.

run.sh starts a second server with RUSTYBIN_CONTROL_AUTH=token (MANAGE_OAUTH_BASE):
there /_rustybin/mcp answers 401 with an RFC 9728 resource_metadata pointer, and
the SDK's OAuthClientProvider discovers the built-in authorization server,
registers itself (RFC 7591), and sends the user to the authorization URL. The
redirect handler plays the user: it follows the URL to the console's consent
page, approves the request through the consent API with the admin token (as
the console would), and hands the code, state and iss from the answer to the
callback handler. Covers discovery, a console-scope and an admin-scope sign-in
(tools/list differs), get_instance, and a token refresh.
"""

import os
from urllib.parse import parse_qs, urlparse

import httpx2
from mcp import Client
from mcp.client.auth import AuthorizationCodeResult, OAuthClientProvider
from mcp.client.streamable_http import streamable_http_client
from mcp.shared._httpx_utils import create_mcp_http_client
from mcp.shared.auth import OAuthClientInformationFull, OAuthClientMetadata, OAuthToken

from common import check, run, section

BASE = os.environ.get("MANAGE_OAUTH_BASE", "http://127.0.0.1:18403").rstrip("/")
TOKEN = os.environ.get("MANAGE_ADMIN_TOKEN", "conformance-admin")
ADMIN = {"Authorization": f"Bearer {TOKEN}"}
PATH = "/_rustybin/mcp"
PRM = "/.well-known/oauth-protected-resource/_rustybin/mcp"
AS_METADATA = "/.well-known/oauth-authorization-server/_rustybin/oauth"
CONSENT = "/ui/#/authorize/"


class MemoryStorage:
    """TokenStorage in memory; counts token writes (sign-in, refresh)."""

    def __init__(self) -> None:
        self.tokens: OAuthToken | None = None
        self.client: OAuthClientInformationFull | None = None
        self.token_writes = 0

    async def get_tokens(self) -> OAuthToken | None:
        return self.tokens

    async def set_tokens(self, tokens: OAuthToken) -> None:
        self.tokens = tokens
        self.token_writes += 1

    async def get_client_info(self) -> OAuthClientInformationFull | None:
        return self.client

    async def set_client_info(self, client_info: OAuthClientInformationFull) -> None:
        self.client = client_info


class User:
    """The person at the browser: opens the consent page and approves."""

    def __init__(self, scope: str) -> None:
        self.scope = scope
        self.consent: dict | None = None
        self.result: AuthorizationCodeResult | None = None

    async def redirect(self, url: str) -> None:
        async with httpx2.AsyncClient() as h:
            r = await h.get(url)
            loc = r.headers.get("location", "")
            check(r.status_code == 302 and loc.startswith(CONSENT), f"authorize redirects to the consent page ({loc[:40]})")
            rid = loc[len(CONSENT):]
            g = await h.get(f"{BASE}/_rustybin/oauth/requests/{rid}", headers=ADMIN)
            self.consent = g.json()
            check(g.status_code == 200, "consent request readable with the admin token")
            a = await h.post(
                f"{BASE}/_rustybin/oauth/requests/{rid}",
                headers=ADMIN,
                json={"decision": "approve", "scope": self.scope},
            )
            check(a.status_code == 200, f"approved with scope {self.scope}")
            q = parse_qs(urlparse(a.json()["redirect_to"]).query)
            self.result = AuthorizationCodeResult(
                code=q["code"][0], state=q.get("state", [None])[0], iss=q.get("iss", [None])[0]
            )

    async def callback(self) -> AuthorizationCodeResult:
        assert self.result is not None
        return self.result


def provider(user: User, storage: MemoryStorage) -> OAuthClientProvider:
    return OAuthClientProvider(
        server_url=f"{BASE}{PATH}",
        client_metadata=OAuthClientMetadata(
            client_name="Rustybin conformance",
            redirect_uris=["http://127.0.0.1:9/callback"],
            grant_types=["authorization_code", "refresh_token"],
            response_types=["code"],
            token_endpoint_auth_method="none",
        ),
        storage=storage,
        redirect_handler=user.redirect,
        callback_handler=user.callback,
    )


def client(auth: OAuthClientProvider) -> Client:
    http = create_mcp_http_client(auth=auth)
    return Client(streamable_http_client(f"{BASE}{PATH}", http_client=http))


async def discovery() -> None:
    section("discovery")
    async with httpx2.AsyncClient() as h:
        r = await h.post(
            f"{BASE}{PATH}",
            json={"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}},
            headers={"Accept": "application/json, text/event-stream"},
        )
        challenge = r.headers.get("www-authenticate", "")
        check(r.status_code == 401, "no token: 401")
        check(f'resource_metadata="{BASE}{PRM}"' in challenge, "challenge names the resource metadata")
        prm = (await h.get(f"{BASE}{PRM}")).json()
        check(prm["resource"] == f"{BASE}{PATH}", "PRM resource is /_rustybin/mcp")
        check(prm["authorization_servers"] == [f"{BASE}/_rustybin/oauth"], "PRM names the built-in server")
        meta = (await h.get(f"{BASE}{AS_METADATA}")).json()
        check(meta["issuer"] == f"{BASE}/_rustybin/oauth", "AS metadata issuer")
        check(meta["code_challenge_methods_supported"] == ["S256"], "PKCE S256")
        check(meta["registration_endpoint"] == f"{BASE}/_rustybin/oauth/register", "registration endpoint")


async def console_scope() -> None:
    section("sign in, console scope")
    user, storage = User("console"), MemoryStorage()
    auth = provider(user, storage)
    async with client(auth) as c:
        tools = {t.name for t in (await c.list_tools()).tools}
        check(user.consent is not None and user.consent["client_name"] == "Rustybin conformance", "consent shows the client name")
        check(user.consent is not None and user.consent["redirect_kind"] == "loopback", "loopback redirect")
        check(storage.tokens is not None and storage.tokens.scope == "console", "token scope console")
        check("list_mocks" in tools and "try_request" in tools, "read tools listed")
        check("create_mock" not in tools and "apply_config" not in tools, "changes hidden")
        r = await c.call_tool("get_instance", {})
        caller = r.structured_content["caller"]
        check(not r.is_error and caller["admin"] is False, "get_instance: not admin")

        # Refresh: mark the access token expired, the next call refreshes it.
        before = storage.tokens.access_token if storage.tokens else None
        writes = storage.token_writes
        auth.context.token_expiry_time = 1.0  # in the past (0 would mean "no expiry")
        r = await c.call_tool("list_mocks", {})
        check(not r.is_error, "call after refresh succeeds")
        check(storage.token_writes == writes + 1, "refresh stored new tokens")
        check(storage.tokens is not None and storage.tokens.access_token != before, "new access token")
        check(storage.tokens is not None and storage.tokens.scope == "console", "refresh keeps the scope")


async def admin_scope() -> None:
    section("sign in, admin scope")
    user, storage = User("admin"), MemoryStorage()
    async with client(provider(user, storage)) as c:
        tools = {t.name for t in (await c.list_tools()).tools}
        check("create_mock" in tools and "apply_config" in tools, "mock changes listed")
        r = await c.call_tool("get_instance", {})
        check(not r.is_error and r.structured_content["caller"]["admin"] is True, "get_instance: admin")
        r = await c.call_tool("set_health", {"healthy": True})
        check(not r.is_error, "an admin tool succeeds")
    async with httpx2.AsyncClient() as h:
        tok = storage.tokens.access_token if storage.tokens else ""
        r = await h.get(f"{BASE}/_rustybin/config", headers={"Authorization": f"Bearer {tok}"})
        check(r.status_code == 401, "the token does not open other control-plane routes")


async def main() -> None:
    await discovery()
    await console_scope()
    await admin_scope()


if __name__ == "__main__":
    run(main)
