//! Per-client scoping and request-origin helpers.
//!
//! A "session key" identifies one client for per-client state (counters,
//! tasks, captured requests on public instances): the `X-Rustybin-Session`
//! header when present and valid, otherwise the client IP.
//!
//! [`client_ip`] and [`request_origin`] are the single place where proxy
//! headers (`Forwarded`, `X-Forwarded-*`, `X-Real-IP`, `Fly-Client-IP`) are
//! interpreted, so every module agrees on who the client is and which URL it
//! used. Proxy headers are only honoured with `RUSTYBIN_TRUST_FORWARD=true`.

use axum::extract::connect_info::MockConnectInfo;
use axum::extract::{ConnectInfo, FromRef, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap, Uri};
use std::convert::Infallible;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
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

// ── Listener tagging ────────────────────────────────────────────────

/// Request extension set by the server on every connection: which listener
/// accepted it (`http` or `https`) and its bound port. Absent in unit tests
/// (then `http` and `Config::http_port` are assumed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListenerInfo {
    pub scheme: &'static str,
    pub port: u16,
}

impl ListenerInfo {
    pub const fn http(port: u16) -> Self {
        Self {
            scheme: "http",
            port,
        }
    }

    pub const fn https(port: u16) -> Self {
        Self {
            scheme: "https",
            port,
        }
    }
}

// ── Client IP ───────────────────────────────────────────────────────

/// True for addresses that belong to internal infrastructure (loopback,
/// private, link-local, unique-local, CGNAT, unspecified). Used to skip our
/// own proxies when walking forwarded hops from the right.
pub fn is_internal_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                // 100.64.0.0/10 carrier-grade NAT (also used by PaaS meshes)
                || (o[0] == 100 && (o[1] & 0xC0) == 64)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_internal_ip(&IpAddr::V4(v4));
            }
            let seg0 = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (seg0 & 0xfe00) == 0xfc00 // fc00::/7 unique local
                || (seg0 & 0xffc0) == 0xfe80 // fe80::/10 link local
        }
    }
}

/// Parse one forwarded address: `1.2.3.4`, `1.2.3.4:567`, `2001:db8::1`,
/// `[2001:db8::1]`, `[2001:db8::1]:443` (optionally quoted). `unknown` and
/// obfuscated identifiers (`_hidden`) yield `None`.
pub fn parse_forwarded_ip(raw: &str) -> Option<IpAddr> {
    let s = raw.trim().trim_matches('"').trim();
    if let Ok(ip) = s.parse::<IpAddr>() {
        return Some(ip);
    }
    if let Some(rest) = s.strip_prefix('[') {
        let end = rest.find(']')?;
        return rest[..end].parse().ok();
    }
    s.parse::<SocketAddr>().ok().map(|sa| sa.ip())
}

/// Pick the client from a list of hops (leftmost = original client,
/// rightmost = closest proxy): the rightmost address that is not internal
/// infrastructure, so a client cannot spoof its address by prepending
/// entries. When every hop is internal (private demo networks) the leftmost
/// valid entry is used.
fn pick_client(hops: &[IpAddr]) -> Option<IpAddr> {
    hops.iter()
        .rev()
        .find(|ip| !is_internal_ip(ip))
        .or_else(|| hops.first())
        .copied()
}

/// Comma-joined values of every instance of a header.
fn joined(headers: &HeaderMap, name: &str) -> Option<String> {
    let parts: Vec<&str> = headers
        .get_all(name)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    (!parts.is_empty()).then(|| parts.join(","))
}

/// One element of an RFC 7239 `Forwarded` header.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct ForwardedElement {
    pub for_: Option<String>,
    pub host: Option<String>,
    pub proto: Option<String>,
}

/// Parse RFC 7239 `Forwarded` (all header instances, in order).
pub fn parse_forwarded(headers: &HeaderMap) -> Vec<ForwardedElement> {
    let Some(raw) = joined(headers, "forwarded") else {
        return Vec::new();
    };
    split_outside_quotes(&raw, ',')
        .into_iter()
        .take(32)
        .map(|element| {
            let mut el = ForwardedElement::default();
            for pair in split_outside_quotes(element, ';') {
                let Some((k, v)) = pair.split_once('=') else {
                    continue;
                };
                let v = v.trim().trim_matches('"').to_string();
                match k.trim().to_ascii_lowercase().as_str() {
                    "for" => el.for_ = Some(v),
                    "host" => el.host = Some(v),
                    "proto" => el.proto = Some(v.to_ascii_lowercase()),
                    _ => {}
                }
            }
            el
        })
        .collect()
}

fn split_outside_quotes(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut in_quotes = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if c == '"' {
            in_quotes = !in_quotes;
        } else if c == sep && !in_quotes {
            parts.push(s[start..i].trim());
            start = i + c.len_utf8();
        }
    }
    parts.push(s[start..].trim());
    parts.into_iter().filter(|p| !p.is_empty()).collect()
}

/// Resolve the client IP. With `RUSTYBIN_TRUST_FORWARD` on, the first usable
/// of `Fly-Client-IP`, RFC 7239 `Forwarded: for=`, `X-Forwarded-For` and
/// `X-Real-IP` (each validated; list headers use the rightmost non-internal
/// hop). Otherwise, or when none is usable, the TCP peer address.
pub fn client_ip(headers: &HeaderMap, extensions: &Extensions, config: &Config) -> Option<IpAddr> {
    if config.trust_forward {
        if let Some(ip) = forwarded_client_ip(headers) {
            return Some(ip);
        }
    }
    peer_addr(extensions).map(|addr| addr.ip())
}

/// The TCP peer address (`ConnectInfo`, or `MockConnectInfo` in tests).
pub fn peer_addr(extensions: &Extensions) -> Option<SocketAddr> {
    extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| *addr)
        .or_else(|| {
            extensions
                .get::<MockConnectInfo<SocketAddr>>()
                .map(|MockConnectInfo(addr)| *addr)
        })
}

/// The client IP claimed by proxy headers (see [`client_ip`]), regardless of
/// the trust setting.
pub fn forwarded_client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    if let Some(ip) = headers
        .get("fly-client-ip")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_forwarded_ip)
    {
        return Some(ip);
    }
    let fwd: Vec<IpAddr> = parse_forwarded(headers)
        .iter()
        .filter_map(|e| e.for_.as_deref().and_then(parse_forwarded_ip))
        .collect();
    if let Some(ip) = pick_client(&fwd) {
        return Some(ip);
    }
    if let Some(xff) = joined(headers, "x-forwarded-for") {
        let hops: Vec<IpAddr> = xff
            .split(',')
            .take(64)
            .filter_map(parse_forwarded_ip)
            .collect();
        if let Some(ip) = pick_client(&hops) {
            return Some(ip);
        }
    }
    headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_forwarded_ip)
}

// ── Request origin ──────────────────────────────────────────────────

/// Where the client thinks it is talking to: scheme, host and port, taking
/// proxy headers into account when `RUSTYBIN_TRUST_FORWARD` is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestOrigin {
    /// `http` or `https` (or `ws`/`wss` when a proxy says so).
    pub scheme: String,
    /// Host name or IP, lower-cased, without brackets or port.
    pub host: String,
    /// Effective port (the scheme default when the Host has none).
    pub port: u16,
}

impl RequestOrigin {
    /// `host[:port]` with IPv6 brackets; the port is omitted when it is the
    /// scheme default.
    pub fn authority(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if Some(self.port) == default_port(&self.scheme) {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }

    /// `scheme://authority`.
    pub fn base_url(&self) -> String {
        format!("{}://{}", self.scheme, self.authority())
    }
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        _ => None,
    }
}

/// Split a Host / X-Forwarded-Host value into host and optional port,
/// handling IPv6 literals (`[::1]:8080`). Returns `None` for invalid values.
pub fn split_host_port(raw: &str) -> Option<(String, Option<u16>)> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 255 {
        return None;
    }
    let (host, port) = if let Some(rest) = raw.strip_prefix('[') {
        let end = rest.find(']')?;
        let host = &rest[..end];
        host.parse::<Ipv6Addr>().ok()?;
        let after = &rest[end + 1..];
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p.parse::<u16>().ok()?),
            None if after.is_empty() => None,
            None => return None,
        };
        (host.to_string(), port)
    } else {
        match raw.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => (h.to_string(), Some(p.parse::<u16>().ok()?)),
            // A bare IPv6 literal is not a valid Host, but be lenient.
            Some(_) => {
                raw.parse::<Ipv6Addr>().ok()?;
                (raw.to_string(), None)
            }
            None => (raw.to_string(), None),
        }
    };
    let valid = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'));
    valid.then(|| (host.to_ascii_lowercase(), port))
}

fn valid_scheme(s: &str) -> Option<String> {
    let s = s.trim().to_ascii_lowercase();
    matches!(s.as_str(), "http" | "https" | "ws" | "wss").then_some(s)
}

fn first_entry<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
}

/// Resolve the request origin (scheme, host, port).
///
/// - Listener: the [`ListenerInfo`] extension (`https` for the TLS listener).
/// - Host: the `Host` header, else the URI authority (HTTP/2), else the bind address.
/// - With `RUSTYBIN_TRUST_FORWARD`: `X-Forwarded-Proto`, `X-Forwarded-Host`,
///   `X-Forwarded-Port` and RFC 7239 `Forwarded` (`proto=`, `host=`) override
///   them (the first list entry, i.e. the proxy closest to the client, wins).
///   Invalid values are ignored.
pub fn request_origin(
    headers: &HeaderMap,
    extensions: &Extensions,
    uri: &Uri,
    config: &Config,
) -> RequestOrigin {
    let listener = extensions
        .get::<ListenerInfo>()
        .copied()
        .unwrap_or(ListenerInfo::http(config.http_port));
    let mut scheme = listener.scheme.to_string();
    let mut host_port = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .and_then(split_host_port)
        .or_else(|| uri.authority().and_then(|a| split_host_port(a.as_str())));
    let mut explicit_port: Option<u16> = None;

    if config.trust_forward {
        let fwd = parse_forwarded(headers);
        let first = fwd.first();
        let proto = first_entry(headers, "x-forwarded-proto")
            .and_then(valid_scheme)
            .or_else(|| {
                first
                    .and_then(|e| e.proto.as_deref())
                    .and_then(valid_scheme)
            });
        let fhost = first_entry(headers, "x-forwarded-host")
            .and_then(split_host_port)
            .or_else(|| {
                first
                    .and_then(|e| e.host.as_deref())
                    .and_then(split_host_port)
            });
        explicit_port =
            first_entry(headers, "x-forwarded-port").and_then(|v| v.parse::<u16>().ok());
        if let Some(p) = proto {
            if p != scheme && fhost.is_none() && explicit_port.is_none() {
                // The scheme changed at the proxy: the Host port (if any)
                // belongs to the hop behind it, not to what the client used.
                host_port = host_port.map(|(h, _)| (h, None));
            }
            scheme = p;
        }
        if let Some(h) = fhost {
            host_port = Some(h);
        }
    }

    let (host, host_port_num) = match host_port {
        Some(hp) => hp,
        None => (config.host.to_string(), Some(listener.port)),
    };
    let port = explicit_port
        .or(host_port_num)
        .or_else(|| default_port(&scheme))
        .unwrap_or(listener.port);
    RequestOrigin { scheme, host, port }
}

/// Extractor: the request origin (never rejects). See [`request_origin`].
pub struct Origin(pub RequestOrigin);

impl<S> FromRequestParts<S> for Origin
where
    Arc<Config>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let config = Arc::<Config>::from_ref(state);
        Ok(Origin(request_origin(
            &parts.headers,
            &parts.extensions,
            &parts.uri,
            &config,
        )))
    }
}

// ── Extractors ──────────────────────────────────────────────────────

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
        Ok(PeerAddr(peer_addr(&parts.extensions)))
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

    fn hdrs(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, HeaderValue::from_static(v));
        }
        h
    }

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
        let h = hdrs(&[("x-forwarded-for", "203.0.113.9, 10.0.0.1")]);
        let mut ext = Extensions::new();
        ext.insert(ConnectInfo::<SocketAddr>(
            "127.0.0.1:9".parse().expect("addr"),
        ));
        assert_eq!(client_ip(&h, &ext, &config), "127.0.0.1".parse().ok());
        config.trust_forward = true;
        assert_eq!(client_ip(&h, &ext, &config), "203.0.113.9".parse().ok());
    }

    #[test]
    fn xff_uses_rightmost_untrusted_hop() {
        // A client prepending a fake entry cannot win: the gateway appended
        // the real address on the right.
        let h = hdrs(&[("x-forwarded-for", "1.1.1.1, 203.0.113.9, 10.0.0.2")]);
        assert_eq!(forwarded_client_ip(&h), "203.0.113.9".parse().ok());
        // All internal: the leftmost valid entry.
        let h = hdrs(&[("x-forwarded-for", "garbage, 10.0.0.1, 192.168.1.1")]);
        assert_eq!(forwarded_client_ip(&h), "10.0.0.1".parse().ok());
        // Multiple header instances are concatenated in order.
        let h = hdrs(&[
            ("x-forwarded-for", "198.51.100.1"),
            ("x-forwarded-for", "203.0.113.5"),
        ]);
        assert_eq!(forwarded_client_ip(&h), "203.0.113.5".parse().ok());
        let h = hdrs(&[("x-forwarded-for", "not-an-ip")]);
        assert_eq!(forwarded_client_ip(&h), None);
    }

    #[test]
    fn fly_forwarded_and_real_ip() {
        let h = hdrs(&[
            ("fly-client-ip", "198.51.100.7"),
            ("x-forwarded-for", "203.0.113.9"),
        ]);
        assert_eq!(forwarded_client_ip(&h), "198.51.100.7".parse().ok());
        let h = hdrs(&[(
            "forwarded",
            "for=192.0.2.60;proto=https;host=api.example.com, for=\"[2001:db8:cafe::17]:4711\"",
        )]);
        assert_eq!(forwarded_client_ip(&h), "2001:db8:cafe::17".parse().ok());
        let h = hdrs(&[("x-real-ip", "203.0.113.77")]);
        assert_eq!(forwarded_client_ip(&h), "203.0.113.77".parse().ok());
        let h = hdrs(&[("x-real-ip", "nope")]);
        assert_eq!(forwarded_client_ip(&h), None);
    }

    #[test]
    fn host_parsing_handles_ipv6_and_ports() {
        assert_eq!(
            split_host_port("[::1]:8080"),
            Some(("::1".to_string(), Some(8080)))
        );
        assert_eq!(
            split_host_port("[2001:db8::1]"),
            Some(("2001:db8::1".to_string(), None))
        );
        assert_eq!(
            split_host_port("Example.com:443"),
            Some(("example.com".to_string(), Some(443)))
        );
        assert_eq!(
            split_host_port("example.com"),
            Some(("example.com".to_string(), None))
        );
        assert_eq!(split_host_port("bad host"), None);
        assert_eq!(split_host_port("x:99999"), None);
        assert_eq!(split_host_port("[::1"), None);
    }

    #[test]
    fn origin_from_listener_host_and_forwarded_headers() {
        let mut config = Config::for_tests();
        config.http_port = 8080;
        let uri: Uri = "/echo".parse().expect("uri");
        let mut ext = Extensions::new();
        let h = hdrs(&[("host", "[::1]:8080")]);
        let o = request_origin(&h, &ext, &uri, &config);
        assert_eq!(
            (o.scheme.as_str(), o.host.as_str(), o.port),
            ("http", "::1", 8080)
        );
        assert_eq!(o.base_url(), "http://[::1]:8080");

        ext.insert(ListenerInfo::https(8443));
        let h = hdrs(&[("host", "localhost:8443")]);
        let o = request_origin(&h, &ext, &uri, &config);
        assert_eq!((o.scheme.as_str(), o.port), ("https", 8443));
        let h = hdrs(&[("host", "localhost")]);
        assert_eq!(request_origin(&h, &ext, &uri, &config).port, 443);

        // Forwarded headers are ignored unless trusted.
        let ext = Extensions::new();
        let h = hdrs(&[
            ("host", "internal:8080"),
            ("x-forwarded-proto", "https"),
            ("x-forwarded-host", "api.example.com"),
        ]);
        assert_eq!(
            request_origin(&h, &ext, &uri, &config).base_url(),
            "http://internal:8080"
        );
        config.trust_forward = true;
        assert_eq!(
            request_origin(&h, &ext, &uri, &config).base_url(),
            "https://api.example.com"
        );
        let h = hdrs(&[
            ("host", "internal:8080"),
            ("x-forwarded-proto", "https"),
            ("x-forwarded-host", "api.example.com"),
            ("x-forwarded-port", "8443"),
        ]);
        assert_eq!(
            request_origin(&h, &ext, &uri, &config).base_url(),
            "https://api.example.com:8443"
        );
        let h = hdrs(&[
            ("host", "internal:8080"),
            (
                "forwarded",
                "for=1.2.3.4;proto=https;host=\"gw.example.com\"",
            ),
        ]);
        assert_eq!(
            request_origin(&h, &ext, &uri, &config).base_url(),
            "https://gw.example.com"
        );
        // Invalid forwarded values are ignored.
        let h = hdrs(&[
            ("host", "internal:8080"),
            ("x-forwarded-proto", "javascript"),
            ("x-forwarded-host", "evil host<>"),
        ]);
        assert_eq!(
            request_origin(&h, &ext, &uri, &config).base_url(),
            "http://internal:8080"
        );
    }
}
