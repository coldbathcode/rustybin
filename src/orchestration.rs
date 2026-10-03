//! Multi-step orchestration pipeline: a 4-step payment flow for gateway
//! request chaining / workflow demos.
//!
//! 1. authenticate (non-empty `X-Api-Key`) -> correlation id
//! 2. enrich (`X-Correlation-Id`) -> deterministic risk score 0..=99
//! 3. validate -> approved unless a rule fails (risk score >= 70, amount
//!    over 50,000, card_payment not permitted)
//! 4. process (`X-Validation-Result: approved`)
//!
//! The risk score is a stable hash (FNV-1a) of the correlation id, merchant,
//! amount and card BIN, so roughly 30% of flows are declined at step 3 and
//! the same input always gets the same answer. Every error (including
//! malformed JSON) goes through content negotiation. The module keeps no
//! state.

use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::{get, post},
    Router,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::catalog::{category, Endpoint, Example};
use crate::content_negotiation::{negotiate, negotiate_with_status};
use crate::state::AppState;
use crate::types::ErrorResponse;

/// Risk scores at or above this value are declined.
pub const RISK_THRESHOLD: u32 = 70;
/// Maximum amount (major currency units) approved by step 3.
pub const AMOUNT_LIMIT: f64 = 50_000.0;
/// Maximum accepted header value length (ids).
const MAX_ID_LEN: usize = 128;

// ── Request / Response types ────────────────────────────────────────

#[derive(Deserialize, Default)]
#[serde(default)]
struct Step1Request {
    merchant_id: Option<String>,
    #[allow(dead_code)]
    request_type: Option<String>,
}

#[derive(Serialize)]
struct Step1Response {
    step: u8,
    name: &'static str,
    status: &'static str,
    correlation_id: String,
    merchant: MerchantInfo,
    permissions: Vec<&'static str>,
    timestamp: String,
}

#[derive(Serialize)]
struct MerchantInfo {
    id: String,
    name: &'static str,
    tier: &'static str,
    mcc: &'static str,
    country: &'static str,
}

/// Amounts are numbers in major currency units (`99.99`).
#[derive(Deserialize, Default)]
#[serde(default)]
struct Step2Request {
    merchant_id: Option<String>,
    amount: Option<f64>,
    currency: Option<String>,
    card_bin: Option<String>,
    card_number: Option<String>,
}

#[derive(Serialize)]
struct Step2Response {
    step: u8,
    name: &'static str,
    status: &'static str,
    correlation_id: String,
    enrichment: Enrichment,
    timestamp: String,
}

#[derive(Serialize)]
struct Enrichment {
    risk_score: u32,
    risk_level: &'static str,
    risk_threshold: u32,
    card_type: &'static str,
    issuing_bank: &'static str,
    issuing_country: &'static str,
    customer_tier: &'static str,
    velocity_check: VelocityCheck,
}

#[derive(Serialize)]
struct VelocityCheck {
    transactions_24h: u32,
    amount_24h: u64,
    flagged: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Step3Request {
    merchant_id: Option<String>,
    amount: Option<f64>,
    risk_score: Option<u32>,
    card_bin: Option<String>,
    card_number: Option<String>,
    permissions: Option<Vec<String>>,
}

#[derive(Serialize)]
struct Step3Response {
    step: u8,
    name: &'static str,
    status: &'static str,
    correlation_id: String,
    validation: Validation,
    timestamp: String,
}

#[derive(Serialize)]
struct Validation {
    approved: bool,
    /// `approved` or `declined`: the value to send as `X-Validation-Result`.
    result: &'static str,
    risk_score: u32,
    risk_score_source: &'static str,
    rules_evaluated: u32,
    rules_passed: u32,
    rules_failed: u32,
    applied_rules: Vec<RuleResult>,
}

#[derive(Serialize)]
struct RuleResult {
    rule: &'static str,
    result: &'static str,
    detail: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Step4Request {
    merchant_id: Option<String>,
    amount: Option<f64>,
    currency: Option<String>,
}

#[derive(Serialize)]
struct Step4Response {
    step: u8,
    name: &'static str,
    status: &'static str,
    correlation_id: String,
    transaction: Transaction,
    timestamp: String,
}

#[derive(Serialize)]
struct Transaction {
    id: String,
    merchant_id: String,
    amount: f64,
    currency: String,
    status: &'static str,
    authorization_code: String,
    processor_response: ProcessorResponse,
}

#[derive(Serialize)]
struct ProcessorResponse {
    code: &'static str,
    message: &'static str,
}

// ── Helpers ─────────────────────────────────────────────────────────

fn now_timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn error(headers: &HeaderMap, status: StatusCode, error: &str, details: String) -> Response {
    negotiate_with_status(
        headers,
        &ErrorResponse {
            error: error.to_string(),
            details: Some(details),
        },
        status,
    )
}

/// Parse a JSON body (empty body = `{}`), answering malformed input with a
/// negotiated 400 instead of axum's plain-text rejection.
#[allow(clippy::result_large_err)]
fn parse_body<T: DeserializeOwned + Default>(
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<T, Response> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|e| {
        error(
            headers,
            StatusCode::BAD_REQUEST,
            "invalid_json",
            format!("Request body must be a JSON object: {e}"),
        )
    })
}

/// A required, non-empty id header (trimmed, at most 128 visible chars).
#[allow(clippy::result_large_err)]
fn required_header(
    headers: &HeaderMap,
    name: &str,
    status: StatusCode,
) -> Result<String, Response> {
    let value = headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .unwrap_or("");
    if value.is_empty() || value.len() > MAX_ID_LEN {
        let error_code = if status == StatusCode::UNAUTHORIZED {
            "unauthorized"
        } else {
            "bad_request"
        };
        return Err(error(
            headers,
            status,
            error_code,
            format!("{name} header is required (1-{MAX_ID_LEN} characters)"),
        ));
    }
    Ok(value.to_string())
}

/// FNV-1a 64-bit: stable across Rust versions and platforms.
fn fnv1a(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for b in part.bytes().chain(std::iter::once(0x1f)) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

fn card_bin(bin: Option<&str>, number: Option<&str>) -> String {
    bin.or(number)
        .map(|s| s.chars().filter(char::is_ascii_digit).take(6).collect())
        .unwrap_or_default()
}

/// Deterministic risk score in 0..=99 for the given inputs.
fn risk_score(correlation_id: &str, merchant_id: &str, amount: Option<f64>, bin: &str) -> u32 {
    let amount = amount.map(|a| format!("{a:.2}")).unwrap_or_default();
    (fnv1a(&[correlation_id, merchant_id, &amount, bin]) % 100) as u32
}

fn risk_level(score: u32) -> &'static str {
    match score {
        s if s >= RISK_THRESHOLD => "high",
        s if s >= 30 => "medium",
        _ => "low",
    }
}

fn gen_auth_code() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("AUTH-{}", id[..6].to_uppercase())
}

fn valid_amount(amount: Option<f64>) -> bool {
    amount.is_none_or(|a| a.is_finite() && a >= 0.0)
}

// ── Handlers ────────────────────────────────────────────────────────

async fn step1(headers: HeaderMap, body: Bytes) -> Response {
    if let Err(resp) = required_header(&headers, "x-api-key", StatusCode::UNAUTHORIZED) {
        return resp;
    }
    let body: Step1Request = match parse_body(&headers, &body) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let merchant_id = match body.merchant_id.map(|m| m.trim().to_string()) {
        Some(id) if !id.is_empty() && id.len() <= MAX_ID_LEN => id,
        _ => {
            return error(
                &headers,
                StatusCode::BAD_REQUEST,
                "bad_request",
                "merchant_id is required in the request body (1-128 characters)".to_string(),
            );
        }
    };

    negotiate(
        &headers,
        &Step1Response {
            step: 1,
            name: "authenticate",
            status: "success",
            correlation_id: uuid::Uuid::new_v4().to_string(),
            merchant: MerchantInfo {
                id: merchant_id,
                name: "Demo Merchant Ltd",
                tier: "gold",
                mcc: "5411",
                country: "GB",
            },
            permissions: vec!["card_payment", "direct_debit", "refund"],
            timestamp: now_timestamp(),
        },
    )
}

async fn step2(headers: HeaderMap, body: Bytes) -> Response {
    let correlation_id =
        match required_header(&headers, "x-correlation-id", StatusCode::BAD_REQUEST) {
            Ok(id) => id,
            Err(resp) => return resp,
        };
    let body: Step2Request = match parse_body(&headers, &body) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    if !valid_amount(body.amount) {
        return error(
            &headers,
            StatusCode::BAD_REQUEST,
            "bad_request",
            "amount must be a non-negative number".to_string(),
        );
    }
    let _ = body.currency;
    let bin = card_bin(body.card_bin.as_deref(), body.card_number.as_deref());
    let merchant = body.merchant_id.unwrap_or_default();
    let score = risk_score(&correlation_id, &merchant, body.amount, &bin);
    let h = fnv1a(&[&correlation_id, "velocity"]);
    let card_type = match bin.chars().next() {
        Some('4') => "visa_credit",
        Some('5') | Some('2') => "mastercard_credit",
        Some('3') => "amex",
        _ => "visa_credit",
    };

    negotiate(
        &headers,
        &Step2Response {
            step: 2,
            name: "enrich",
            status: "success",
            correlation_id,
            enrichment: Enrichment {
                risk_score: score,
                risk_level: risk_level(score),
                risk_threshold: RISK_THRESHOLD,
                card_type,
                issuing_bank: "Demo Bank PLC",
                issuing_country: "GB",
                customer_tier: "premium",
                velocity_check: VelocityCheck {
                    transactions_24h: (h % 10) as u32 + 1,
                    amount_24h: ((h >> 16) % 30_000) + 1_000,
                    flagged: score >= RISK_THRESHOLD,
                },
            },
            timestamp: now_timestamp(),
        },
    )
}

async fn step3(headers: HeaderMap, body: Bytes) -> Response {
    let correlation_id =
        match required_header(&headers, "x-correlation-id", StatusCode::BAD_REQUEST) {
            Ok(id) => id,
            Err(resp) => return resp,
        };
    let body: Step3Request = match parse_body(&headers, &body) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    if !valid_amount(body.amount) {
        return error(
            &headers,
            StatusCode::BAD_REQUEST,
            "bad_request",
            "amount must be a non-negative number".to_string(),
        );
    }

    let amount = body.amount.unwrap_or(0.0);
    // The score from step 2 when the client forwards it, else the same
    // deterministic computation step 2 would have made.
    let (score, source) = match body.risk_score {
        Some(s) => (s.min(100), "request"),
        None => {
            let bin = card_bin(body.card_bin.as_deref(), body.card_number.as_deref());
            let merchant = body.merchant_id.clone().unwrap_or_default();
            (
                risk_score(&correlation_id, &merchant, body.amount, &bin),
                "computed",
            )
        }
    };
    let permission_ok = body
        .permissions
        .as_ref()
        .is_none_or(|p| p.iter().any(|s| s == "card_payment"));
    let amount_ok = amount <= AMOUNT_LIMIT;
    let risk_ok = score < RISK_THRESHOLD;

    let rules = vec![
        RuleResult {
            rule: "amount_limit",
            result: if amount_ok { "pass" } else { "fail" },
            detail: if amount_ok {
                format!("Amount {amount:.2} within the {AMOUNT_LIMIT:.0} limit")
            } else {
                format!("Amount {amount:.2} exceeds the {AMOUNT_LIMIT:.0} limit")
            },
        },
        RuleResult {
            rule: "risk_threshold",
            result: if risk_ok { "pass" } else { "fail" },
            detail: if risk_ok {
                format!("Risk score {score} is below the threshold {RISK_THRESHOLD}")
            } else {
                format!("Risk score {score} is at or above the threshold {RISK_THRESHOLD}")
            },
        },
        RuleResult {
            rule: "merchant_active",
            result: "pass",
            detail: "Merchant is active".to_string(),
        },
        RuleResult {
            rule: "permission_check",
            result: if permission_ok { "pass" } else { "fail" },
            detail: if permission_ok {
                "card_payment permitted".to_string()
            } else {
                "card_payment not in permissions".to_string()
            },
        },
        RuleResult {
            rule: "velocity_check",
            result: "pass",
            detail: "Under 24h velocity limit".to_string(),
        },
    ];

    let failed = rules.iter().filter(|r| r.result == "fail").count() as u32;
    let approved = failed == 0;

    negotiate(
        &headers,
        &Step3Response {
            step: 3,
            name: "validate",
            status: "success",
            correlation_id,
            validation: Validation {
                approved,
                result: if approved { "approved" } else { "declined" },
                risk_score: score,
                risk_score_source: source,
                rules_evaluated: rules.len() as u32,
                rules_passed: rules.len() as u32 - failed,
                rules_failed: failed,
                applied_rules: rules,
            },
            timestamp: now_timestamp(),
        },
    )
}

async fn step4(headers: HeaderMap, body: Bytes) -> Response {
    let correlation_id =
        match required_header(&headers, "x-correlation-id", StatusCode::BAD_REQUEST) {
            Ok(id) => id,
            Err(resp) => return resp,
        };
    let validation_result = headers
        .get("x-validation-result")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .unwrap_or("");
    if !validation_result.eq_ignore_ascii_case("approved") {
        return error(
            &headers,
            StatusCode::FORBIDDEN,
            "transaction_denied",
            "Validation did not approve this transaction (X-Validation-Result: approved required)"
                .to_string(),
        );
    }
    let body: Step4Request = match parse_body(&headers, &body) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    if !valid_amount(body.amount) {
        return error(
            &headers,
            StatusCode::BAD_REQUEST,
            "bad_request",
            "amount must be a non-negative number".to_string(),
        );
    }

    negotiate(
        &headers,
        &Step4Response {
            step: 4,
            name: "process",
            status: "success",
            correlation_id,
            transaction: Transaction {
                id: uuid::Uuid::new_v4().to_string(),
                merchant_id: body.merchant_id.unwrap_or_else(|| "unknown".to_string()),
                amount: body.amount.unwrap_or(0.0),
                currency: body.currency.unwrap_or_else(|| "GBP".to_string()),
                status: "completed",
                authorization_code: gen_auth_code(),
                processor_response: ProcessorResponse {
                    code: "00",
                    message: "Approved",
                },
            },
            timestamp: now_timestamp(),
        },
    )
}

// ── Status types ────────────────────────────────────────────────────

#[derive(Serialize)]
struct StatusDoc {
    description: &'static str,
    risk_threshold: u32,
    amount_limit: f64,
    steps: Vec<StepDoc>,
}

#[derive(Serialize)]
struct StepDoc {
    step: u8,
    name: &'static str,
    method: &'static str,
    path: &'static str,
    description: &'static str,
    required_headers: Vec<HeaderDoc>,
    example_body: &'static str,
    output_keys: Vec<&'static str>,
}

#[derive(Serialize)]
struct HeaderDoc {
    name: &'static str,
    value: &'static str,
}

async fn status_handler(headers: HeaderMap) -> Response {
    let doc = StatusDoc {
        description: "Multi-step orchestration pipeline - 4-step payment processing flow. \
                      Step 3 declines about 30% of flows (deterministic risk score >= 70).",
        risk_threshold: RISK_THRESHOLD,
        amount_limit: AMOUNT_LIMIT,
        steps: vec![
            StepDoc {
                step: 1,
                name: "authenticate",
                method: "POST",
                path: "/orchestration/step/1",
                description:
                    "Validate API key and return merchant metadata with a correlation token",
                required_headers: vec![HeaderDoc {
                    name: "X-Api-Key",
                    value: "any non-empty value",
                }],
                example_body: r#"{"merchant_id":"M001","request_type":"payment"}"#,
                output_keys: vec!["correlation_id", "merchant", "permissions"],
            },
            StepDoc {
                step: 2,
                name: "enrich",
                method: "POST",
                path: "/orchestration/step/2",
                description: "Enrich the transaction with a deterministic risk score (0-99), card and customer data",
                required_headers: vec![HeaderDoc {
                    name: "X-Correlation-Id",
                    value: "<from step 1>",
                }],
                example_body: r#"{"merchant_id":"M001","amount":50.00,"currency":"GBP","card_bin":"411111"}"#,
                output_keys: vec![
                    "enrichment.risk_score",
                    "enrichment.risk_level",
                    "enrichment.velocity_check",
                ],
            },
            StepDoc {
                step: 3,
                name: "validate",
                method: "POST",
                path: "/orchestration/step/3",
                description: "Run validation rules (risk score < 70, amount <= 50000, card_payment permitted)",
                required_headers: vec![HeaderDoc {
                    name: "X-Correlation-Id",
                    value: "<from step 1>",
                }],
                example_body: r#"{"merchant_id":"M001","amount":50.00,"risk_score":15,"permissions":["card_payment"]}"#,
                output_keys: vec![
                    "validation.approved",
                    "validation.result",
                    "validation.applied_rules",
                ],
            },
            StepDoc {
                step: 4,
                name: "process",
                method: "POST",
                path: "/orchestration/step/4",
                description: "Process the final transaction after validation approval",
                required_headers: vec![
                    HeaderDoc {
                        name: "X-Correlation-Id",
                        value: "<from step 1>",
                    },
                    HeaderDoc {
                        name: "X-Validation-Result",
                        value: "approved",
                    },
                ],
                example_body: r#"{"merchant_id":"M001","amount":50.00,"currency":"GBP"}"#,
                output_keys: vec![
                    "transaction.id",
                    "transaction.authorization_code",
                    "transaction.status",
                ],
            },
        ],
    };

    negotiate(&headers, &doc)
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/orchestration/step/1", post(step1))
        .route("/orchestration/step/2", post(step2))
        .route("/orchestration/step/3", post(step3))
        .route("/orchestration/step/4", post(step4))
        .route("/orchestration/status", get(status_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/orchestration/step/1",
            &["POST"],
            category::ORCHESTRATION,
            "Step 1: authenticate (non-empty X-Api-Key required)",
        )
        .example(
            Example::post("Step 1: Authenticate", "/orchestration/step/1")
                .header("X-Api-Key", "my-api-key")
                .json(r#"{"merchant_id":"merchant_123"}"#),
        ),
        Endpoint::new(
            "/orchestration/step/2",
            &["POST"],
            category::ORCHESTRATION,
            "Step 2: enrich with a deterministic risk score (X-Correlation-Id required)",
        )
        .example(
            Example::post("Step 2: Enrich", "/orchestration/step/2")
                .header("X-Correlation-Id", "<from-step-1>")
                .json(r#"{"merchant_id":"merchant_123","card_number":"4111111111111111","amount":99.99}"#),
        ),
        Endpoint::new(
            "/orchestration/step/3",
            &["POST"],
            category::ORCHESTRATION,
            "Step 3: validate (declined when risk score >= 70 or amount > 50000)",
        )
        .example(
            Example::post("Step 3: Validate", "/orchestration/step/3")
                .header("X-Correlation-Id", "<from-step-1>")
                .json(r#"{"merchant_id":"merchant_123","amount":99.99,"currency":"USD","risk_score":35}"#),
        ),
        Endpoint::new(
            "/orchestration/step/4",
            &["POST"],
            category::ORCHESTRATION,
            "Step 4: process (requires X-Validation-Result: approved)",
        )
        .example(
            Example::post("Step 4: Process", "/orchestration/step/4")
                .header("X-Correlation-Id", "<from-step-1>")
                .header("X-Validation-Result", "approved")
                .json(r#"{"amount":99.99,"currency":"USD"}"#),
        ),
        Endpoint::new(
            "/orchestration/status",
            &["GET"],
            category::ORCHESTRATION,
            "Pipeline documentation",
        )
        .example(Example::get("Pipeline status", "/orchestration/status")),
    ]
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{body_json, get_request};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_app() -> Router {
        crate::test_support::module_app(router)
    }

    async fn post(
        uri: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> (StatusCode, serde_json::Value) {
        let mut b = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let resp = test_app()
            .oneshot(b.body(Body::from(body.to_string())).expect("request"))
            .await
            .expect("response");
        let status = resp.status();
        (status, body_json(resp).await)
    }

    #[tokio::test]
    async fn step1_success_and_validation() {
        let (status, json) = post(
            "/orchestration/step/1",
            &[("x-api-key", "test-key")],
            r#"{"merchant_id":"M001","request_type":"payment"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["step"], 1);
        assert_eq!(json["merchant"]["id"], "M001");
        assert!(json["correlation_id"].is_string());

        let (status, json) = post("/orchestration/step/1", &[], r#"{"merchant_id":"M001"}"#).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(json["error"], "unauthorized");
        // Present but empty / blank is rejected too.
        let (status, _) = post(
            "/orchestration/step/1",
            &[("x-api-key", "   ")],
            r#"{"merchant_id":"M001"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = post(
            "/orchestration/step/1",
            &[("x-api-key", "k")],
            r#"{"request_type":"payment"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn malformed_json_is_negotiated() {
        let (status, json) =
            post("/orchestration/step/1", &[("x-api-key", "k")], "{not json").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"], "invalid_json");
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/orchestration/step/2")
                    .header("x-correlation-id", "c")
                    .header("accept", "application/xml")
                    .body(Body::from("[1,2"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(resp.headers()["content-type"], "application/xml");
        // Wrong types (amount as a string) are also a negotiated 400.
        let (status, json) = post(
            "/orchestration/step/2",
            &[("x-correlation-id", "c")],
            r#"{"amount":"lots"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"], "invalid_json");
    }

    #[tokio::test]
    async fn step2_is_deterministic_and_accepts_decimal_amounts() {
        let body = r#"{"merchant_id":"M001","amount":99.99,"card_number":"4111111111111111"}"#;
        let (status, a) = post(
            "/orchestration/step/2",
            &[("x-correlation-id", "corr-1")],
            body,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (_, b) = post(
            "/orchestration/step/2",
            &[("x-correlation-id", "corr-1")],
            body,
        )
        .await;
        assert_eq!(a["enrichment"]["risk_score"], b["enrichment"]["risk_score"]);
        assert_eq!(a["enrichment"]["card_type"], "visa_credit");
        let (status, _) = post("/orchestration/step/2", &[], body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn risk_distribution_declines_a_realistic_share() {
        let mut high = 0;
        let mut levels = std::collections::HashSet::new();
        for i in 0..1000 {
            let s = risk_score(&format!("corr-{i}"), "M001", Some(10.0), "411111");
            assert!(s < 100);
            levels.insert(risk_level(s));
            if s >= RISK_THRESHOLD {
                high += 1;
            }
        }
        assert!((200..400).contains(&high), "{high} of 1000 declined");
        assert_eq!(levels.len(), 3);
    }

    /// Step 2 and step 3 (without a forwarded score) agree, and a high score
    /// is declined with the right message.
    #[tokio::test]
    async fn step3_fails_when_risk_is_high() {
        let corr = (0..)
            .map(|i| format!("corr-{i}"))
            .find(|c| risk_score(c, "M001", Some(10.0), "") >= RISK_THRESHOLD)
            .unwrap_or_default();
        let (_, s2) = post(
            "/orchestration/step/2",
            &[("x-correlation-id", &corr)],
            r#"{"merchant_id":"M001","amount":10.0}"#,
        )
        .await;
        let score = s2["enrichment"]["risk_score"].as_u64().unwrap_or(0);
        assert!(score >= u64::from(RISK_THRESHOLD));
        assert_eq!(s2["enrichment"]["risk_level"], "high");
        assert_eq!(s2["enrichment"]["velocity_check"]["flagged"], true);

        let (status, s3) = post(
            "/orchestration/step/3",
            &[("x-correlation-id", &corr)],
            r#"{"merchant_id":"M001","amount":10.0}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(s3["validation"]["approved"], false);
        assert_eq!(s3["validation"]["result"], "declined");
        assert_eq!(s3["validation"]["risk_score"].as_u64(), Some(score));
        assert_eq!(s3["validation"]["risk_score_source"], "computed");
        let rule = &s3["validation"]["applied_rules"][1];
        assert_eq!(rule["rule"], "risk_threshold");
        assert_eq!(rule["result"], "fail");
        assert!(rule["detail"]
            .as_str()
            .unwrap_or("")
            .contains("at or above the threshold 70"));
    }

    #[tokio::test]
    async fn step3_rules() {
        let h = [("x-correlation-id", "c")];
        let (_, ok) = post(
            "/orchestration/step/3",
            &h,
            r#"{"amount":50.0,"risk_score":15,"permissions":["card_payment"]}"#,
        )
        .await;
        assert_eq!(ok["validation"]["approved"], true);
        assert_eq!(ok["validation"]["rules_evaluated"], 5);
        let (_, edge) = post("/orchestration/step/3", &h, r#"{"risk_score":70}"#).await;
        assert_eq!(edge["validation"]["approved"], false);
        let (_, edge) = post("/orchestration/step/3", &h, r#"{"risk_score":69}"#).await;
        assert_eq!(edge["validation"]["approved"], true);
        let (_, big) = post(
            "/orchestration/step/3",
            &h,
            r#"{"amount":50000.01,"risk_score":1}"#,
        )
        .await;
        assert_eq!(big["validation"]["approved"], false);
        assert_eq!(big["validation"]["rules_failed"], 1);
        let (_, perm) = post(
            "/orchestration/step/3",
            &h,
            r#"{"risk_score":1,"permissions":["refund"]}"#,
        )
        .await;
        assert_eq!(perm["validation"]["approved"], false);
        let (status, _) = post("/orchestration/step/3", &h, r#"{"amount":-5}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn step4_requires_approval() {
        let (status, json) = post(
            "/orchestration/step/4",
            &[
                ("x-correlation-id", "c"),
                ("x-validation-result", "approved"),
            ],
            r#"{"merchant_id":"M001","amount":50.5,"currency":"GBP"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["transaction"]["amount"], 50.5);
        assert!(json["transaction"]["authorization_code"]
            .as_str()
            .unwrap_or("")
            .starts_with("AUTH-"));
        let (status, _) = post(
            "/orchestration/step/4",
            &[
                ("x-correlation-id", "c"),
                ("x-validation-result", "declined"),
            ],
            "{}",
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = post("/orchestration/step/4", &[("x-correlation-id", "c")], "").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn status_returns_docs() {
        let resp = test_app()
            .oneshot(get_request("/orchestration/status"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["steps"].as_array().map(Vec::len), Some(4));
        assert_eq!(json["risk_threshold"], 70);
    }
}
