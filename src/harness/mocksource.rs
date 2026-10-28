//! Mock-driven traffic (`docs/plans/harness.md`, phase 6): a layer whose
//! source names an OpenAPI mock sends requests for the operations of the
//! mock's live revision on the client node. When the run starts the source
//! is resolved into a [`MockPlan`]: a few request variants per operation
//! (generated parameters and bodies, `checks::sample_request_variant`, so
//! every one is a request the operation accepts), an invalid variant where
//! the operation has one, weights, and with `stateful` the create-then-read
//! pairs (a POST on a collection followed by the GET of the item it made).

use serde_json::Value;

use super::scenario::{RequestTemplate, Source, MAX_REQUESTS_PER_LAYER};
use crate::mocks::checks::{sample_request_variant, SampleRequest};
use crate::mocks::compile::Compiled;
use crate::mocks::spec::{Location, Operation};

/// Valid variants per operation.
pub const VARIANTS: u64 = 4;
/// Most operations a mock layer sends.
pub const MAX_OPERATIONS: usize = MAX_REQUESTS_PER_LAYER;
/// The placeholder a follow-up read's path carries for the created id.
pub const FOLLOW: &str = "{{follow}}";
/// The statuses an invalid request expects.
fn client_errors() -> Vec<u16> {
    (400..=499).collect()
}

/// One generated request of a plan.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedRequest {
    pub template: RequestTemplate,
    /// The operation it exercises (index into [`MockPlan::operations`]).
    pub operation: usize,
    pub invalid: bool,
    /// A create of a stateful pair: the read to send after it, and the
    /// field of the answer that holds the new item's id.
    pub follow: Option<(usize, String)>,
}

/// A mock source resolved against the client node's live revision.
#[derive(Clone, Debug, PartialEq)]
pub struct MockPlan {
    pub mock: String,
    pub operations: Vec<String>,
    pub requests: Vec<PlannedRequest>,
    /// Cumulative weights over `requests` (follow-up reads weigh nothing).
    pub cumulative: Vec<u32>,
}

impl MockPlan {
    /// The request for sequence number `seq`: a weighted pick, mixed so a
    /// short run already sees every operation.
    pub fn pick(&self, seq: u64) -> usize {
        let Some(&total) = self.cumulative.last() else {
            return 0;
        };
        let point = (mix(seq) % u64::from(total.max(1))) as u32;
        self.cumulative.iter().position(|&c| point < c).unwrap_or(0)
    }
}

/// splitmix64: spreads consecutive sequence numbers over the weights.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn encode_query(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A sample request as a request template under `base`.
fn template_of(base: &str, s: &SampleRequest, expect: Vec<u16>) -> RequestTemplate {
    let mut path = format!("{base}{}", s.path);
    if path.is_empty() {
        path = "/".into();
    }
    for (i, (k, v)) in s.query.iter().enumerate() {
        path.push(if i == 0 && !path.contains('?') {
            '?'
        } else {
            '&'
        });
        path.push_str(&encode_query(k));
        path.push('=');
        path.push_str(&encode_query(v));
    }
    let mut headers = std::collections::BTreeMap::new();
    for (k, v) in &s.headers {
        headers.insert(k.clone(), v.clone());
    }
    if let Some(ct) = &s.content_type {
        headers.insert("content-type".into(), ct.clone());
    }
    RequestTemplate {
        method: s.method.clone(),
        path,
        headers,
        // A string body would go as is: JSON bodies stay JSON values.
        body: s.body.clone(),
        expect_status: expect,
        weight: 1,
    }
}

/// The last path segment's parameter (`/pets/{petId}` -> `petId`).
fn item_param(path: &str) -> Option<&str> {
    let last = path.trim_end_matches('/').rsplit('/').next()?;
    last.strip_prefix('{')?.strip_suffix('}')
}

/// A path without its last segment.
fn parent(path: &str) -> &str {
    let p = path.trim_end_matches('/');
    match p.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &p[..i],
    }
}

/// The documented success statuses of an operation (empty: 2xx and 3xx).
fn success_statuses(op: &Operation) -> Vec<u16> {
    op.responses
        .iter()
        .filter_map(|r| r.code())
        .filter(|c| (200..400).contains(c))
        .collect()
}

/// An invalid form of a valid request: a body of the wrong type, a
/// required query parameter left out, or a numeric path parameter that is
/// not a number. `None` when the operation takes nothing to get wrong.
fn invalid_of(op: &Operation, valid: &SampleRequest) -> Option<SampleRequest> {
    let mut s = valid.clone();
    if let Some(Value::Object(_)) = &s.body {
        s.body = Some(Value::Array(Vec::new()));
        return Some(s);
    }
    if let Some(p) = op.params_in(Location::Query).find(|p| p.required) {
        let before = s.query.len();
        s.query.retain(|(k, _)| *k != p.name);
        if s.query.len() < before {
            return Some(s);
        }
    }
    for p in op.params_in(Location::Path) {
        let ty = p.schema.get("type").and_then(Value::as_str);
        if matches!(ty, Some("integer") | Some("number")) {
            let t: Vec<&str> = op.path.trim_start_matches('/').split('/').collect();
            let mut segs: Vec<String> = valid
                .path
                .trim_start_matches('/')
                .split('/')
                .map(str::to_string)
                .collect();
            let offset = segs.len().checked_sub(t.len())?;
            let i = t.iter().position(|seg| *seg == format!("{{{}}}", p.name))?;
            segs[offset + i] = "not-a-number".into();
            s.path = format!("/{}", segs.join("/"));
            return Some(s);
        }
    }
    None
}

/// The value `concrete` has where `template` has `{name}`.
fn segment_value(template: &str, concrete: &str, name: &str) -> Option<String> {
    let t: Vec<&str> = template.trim_start_matches('/').split('/').collect();
    let c: Vec<&str> = concrete.trim_start_matches('/').split('/').collect();
    let offset = c.len().checked_sub(t.len())?;
    let i = t.iter().position(|s| *s == format!("{{{name}}}"))?;
    c.get(offset + i).map(|s| s.to_string())
}

/// Resolve a mock source against a compiled revision.
pub fn resolve(c: &Compiled, src: &Source) -> Result<MockPlan, String> {
    let name = src.mock.clone().unwrap_or_default();
    let filter = src.operations.clone().unwrap_or_default();
    let known: Vec<&str> = c.spec.operations.iter().map(|o| o.id.as_str()).collect();
    let unknown: Vec<&String> = filter
        .include
        .iter()
        .chain(&filter.exclude)
        .chain(filter.weights.keys())
        .filter(|n| !known.contains(&n.as_str()))
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "mock {name} has no operation {} (it has {})",
            unknown
                .iter()
                .map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            known.join(", ")
        ));
    }
    let ops: Vec<&Operation> = c
        .spec
        .operations
        .iter()
        .filter(|o| filter.include.is_empty() || filter.include.contains(&o.id))
        .filter(|o| !filter.exclude.contains(&o.id))
        .collect();
    if ops.is_empty() {
        return Err(format!("mock {name}: no operation left to send"));
    }
    if ops.len() > MAX_OPERATIONS {
        return Err(format!(
            "mock {name} has {} operations, a layer sends at most {MAX_OPERATIONS} (use operations.include)",
            ops.len()
        ));
    }
    let base = src.mock_base_path();
    let invalid_fraction = src.invalid_fraction.unwrap_or(0.0).clamp(0.0, 1.0);
    let stateful = src.stateful.unwrap_or(false);
    let mut plan = MockPlan {
        mock: name,
        operations: ops.iter().map(|o| o.id.clone()).collect(),
        requests: Vec::new(),
        cumulative: Vec::new(),
    };
    let mut weights: Vec<u32> = Vec::new();
    // Stateful: the reads that follow creates, by collection path.
    let mut reads: Vec<(String, usize, String)> = Vec::new();
    if stateful {
        for (k, op) in ops.iter().enumerate() {
            if op.method == "GET" {
                if let Some(param) = item_param(&op.path) {
                    let variant = sample_request_variant(c, op, 0);
                    let mut t = template_of(&base, &variant, vec![200]);
                    // The created item's id goes where the sample's id was.
                    if let Some(v) = segment_value(&op.path, &variant.path, param) {
                        let cut = t.path.split('?').next().unwrap_or("").len();
                        let (p, q) = t.path.split_at(cut);
                        if let Some(stem) = p.strip_suffix(&v) {
                            t.path = format!("{stem}{FOLLOW}{q}");
                        }
                    }
                    if t.path.contains(FOLLOW) {
                        reads.push((
                            parent(&op.path).to_string(),
                            plan.requests.len(),
                            param.to_string(),
                        ));
                        plan.requests.push(PlannedRequest {
                            template: t,
                            operation: k,
                            invalid: false,
                            follow: None,
                        });
                        weights.push(0);
                    }
                }
            }
        }
    }
    for (k, op) in ops.iter().enumerate() {
        let w = filter.weights.get(&op.id).copied().unwrap_or(1).max(1);
        let mut expect = success_statuses(op);
        if stateful && item_param(&op.path).is_some() && !expect.is_empty() {
            // An item that was never created, or already deleted.
            expect.push(404);
        }
        let follow = (stateful && op.method == "POST" && item_param(&op.path).is_none())
            .then(|| {
                reads
                    .iter()
                    .find(|(collection, _, _)| {
                        collection.trim_end_matches('/') == op.path.trim_end_matches('/')
                    })
                    .map(|(_, i, param)| (*i, param.clone()))
            })
            .flatten();
        let valid_weight =
            (f64::from(w) * 100.0 * (1.0 - invalid_fraction) / VARIANTS as f64).round() as u32;
        let first = sample_request_variant(c, op, 0);
        for v in 0..VARIANTS {
            let sample = if v == 0 {
                first.clone()
            } else {
                sample_request_variant(c, op, v)
            };
            plan.requests.push(PlannedRequest {
                template: template_of(&base, &sample, expect.clone()),
                operation: k,
                invalid: false,
                follow: follow.clone(),
            });
            weights.push(if invalid_fraction < 1.0 {
                valid_weight.max(1)
            } else {
                0
            });
        }
        if invalid_fraction > 0.0 {
            if let Some(bad) = invalid_of(op, &first) {
                plan.requests.push(PlannedRequest {
                    template: template_of(&base, &bad, client_errors()),
                    operation: k,
                    invalid: true,
                    follow: None,
                });
                weights.push((f64::from(w) * 100.0 * invalid_fraction).round().max(1.0) as u32);
            }
        }
    }
    let mut acc = 0u32;
    plan.cumulative = weights
        .into_iter()
        .map(|w| {
            acc = acc.saturating_add(w);
            acc
        })
        .collect();
    Ok(plan)
}

/// The id of a created item: the `id` field of a JSON answer, else the
/// path parameter's name, else a field ending in `id`.
pub fn created_id(body: &[u8], param: &str) -> Option<String> {
    let Value::Object(m) = serde_json::from_slice::<Value>(body).ok()? else {
        return None;
    };
    let text = |v: &Value| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    m.get("id")
        .and_then(text)
        .or_else(|| m.get(param).and_then(text))
        .or_else(|| {
            m.iter()
                .find(|(k, _)| k.to_ascii_lowercase().ends_with("id"))
                .and_then(|(_, v)| text(v))
        })
        .filter(|id| !id.is_empty() && id.len() <= 256)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::scenario::OperationFilter;

    fn petstore() -> Compiled {
        Compiled::build(
            1,
            crate::mocks::SAMPLE_SPEC.into(),
            crate::mocks::sample_settings(),
            &crate::mocks::compile::tests::LIMITS,
        )
        .expect("petstore compiles")
    }

    fn source(yaml: &str) -> Source {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn every_operation_gets_variants_with_their_statuses() {
        let c = petstore();
        let plan = resolve(&c, &source("mock: petstore")).unwrap();
        assert_eq!(plan.operations.len(), c.spec.operations.len());
        assert_eq!(
            plan.requests.len(),
            plan.operations.len() * VARIANTS as usize
        );
        let get = plan
            .requests
            .iter()
            .find(|r| plan.operations[r.operation] == "getPetById")
            .unwrap();
        assert!(
            get.template.path.starts_with("/mock/petstore/"),
            "{}",
            get.template.path
        );
        assert!(get.template.expect_status.contains(&200));
        // Variants differ.
        let paths: std::collections::BTreeSet<&str> = plan
            .requests
            .iter()
            .filter(|r| plan.operations[r.operation] == "getPetById")
            .map(|r| r.template.path.as_str())
            .collect();
        assert!(paths.len() > 1, "{paths:?}");
        // Every operation is picked within a short run.
        let mut seen = std::collections::BTreeSet::new();
        for seq in 0..200 {
            seen.insert(plan.requests[plan.pick(seq)].operation);
        }
        assert_eq!(seen.len(), plan.operations.len());
    }

    #[test]
    fn filters_weights_invalid_and_stateful_pairs() {
        let c = petstore();
        let err = resolve(
            &c,
            &source("mock: petstore\noperations: { include: [nope] }"),
        )
        .unwrap_err();
        assert!(err.contains("no operation nope"), "{err}");
        let mut src =
            source("mock: petstore\ninvalid_fraction: 0.5\nstateful: true\nbase_path: ''");
        src.operations = Some(OperationFilter {
            exclude: vec!["deletePet".into()],
            ..OperationFilter::default()
        });
        let plan = resolve(&c, &src).unwrap();
        assert!(!plan.operations.contains(&"deletePet".to_string()));
        let invalid = plan.requests.iter().find(|r| r.invalid).unwrap();
        assert!(invalid.template.expect_status.contains(&400));
        assert!(!invalid.template.path.starts_with("/mock"));
        let create = plan
            .requests
            .iter()
            .find(|r| plan.operations[r.operation] == "createPet" && !r.invalid)
            .unwrap();
        let (read, param) = create.follow.clone().unwrap();
        assert_eq!(param, "petId");
        assert!(
            plan.requests[read].template.path.ends_with(FOLLOW),
            "{}",
            plan.requests[read].template.path
        );
        assert_eq!(
            created_id(br#"{"id": 7, "name": "x"}"#, "petId").as_deref(),
            Some("7")
        );
        assert_eq!(
            created_id(br#"{"petId": "a"}"#, "petId").as_deref(),
            Some("a")
        );
    }
}
