# The demo on a VPS

Locked: the console at `demo.rustybin.dev` needs the operator's sign-in, and nobody else has an
account. There are no visitors. The operator's mocks are served at `<name>.rustybin-mock.dev`, and the
bare `rustybin-mock.dev` redirects to the console. `compose.yaml` here pins the signed release by
digest.

The sign-in guards the console and the control plane. The data plane is open, as on every instance:
the landing page, the built-in test endpoints such as `/ip`, and any mock, unless the mock turns on
its own security. Public mode stays on for that reason, so a mock expires after 24 hours.

To open it to visitors, set `RUSTYBIN_MOCKS_SELF_SERVICE`, `_HOST` and `_HOME`
([An operator who signs in](../../docs/src/mocks/self-service.md#an-operator-who-signs-in)) in place
of `RUSTYBIN_MOCKS_HOST`.

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
- A mock made signed in answers at `https://<name>.rustybin-mock.dev/`.
- Without a credential, `GET /_rustybin/settings`, `/status`, `/config` and `/mocks` answer `401`, and
  so does `POST /_rustybin/mocks`.
- `GET /ip` with `X-Real-IP: 1.2.3.4` still names your own address, and with `Cf-Connecting-Ip` set
  Cloudflare itself answers `403`.
