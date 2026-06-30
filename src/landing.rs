use axum::{extract::State, response::Html, routing::get, Router};
use std::sync::Arc;

use crate::config::Config;

async fn landing_page(State(config): State<Arc<Config>>) -> Html<String> {
    let version = env!("CARGO_PKG_VERSION");
    let instance_id = &config.instance_id;
    let hostname = gethostname::gethostname()
        .to_string_lossy()
        .to_string();

    Html(format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>Rustybin</title>
<style>
*{{margin:0;padding:0;box-sizing:border-box}}
body{{background:#0f1117;color:#c9d1d9;font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,sans-serif;line-height:1.6}}
a{{color:#58a6ff;text-decoration:none}}
a:hover{{text-decoration:underline}}
.container{{max-width:960px;margin:0 auto;padding:24px 16px}}
header{{text-align:center;padding:40px 0 32px;border-bottom:1px solid #21262d}}
header h1{{font-size:2.4rem;color:#f0f6fc;font-weight:700;letter-spacing:-0.02em}}
header p.tagline{{color:#8b949e;font-size:1rem;margin-top:8px}}
.badge{{display:inline-block;background:#1f6feb;color:#f0f6fc;font-size:0.75rem;padding:2px 10px;border-radius:12px;margin-top:12px;font-family:monospace}}
.nav{{display:flex;justify-content:center;gap:16px;margin-top:16px;font-size:0.9rem}}
section{{margin-top:32px}}
section h2{{font-size:1.2rem;color:#f0f6fc;margin-bottom:12px;padding-bottom:6px;border-bottom:1px solid #21262d}}
.category{{margin-bottom:24px}}
.category h3{{font-size:0.95rem;color:#8b949e;text-transform:uppercase;letter-spacing:0.05em;margin-bottom:8px}}
table{{width:100%;border-collapse:collapse;font-size:0.85rem}}
table td{{padding:5px 8px;border-bottom:1px solid #161b22}}
td.route{{font-family:"SFMono-Regular",Consolas,"Liberation Mono",Menlo,monospace;color:#7ee787;white-space:nowrap}}
td.methods{{font-family:monospace;color:#d2a8ff;white-space:nowrap;font-size:0.78rem}}
td.desc{{color:#8b949e}}
.quickstart{{background:#161b22;border:1px solid #21262d;border-radius:8px;padding:16px;margin-top:12px}}
.quickstart pre{{font-family:"SFMono-Regular",Consolas,"Liberation Mono",Menlo,monospace;font-size:0.82rem;color:#c9d1d9;overflow-x:auto;white-space:pre-wrap;word-break:break-all}}
.quickstart pre .comment{{color:#8b949e}}
footer{{margin-top:40px;padding-top:16px;border-top:1px solid #21262d;font-size:0.78rem;color:#484f58;text-align:center}}
footer span{{margin:0 8px}}
@media(max-width:600px){{
  header h1{{font-size:1.8rem}}
  table{{font-size:0.78rem}}
  td.methods{{display:none}}
}}
</style>
</head>
<body>
<div class="container">
<header>
  <h1>Rustybin</h1>
  <p class="tagline">High-performance HTTP stub service for API gateway testing</p>
  <span class="badge">v{version}</span>
  <div class="nav">
    <a href="/docs">API Explorer</a>
    <a href="/openapi.json">OpenAPI JSON</a>
    <a href="/openapi.yaml">OpenAPI YAML</a>
    <a href="/graphql">GraphQL Playground</a>
    <a href="/export/postman.json" download>Postman</a>
    <a href="/export/insomnia.json" download>Insomnia</a>
    <a href="/export/curl.sh" download>cURL</a>
    <a href="/export/bruno.json" download>Bruno</a>
    <a href="/export/requests.http" download>.http</a>
    <a href="/export/requests.hurl" download>Hurl</a>
    <a href="/export/k6.js" download>k6</a>
    <a href="/export/har.json" download>HAR</a>
  </div>
</header>

<section>
<h2>Endpoint Directory</h2>

<div class="category">
<h3>Echo &amp; Reflection</h3>
<table>
<tr><td class="route">/echo</td><td class="methods">ALL</td><td class="desc">Echo back the full request (headers, body, query params)</td></tr>
<tr><td class="route">/anything</td><td class="methods">ALL</td><td class="desc">Alias for /echo</td></tr>
</table>
</div>

<div class="category">
<h3>Status Codes</h3>
<table>
<tr><td class="route">/status/:code</td><td class="methods">GET POST PUT PATCH DELETE</td><td class="desc">Return any HTTP status code (100-599)</td></tr>
</table>
</div>

<div class="category">
<h3>Response Shaping</h3>
<table>
<tr><td class="route">/delay/:ms</td><td class="methods">GET</td><td class="desc">Wait then respond (?jitter=true for variance)</td></tr>
<tr><td class="route">/cache/:ttl</td><td class="methods">GET</td><td class="desc">Caching headers with ETag and conditional 304</td></tr>
<tr><td class="route">/response-headers</td><td class="methods">GET</td><td class="desc">Query params become response headers</td></tr>
</table>
</div>

<div class="category">
<h3>Redirects &amp; Cookies</h3>
<table>
<tr><td class="route">/redirect/:n</td><td class="methods">GET</td><td class="desc">Chain of n relative 302 redirects</td></tr>
<tr><td class="route">/cookies</td><td class="methods">GET</td><td class="desc">Return current cookies as JSON</td></tr>
<tr><td class="route">/cookies/set</td><td class="methods">GET</td><td class="desc">Set cookies from query params</td></tr>
<tr><td class="route">/cookies/delete</td><td class="methods">GET</td><td class="desc">Delete cookies by name</td></tr>
</table>
</div>

<div class="category">
<h3>Info &amp; Random</h3>
<table>
<tr><td class="route">/ip</td><td class="methods">GET</td><td class="desc">Client IP address (v4 and v6)</td></tr>
<tr><td class="route">/date</td><td class="methods">GET</td><td class="desc">Current date (UTC or /date/:timezone)</td></tr>
<tr><td class="route">/time</td><td class="methods">GET</td><td class="desc">Current time ISO 8601 (UTC or /time/:timezone)</td></tr>
<tr><td class="route">/uuid</td><td class="methods">GET</td><td class="desc">UUID v4</td></tr>
<tr><td class="route">/guuid</td><td class="methods">GET</td><td class="desc">Braced GUID</td></tr>
<tr><td class="route">/random/*</td><td class="methods">GET</td><td class="desc">Random int, uint, lorem-ipsum</td></tr>
<tr><td class="route">/image/:type</td><td class="methods">GET</td><td class="desc">Minimal PNG, JPEG, or GIF image</td></tr>
</table>
</div>

<div class="category">
<h3>Auth: Basic &amp; API Key</h3>
<table>
<tr><td class="route">/auth/basic-auth</td><td class="methods">ALL</td><td class="desc">HTTP Basic auth (default: basic / password)</td></tr>
<tr><td class="route">/auth/basic-auth/:user/:pass</td><td class="methods">ALL</td><td class="desc">Custom credentials</td></tr>
<tr><td class="route">/auth/api-key</td><td class="methods">ALL</td><td class="desc">API key in header (default: apikey: my-key)</td></tr>
<tr><td class="route">/auth/api-key/:header/:key</td><td class="methods">ALL</td><td class="desc">Custom header and key</td></tr>
</table>
</div>

<div class="category">
<h3>Auth: HMAC</h3>
<table>
<tr><td class="route">/auth/hmac</td><td class="methods">ALL</td><td class="desc">Validate HMAC signature (default: alice / secret), gateway hmac-auth style</td></tr>
<tr><td class="route">/auth/hmac/:username/:secret</td><td class="methods">ALL</td><td class="desc">Custom username and secret; supports sha1/sha256/sha384/sha512</td></tr>
</table>
</div>

<div class="category">
<h3>Auth: JWT</h3>
<table>
<tr><td class="route">/auth/jwt</td><td class="methods">ALL</td><td class="desc">Validate JWT structure (decode, no signature check)</td></tr>
<tr><td class="route">/auth/jwt/exchange</td><td class="methods">ALL</td><td class="desc">Exchange JWT for new HS256-signed token</td></tr>
</table>
</div>

<div class="category">
<h3>Auth: OIDC Provider</h3>
<table>
<tr><td class="route">/.well-known/openid-configuration</td><td class="methods">GET</td><td class="desc">OIDC discovery document</td></tr>
<tr><td class="route">/oauth/token</td><td class="methods">POST</td><td class="desc">Token endpoint (client_credentials, password, authorization_code, token-exchange)</td></tr>
<tr><td class="route">/oauth/jwks</td><td class="methods">GET</td><td class="desc">RS256 public key in JWK format</td></tr>
<tr><td class="route">/oauth/authorize</td><td class="methods">GET POST</td><td class="desc">Authorization code flow with login form</td></tr>
<tr><td class="route">/oauth/userinfo</td><td class="methods">GET</td><td class="desc">User claims from Bearer token</td></tr>
<tr><td class="route">/oauth/introspect</td><td class="methods">POST</td><td class="desc">Token introspection (RFC 7662)</td></tr>
</table>
</div>

<div class="category">
<h3>Auth: mTLS</h3>
<table>
<tr><td class="route">/auth/mtls</td><td class="methods">ALL</td><td class="desc">Validate client certificate</td></tr>
<tr><td class="route">/auth/mtls/get-client-cert</td><td class="methods">GET</td><td class="desc">Download demo client cert + key</td></tr>
<tr><td class="route">/auth/mtls/get-ca-cert</td><td class="methods">GET</td><td class="desc">Download CA certificate</td></tr>
</table>
</div>

<div class="category">
<h3>AI Gateway (OpenAI-compatible)</h3>
<table>
<tr><td class="route">/ai/v1/chat/completions</td><td class="methods">POST</td><td class="desc">Chat completions (streaming SSE supported)</td></tr>
<tr><td class="route">/ai/v1/completions</td><td class="methods">POST</td><td class="desc">Legacy text completions</td></tr>
<tr><td class="route">/ai/v1/embeddings</td><td class="methods">POST</td><td class="desc">Deterministic 1536-dim embeddings</td></tr>
<tr><td class="route">/ai/v1/models</td><td class="methods">GET</td><td class="desc">List available models</td></tr>
</table>
</div>

<div class="category">
<h3>AI Gateway (Anthropic-compatible)</h3>
<table>
<tr><td class="route">/ai/anthropic/v1/messages</td><td class="methods">POST</td><td class="desc">Anthropic Messages API (native SSE event stream supported)</td></tr>
</table>
</div>

<div class="category">
<h3>GraphQL</h3>
<table>
<tr><td class="route">/graphql</td><td class="methods">GET POST</td><td class="desc">GraphQL endpoint (GET: playground, POST: query)</td></tr>
<tr><td class="route">/graphql/schema</td><td class="methods">GET</td><td class="desc">SDL schema definition</td></tr>
</table>
</div>

<div class="category">
<h3>Orchestration (Multi-step Pipeline)</h3>
<table>
<tr><td class="route">/orchestration/step/1</td><td class="methods">POST</td><td class="desc">Authenticate (X-Api-Key required)</td></tr>
<tr><td class="route">/orchestration/step/2</td><td class="methods">POST</td><td class="desc">Enrich (X-Correlation-Id required)</td></tr>
<tr><td class="route">/orchestration/step/3</td><td class="methods">POST</td><td class="desc">Validate (risk scoring)</td></tr>
<tr><td class="route">/orchestration/step/4</td><td class="methods">POST</td><td class="desc">Process (requires X-Validation-Result: approved)</td></tr>
<tr><td class="route">/orchestration/status</td><td class="methods">GET</td><td class="desc">Pipeline documentation</td></tr>
</table>
</div>

<div class="category">
<h3>SOAP / XML</h3>
<table>
<tr><td class="route">/soap</td><td class="methods">POST</td><td class="desc">SOAP operations (GetUser, CreateOrder, GetStatus)</td></tr>
<tr><td class="route">/soap/wsdl</td><td class="methods">GET</td><td class="desc">WSDL 1.1 service description</td></tr>
</table>
</div>

<div class="category">
<h3>WebSocket</h3>
<table>
<tr><td class="route">/ws</td><td class="methods">GET (upgrade)</td><td class="desc">Echo every text/binary frame back to the client</td></tr>
<tr><td class="route">/ws/time</td><td class="methods">GET (upgrade)</td><td class="desc">Server-push timestamp ticker (?interval_ms=&amp;count=)</td></tr>
</table>
</div>

<div class="category">
<h3>gRPC (separate port, default :50051)</h3>
<table>
<tr><td class="route">EchoService/Echo</td><td class="methods">unary</td><td class="desc">Reflect message + request metadata + instance_id</td></tr>
<tr><td class="route">EchoService/ServerStream</td><td class="methods">server-stream</td><td class="desc">Emit N responses (request.count)</td></tr>
<tr><td class="route">EchoService/ClientStream</td><td class="methods">client-stream</td><td class="desc">Consume a stream, return one summary</td></tr>
<tr><td class="route">EchoService/BidiStream</td><td class="methods">bidi-stream</td><td class="desc">Echo each request as it arrives</td></tr>
</table>
</div>

<div class="category">
<h3>Reliability Testing</h3>
<table>
<tr><td class="route">/flaky/:rate</td><td class="methods">ALL</td><td class="desc">Fail rate% of requests with 503</td></tr>
<tr><td class="route">/flaky/pattern/:pattern</td><td class="methods">ALL</td><td class="desc">Follow S/F pattern (e.g. SSFSS)</td></tr>
<tr><td class="route">/flaky/after/:n</td><td class="methods">ALL</td><td class="desc">Succeed n times then fail forever</td></tr>
<tr><td class="route">/flaky/recover/:n</td><td class="methods">ALL</td><td class="desc">Fail n times then succeed forever</td></tr>
<tr><td class="route">/flaky/reset</td><td class="methods">POST</td><td class="desc">Reset all flaky counters</td></tr>
<tr><td class="route">/flaky/status</td><td class="methods">GET</td><td class="desc">Current counter state</td></tr>
</table>
</div>

<div class="category">
<h3>Utility</h3>
<table>
<tr><td class="route">/health</td><td class="methods">GET</td><td class="desc">Health check (503 when toggled unhealthy)</td></tr>
<tr><td class="route">/health/healthy</td><td class="methods">POST</td><td class="desc">Mark instance healthy (active health-check demos)</td></tr>
<tr><td class="route">/health/unhealthy</td><td class="methods">POST</td><td class="desc">Mark instance unhealthy (returns 503)</td></tr>
<tr><td class="route">/health/toggle</td><td class="methods">POST</td><td class="desc">Flip current health state</td></tr>
<tr><td class="route">/identity</td><td class="methods">ALL</td><td class="desc">Instance info, uptime, request count</td></tr>
<tr><td class="route">/openapi.json</td><td class="methods">GET</td><td class="desc">OpenAPI 3.0.3 specification (JSON)</td></tr>
<tr><td class="route">/openapi.yaml</td><td class="methods">GET</td><td class="desc">OpenAPI 3.0.3 specification (YAML)</td></tr>
<tr><td class="route">/docs</td><td class="methods">GET</td><td class="desc">Interactive API explorer (Scalar)</td></tr>
</table>
</div>

</section>

<section>
<h2>Quick Start</h2>
<div class="quickstart"><pre><span class="comment"># Echo back a request</span>
curl http://localhost/echo

<span class="comment"># Get a specific HTTP status code</span>
curl http://localhost/status/418

<span class="comment"># Get an OAuth token</span>
curl -X POST http://localhost/oauth/token \
  -d 'grant_type=client_credentials&amp;client_id=rustybin&amp;client_secret=secret'

<span class="comment"># AI Gateway (OpenAI-compatible)</span>
curl -X POST http://localhost/ai/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{{"model":"rustybin","messages":[{{"role":"user","content":"hello"}}]}}'</pre>
</div>
</section>

<footer>
  <span>instance: {instance_id}</span>
  <span>host: {hostname}</span>
  <span>v{version}</span>
</footer>
</div>
</body>
</html>"##
    ))
}

pub fn router() -> Router<Arc<Config>> {
    Router::new().route("/", get(landing_page))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_config() -> Arc<Config> {
        Arc::new(Config {
            http_port: 80,
            https_port: 443,
            host: "0.0.0.0".to_string(),
            log_level: "info".to_string(),
            trust_forward: false,
            body_limit: 1_048_576,
            instance_id: "test-landing-instance".to_string(),
            tls_cert: "certs/server.crt".to_string(),
            tls_key: "certs/server.key".to_string(),
            mtls_in_header: None,
        })
    }

    fn test_app() -> Router {
        router().with_state(test_config())
    }

    #[tokio::test]
    async fn landing_returns_html() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), 200);
        let ct = resp
            .headers()
            .get("content-type")
            .expect("content-type")
            .to_str()
            .expect("str");
        assert!(ct.contains("text/html"));
    }

    #[tokio::test]
    async fn landing_contains_key_content() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let html = String::from_utf8(body.to_vec()).expect("utf8");

        assert!(html.contains("Rustybin"), "should contain title");
        assert!(
            html.contains(env!("CARGO_PKG_VERSION")),
            "should contain version"
        );
        assert!(html.contains("/echo"), "should list echo endpoint");
        assert!(html.contains("/docs"), "should link to docs");
        assert!(html.contains("/oauth/token"), "should list oauth endpoint");
        assert!(html.contains("test-landing-instance"), "should show instance_id");
    }
}
