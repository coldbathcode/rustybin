# Step 06: Egress pinning and the git transport

## Goal

The address the policy checked is the address the socket connects to, for every outbound feature,
and git cannot leave the remote the operator configured.

## Findings

| Sev | Where | Problem | Fix |
|---|---|---|---|
| high, verify first | `src/egress.rs:452,863` | `parse_url` strips a trailing dot and pins `evil.com`; callers send the original URL (`evil.com.`), the reqwest override lookup is exact, so the connect re-resolves through system DNS: DNS rebinding to metadata or loopback where the allowlist has CIDR or wildcard entries, or A2A push allows all | rebuild the request URL from the parsed target, or a custom `dns_resolver` that refuses any name not in the pinned map (preferred: it closes the class) |
| medium | `src/egress.rs:850-864` | the client honours `HTTP_PROXY` and `ALL_PROXY`; the proxy does its own DNS, so pinning and forbidden addresses do nothing | `.no_proxy()`; an explicit egress proxy setting if ever wanted |
| low | `src/egress.rs:338-374` | `forbidden_address` misses NAT64 `64:ff9b::/96`, 6to4 `2002::/16`, IPv4-compatible `::a.b.c.d`, and the metadata addresses 100.100.100.200 and 192.0.0.192; a wildcard name entry reaches RFC 1918 addresses | embed and check the IPv4 part; add the addresses; refuse private ranges for wildcard name matches unless a CIDR entry covers them |
| low | `src/a2a/push.rs:286` | push URLs (query tokens) logged whole | log the origin |
| medium | `src/sourcecontrol/mod.rs` `git_env` | `http.followRedirects=initial` and git's own DNS bypass the pinning; askpass answers any host's password prompt | `http.followRedirects=false`; `http.curloptResolve=<host>:<port>:<ip>` from the pinned address; askpass checks the prompt's host |
| low | `src/sourcecontrol/mod.rs` `parse_remote` | scp-style `helper::address` accepted (`ext::sh` blocked only by git defaults); git inherits the whole environment (`RUSTYBIN_SECRETS_KEY`, provider keys) | reject `::`; `-c protocol.allow=never` plus `https`, `ssh` (and `file` when loopback is allowed) set to `always`; `env_clear()` plus `PATH`, `HOME` and the git variables |

## Tests

A test resolver that answers a public address first and loopback second, with `http://name./`,
must connect to the first or refuse (this is the "verify first" test); a `no_proxy` test with
`HTTP_PROXY` set in the test's client builder; `forbidden_address` table tests; a git test that
the child environment holds no `RUSTYBIN_*` variable but the ones it needs.

## Done when

The tests pass and `docs/src/configuration.md` notes that outbound traffic never uses a system proxy.
