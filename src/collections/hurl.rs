use super::{split_path_query, AuthDef, BodyDef, Category};

pub fn build(categories: &[Category]) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let mut out = String::with_capacity(8192);

    out.push_str(&format!(
        "# Rustybin v{version} — Hurl file\n"
    ));
    out.push_str("# Run: hurl --variable base_url=http://localhost rustybin.hurl\n\n");

    for cat in categories {
        out.push_str(&format!("# ── {} ──\n\n", cat.name));

        for req in &cat.requests {
            out.push_str(&format!("# {}\n", req.name));

            let (path, query) = split_path_query(req.path);
            let mut url = format!("{{{{base_url}}}}{path}");
            if !query.is_empty() {
                url.push('?');
                url.push_str(query);
            }

            out.push_str(&format!("{} {url}\n", req.method));

            // Headers
            for (k, v) in req.headers {
                out.push_str(&format!("{k}: {v}\n"));
            }

            // Auth
            if let Some(ref auth) = req.auth {
                match auth {
                    AuthDef::Basic { user, pass } => {
                        let encoded = base64_encode(&format!("{user}:{pass}"));
                        out.push_str(&format!("Authorization: Basic {encoded}\n"));
                    }
                    AuthDef::Bearer(token) => {
                        out.push_str(&format!("Authorization: Bearer {token}\n"));
                    }
                }
            }

            // Body
            if let Some(ref body) = req.body {
                match body {
                    BodyDef::Json(raw) => {
                        out.push_str(&format!("```json\n{raw}\n```\n"));
                    }
                    BodyDef::Form(params) => {
                        out.push_str("[FormParams]\n");
                        for (k, v) in *params {
                            out.push_str(&format!("{k}: {v}\n"));
                        }
                    }
                    BodyDef::Xml(raw) => {
                        out.push_str(&format!("```xml\n{raw}\n```\n"));
                    }
                }
            }

            // Response assertion (wildcard status)
            out.push_str("HTTP *\n\n");
        }
    }

    out
}

fn base64_encode(input: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(input.as_bytes())
}
