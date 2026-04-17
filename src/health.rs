use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::{routing::get, Router};
use serde::Serialize;
use std::sync::Arc;

use crate::config::Config;
use crate::content_negotiation::negotiate;

#[derive(Serialize, Clone, Debug)]
struct HealthResponse {
    status: String,
    service: String,
    version: String,
    instance_id: String,
}

async fn health(State(config): State<Arc<Config>>, headers: HeaderMap) -> Response {
    let resp = HealthResponse {
        status: "healthy".to_string(),
        service: "rustybin".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        instance_id: config.instance_id.clone(),
    };
    negotiate(&headers, &resp)
}

pub fn router() -> Router<Arc<Config>> {
    Router::new().route("/health", get(health))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_config() -> Arc<Config> {
        Arc::new(Config {
            http_port: 80,
            https_port: 443,
            host: "0.0.0.0".to_string(),
            log_level: "info".to_string(),
            trust_forward: false,
            body_limit: 1_048_576,
            instance_id: "test-instance".to_string(),
            tls_cert: "certs/server.crt".to_string(),
            tls_key: "certs/server.key".to_string(),
            mtls_in_header: None,
        })
    }

    #[tokio::test]
    async fn health_returns_json() {
        let app = router().with_state(test_config());

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["status"], "healthy");
        assert_eq!(json["service"], "rustybin");
        assert_eq!(json["instance_id"], "test-instance");
        assert!(json["version"].is_string());
    }

    #[tokio::test]
    async fn health_returns_xml_when_requested() {
        let app = router().with_state(test_config());

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header("Accept", "application/xml")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);

        let content_type = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(content_type, "application/xml");
    }
}
