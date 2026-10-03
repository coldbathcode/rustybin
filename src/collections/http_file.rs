use super::{AuthDef, BodyDef, Category};

pub fn build(categories: &[Category], base_url: &str) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let mut out = String::with_capacity(8192);

    out.push_str(&format!("# Rustybin v{version} - HTTP requests\n"));
    out.push_str("# Compatible with JetBrains HTTP Client and VS Code REST Client\n\n");
    out.push_str(&format!("@base_url = {base_url}\n\n"));

    for cat in categories {
        out.push_str(&format!(
            "# ══════════════════════════════════════════\n# {}\n# ══════════════════════════════════════════\n\n",
            cat.name
        ));

        for req in &cat.requests {
            out.push_str(&format!("### {}\n", req.name));
            out.push_str(&format!("{} {{{{base_url}}}}{}\n", req.method, req.path));

            // Headers
            for (k, v) in &req.headers {
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
                out.push('\n');
                match body {
                    BodyDef::Json(raw) => {
                        out.push_str(raw);
                        out.push('\n');
                    }
                    BodyDef::Form(params) => {
                        let encoded: Vec<String> =
                            params.iter().map(|(k, v)| format!("{k}={v}")).collect();
                        out.push_str(&encoded.join("&"));
                        out.push('\n');
                    }
                    BodyDef::Xml(raw) => {
                        out.push_str(raw);
                        out.push('\n');
                    }
                }
            }

            out.push('\n');
        }
    }

    out
}

fn base64_encode(input: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(input.as_bytes())
}
