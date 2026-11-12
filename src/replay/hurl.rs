//! Saved tests as one Hurl file for CI: in-process tests call
//! `{{base_url}}` (pass `--variable base_url=https://...`), gateway tests
//! their gateway; `{{name}}` variables stay Hurl variables.

use serde_json::Value;

use std::collections::BTreeMap;

use super::{Assertion, SavedTest, TestRequest};

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

/// `{{steps.<id>.outputs.<name>}}` as the Hurl variable its capture sets.
fn capture_name(id: &str, name: &str) -> String {
    format!("{id}_{name}").replace('-', "_")
}

/// A text with its step references as Hurl variables.
fn with_captures(text: &str) -> String {
    let mut out = text.to_string();
    for (id, name) in super::run::step_refs(text) {
        // Whatever the spacing inside the braces.
        let mut rest = out.as_str();
        let mut next = String::with_capacity(out.len());
        while let Some(start) = rest.find("{{") {
            next.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find("}}") else {
                next.push_str(&rest[start..]);
                rest = "";
                break;
            };
            if after[..end].trim() == format!("steps.{id}.outputs.{name}") {
                next.push_str(&format!("{{{{{}}}}}", capture_name(&id, &name)));
            } else {
                next.push_str(&rest[start..start + 2 + end + 2]);
            }
            rest = &after[end + 2..];
        }
        next.push_str(rest);
        out = next;
    }
    out
}

fn assert_lines(assert: &[Assertion]) -> Vec<String> {
    let mut asserts = Vec::new();
    for a in assert {
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
    asserts
}

/// One Hurl entry: the request, the captures of a step's outputs, the
/// asserts.
fn entry(
    base: &str,
    r: &TestRequest,
    assert: &[Assertion],
    step: Option<(&str, &BTreeMap<String, String>)>,
) -> String {
    let mut s = format!("{} {base}{}\n", r.method, with_captures(&r.path));
    for (k, v) in &r.headers {
        s.push_str(&format!("{k}: {}\n", with_captures(v)));
    }
    if let Some(b) = &r.body {
        if b.contains("```") {
            s.push_str(
                "# the body contains ``` and cannot be written as a Hurl multiline string\n",
            );
        } else {
            s.push_str(&format!("```\n{}\n```\n", with_captures(b)));
        }
    }
    s.push_str("HTTP *\n");
    if let Some((id, outputs)) = step.filter(|(_, o)| !o.is_empty()) {
        s.push_str("[Captures]\n");
        for (name, expr) in outputs {
            let var = capture_name(id, name);
            let expr = expr.trim();
            if expr == "$statusCode" {
                s.push_str(&format!("{var}: status\n"));
            } else if let Some(h) = expr.strip_prefix("$response.header.") {
                s.push_str(&format!("{var}: header {}\n", quote(h)));
            } else if let Some(p) = expr.strip_prefix("$response.body#") {
                s.push_str(&format!(
                    "{var}: jsonpath {}\n",
                    quote(&super::arazzo::pointer_path(p))
                ));
            }
        }
    }
    let asserts = assert_lines(assert);
    if !asserts.is_empty() {
        s.push_str("[Asserts]\n");
        for a in asserts {
            s.push_str(&a);
            s.push('\n');
        }
    }
    s
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
    if t.steps.is_empty() {
        s.push_str(&entry(&base, &t.request, &t.assert, None));
        return s;
    }
    // A multi-step test: an entry a step, in order; a step's outputs are
    // captures the later entries use.
    for step in &t.steps {
        s.push_str(&format!("# step {}\n", step.id));
        s.push_str(&entry(
            &base,
            &step.request,
            &step.assert,
            Some((&step.id, &step.outputs)),
        ));
        s.push('\n');
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
