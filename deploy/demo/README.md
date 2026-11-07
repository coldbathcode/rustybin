# The public demo on Fly.io

Visitors create mocks at `demo.rustybin.dev` without an account, each served at
`<name>.rustybin-mock.dev`, and only the operator signs in. The bare `rustybin-mock.dev`
redirects to the demo. `fly.toml` here pins the signed release by digest.

The console stays on `rustybin.dev` and visitors' mocks on their own registrable domain, so what
a visitor serves never shares a site with the console.

## What it needs

An app and a volume, once:

```bash
fly apps create rustybin-demo
fly volumes create rustybin_demo_data --app rustybin-demo --region lhr --size 1
```

The secrets, never in this file. The hash is made on your own machine, so the password reaches
neither a command line nor the instance:

```bash
HASH=$(rustybin password hash)   # prompts, prints the hash and nothing else
printf "RUSTYBIN_CONSOLE_PASSWORD_HASH=%s\n" "$HASH" | fly secrets import --app rustybin-demo
fly secrets set --app rustybin-demo RUSTYBIN_LEADS_SALT="$(openssl rand -hex 32)"
```

## Deploy

```bash
cosign verify "$(sed -n 's/^  image = "\(.*\)"/\1/p' deploy/demo/fly.toml)" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp '^https://github.com/coldbathcode/rustybin/'
fly deploy --config deploy/demo/fly.toml
```

## DNS and TLS

`demo.rustybin.dev`, `rustybin-mock.dev` and `*.rustybin-mock.dev` point at the app, not proxied
(DNS only), so `Fly-Client-IP` is the visitor's address, which is what the per-address limits count:

```bash
fly certs add demo.rustybin.dev --app rustybin-demo
fly certs add rustybin-mock.dev --app rustybin-demo
fly certs add '*.rustybin-mock.dev' --app rustybin-demo   # asks for a DNS challenge record
```

## Check it

- `https://demo.rustybin.dev/_rustybin/diagnostics` signed in: `client_address` is ok.
- A mock made without signing in answers at `https://<name>.rustybin-mock.dev/`.
- `GET /_rustybin/settings` without a credential answers `401`.
