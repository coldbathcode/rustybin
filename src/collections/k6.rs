use super::{split_path_query, AuthDef, BodyDef, Category};

pub fn build(categories: &[Category], base_url: &str) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let mut out = String::with_capacity(16384);

    out.push_str(&format!("// Rustybin v{version} - k6 load test script\n"));
    out.push_str(&format!(
        "// Run: BASE_URL={base_url} k6 run rustybin-k6.js\n\n"
    ));
    out.push_str("import http from 'k6/http';\n");
    out.push_str("import { check, group } from 'k6';\n\n");
    out.push_str(&format!(
        "const BASE_URL = __ENV.BASE_URL || '{base_url}';\n\n"
    ));
    out.push_str("export const options = {\n");
    out.push_str("  vus: 1,\n");
    out.push_str("  iterations: 1,\n");
    out.push_str("};\n\n");
    out.push_str("export default function () {\n");

    for cat in categories {
        let group_name = cat.name.replace('\'', "\\'");
        out.push_str(&format!("  group('{}', function () {{\n", group_name));

        for req in &cat.requests {
            let (path, query) = split_path_query(req.path);
            let mut url = format!("${{BASE_URL}}{path}");
            if !query.is_empty() {
                url.push('?');
                url.push_str(query);
            }

            let req_name = req.name.replace('\'', "\\'");

            // Build headers object
            let mut header_entries: Vec<String> = Vec::new();
            for (k, v) in &req.headers {
                header_entries.push(format!("'{k}': '{v}'", k = escape_js(k), v = escape_js(v)));
            }
            if let Some(ref auth) = req.auth {
                match auth {
                    AuthDef::Basic { user, pass } => {
                        let encoded = base64_encode(&format!("{user}:{pass}"));
                        header_entries.push(format!("'Authorization': 'Basic {encoded}'"));
                    }
                    AuthDef::Bearer(token) => {
                        header_entries.push(format!("'Authorization': 'Bearer {token}'"));
                    }
                }
            }

            let headers_obj = if header_entries.is_empty() {
                "{}".to_string()
            } else {
                format!("{{ {} }}", header_entries.join(", "))
            };

            let method_lower = req.method.to_lowercase();

            match &req.body {
                Some(BodyDef::Json(raw)) => {
                    out.push_str(&format!(
                        "    const r_{safe} = http.{method_lower}(`{url}`, '{raw}', {{ headers: {headers_obj} }});\n",
                        safe = safe_var_name(req.name),
                        raw = escape_js(raw),
                    ));
                }
                Some(BodyDef::Form(params)) => {
                    let form_entries: Vec<String> = params
                        .iter()
                        .map(|(k, v)| format!("'{k}': '{v}'", k = escape_js(k), v = escape_js(v)))
                        .collect();
                    let form_obj = format!("{{ {} }}", form_entries.join(", "));
                    out.push_str(&format!(
                        "    const r_{safe} = http.{method_lower}(`{url}`, {form_obj}, {{ headers: {headers_obj} }});\n",
                        safe = safe_var_name(req.name),
                    ));
                }
                Some(BodyDef::Xml(raw)) => {
                    out.push_str(&format!(
                        "    const r_{safe} = http.{method_lower}(`{url}`, '{raw}', {{ headers: {headers_obj} }});\n",
                        safe = safe_var_name(req.name),
                        raw = escape_js(raw),
                    ));
                }
                None => {
                    if method_lower == "post" {
                        out.push_str(&format!(
                            "    const r_{safe} = http.{method_lower}(`{url}`, null, {{ headers: {headers_obj} }});\n",
                            safe = safe_var_name(req.name),
                        ));
                    } else {
                        out.push_str(&format!(
                            "    const r_{safe} = http.{method_lower}(`{url}`, {{ headers: {headers_obj} }});\n",
                            safe = safe_var_name(req.name),
                        ));
                    }
                }
            }

            out.push_str(&format!(
                "    check(r_{safe}, {{ '{req_name} responded': (r) => r.status > 0 }});\n",
                safe = safe_var_name(req.name),
            ));
        }

        out.push_str("  });\n\n");
    }

    out.push_str("}\n");
    out
}

fn safe_var_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_end_matches('_')
        .to_string()
}

fn escape_js(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

fn base64_encode(input: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(input.as_bytes())
}
