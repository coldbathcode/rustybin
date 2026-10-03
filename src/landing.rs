//! Landing page (`GET /`), rendered from the route catalogue.
//!
//! `GET /` must always return 200: it is the platform liveness check
//! (`/health` is a demo toggle that can return 503).

use axum::{extract::State, response::Html, routing::get, Router};
use std::fmt::Write as _;
use std::sync::Arc;

use crate::catalog::{self, category, Endpoint, Example, Protocol};
use crate::config::Config;
use crate::state::AppState;

/// Escape text for HTML element content and quoted attribute values.
pub fn html_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

const EXPORTS: &[(&str, &str)] = &[
    ("Postman", "/export/postman.json"),
    ("Insomnia", "/export/insomnia.json"),
    ("Bruno", "/export/bruno.json"),
    ("cURL", "/export/curl.sh"),
    (".http", "/export/requests.http"),
    ("Hurl", "/export/requests.hurl"),
    ("k6", "/export/k6.js"),
    ("HAR", "/export/har.json"),
];

const STYLE: &str = r#"
:root{--bg:#ffffff;--fg:#1f2328;--muted:#59636e;--card:#f6f8fa;--border:#d1d9e0;--accent:#0969da;--accent-fg:#ffffff;--code:#0a3069;
--get:#1a7f37;--post:#0969da;--put:#9a6700;--patch:#8250df;--delete:#cf222e;--any:#59636e}
@media (prefers-color-scheme: dark){:root{--bg:#0d1117;--fg:#e6edf3;--muted:#9198a1;--card:#151b23;--border:#3d444d;--accent:#4493f8;--accent-fg:#0d1117;--code:#a5d6ff;
--get:#3fb950;--post:#4493f8;--put:#d29922;--patch:#ab7df8;--delete:#f85149;--any:#9198a1}}
*{margin:0;padding:0;box-sizing:border-box}
body{background:var(--bg);color:var(--fg);font:15px/1.55 -apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,sans-serif}
a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline}
code,.path{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,"Liberation Mono",monospace}
.wrap{max-width:1040px;margin:0 auto;padding:24px 16px 48px}
header{display:flex;flex-wrap:wrap;gap:16px;align-items:center;justify-content:space-between;padding:24px 0;border-bottom:1px solid var(--border)}
h1{font-size:2rem;letter-spacing:-.02em}.tag{color:var(--muted)}
.ver{font:12px ui-monospace,monospace;border:1px solid var(--border);border-radius:999px;padding:2px 10px;color:var(--muted);margin-left:8px;vertical-align:middle}
.actions{display:flex;flex-wrap:wrap;gap:8px}
.btn{display:inline-block;border:1px solid var(--border);border-radius:8px;padding:7px 14px;background:var(--card);color:var(--fg);font-weight:500}
.btn:hover{text-decoration:none;border-color:var(--accent)}
.btn.primary{background:var(--accent);color:var(--accent-fg);border-color:var(--accent)}
.exports{margin-top:14px;color:var(--muted);font-size:.88rem}.exports a{margin-right:10px}
.search{position:sticky;top:0;background:var(--bg);padding:16px 0 10px;z-index:1}
.search input{width:100%;padding:10px 14px;font-size:1rem;border:1px solid var(--border);border-radius:8px;background:var(--card);color:var(--fg)}
.cat{margin-top:22px}
.cat h2{font-size:.82rem;text-transform:uppercase;letter-spacing:.06em;color:var(--muted);margin-bottom:6px}
.ep{display:grid;grid-template-columns:150px minmax(180px,330px) 1fr;gap:10px;align-items:baseline;padding:7px 10px;border-bottom:1px solid var(--border)}
.ep:hover{background:var(--card)}
.methods{display:flex;flex-wrap:wrap;gap:4px}
.m{font:600 11px ui-monospace,monospace;padding:1px 6px;border-radius:4px;border:1px solid currentColor}
.m-GET{color:var(--get)}.m-POST{color:var(--post)}.m-PUT{color:var(--put)}.m-PATCH{color:var(--patch)}.m-DELETE{color:var(--delete)}.m-ANY,.m-WS,.m-SSE{color:var(--any)}
.path{color:var(--code);word-break:break-all;font-size:.88rem}
.desc{color:var(--muted);font-size:.9rem}
.empty{display:none;color:var(--muted);padding:24px 0}
.panel{margin-top:32px;background:var(--card);border:1px solid var(--border);border-radius:10px;padding:16px 18px}
.panel h2{font-size:1.05rem;margin-bottom:8px}
.panel pre{font:13px/1.5 ui-monospace,monospace;white-space:pre-wrap;word-break:break-all;color:var(--fg)}
.panel ul{margin-left:18px;color:var(--muted)}
footer{margin-top:36px;color:var(--muted);font-size:.8rem;text-align:center}footer span{margin:0 8px}
@media (max-width:700px){.ep{grid-template-columns:1fr;gap:2px}h1{font-size:1.6rem}}
"#;

const SCRIPT: &str = r#"
(function(){
  var input=document.getElementById('q');
  var empty=document.getElementById('empty');
  input.addEventListener('input',function(){
    var q=input.value.trim().toLowerCase();
    var shown=0;
    document.querySelectorAll('.cat').forEach(function(cat){
      var any=false;
      cat.querySelectorAll('.ep').forEach(function(ep){
        var hit=!q||ep.getAttribute('data-search').indexOf(q)!==-1;
        ep.style.display=hit?'':'none';
        if(hit){any=true;shown++;}
      });
      cat.style.display=any?'':'none';
    });
    empty.style.display=shown?'none':'block';
  });
})();
"#;

fn render_endpoint(out: &mut String, ep: &Endpoint) {
    let search = format!(
        "{} {} {} {} {}",
        ep.path,
        ep.methods_label(),
        ep.category,
        ep.summary,
        ep.description
    )
    .to_lowercase();
    let _ = write!(
        out,
        "<div class=\"ep\" data-search=\"{}\"><div class=\"methods\">",
        html_escape(&search)
    );
    for m in ep.methods {
        let _ = write!(out, "<span class=\"m m-{0}\">{0}</span>", html_escape(m));
    }
    match ep.protocol {
        Protocol::WebSocket => out.push_str("<span class=\"m m-WS\">WS</span>"),
        Protocol::Sse => out.push_str("<span class=\"m m-SSE\">SSE</span>"),
        Protocol::Http => {}
    }
    out.push_str("</div>");
    // Link the path when a parameter-free GET example exists.
    let link = ep.examples.iter().find(|ex| {
        ex.method == "GET"
            && ep.protocol == Protocol::Http
            && ex.headers.is_empty()
            && ex.auth.is_none()
    });
    match link {
        Some(ex) => {
            let _ = write!(
                out,
                "<a class=\"path\" href=\"{}\">{}</a>",
                html_escape(ex.path),
                html_escape(ep.path)
            );
        }
        None => {
            let _ = write!(out, "<span class=\"path\">{}</span>", html_escape(ep.path));
        }
    }
    let _ = write!(out, "<span class=\"desc\">{}", html_escape(ep.summary));
    if !ep.description.is_empty() {
        let _ = write!(out, "<br>{}", html_escape(ep.description));
    }
    out.push_str("</span></div>\n");
}

/// Render the landing page HTML.
pub fn render(config: &Config) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let hostname = gethostname::gethostname().to_string_lossy().to_string();
    let total = catalog::all().len();

    let mut out = String::with_capacity(64 * 1024);
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str(
        "<meta name=\"color-scheme\" content=\"light dark\">\n<title>Rustybin</title>\n<style>",
    );
    out.push_str(STYLE);
    out.push_str("</style>\n</head>\n<body>\n<div class=\"wrap\">\n<header><div>");
    let _ = write!(
        out,
        "<h1>Rustybin<span class=\"ver\">v{}</span></h1>",
        html_escape(version)
    );
    out.push_str("<p class=\"tag\">HTTP stub service for API and AI gateway demos</p></div>");
    out.push_str("<div class=\"actions\"><a class=\"btn primary\" href=\"/ui\">Open console</a>");
    out.push_str("<a class=\"btn\" href=\"/docs\">API docs</a>");
    out.push_str("<a class=\"btn\" href=\"/openapi.json\">OpenAPI JSON</a>");
    out.push_str("<a class=\"btn\" href=\"/openapi.yaml\">YAML</a></div></header>\n");

    out.push_str("<div class=\"exports\">Export collections: ");
    for (name, href) in EXPORTS {
        let _ = write!(
            out,
            "<a href=\"{}\" download>{}</a>",
            html_escape(href),
            html_escape(name)
        );
    }
    out.push_str("</div>\n");

    let _ = writeln!(
        out,
        "<div class=\"search\"><input id=\"q\" type=\"search\" placeholder=\"Filter {total} endpoints by path, method or description\" aria-label=\"Filter endpoints\" autocomplete=\"off\"></div>"
    );

    for (name, endpoints) in catalog::grouped() {
        let _ = writeln!(out, "<section class=\"cat\"><h2>{}</h2>", html_escape(name));
        for ep in endpoints {
            render_endpoint(&mut out, ep);
        }
        out.push_str("</section>\n");
    }
    out.push_str("<p id=\"empty\" class=\"empty\">No endpoint matches this filter.</p>\n");

    let _ = writeln!(
        out,
        "<section class=\"panel\"><h2>gRPC (port {})</h2><ul>\
         <li><code>rustybin.echo.v1.EchoService/Echo</code>: unary echo with request metadata</li>\
         <li><code>ServerStream</code>, <code>ClientStream</code>, <code>BidiStream</code>: streaming variants</li>\
         </ul></section>",
        config.grpc_port
    );

    out.push_str(
        "<section class=\"panel\"><h2>Works on every route</h2><ul>\
         <li><code>X-Rustybin-Delay: 500</code> delays the response (ms, capped)</li>\
         <li><code>X-Rustybin-Fail: 503</code> or <code>503:50</code> injects an error (always, or 50% of the time)</li>\
         <li><code>X-Request-Id</code> is propagated or generated and echoed back</li>\
         <li><code>X-Rustybin-Session: my-demo</code> tags requests so you can find them in the inspector (<code>/_rustybin/requests?session=my-demo</code>)</li>\
         <li>Send <code>Accept: application/xml</code> for XML instead of JSON</li>\
         </ul></section>\n",
    );

    out.push_str(
        "<section class=\"panel\"><h2>Quick start</h2><pre>curl http://localhost/echo\n\
         curl -i http://localhost/status/418\n\
         curl -X POST http://localhost/oauth/token -d 'grant_type=client_credentials&amp;client_id=rustybin&amp;client_secret=secret'\n\
         curl -X POST http://localhost/ai/v1/chat/completions -H 'Content-Type: application/json' \\\n  \
         -d '{\"model\":\"rustybin\",\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}'</pre></section>\n",
    );

    let _ = writeln!(
        out,
        "<footer><span>instance: {}</span><span>host: {}</span><span>v{}</span>{}</footer>",
        html_escape(&config.instance_id),
        html_escape(&hostname),
        html_escape(version),
        if config.public_mode {
            "<span>public mode</span>"
        } else {
            ""
        }
    );
    out.push_str("</div>\n<script>");
    out.push_str(SCRIPT);
    out.push_str("</script>\n</body>\n</html>\n");
    out
}

async fn landing_page(State(config): State<Arc<Config>>) -> Html<String> {
    Html(render(&config))
}

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new().route("/", get(landing_page))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![Endpoint::new(
        "/",
        &["GET"],
        category::DOCS,
        "This landing page (always 200, safe for liveness checks)",
    )
    .example(Example::get("Landing page", "/"))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_string, get_request, module_app_with_config, test_config};
    use tower::ServiceExt;

    #[test]
    fn escapes_html() {
        assert_eq!(
            html_escape("<a href=\"x\">&'</a>"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;&lt;/a&gt;"
        );
    }

    #[tokio::test]
    async fn landing_returns_html() {
        let resp = crate::test_support::module_app(router)
            .oneshot(get_request("/"))
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
        let mut config = test_config();
        config.instance_id = "test-landing-<instance>".to_string();
        let resp = module_app_with_config(config, router)
            .oneshot(get_request("/"))
            .await
            .expect("response");
        let html = body_string(resp).await;

        assert!(html.contains("Rustybin"), "should contain title");
        assert!(
            html.contains(env!("CARGO_PKG_VERSION")),
            "should contain version"
        );
        assert!(html.contains("/echo"), "should list echo endpoint");
        assert!(html.contains("href=\"/docs\""), "should link to docs");
        assert!(html.contains("href=\"/ui\""), "should link to the console");
        assert!(html.contains("/oauth/token"), "should list oauth endpoint");
        assert!(html.contains("prefers-color-scheme"), "dark mode support");
        assert!(
            html.contains("test-landing-&lt;instance&gt;"),
            "instance id must be escaped"
        );
        assert!(!html.contains("<instance>"));
    }

    #[tokio::test]
    async fn landing_lists_every_catalogue_path() {
        let html = render(&test_config());
        for ep in catalog::all() {
            assert!(
                html.contains(&html_escape(ep.path)),
                "landing page is missing {}",
                ep.path
            );
        }
    }
}
