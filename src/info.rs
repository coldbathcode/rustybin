use axum::{
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::get,
    Router,
};
use chrono::Utc;
use chrono_tz::Tz;
use serde::Serialize;
use std::net::IpAddr;

use crate::catalog::{category, Endpoint, Example};
use crate::content_negotiation::{negotiate, negotiate_with_status};
use crate::session::ClientIp;
use crate::state::AppState;
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

// The client IP comes from `session::client_ip` (rightmost untrusted hop of
// the proxy headers when RUSTYBIN_TRUST_FORWARD is on, else the peer).

async fn ip_handler(ClientIp(ip): ClientIp, headers: HeaderMap) -> Response {
    let (ipv4, ipv6) = classify_ip(ip);
    negotiate(&headers, &IpResponse { ipv4, ipv6 })
}

async fn ip_v4_handler(ClientIp(ip): ClientIp, headers: HeaderMap) -> Response {
    let (ipv4, _) = classify_ip(ip);
    negotiate(&headers, &Ipv4Response { ipv4 })
}

async fn ip_v6_handler(ClientIp(ip): ClientIp, headers: HeaderMap) -> Response {
    let (_, ipv6) = classify_ip(ip);
    negotiate(&headers, &Ipv6Response { ipv6 })
}

fn classify_ip(ip: Option<IpAddr>) -> (Option<String>, Option<String>) {
    match ip {
        None => (None, None),
        Some(IpAddr::V4(v4)) => (Some(v4.to_string()), None),
        Some(IpAddr::V6(v6)) => {
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

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/ip", get(ip_handler))
        .route("/ip/v4", get(ip_v4_handler))
        .route("/ip/v6", get(ip_v6_handler))
        .route("/date", get(date_handler))
        .route("/date/{*timezone}", get(date_tz_handler))
        .route("/time", get(time_handler))
        .route("/time/{*timezone}", get(time_tz_handler))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/ip",
            &["GET"],
            category::INFO,
            "Client IP address (IPv4 and IPv6)",
        )
        .example(Example::get("Client IP", "/ip")),
        Endpoint::new("/ip/v4", &["GET"], category::INFO, "Client IPv4 address")
            .example(Example::get("Client IPv4", "/ip/v4")),
        Endpoint::new("/ip/v6", &["GET"], category::INFO, "Client IPv6 address")
            .example(Example::get("Client IPv6", "/ip/v6")),
        Endpoint::new("/date", &["GET"], category::INFO, "Current date (UTC)")
            .example(Example::get("Current date (UTC)", "/date")),
        Endpoint::new(
            "/date/{*timezone}",
            &["GET"],
            category::INFO,
            "Current date in an IANA timezone",
        )
        .example(Example::get(
            "Current date (New York)",
            "/date/America/New_York",
        )),
        Endpoint::new(
            "/time",
            &["GET"],
            category::INFO,
            "Current time, ISO 8601 (UTC)",
        )
        .example(Example::get("Current time (UTC)", "/time")),
        Endpoint::new(
            "/time/{*timezone}",
            &["GET"],
            category::INFO,
            "Current time in an IANA timezone",
        )
        .example(Example::get("Current time (London)", "/time/Europe/London")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::Request;
    use std::net::SocketAddr;
    use tower::ServiceExt;

    #[tokio::test]
    async fn ip_ignores_spoofed_leftmost_xff_entry() {
        let config = Config {
            trust_forward: true,
            ..crate::test_support::test_config()
        };
        let app = crate::test_support::module_app_with_config(config, router);
        let req = Request::builder()
            .uri("/ip")
            .header("x-forwarded-for", "1.2.3.4, 2001:db8::5, 10.0.0.9")
            .body(Body::empty())
            .expect("request");
        let json = crate::test_support::body_json(app.oneshot(req).await.expect("resp")).await;
        assert_eq!(json["ipv6"], "2001:db8::5");
        assert!(json["ipv4"].is_null());
    }

    fn test_app() -> Router {
        crate::test_support::module_app(router)
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
        let config = Config {
            trust_forward: true,
            ..crate::test_support::test_config()
        };
        let app = crate::test_support::module_app_with_config(config, router);

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
