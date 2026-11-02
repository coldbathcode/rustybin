//! Saved tests as one Hurl file for CI: in-process tests call
//! `{{base_url}}` (pass `--variable base_url=https://...`), gateway tests
//! their gateway; `{{name}}` variables stay Hurl variables.

use serde_json::Value;

use super::SavedTest;

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A predicate value Hurl can compare (`None` for objects and arrays).
fn literal(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(quote(s)),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => Some("null".into()),
        _ => None,
    }
}

fn one(t: &SavedTest) -> String {
    let mut s = format!("# {}\n", t.name);
    if let Some(d) = &t.description {
        for line in d.lines() {
            s.push_str(&format!("# {line}\n"));
        }
    }
    let base = t
        .gateway
        .as_deref()
        .map(|g| g.trim_end_matches('/').to_string())
        .unwrap_or_else(|| "{{base_url}}".into());
    s.push_str(&format!("{} {base}{}\n", t.request.method, t.request.path));
    for (k, v) in &t.request.headers {
        s.push_str(&format!("{k}: {v}\n"));
    }
    if let Some(b) = &t.request.body {
        if b.contains("```") {
            s.push_str(
                "# the body contains ``` and cannot be written as a Hurl multiline string\n",
            );
        } else {
            s.push_str(&format!("```\n{b}\n```\n"));
        }
    }
    s.push_str("HTTP *\n");
    let mut asserts = Vec::new();
    for a in &t.assert {
        if let Some(code) = a.status {
            asserts.push(format!("status == {code}"));
        } else if let Some(h) = &a.header {
            match a.equals.as_ref().and_then(Value::as_str) {
                Some(v) => asserts.push(format!("header {} == {}", quote(h), quote(v))),
                None => asserts.push(format!("header {} exists", quote(h))),
            }
        } else if let Some(b) = &a.body_contains {
            asserts.push(format!("body contains {}", quote(b)));
        } else if let Some(p) = &a.json {
            match &a.equals {
                None => asserts.push(format!("jsonpath {} exists", quote(p))),
                Some(v) => match literal(v) {
                    Some(l) => asserts.push(format!("jsonpath {} == {l}", quote(p))),
                    None => asserts.push(format!(
                        "# jsonpath {} equals {v}: Hurl compares strings, numbers, booleans and null only",
                        quote(p)
                    )),
                },
            }
        } else if let Some(ms) = a.max_ms {
            asserts.push(format!("duration <= {ms}"));
        }
    }
    if !asserts.is_empty() {
        s.push_str("[Asserts]\n");
        for a in asserts {
            s.push_str(&a);
            s.push('\n');
        }
    }
    s
}

/// Every test, in name order.
pub fn export(tests: &[SavedTest]) -> String {
    let mut out = String::from(
        "# Rustybin saved tests (GET /_rustybin/tests/export).\n\
         # Run: hurl --test --variable base_url=https://<instance> [--variable NAME=value ...] tests.hurl\n\n",
    );
    for t in tests {
        out.push_str(&one(t));
        out.push('\n');
    }
    out
}
