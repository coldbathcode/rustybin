//! Per-client scoping helpers.
//!
//! A "session key" identifies one client for per-client state (counters,
//! tasks, captured requests on public instances): the `X-Rustybin-Session`
//! header when present and valid, otherwise the client IP.

use axum::extract::{ConnectInfo, FromRef, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap};
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use crate::config::Config;

/// Header clients use to scope their state.
pub const SESSION_HEADER: &str = "x-rustybin-session";
/// Maximum accepted session id length.
pub const MAX_SESSION_LEN: usize = 128;

/// The validated `X-Rustybin-Session` header value, if any.
pub fn session_header(headers: &HeaderMap) -> Option<String> {
    headers
        .get(SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| is_valid_session(s))
        .map(str::to_string)
}

/// Session ids are 1..=128 chars of `[A-Za-z0-9._:-]`.
pub fn is_valid_session(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_SESSION_LEN
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// Session key: the session header, else `ip:<client ip>`, else `anonymous`.
pub fn session_key(headers: &HeaderMap, client_ip: Option<IpAddr>) -> String {
    if let Some(s) = session_header(headers) {
        return s;
    }
    match client_ip {
        Some(ip) => format!("ip:{ip}"),
        None => "anonymous".to_string(),
    }
}

/// Resolve the client IP: first `X-Forwarded-For` hop when
/// `RUSTYBIN_TRUST_FORWARD` is on, else the TCP peer address.
pub fn client_ip(headers: &HeaderMap, extensions: &Extensions, config: &Config) -> Option<IpAddr> {
    if config.trust_forward {
        if let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .and_then(|first| first.trim().parse::<IpAddr>().ok())
        {
            return Some(ip);
        }
    }
    extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip())
}

/// Extractor: the caller's session key (never rejects).
///
/// ```ignore
/// async fn handler(Session(key): Session) -> String { key }
/// ```
pub struct Session(pub String);

impl<S> FromRequestParts<S> for Session
where
    Arc<Config>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let config = Arc::<Config>::from_ref(state);
        let ip = client_ip(&parts.headers, &parts.extensions, &config);
        Ok(Session(session_key(&parts.headers, ip)))
    }
}

/// Extractor: the TCP peer address, if known (never rejects). Replaces
/// `Option<ConnectInfo<SocketAddr>>`, which axum 0.8 no longer supports.
pub struct PeerAddr(pub Option<SocketAddr>);

impl<S: Send + Sync> FromRequestParts<S> for PeerAddr {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(PeerAddr(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ConnectInfo(addr)| *addr),
        ))
    }
}

/// Extractor: the resolved client IP, if known (never rejects).
pub struct ClientIp(pub Option<IpAddr>);

impl<S> FromRequestParts<S> for ClientIp
where
    Arc<Config>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let config = Arc::<Config>::from_ref(state);
        Ok(ClientIp(client_ip(
            &parts.headers,
            &parts.extensions,
            &config,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn header_wins_over_ip() {
        let mut h = HeaderMap::new();
        h.insert(SESSION_HEADER, HeaderValue::from_static("demo-1"));
        let ip: IpAddr = "10.0.0.1".parse().expect("ip");
        assert_eq!(session_key(&h, Some(ip)), "demo-1");
    }

    #[test]
    fn falls_back_to_ip_then_anonymous() {
        let h = HeaderMap::new();
        let ip: IpAddr = "::1".parse().expect("ip");
        assert_eq!(session_key(&h, Some(ip)), "ip:::1");
        assert_eq!(session_key(&h, None), "anonymous");
    }

    #[test]
    fn invalid_session_header_is_ignored() {
        let mut h = HeaderMap::new();
        h.insert(SESSION_HEADER, HeaderValue::from_static("bad value!"));
        assert_eq!(session_header(&h), None);
        assert!(!is_valid_session(&"x".repeat(129)));
    }

    #[test]
    fn client_ip_honours_trust_forward() {
        let mut config = Config::for_tests();
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.9, 10.0.0.1"),
        );
        let mut ext = Extensions::new();
        ext.insert(ConnectInfo::<SocketAddr>(
            "127.0.0.1:9".parse().expect("addr"),
        ));
        assert_eq!(client_ip(&h, &ext, &config), "127.0.0.1".parse().ok());
        config.trust_forward = true;
        assert_eq!(client_ip(&h, &ext, &config), "203.0.113.9".parse().ok());
    }
}
