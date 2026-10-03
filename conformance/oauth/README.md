# OAuth / OIDC conformance

`oauth_flow.py` walks through what an MCP-style client does against the built-in
identity provider: RFC 8414 metadata, RFC 7591 dynamic registration, authorization
code + PKCE with a `resource` indicator and nonce, refresh token rotation, userinfo,
introspection, revocation and RFC 8693 token exchange.

```bash
PYTHON=/path/to/venv/bin/python conformance/oauth/run.sh   # builds, starts on :18470, runs the checks
```

Against a running server: `python conformance/oauth/oauth_flow.py http://127.0.0.1:8080` (needs `httpx`).
