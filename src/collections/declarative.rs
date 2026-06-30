use std::collections::{BTreeMap, BTreeSet};

use super::{split_path_query, Category};

pub fn build(categories: &[Category]) -> String {
    // Collect unique (path, methods) — aggregate methods per path
    let mut path_methods: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for cat in categories {
        for req in &cat.requests {
            let (path, _query) = split_path_query(req.path);
            path_methods
                .entry(path.to_string())
                .or_default()
                .insert(req.method.to_string());
        }
    }

    let version = env!("CARGO_PKG_VERSION");
    let mut out = String::with_capacity(4096);

    out.push_str(&format!(
        "# Rustybin v{version} — Gateway declarative configuration\n"
    ));
    out.push_str("# Apply: your gateway's sync tool: rustybin-gateway.yaml\n\n");
    out.push_str("_format_version: \"3.0\"\n\n");

    out.push_str("services:\n");
    out.push_str("  - name: rustybin\n");
    out.push_str("    url: http://localhost:80\n");
    out.push_str("    routes:\n");

    for (path, methods) in &path_methods {
        let route_name = path
            .trim_start_matches('/')
            .replace(['/', '.'], "-");
        let route_name = if route_name.is_empty() {
            "root".to_string()
        } else {
            route_name
        };

        out.push_str(&format!("      - name: rustybin-{route_name}\n"));
        out.push_str("        paths:\n");
        out.push_str(&format!("          - {path}\n"));
        out.push_str("        methods:\n");
        for m in methods {
            out.push_str(&format!("          - {m}\n"));
        }
        out.push_str("        strip_path: false\n");
    }

    out
}
