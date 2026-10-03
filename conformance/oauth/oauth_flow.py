"""OAuth 2.0 / OIDC conformance walk-through against a running Rustybin.

Exercises what an MCP-style client does: RFC 8414 metadata, RFC 7591 dynamic
registration (public client), authorization code + PKCE (S256) with a
`resource` indicator and nonce, refresh token rotation, userinfo,
introspection and revocation, plus RFC 8693 token exchange.

Run (server on http://127.0.0.1:18470):
    RUSTYBIN_HTTP_PORT=18470 RUSTYBIN_HTTPS_PORT=18471 RUSTYBIN_GRPC_PORT=18472 cargo run &
    python conformance/oauth/oauth_flow.py [base_url]

Requires httpx. Exits non-zero on the first failed check.
"""

import base64
import hashlib
import json
import secrets
import sys
from urllib.parse import parse_qs, urlparse

import httpx

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18470"
REDIRECT = "http://127.0.0.1:9999/callback"
RESOURCE = "https://mcp.example.com/mcp"


def check(cond, msg):
    if not cond:
        print(f"FAIL: {msg}")
        sys.exit(1)
    print(f"ok   {msg}")


def claims(jwt):
    payload = jwt.split(".")[1]
    payload += "=" * (-len(payload) % 4)
    return json.loads(base64.urlsafe_b64decode(payload))


def main():
    c = httpx.Client(base_url=BASE, follow_redirects=False, timeout=10)

    meta = c.get("/.well-known/oauth-authorization-server").json()
    check("S256" in meta["code_challenge_methods_supported"], "AS metadata advertises S256")
    oidc = c.get("/.well-known/openid-configuration").json()
    check(oidc["issuer"] == meta["issuer"], "OIDC and RFC 8414 issuers agree")
    path = lambda url: urlparse(url).path  # noqa: E731

    reg = c.post(
        path(meta["registration_endpoint"]),
        json={
            "client_name": "conformance",
            "redirect_uris": [REDIRECT],
            "grant_types": ["authorization_code", "refresh_token"],
            "token_endpoint_auth_method": "none",
        },
    )
    check(reg.status_code == 201, "dynamic client registration returns 201")
    client_id = reg.json()["client_id"]

    verifier = secrets.token_urlsafe(48)
    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
    authz = {
        "response_type": "code",
        "client_id": client_id,
        "redirect_uri": REDIRECT,
        "scope": "openid profile email",
        "state": "st-1",
        "nonce": "nonce-1",
        "code_challenge": challenge,
        "code_challenge_method": "S256",
        "resource": RESOURCE,
    }
    page = c.get(path(meta["authorization_endpoint"]), params=authz)
    check(page.status_code == 200 and "<form" in page.text, "login page rendered")
    resp = c.post(path(meta["authorization_endpoint"]), data={**authz, "username": "alice", "password": "alice"})
    check(resp.status_code == 303, "login redirects")
    q = parse_qs(urlparse(resp.headers["location"]).query)
    check(q["state"] == ["st-1"], "state round-trips")
    code = q["code"][0]

    tok = c.post(
        path(meta["token_endpoint"]),
        data={
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": REDIRECT,
            "client_id": client_id,
            "code_verifier": verifier,
            "resource": RESOURCE,
        },
    )
    check(tok.status_code == 200, "code exchanged with PKCE")
    check(tok.headers.get("cache-control") == "no-store", "token response is no-store")
    t = tok.json()
    check(claims(t["access_token"])["aud"] == RESOURCE, "access token aud = resource")
    idt = claims(t["id_token"])
    check(idt["aud"] == client_id and idt["nonce"] == "nonce-1", "id_token aud and nonce")

    reuse = c.post(
        path(meta["token_endpoint"]),
        data={"grant_type": "authorization_code", "code": code, "redirect_uri": REDIRECT,
              "client_id": client_id, "code_verifier": verifier},
    )
    check(reuse.json().get("error") == "invalid_grant", "code is single use")

    ui = c.get(path(oidc["userinfo_endpoint"]), headers={"Authorization": f"Bearer {t['access_token']}"})
    check(ui.json().get("email") == "alice@rustybin.local", "userinfo returns the user")

    ref = c.post(path(meta["token_endpoint"]), data={
        "grant_type": "refresh_token", "refresh_token": t["refresh_token"], "client_id": client_id})
    check(ref.status_code == 200 and ref.json()["refresh_token"] != t["refresh_token"], "refresh token rotates")

    auth = ("rustybin", "secret")
    intro = c.post(path(meta["introspection_endpoint"]), data={"token": t["access_token"]}, auth=auth).json()
    check(intro["active"] and intro["client_id"] == client_id, "introspection active")
    c.post(path(meta["revocation_endpoint"]), data={"token": ref.json()["refresh_token"], "client_id": client_id})
    intro = c.post(path(meta["introspection_endpoint"]), data={"token": ref.json()["refresh_token"]}, auth=auth).json()
    check(intro == {"active": False}, "revoked refresh token is inactive")

    cc = c.post(path(meta["token_endpoint"]), data={"grant_type": "client_credentials"}, auth=auth).json()
    tx = c.post(path(meta["token_endpoint"]), auth=auth, data={
        "grant_type": "urn:ietf:params:oauth:grant-type:token-exchange",
        "subject_token": t["access_token"],
        "subject_token_type": "urn:ietf:params:oauth:token-type:access_token",
        "actor_token": cc["access_token"],
        "actor_token_type": "urn:ietf:params:oauth:token-type:access_token",
        "audience": "downstream",
    })
    check(tx.status_code == 200 and claims(tx.json()["access_token"])["act"]["sub"] == "rustybin",
          "token exchange with delegation")
    forged = t["access_token"][:-4] + "AAAA"
    bad = c.post(path(meta["token_endpoint"]), auth=auth, data={
        "grant_type": "urn:ietf:params:oauth:grant-type:token-exchange",
        "subject_token": forged,
        "subject_token_type": "urn:ietf:params:oauth:token-type:access_token",
    })
    check(bad.json().get("error") == "invalid_request", "forged subject token rejected")
    print("all OAuth checks passed")


if __name__ == "__main__":
    main()
