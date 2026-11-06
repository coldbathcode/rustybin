# Step 03: The rest of the anonymous surface

## Goal

Read what an anonymous caller gets from every route, in full, and close what the threat model does
not allow.

## Model

- `inspector::keeps_runs`: on a public instance only `mcp.connections` keeps and reads saved check
  runs; a visitor's run is answered and not kept.
- `GET /_rustybin/metrics` needs `settings.read` on a public instance (a row of `BODY_CHECKS`).
- A public instance that believes every forwarding header warns at startup and in the `client_address`
  diagnostic. The fix itself is the proxy's: one header, named.

## Surfaces

No new route, tool or setting. `run_mcp_checks` and `list_mcp_check_runs` follow the REST rule. The
console's Saved tests view says once that the tests are the operator's. The new diagnostic reaches
`rustybin doctor`, `diagnose_instance` and Management, Diagnostics through the report.

## Tests

`a_visitors_check_runs_are_answered_and_never_kept`, `a_public_instance_keeps_its_metrics_for_the_operator`,
`a_public_instance_that_believes_every_forwarding_header_is_warned`; the Hurl examples
`ss_metrics_refused` and `ss_checks_not_kept`.

## Build notes (built 2026-11-06)

- **The address was the visitor's to choose, the highest finding here**: with `any`, `Fly-Client-IP`
  and `Forwarded` come first and neither Cloudflare nor the demo's Caddyfile touches them, so one
  header stepped around the rate limit, the visits per day and the sessions per address. Replayed
  through a real Caddy. Without it, `X-Forwarded-For` as Caddy sends it ends in Cloudflare's edge, a
  public address, so `pick_client` would have counted all of an edge's visitors as one. Fixed in the
  proxy, not here: `header_up X-Real-IP {client_ip}` with `RUSTYBIN_TRUST_FORWARD_HEADER=x-real-ip`,
  also replayed: `Fly-Client-IP`, `Forwarded`, `X-Real-IP` and `X-Forwarded-For` from the client are
  all ignored. `any` stays the default: it is right on Fly, and changing it would break those.
- **Metrics**: the platform's runbook already says they need the admin token, so the open route was a
  mismatch, not a choice. They hold no identities, only totals of what visitors do.
- **Check runs**: a decision of section 6, taken here. Scoping runs to a session would need a new
  field on a stored run for a feature whose only reachable targets are this instance's own servers.
  Not keeping them closes both problems, and the visitor still gets the report.
- **Hostname, left**: `GET /identity` and the landing page publish it on purpose for load balancer
  tests, so hiding it in `/status` would hide nothing. On the demo it is a container id; `hostname:
  demo` in the compose file settles it (step 05).
- **`/_rustybin/config`, left**: read in full, it holds settings and whether secrets are set, never a
  value.
- **Every other anonymous answer read**: 33 `GET`s answer `200`; of the writes, 80 answer `401`, and
  what is left is what a visitor may do (creating a visit or a mock, previews, validation, the relay
  that refuses every target). A placeholder id answered `400` on the portal routes, so those were
  called again with real ids, along with the mock, settings and document routes: all `401`.
- **Also found**: `rustybin-mock.dev` is not registered yet, so step 06 starts with buying it.
