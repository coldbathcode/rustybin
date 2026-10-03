use super::{split_path_query, AuthDef, BodyDef, Category};

pub fn build(categories: &[Category], base_url: &str) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let mut out = String::with_capacity(8192);

    out.push_str("#!/usr/bin/env bash\n");
    out.push_str(&format!("# Rustybin v{version} - cURL collection\n"));
    out.push_str(&format!(
        "# Usage: BASE_URL={base_url} bash rustybin-curl.sh\n"
    ));
    out.push_str("set -euo pipefail\n\n");
    out.push_str(&format!("BASE_URL=\"${{BASE_URL:-{base_url}}}\"\n\n"));

    for cat in categories {
        out.push_str(&format!(
            "# ── {} ──────────────────────────────────────\n\n",
            cat.name
        ));
        for req in &cat.requests {
            out.push_str(&format!("echo \">>> {}\"\n", req.name));

            let (path, query) = split_path_query(req.path);
            let mut cmd = format!("curl -sS -X {} \"${{BASE_URL}}{path}", req.method);
            if !query.is_empty() {
                cmd.push('?');
                cmd.push_str(query);
            }
            cmd.push('"');

            // Headers
            for (k, v) in &req.headers {
                cmd.push_str(&format!(" \\\n  -H '{k}: {v}'"));
            }

            // Auth
            if let Some(ref auth) = req.auth {
                match auth {
                    AuthDef::Basic { user, pass } => {
                        cmd.push_str(&format!(" \\\n  -u '{user}:{pass}'"));
                    }
                    AuthDef::Bearer(token) => {
                        cmd.push_str(&format!(" \\\n  -H 'Authorization: Bearer {token}'"));
                    }
                }
            }

            // Body
            if let Some(ref body) = req.body {
                match body {
                    BodyDef::Json(raw) => {
                        cmd.push_str(&format!(" \\\n  -d '{raw}'"));
                    }
                    BodyDef::Form(params) => {
                        for (k, v) in *params {
                            cmd.push_str(&format!(" \\\n  --data-urlencode '{k}={v}'"));
                        }
                    }
                    BodyDef::Xml(raw) => {
                        cmd.push_str(&format!(" \\\n  -d '{raw}'"));
                    }
                }
            }

            out.push_str(&cmd);
            out.push_str("\necho\n\n");
        }
    }

    out.push_str("echo \"Done - all requests sent.\"\n");
    out
}
