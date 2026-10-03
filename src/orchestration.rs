use axum::{
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::catalog::{category, Endpoint, Example};
use crate::content_negotiation::{negotiate, negotiate_with_status};
use crate::state::AppState;
use crate::types::ErrorResponse;

// ── Request / Response types ────────────────────────────────────────

#[derive(Deserialize)]
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

#[derive(Deserialize)]
struct Step2Request {
    #[allow(dead_code)]
    merchant_id: Option<String>,
    #[allow(dead_code)]
    amount: Option<u64>,
    #[allow(dead_code)]
    currency: Option<String>,
    #[allow(dead_code)]
    card_bin: Option<String>,
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

#[derive(Deserialize)]
struct Step3Request {
    #[allow(dead_code)]
    merchant_id: Option<String>,
    amount: Option<u64>,
    risk_score: Option<u32>,
    #[allow(dead_code)]
    risk_level: Option<String>,
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

#[derive(Deserialize)]
struct Step4Request {
    merchant_id: Option<String>,
    amount: Option<u64>,
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
    amount: u64,
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

fn hash_correlation(id: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

fn gen_auth_code() -> String {
    let id = uuid::Uuid::new_v4().to_string();
    let chars: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(6)
        .collect();
    format!("AUTH-{}", chars.to_uppercase())
}

// ── Handlers ────────────────────────────────────────────────────────

async fn step1(headers: HeaderMap, Json(body): Json<Step1Request>) -> Response {
    // Require X-Api-Key
    if headers.get("x-api-key").is_none() {
        return negotiate_with_status(
            &headers,
            &ErrorResponse {
                error: "unauthorized".to_string(),
                details: Some("X-Api-Key header is required".to_string()),
            },
            StatusCode::UNAUTHORIZED,
        );
    }

    let merchant_id = match body.merchant_id {
        Some(id) if !id.is_empty() => id,
        _ => {
            return negotiate_with_status(
                &headers,
                &ErrorResponse {
                    error: "bad_request".to_string(),
                    details: Some("merchant_id is required in request body".to_string()),
                },
                StatusCode::BAD_REQUEST,
            );
        }
    };

    let correlation_id = uuid::Uuid::new_v4().to_string();

    negotiate(
        &headers,
        &Step1Response {
            step: 1,
            name: "authenticate",
            status: "success",
            correlation_id,
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

async fn step2(headers: HeaderMap, Json(_body): Json<Step2Request>) -> Response {
    let correlation_id = match headers
        .get("x-correlation-id")
        .and_then(|v| v.to_str().ok())
    {
        Some(id) => id.to_string(),
        None => {
            return negotiate_with_status(
                &headers,
                &ErrorResponse {
                    error: "bad_request".to_string(),
                    details: Some("X-Correlation-Id header is required".to_string()),
                },
                StatusCode::BAD_REQUEST,
            );
        }
    };

    let h = hash_correlation(&correlation_id);
    let risk_score = (h % 50) as u32; // 0-49 range for deterministic but low scores
    let risk_level = if risk_score > 40 {
        "medium"
    } else if risk_score > 20 {
        "low"
    } else {
        "very_low"
    };
    let transactions_24h = ((h >> 8) % 10) as u32 + 1;
    let amount_24h = ((h >> 16) % 30000) + 1000;

    negotiate(
        &headers,
        &Step2Response {
            step: 2,
            name: "enrich",
            status: "success",
            correlation_id,
            enrichment: Enrichment {
                risk_score,
                risk_level,
                card_type: "visa_credit",
                issuing_bank: "Demo Bank PLC",
                issuing_country: "GB",
                customer_tier: "premium",
                velocity_check: VelocityCheck {
                    transactions_24h,
                    amount_24h,
                    flagged: false,
                },
            },
            timestamp: now_timestamp(),
        },
    )
}

async fn step3(headers: HeaderMap, Json(body): Json<Step3Request>) -> Response {
    let correlation_id = match headers
        .get("x-correlation-id")
        .and_then(|v| v.to_str().ok())
    {
        Some(id) => id.to_string(),
        None => {
            return negotiate_with_status(
                &headers,
                &ErrorResponse {
                    error: "bad_request".to_string(),
                    details: Some("X-Correlation-Id header is required".to_string()),
                },
                StatusCode::BAD_REQUEST,
            );
        }
    };

    let amount = body.amount.unwrap_or(0);
    let risk_score = body.risk_score.unwrap_or(0);
    let has_card_payment = body
        .permissions
        .as_ref()
        .map(|p| p.iter().any(|s| s == "card_payment"))
        .unwrap_or(true);

    let amount_ok = amount <= 5_000_000;
    let risk_ok = risk_score <= 70;
    let permission_ok = has_card_payment;

    let mut rules = vec![
        RuleResult {
            rule: "amount_limit",
            result: if amount_ok { "pass" } else { "fail" },
            detail: if amount_ok {
                "Under £50,000 limit".to_string()
            } else {
                format!("Amount {} exceeds £50,000 limit", amount)
            },
        },
        RuleResult {
            rule: "risk_threshold",
            result: if risk_ok { "pass" } else { "fail" },
            detail: if risk_ok {
                format!("Score {} < threshold 70", risk_score)
            } else {
                format!("Score {} exceeds threshold 70", risk_score)
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
    let passed = rules.len() as u32 - failed;
    let approved = failed == 0;

    // Ensure consistent ordering if needed
    let _ = &mut rules;

    negotiate(
        &headers,
        &Step3Response {
            step: 3,
            name: "validate",
            status: "success",
            correlation_id,
            validation: Validation {
                approved,
                rules_evaluated: rules.len() as u32,
                rules_passed: passed,
                rules_failed: failed,
                applied_rules: rules,
            },
            timestamp: now_timestamp(),
        },
    )
}

async fn step4(headers: HeaderMap, Json(body): Json<Step4Request>) -> Response {
    let correlation_id = match headers
        .get("x-correlation-id")
        .and_then(|v| v.to_str().ok())
    {
        Some(id) => id.to_string(),
        None => {
            return negotiate_with_status(
                &headers,
                &ErrorResponse {
                    error: "bad_request".to_string(),
                    details: Some("X-Correlation-Id header is required".to_string()),
                },
                StatusCode::BAD_REQUEST,
            );
        }
    };

    // Require X-Validation-Result: approved
    let validation_result = headers
        .get("x-validation-result")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if validation_result != "approved" {
        return negotiate_with_status(
            &headers,
            &ErrorResponse {
                error: "transaction_denied".to_string(),
                details: Some("Validation did not approve this transaction".to_string()),
            },
            StatusCode::FORBIDDEN,
        );
    }

    let merchant_id = body.merchant_id.unwrap_or_else(|| "unknown".to_string());
    let amount = body.amount.unwrap_or(0);
    let currency = body.currency.unwrap_or_else(|| "GBP".to_string());

    negotiate(
        &headers,
        &Step4Response {
            step: 4,
            name: "process",
            status: "success",
            correlation_id,
            transaction: Transaction {
                id: uuid::Uuid::new_v4().to_string(),
                merchant_id,
                amount,
                currency,
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
        description: "Multi-step orchestration pipeline - 4-step payment processing flow",
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
                description: "Enrich transaction with risk score, geo data, and customer tier",
                required_headers: vec![HeaderDoc {
                    name: "X-Correlation-Id",
                    value: "<from step 1>",
                }],
                example_body: r#"{"merchant_id":"M001","amount":5000,"currency":"GBP","card_bin":"411111"}"#,
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
                description: "Run validation rules on enriched data and return approval/denial",
                required_headers: vec![HeaderDoc {
                    name: "X-Correlation-Id",
                    value: "<from step 1>",
                }],
                example_body: r#"{"merchant_id":"M001","amount":5000,"risk_score":15,"risk_level":"low","permissions":["card_payment"]}"#,
                output_keys: vec!["validation.approved", "validation.applied_rules"],
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
                example_body: r#"{"merchant_id":"M001","amount":5000,"currency":"GBP"}"#,
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
            "Step 1: authenticate (X-Api-Key required)",
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
            "Step 2: enrich (X-Correlation-Id required)",
        )
        .example(
            Example::post("Step 2: Enrich", "/orchestration/step/2")
                .header("X-Correlation-Id", "<from-step-1>")
                .json(r#"{"card_number":"4111111111111111","amount":99.99}"#),
        ),
        Endpoint::new(
            "/orchestration/step/3",
            &["POST"],
            category::ORCHESTRATION,
            "Step 3: validate (risk scoring)",
        )
        .example(
            Example::post("Step 3: Validate", "/orchestration/step/3")
                .header("X-Correlation-Id", "<from-step-1>")
                .json(r#"{"amount":99.99,"currency":"USD"}"#),
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
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_app() -> Router {
        crate::test_support::module_app(router)
    }

    async fn json_body(resp: axum::http::Response<Body>) -> serde_json::Value {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&body).expect("json")
    }

    #[tokio::test]
    async fn step1_success() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/orchestration/step/1")
                    .header("content-type", "application/json")
                    .header("x-api-key", "test-key")
                    .body(Body::from(
                        r#"{"merchant_id":"M001","request_type":"payment"}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["step"], 1);
        assert_eq!(json["name"], "authenticate");
        assert_eq!(json["status"], "success");
        assert!(json["correlation_id"].is_string());
        assert_eq!(json["merchant"]["id"], "M001");
        assert_eq!(json["merchant"]["tier"], "gold");
        assert!(json["permissions"].is_array());
    }

    #[tokio::test]
    async fn step1_missing_api_key() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/orchestration/step/1")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"merchant_id":"M001"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn step1_missing_merchant_id() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/orchestration/step/1")
                    .header("content-type", "application/json")
                    .header("x-api-key", "test")
                    .body(Body::from(r#"{"request_type":"payment"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn step2_success() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/orchestration/step/2")
                    .header("content-type", "application/json")
                    .header("x-correlation-id", "test-corr-123")
                    .body(Body::from(
                        r#"{"merchant_id":"M001","amount":5000,"currency":"GBP","card_bin":"411111"}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["step"], 2);
        assert_eq!(json["name"], "enrich");
        assert_eq!(json["correlation_id"], "test-corr-123");
        assert!(json["enrichment"]["risk_score"].is_number());
        assert!(json["enrichment"]["risk_level"].is_string());
        assert_eq!(json["enrichment"]["card_type"], "visa_credit");
    }

    #[tokio::test]
    async fn step2_deterministic() {
        let app1 = test_app();
        let resp1 = app1
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/orchestration/step/2")
                    .header("content-type", "application/json")
                    .header("x-correlation-id", "same-id")
                    .body(Body::from(r#"{"merchant_id":"M001"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        let json1 = json_body(resp1).await;

        let app2 = test_app();
        let resp2 = app2
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/orchestration/step/2")
                    .header("content-type", "application/json")
                    .header("x-correlation-id", "same-id")
                    .body(Body::from(r#"{"merchant_id":"M001"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        let json2 = json_body(resp2).await;

        assert_eq!(
            json1["enrichment"]["risk_score"],
            json2["enrichment"]["risk_score"]
        );
    }

    #[tokio::test]
    async fn step2_missing_correlation() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/orchestration/step/2")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"merchant_id":"M001"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn step3_approved() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/orchestration/step/3")
                    .header("content-type", "application/json")
                    .header("x-correlation-id", "test-corr")
                    .body(Body::from(
                        r#"{"merchant_id":"M001","amount":5000,"risk_score":15,"risk_level":"low","permissions":["card_payment"]}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["step"], 3);
        assert_eq!(json["validation"]["approved"], true);
        assert_eq!(json["validation"]["rules_failed"], 0);
        assert_eq!(json["validation"]["rules_evaluated"], 5);
    }

    #[tokio::test]
    async fn step3_high_risk_denied() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/orchestration/step/3")
                    .header("content-type", "application/json")
                    .header("x-correlation-id", "test-corr")
                    .body(Body::from(
                        r#"{"merchant_id":"M001","amount":5000,"risk_score":85,"risk_level":"high","permissions":["card_payment"]}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["validation"]["approved"], false);
        assert_eq!(json["validation"]["rules_failed"], 1);
    }

    #[tokio::test]
    async fn step3_high_amount_denied() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/orchestration/step/3")
                    .header("content-type", "application/json")
                    .header("x-correlation-id", "test-corr")
                    .body(Body::from(
                        r#"{"merchant_id":"M001","amount":6000000,"risk_score":10,"permissions":["card_payment"]}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        let json = json_body(resp).await;
        assert_eq!(json["validation"]["approved"], false);
    }

    #[tokio::test]
    async fn step4_success() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/orchestration/step/4")
                    .header("content-type", "application/json")
                    .header("x-correlation-id", "test-corr")
                    .header("x-validation-result", "approved")
                    .body(Body::from(
                        r#"{"merchant_id":"M001","amount":5000,"currency":"GBP"}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        assert_eq!(json["step"], 4);
        assert_eq!(json["name"], "process");
        assert_eq!(json["transaction"]["merchant_id"], "M001");
        assert_eq!(json["transaction"]["amount"], 5000);
        assert_eq!(json["transaction"]["status"], "completed");
        assert!(json["transaction"]["authorization_code"]
            .as_str()
            .expect("auth_code")
            .starts_with("AUTH-"));
    }

    #[tokio::test]
    async fn step4_not_approved() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/orchestration/step/4")
                    .header("content-type", "application/json")
                    .header("x-correlation-id", "test-corr")
                    .header("x-validation-result", "denied")
                    .body(Body::from(r#"{"merchant_id":"M001","amount":5000}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn step4_missing_validation_header() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/orchestration/step/4")
                    .header("content-type", "application/json")
                    .header("x-correlation-id", "test-corr")
                    .body(Body::from(r#"{"merchant_id":"M001","amount":5000}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn status_returns_docs() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/orchestration/status")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        let steps = json["steps"].as_array().expect("steps array");
        assert_eq!(steps.len(), 4);
        assert_eq!(steps[0]["name"], "authenticate");
        assert_eq!(steps[3]["name"], "process");
    }

    #[tokio::test]
    async fn status_xml_negotiation() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/orchestration/status")
                    .header("accept", "application/xml")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(
            resp.headers()
                .get("content-type")
                .expect("ct")
                .to_str()
                .expect("str"),
            "application/xml"
        );
    }
}
