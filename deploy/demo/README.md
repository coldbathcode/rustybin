# The public demo on a VPS

Visitors create mocks at `demo.rustybin.dev` without an account, each served at
`<name>.rustybin-mock.dev`, and only the operator signs in. The bare `rustybin-mock.dev`
redirects to the demo. `compose.yaml` here pins the signed release by digest.

The console stays on `rustybin.dev` and visitors' mocks on their own registrable domain, so what
a visitor serves never shares a site with the console.

```
visitor -> Cloudflare -> tunnel (cloudflared) -> Caddy -> Rustybin
```

It is its own compose project with its own tunnel, so it shares nothing with what else runs on the
machine. No port is published. Rustybin has no route to the internet: anything the demo should call
needs a network here first. It can still reach ports the host binds on every interface, so the
outbound policy stays what guards those: leave `RUSTYBIN_EGRESS_ALLOW` unset.

## What it needs

A remotely managed Cloudflare Tunnel named `rb-demo`, with these public hostnames, each to
`http://caddy:8080`, and a proxied CNAME for each to `<tunnel id>.cfargotunnel.com`:

- `demo.rustybin.dev`
- `rustybin-mock.dev`
- `*.rustybin-mock.dev`

Proxied is what makes `Cf-Connecting-Ip` the visitor's address, which the per-address limits count.
Caching is off for `rustybin-mock.dev`, so a visitor's edit shows at once.

Two files next to `compose.yaml`, mode 600, never committed:

```bash
umask 077
HASH=$(rustybin password hash)   # prompts, prints the hash and nothing else
printf "RUSTYBIN_CONSOLE_PASSWORD_HASH='%s'\nRUSTYBIN_LEADS_SALT=%s\n" \
  "$HASH" "$(openssl rand -hex 32)" > rustybin.env
printf "TUNNEL_TOKEN=%s\n" "<the tunnel's token>" > tunnel.env
```

The single quotes matter: the hash holds `$`.

## Deploy

```bash
cosign verify "$(sed -n 's/^ *image: \(ghcr.*\)$/\1/p' compose.yaml)" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp '^https://github.com/coldbathcode/rustybin/'
docker compose up -d
```

To upgrade, change the digest and run `docker compose up -d` again. To take the demo offline,
`docker compose down`: the volume keeps its state.

## Check it

- `https://demo.rustybin.dev/_rustybin/diagnostics` signed in: `client_address` and `state_dir` are ok.
- A mock made without signing in answers at `https://<name>.rustybin-mock.dev/`.
- `GET /_rustybin/settings` without a credential answers `401`.
- `GET /ip` with `X-Real-IP: 1.2.3.4` still names your own address, and with `Cf-Connecting-Ip` set
  Cloudflare itself answers `403`.
