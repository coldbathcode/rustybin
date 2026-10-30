# Troubleshooting

Start with the symptom below. Commands assume the [quick-start](quick-start.md)
instance at `http://localhost:8080`. For a different host or port, replace the URL.

## I cannot connect to the instance

Check the HTTP listener first:

```bash
curl -i http://localhost:8080/_rustybin/ready
```

Expect `200`. With Docker, run `docker ps` and `docker logs rustybin`; the
quick-start mapping is host port `8080` to container port `80`. Compose uses host
port `80`, so its URL is `http://localhost` instead.

If Cargo reports a bind error, use high ports as shown in the
[Cargo setup](quick-start.md#cargo), or stop the process already using the port.
HTTPS and gRPC are optional: a warning about one of them does not mean HTTP failed.

## The console or API returns 401 or 403

A fresh instance's console at `/ui` uses **`rustybin` / `rustybin`**. A
`/_rustybin/*` call needs the token from that sign-in; the console attaches it for
you. For the CLI, run `rustybin login --url http://localhost:8080`.

- **401:** sign in again if the token expired or was revoked. Check that the CLI
  profile or base URL points to the instance you intended.
- **403:** the credential may lack a required scope, an admin guard may apply, or
  the feature may be disabled in public mode. Read the JSON error for the reason.
- **A data-plane endpoint asks for credentials:** authentication demo routes,
  secured mocks and instances with required API keys have their own credentials.
  The console password is for managing the instance. See
  [mocked security](mocks/security.md) or [authentication](reference/auth.md).

For password recovery with an admin token, alternative auth modes and hosted
instances, see [control-plane security](concepts/control-plane-security.md).

## An example fails when I copy it

Most reference pages show **Hurl**, a request followed by response assertions. It
is a file format, so paste the block into a `.hurl` file and run Hurl; lines such
as `HTTP 200` and `[Asserts]` are not shell commands. `{{base_url}}` is a Hurl
variable. The [examples guide](examples.md) explains how to set it and gives the
equivalent curl request.

The full example runner configures several fresh instances, including an open
control plane and an admin token. On your default instance, sign in for control
routes. Some examples also need sample mocks, a particular auth mode or a shared
session; check the page's prerequisites and the
[runner settings](examples.md#running-them).

## HTTPS reports a certificate error

Rustybin generates a demo CA and server certificate at startup. Download the CA
from `/auth/mtls/get-ca-cert`, extract its PEM value with jq, then pass it to curl:

```bash
curl -fsS http://localhost:8080/auth/mtls/get-ca-cert | jq -r .ca_cert_pem > rustybin-ca.crt
curl --cacert rustybin-ca.crt https://localhost:8443/health
```

Use the host names covered by the demo certificate (`localhost` or loopback).
If certificates change after a restart, persist `/app/certs` in Docker. See
[TLS configuration](configuration.md#proxies-and-tls) and
[mTLS](reference/auth.md#mtls) for client certificates.

## My changes disappeared after a restart

Without a state directory, settings and API-created mocks are in memory. Set
`RUSTYBIN_STATE_DIR` to a writable persistent directory. In Docker, use a volume:

```bash
docker run -d --name rustybin-saved -p 8081:80 \
  -e RUSTYBIN_STATE_DIR=/app/certs/state \
  -v rustybin-data:/app/certs \
  ghcr.io/coldbathcode/rustybin:latest
```

This example uses port `8081` so it can run alongside the quick-start container;
open `http://localhost:8081/ui`. For a bind mount, the directory must be writable
by the container's user, uid `10001`. This command puts state under `/app/certs`
because the image already makes that volume location writable by uid `10001`.
A new volume at `/app/state` needs its ownership prepared first; otherwise the
instance can answer requests while its logs warn that secrets cannot be saved.

The state directory keeps settings, sealed secrets, saved mocks and other
persisted configuration. It is not a log of captured traffic. For a portable,
reviewable backup, [export the instance as code](configuration-as-code.md) and
keep it in [source control](source-control.md). See
[state directory details](configuration.md#state-directory-settings-and-secrets).

## My mock returns 404 or 503

- **404:** check the mock name and the spec's server prefix. The built-in sample
  is `/mock/petstore/v1/pets/42`, including `/v1`. An unknown operation is also 404;
  the problem response includes the base path.
- **503:** check whether the mock is disabled or has a draft that has never been
  deployed. In the console's **Mock APIs**, inspect its state and deploy report.
- **Petstore is missing:** the sample is off by default in public mode and can be
  disabled with `RUSTYBIN_MOCKS_SAMPLE=false`.

The [mock URL reference](mocks/index.md#urls) explains prefixes, methods and
reserved paths. [Draft, deploy, rollback](mocks/index.md#draft-deploy-rollback)
explains when an edit reaches the live URL.

## An outbound connection is refused

Rustybin checks outbound destinations against its egress policy. Inspect
**Management, Network** in the console and look for the refusal reason.

An external MCP server usually needs its host in `RUSTYBIN_EGRESS_ALLOW` (or the
`egress.allow` setting). Loopback also needs `RUSTYBIN_EGRESS_ALLOW_LOOPBACK=true`.
A destination can still be refused by a feature's own rules. Read
[outbound connections](configuration.md#outbound-connections) before choosing the
allowlist for your setup.

If a gateway runs in another container, `localhost` means that container. Use the
Rustybin service name on a shared Docker network for its upstream address.

## I cannot see a request in the inspector

In public mode the inspector captures requests tagged with
`X-Rustybin-Session`; use the same session when sending and reading traffic.
Some control routes are excluded and a time-to-headers timeout does not appear
in the inspector. Check [request inspector](concepts/inspector.md) and
[sessions](concepts/sessions.md) for capture rules and limits.

For a mock, its **Traffic** tab shows calls to that mock. For AI requests, use the
[AI request inspector](ai/gateway-features.md#request-inspection).

## A health check returns 503

`/health` is a demo switch: a test or a person can make it unhealthy on purpose.
Use `/_rustybin/ready` for a readiness probe and `/` for liveness. To restore the
demo health state, use **Chaos and health** in the console. See
[health endpoints](reference/reliability.md).
