use axum::{
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::get,
    Router,
};
use chrono::Utc;
use chrono_tz::Tz;
use serde::Serialize;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use crate::config::Config;
use crate::content_negotiation::{negotiate, negotiate_with_status};
use crate::types::ErrorResponse;

// ── Response types ───────────────────────────────────────────────────

#[derive(Serialize)]
struct IpResponse {
    ipv4: Option<String>,
    ipv6: Option<String>,
}

#[derive(Serialize)]
struct Ipv4Response {
    ipv4: Option<String>,
}

#[derive(Serialize)]
struct Ipv6Response {
    ipv6: Option<String>,
}

#[derive(Serialize)]
struct DateResponse {
    date: String,
    timezone: String,
}

#[derive(Serialize)]
struct TimeResponse {
    time: String,
    timezone: String,
}

// ── IP handlers ──────────────────────────────────────────────────────

async fn ip_handler(
    State(config): State<Arc<Config>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let ip = resolve_ip(&headers, &config, &addr);
    let (ipv4, ipv6) = classify_ip(ip);
    negotiate(&headers, &IpResponse { ipv4, ipv6 })
}

async fn ip_v4_handler(
    State(config): State<Arc<Config>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let ip = resolve_ip(&headers, &config, &addr);
    let (ipv4, _) = classify_ip(ip);
    negotiate(&headers, &Ipv4Response { ipv4 })
}

async fn ip_v6_handler(
    State(config): State<Arc<Config>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let ip = resolve_ip(&headers, &config, &addr);
    let (_, ipv6) = classify_ip(ip);
    negotiate(&headers, &Ipv6Response { ipv6 })
}

fn resolve_ip(headers: &HeaderMap, config: &Config, addr: &SocketAddr) -> IpAddr {
    if config.trust_forward {
        if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(first) = xff.split(',').next() {
                if let Ok(ip) = first.trim().parse::<IpAddr>() {
                    return ip;
                }
            }
        }
    }
    addr.ip()
}

fn classify_ip(ip: IpAddr) -> (Option<String>, Option<String>) {
    match ip {
        IpAddr::V4(v4) => (Some(v4.to_string()), None),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                (Some(v4.to_string()), None)
            } else {
                (None, Some(v6.to_string()))
            }
        }
    }
}

// ── Date handlers ────────────────────────────────────────────────────

async fn date_handler(headers: HeaderMap) -> Response {
    let now = Utc::now();
    negotiate(
        &headers,
        &DateResponse {
            date: now.format("%Y-%m-%d").to_string(),
            timezone: "UTC".to_string(),
        },
    )
}

async fn date_tz_handler(Path(tz_path): Path<String>, headers: HeaderMap) -> Response {
    let tz_name = tz_path.trim_start_matches('/');
    let timezone: Tz = match tz_name.parse() {
        Ok(tz) => tz,
        Err(_) => {
            return negotiate_with_status(
                &headers,
                &ErrorResponse {
                    error: "unknown_timezone".to_string(),
                    details: Some(format!("Unknown timezone: {tz_name}")),
                },
                StatusCode::NOT_FOUND,
            );
        }
    };

    let now = Utc::now().with_timezone(&timezone);
    negotiate(
        &headers,
        &DateResponse {
            date: now.format("%Y-%m-%d").to_string(),
            timezone: tz_name.to_string(),
        },
    )
}

// ── Time handlers ────────────────────────────────────────────────────

async fn time_handler(headers: HeaderMap) -> Response {
    let now = Utc::now();
    negotiate(
        &headers,
        &TimeResponse {
            time: now.to_rfc3339(),
            timezone: "UTC".to_string(),
        },
    )
}

async fn time_tz_handler(Path(tz_path): Path<String>, headers: HeaderMap) -> Response {
    let tz_name = tz_path.trim_start_matches('/');
    let timezone: Tz = match tz_name.parse() {
        Ok(tz) => tz,
        Err(_) => {
            return negotiate_with_status(
                &headers,
                &ErrorResponse {
                    error: "unknown_timezone".to_string(),
                    details: Some(format!("Unknown timezone: {tz_name}")),
                },
                StatusCode::NOT_FOUND,
            );
        }
    };

    let now = Utc::now().with_timezone(&timezone);
    negotiate(
        &headers,
        &TimeResponse {
            time: now.to_rfc3339(),
            timezone: tz_name.to_string(),
        },
    )
}

// ── Router ───────────────────────────────────────────────────────────

pub fn router() -> Router<Arc<Config>> {
    Router::new()
        .route("/ip", get(ip_handler))
        .route("/ip/v4", get(ip_v4_handler))
        .route("/ip/v6", get(ip_v6_handler))
        .route("/date", get(date_handler))
        .route("/date/*tz", get(date_tz_handler))
        .route("/time", get(time_handler))
        .route("/time/*tz", get(time_tz_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
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

    fn test_app() -> Router {
        router().with_state(test_config())
    }

    fn req_with_ip(uri: &str) -> Request<Body> {
        let mut req = Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("request");
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))));
        req
    }

    #[tokio::test]
    async fn ip_returns_ipv4() {
        let app = test_app();
        let resp = app.oneshot(req_with_ip("/ip")).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["ipv4"], "127.0.0.1");
        assert!(json["ipv6"].is_null());
    }

    #[tokio::test]
    async fn ip_v4_only() {
        let app = test_app();
        let resp = app.oneshot(req_with_ip("/ip/v4")).await.expect("response");
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["ipv4"], "127.0.0.1");
        assert!(json.get("ipv6").is_none());
    }

    #[tokio::test]
    async fn ip_trusts_xff() {
        let config = Arc::new(Config {
            trust_forward: true,
            ..(*test_config()).clone()
        });
        let app = router().with_state(config);

        let mut req = Request::builder()
            .uri("/ip")
            .header("x-forwarded-for", "10.0.0.1, 192.168.1.1")
            .body(Body::empty())
            .expect("request");
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345))));

        let resp = app.oneshot(req).await.expect("response");
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["ipv4"], "10.0.0.1");
    }

    #[tokio::test]
    async fn date_utc() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/date")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["timezone"], "UTC");
        assert!(json["date"].as_str().expect("str").contains('-'));
    }

    #[tokio::test]
    async fn date_with_timezone() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/date/America/New_York")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["timezone"], "America/New_York");
    }

    #[tokio::test]
    async fn date_unknown_tz_returns_404() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/date/Fake/Zone")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn time_utc() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/time")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["timezone"], "UTC");
        assert!(json["time"].as_str().expect("str").len() > 10);
    }

    #[tokio::test]
    async fn date_xml_negotiation() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/date")
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
