# Webhooks

Rustybin verifies and signs webhooks in three common schemes, so you can show a
gateway's webhook signature validation (or signing) against a counterpart that
explains exactly why a signature fails. It never sends webhooks itself: there is no
outbound delivery, so these endpoints cannot be abused for SSRF.

| Scheme | Headers | Signature |
|---|---|---|
| Standard Webhooks (`standard`) | `webhook-id`, `webhook-timestamp` (unix seconds), `webhook-signature` | space-separated `v1,<base64 HMAC-SHA256>` over `{id}.{timestamp}.{body}`; the secret is `whsec_<base64 key>` and the HMAC key is the decoded part |
| GitHub style (`github`) | `X-Hub-Signature-256` | `sha256=<hex HMAC-SHA256(secret, body)>`, the secret used as is |
| Stripe style (`stripe`) | `Stripe-Signature` | `t=<unix>,v1=<hex HMAC-SHA256(secret, "{t}.{body}")>`, the full secret (with its `whsec_` prefix) as key |

Timestamps (Standard Webhooks and Stripe) must be within 300 seconds (replay
protection).

| Route | Purpose |
|---|---|
| `GET /webhooks` | The schemes and the demo secrets |
| `POST /webhooks/verify` | Verify a Standard Webhooks request |
| `POST /webhooks/verify/{scheme}` | Verify `standard`, `github` or `stripe` |
| `GET, POST /webhooks/sign` | Produce a signed example: headers, body and a ready-to-run curl command |
| `POST /webhooks/receive/{secret_id}` | A receiver with a demo secret: `204` when valid, `401` otherwise |

## Verifying

The secret comes from the `X-Webhook-Secret` header or `?secret=`. Options:
`?tolerance=` (seconds) and `?ignore_timestamp=true`. Valid requests get `200`;
failures get `401` (`timestamp_too_old`, `timestamp_too_new`, `signature_mismatch`,
`no_supported_signature`) or `400` (`missing_headers`, `missing_secret`,
`invalid_secret`, `invalid_timestamp`). The reply always includes the expected
signature, which makes debugging a signing plugin quick.

```hurl
{{#include ../../examples/protocols/webhooks.hurl:schemes}}
```

```hurl
{{#include ../../examples/protocols/webhooks.hurl:verify_standard}}
```

```hurl
{{#include ../../examples/protocols/webhooks.hurl:verify_stale}}
```

```hurl
{{#include ../../examples/protocols/webhooks.hurl:verify_github}}
```

```hurl
{{#include ../../examples/protocols/webhooks.hurl:verify_stripe}}
```

## Signing and receiving

`/webhooks/sign` takes `?scheme=` (`standard` by default), `?secret=` (default: the
`demo` secret), the payload (POST body or `?payload=`, else a sample
`order.created` event), `?id=` and `?timestamp=`. Replay its output through a
gateway to the receiver, which detects the scheme from the headers. Demo secrets:

| `secret_id` | Secret |
|---|---|
| `demo` | `whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw` |
| `github-docs` | `It's a Secret to Everybody` |

```hurl
{{#include ../../examples/protocols/webhooks.hurl:sign_and_receive}}
```

```hurl
{{#include ../../examples/protocols/webhooks.hurl:receive_tampered}}
```

To capture webhooks a gateway or a third party sends, use a
[request bin](request-bin.md).
