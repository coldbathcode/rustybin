//! End-to-end tests: start the real server (all listeners) on ephemeral
//! ports and talk to it over TCP.

use std::net::SocketAddr;
use std::time::Duration;

use rustybin::{AppState, Config, RunningServer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).expect("json body")
    }
}

/// Minimal HTTP/1.1 client (`Connection: close`, reads until EOF).
async fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> HttpResponse {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if !body.is_empty() {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    req.push_str(body);
    stream.write_all(req.as_bytes()).await.expect("write");

    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .expect("response within 10s")
        .expect("read");
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .expect("status line");
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect::<Vec<_>>();
    let body = if headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked"))
    {
        dechunk(body)
    } else {
        body.to_string()
    };
    HttpResponse {
        status,
        headers,
        body,
    }
}

fn dechunk(mut s: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = s.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 || rest.len() < n {
            break;
        }
        out.push_str(&rest[..n]);
        s = rest[n..].trim_start_matches("\r\n");
    }
    out
}

async fn get(addr: SocketAddr, path: &str) -> HttpResponse {
    request(addr, "GET", path, &[], "").await
}

/// Test configuration on loopback with ephemeral ports and TLS files in a
/// temporary directory (never the repository's certs/).
fn config(dir: &tempfile::TempDir) -> Config {
    let mut c = Config::for_tests();
    c.tls_cert = dir.path().join("server.crt").display().to_string();
    c.tls_key = dir.path().join("server.key").display().to_string();
    c.body_limit = 1024;
    c
}

async fn start(config: Config) -> RunningServer {
    rustybin::start_with_state(AppState::for_tests(config))
        .await
        .expect("server starts")
}

#[tokio::test]
async fn serves_core_routes_and_middleware() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = start(config(&dir)).await;
    let addr = server.http_addr;
    assert!(server.https_addr.is_some(), "HTTPS should start");
    assert!(server.grpc_addr.is_some(), "gRPC should start");

    // Health
    let resp = get(addr, "/health").await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.json()["status"], "healthy");

    // Landing page always 200
    assert_eq!(get(addr, "/").await.status, 200);

    // Echo with a body
    let resp = request(
        addr,
        "POST",
        "/echo?x=1",
        &[("Content-Type", "application/json")],
        r#"{"hello":"world"}"#,
    )
    .await;
    assert_eq!(resp.status, 200);
    assert!(resp.body.contains("hello"));

    // 404 is JSON
    let resp = get(addr, "/definitely/not/here").await;
    assert_eq!(resp.status, 404);
    assert_eq!(resp.json()["error"], "not found");

    // Request id generated and propagated
    let resp = get(addr, "/status/200").await;
    assert!(resp.header("x-request-id").is_some_and(|v| !v.is_empty()));
    let resp = request(
        addr,
        "GET",
        "/status/200",
        &[("X-Request-Id", "abc-123")],
        "",
    )
    .await;
    assert_eq!(resp.header("x-request-id"), Some("abc-123"));

    // CORS preflight
    let resp = request(
        addr,
        "OPTIONS",
        "/echo",
        &[
            ("Origin", "https://app.example"),
            ("Access-Control-Request-Method", "POST"),
        ],
        "",
    )
    .await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.header("access-control-allow-origin"), Some("*"));

    // Fault injection
    let resp = request(addr, "GET", "/echo", &[("X-Rustybin-Fail", "503")], "").await;
    assert_eq!(resp.status, 503);
    assert_eq!(resp.json()["error"], "injected fault");

    // Inspector capture
    let resp = request(
        addr,
        "GET",
        "/anything/inspect-me",
        &[("X-Rustybin-Session", "itest")],
        "",
    )
    .await;
    assert_eq!(resp.status, 200);
    let list = get(addr, "/_rustybin/requests?session=itest").await.json();
    assert_eq!(list["count"], 1);
    assert_eq!(list["requests"][0]["path"], "/anything/inspect-me");
    assert_eq!(list["requests"][0]["client_ip"], "127.0.0.1");

    // Body limit is enforced (413)
    let big = "x".repeat(8 * 1024);
    let resp = request(addr, "POST", "/echo", &[], &big).await;
    assert_eq!(resp.status, 413);

    server.shutdown();
    tokio::time::timeout(Duration::from_secs(15), server.wait())
        .await
        .expect("shutdown in time")
        .expect("clean shutdown");
}

#[tokio::test]
async fn missing_cert_files_use_generated_certificate() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A path whose parent is a regular file: the files can never be written.
    let blocker = dir.path().join("not-a-dir");
    std::fs::write(&blocker, b"x").expect("write blocker");
    let mut c = Config::for_tests();
    c.tls_cert = blocker.join("server.crt").display().to_string();
    c.tls_key = blocker.join("server.key").display().to_string();

    let server = start(c).await;
    assert!(
        server.https_addr.is_some(),
        "HTTPS falls back to the demo cert"
    );
    assert_eq!(get(server.http_addr, "/health").await.status, 200);
    server.shutdown();
    server.wait().await.expect("clean shutdown");
}

#[tokio::test]
async fn broken_cert_files_disable_https_but_http_serves() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cert = dir.path().join("server.crt");
    let key = dir.path().join("server.key");
    std::fs::write(&cert, b"not a certificate").expect("write cert");
    std::fs::write(&key, b"not a key").expect("write key");
    let mut c = Config::for_tests();
    c.tls_cert = cert.display().to_string();
    c.tls_key = key.display().to_string();

    let server = start(c).await;
    assert!(server.https_addr.is_none(), "HTTPS must not start");
    assert_eq!(get(server.http_addr, "/health").await.status, 200);
    server.shutdown();
    server.wait().await.expect("clean shutdown");
}

#[tokio::test]
async fn optional_listener_bind_failure_is_not_fatal_but_http_is() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = taken.local_addr().expect("addr").port();

    // gRPC port already in use: warning only, HTTP serves.
    let mut c = Config::for_tests();
    c.grpc_port = port;
    let server = start(c).await;
    assert!(server.grpc_addr.is_none());
    assert_eq!(get(server.http_addr, "/health").await.status, 200);
    server.shutdown();
    server.wait().await.expect("clean shutdown");

    // HTTP port already in use: fatal.
    let mut c = Config::for_tests();
    c.http_port = port;
    let result = rustybin::start_with_state(AppState::for_tests(c)).await;
    assert!(result.is_err(), "HTTP bind failure must be an error");
}

/// Each listener tags its requests: /echo over the HTTPS listener reports
/// scheme https and the bound port; over HTTP, http and the HTTP port.
#[tokio::test]
async fn echo_reports_listener_scheme_and_port() {
    use std::sync::Arc;
    use tokio_rustls::rustls;

    let state = AppState::for_tests(Config::for_tests());
    let ca_pem = state.certs.ca_cert_pem.clone();
    let server = rustybin::start_with_state(state)
        .await
        .expect("server starts");
    let https = server.https_addr.expect("HTTPS should start");

    let http_json = get(server.http_addr, "/echo").await.json();
    assert_eq!(http_json["scheme"], "http");
    assert_eq!(http_json["port"], server.http_addr.port());

    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut ca_pem.as_bytes()) {
        roots.add(cert.expect("ca cert")).expect("add root");
    }
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(tls));
    let tcp = TcpStream::connect(https).await.expect("connect");
    let name = rustls::pki_types::ServerName::try_from("localhost").expect("name");
    let mut stream = connector.connect(name, tcp).await.expect("TLS handshake");
    let req = format!(
        "GET /echo HTTP/1.1\r\nHost: localhost:{}\r\nConnection: close\r\n\r\n",
        https.port()
    );
    stream.write_all(req.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw)).await;
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text.split_once("\r\n\r\n").expect("response");
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(body)
    } else {
        body.to_string()
    };
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(json["scheme"], "https");
    assert_eq!(json["port"], https.port());
    assert_eq!(
        json["url"],
        format!("https://localhost:{}/echo", https.port())
    );

    server.shutdown();
    server.wait().await.expect("clean shutdown");
}

// Shared with src/test_support.rs; not every key is used here.
#[allow(dead_code)]
mod common {
    include!("common/control_jwt_keys.rs");
}

/// `RUSTYBIN_GRPC_ON_HTTP`: gRPC (h2c, as forwarded by a TLS-terminating
/// proxy) on the HTTP listener next to the web routes; EchoService, health
/// and reflection all answer, and the separate gRPC port keeps working.
#[tokio::test]
async fn grpc_on_the_http_listener() {
    use rustybin::grpc::pb::echo_service_client::EchoServiceClient;
    use rustybin::grpc::pb::EchoRequest;
    use tokio_stream::StreamExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let mut c = config(&dir);
    c.grpc_on_http = true;
    let server = start(c).await;
    let url = format!("http://{}", server.http_addr);

    let mut client = EchoServiceClient::connect(url.clone())
        .await
        .expect("h2c connect");
    let resp = client
        .echo(EchoRequest {
            message: "over http".to_string(),
            count: 0,
        })
        .await
        .expect("echo")
        .into_inner();
    assert_eq!(resp.message, "over http");
    assert_eq!(resp.instance_id, "test-instance");

    let channel = tonic::transport::Endpoint::from_shared(url)
        .expect("endpoint")
        .connect()
        .await
        .expect("connect");
    let mut health = tonic_health::pb::health_client::HealthClient::new(channel.clone());
    let status = health
        .check(tonic_health::pb::HealthCheckRequest {
            service: String::new(),
        })
        .await
        .expect("health")
        .into_inner()
        .status;
    assert_eq!(
        status,
        tonic_health::pb::health_check_response::ServingStatus::Serving as i32
    );

    {
        use tonic_reflection::pb::v1::server_reflection_client::ServerReflectionClient;
        use tonic_reflection::pb::v1::server_reflection_request::MessageRequest;
        use tonic_reflection::pb::v1::server_reflection_response::MessageResponse;
        use tonic_reflection::pb::v1::ServerReflectionRequest;
        let mut reflection = ServerReflectionClient::new(channel);
        let request = ServerReflectionRequest {
            host: String::new(),
            message_request: Some(MessageRequest::ListServices(String::new())),
        };
        let mut stream = reflection
            .server_reflection_info(tokio_stream::iter(vec![request]))
            .await
            .expect("reflection")
            .into_inner();
        let resp = stream.next().await.expect("item").expect("ok");
        let Some(MessageResponse::ListServicesResponse(list)) = resp.message_response else {
            panic!("unexpected reflection response");
        };
        let names: Vec<String> = list.service.into_iter().map(|s| s.name).collect();
        assert!(names.contains(&rustybin::grpc::ECHO_SERVICE_NAME.to_string()));
        assert!(names.contains(&"grpc.health.v1.Health".to_string()));
    }

    // Web routes on the same port are untouched; the metrics count gRPC.
    assert_eq!(get(server.http_addr, "/echo").await.status, 200);
    let metrics = get(server.http_addr, "/_rustybin/metrics").await.body;
    assert!(
        metrics.lines().any(|l| l
            .starts_with("rustybin_protocol_requests_total{protocol=\"grpc\"}")
            && !l.ends_with(" 0")),
        "{metrics}"
    );
    // The separate gRPC port still serves.
    let grpc = server.grpc_addr.expect("gRPC listener");
    let mut client = EchoServiceClient::connect(format!("http://{grpc}"))
        .await
        .expect("connect");
    assert!(client
        .echo(EchoRequest {
            message: "port".to_string(),
            count: 0,
        })
        .await
        .is_ok());

    server.shutdown();
    server.wait().await.expect("clean shutdown");
}

/// Without `RUSTYBIN_GRPC_ON_HTTP` the HTTP listener does not speak gRPC.
#[tokio::test]
async fn grpc_is_not_on_the_http_listener_by_default() {
    use rustybin::grpc::pb::echo_service_client::EchoServiceClient;
    use rustybin::grpc::pb::EchoRequest;
    let dir = tempfile::tempdir().expect("tempdir");
    let server = start(config(&dir)).await;
    let mut client = EchoServiceClient::connect(format!("http://{}", server.http_addr))
        .await
        .expect("h2c connect");
    let err = client
        .echo(EchoRequest {
            message: "x".to_string(),
            count: 0,
        })
        .await
        .expect_err("not a gRPC endpoint");
    assert_ne!(err.code(), tonic::Code::Ok);
    server.shutdown();
    server.wait().await.expect("clean shutdown");
}

/// gRPC over TLS on the HTTPS listener: ALPN offers h2 when
/// `RUSTYBIN_GRPC_ON_HTTP` is on.
#[tokio::test]
async fn grpc_on_the_https_listener() {
    use rustybin::grpc::pb::echo_service_client::EchoServiceClient;
    use rustybin::grpc::pb::EchoRequest;
    use std::sync::Arc;
    use tokio_rustls::rustls;

    let dir = tempfile::tempdir().expect("tempdir");
    let mut c = config(&dir);
    c.grpc_on_http = true;
    let state = AppState::for_tests(c);
    let ca_pem = state.certs.ca_cert_pem.clone();
    let server = rustybin::start_with_state(state)
        .await
        .expect("server starts");
    let https = server.https_addr.expect("HTTPS should start");

    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut ca_pem.as_bytes()) {
        roots.add(cert.expect("ca cert")).expect("add root");
    }
    let mut tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h2".to_vec()];
    let connector = tokio_rustls::TlsConnector::from(Arc::new(tls));
    let negotiated = Arc::new(std::sync::Mutex::new(None::<Vec<u8>>));
    let seen = negotiated.clone();
    let channel = tonic::transport::Endpoint::from_static("http://localhost")
        .connect_with_connector(tower::service_fn(move |_uri: axum::http::Uri| {
            let connector = connector.clone();
            let seen = seen.clone();
            async move {
                let tcp = TcpStream::connect(https).await?;
                let name = rustls::pki_types::ServerName::try_from("localhost")
                    .map_err(std::io::Error::other)?;
                let stream = connector.connect(name, tcp).await?;
                if let Ok(mut s) = seen.lock() {
                    *s = stream.get_ref().1.alpn_protocol().map(<[u8]>::to_vec);
                }
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await
        .expect("TLS h2 connect");
    let mut client = EchoServiceClient::new(channel);
    let resp = client
        .echo(EchoRequest {
            message: "over tls".to_string(),
            count: 0,
        })
        .await
        .expect("echo")
        .into_inner();
    assert_eq!(resp.message, "over tls");
    assert_eq!(
        negotiated.lock().ok().and_then(|n| n.clone()),
        Some(b"h2".to_vec())
    );

    server.shutdown();
    server.wait().await.expect("clean shutdown");
}

/// `RUSTYBIN_CONTROL_AUTH=jwt` end to end: 401 without a token, 200 with a
/// signed console token, readiness and data plane open.
#[tokio::test]
async fn control_plane_jwt_end_to_end() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut c = config(&dir);
    c.control_auth = rustybin::control_auth::ControlAuth::Jwt;
    c.control_jwt_key = rustybin::control_auth::parse_public_key(common::CONTROL_JWT_PUBLIC_PEM);
    c.control_jwt_audience = "pod-1".to_string();
    let server = start(c).await;
    let addr = server.http_addr;

    let resp = get(addr, "/_rustybin/status").await;
    assert_eq!(resp.status, 401);
    assert!(resp.header("www-authenticate").is_some());
    assert_eq!(get(addr, "/_rustybin/ready").await.status, 200);
    assert_eq!(get(addr, "/echo").await.status, 200);
    assert_eq!(get(addr, "/ui/").await.status, 200);

    let key = jsonwebtoken::EncodingKey::from_ed_pem(common::CONTROL_JWT_PRIVATE_PEM.as_bytes())
        .expect("key");
    let exp = chrono::Utc::now().timestamp() + 300;
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::EdDSA),
        &serde_json::json!({"aud": "pod-1", "exp": exp, "scope": "console", "sub": "u1"}),
        &key,
    )
    .expect("sign");
    let auth = format!("Bearer {token}");
    let resp = request(
        addr,
        "GET",
        "/_rustybin/status",
        &[("Authorization", &auth)],
        "",
    )
    .await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.json()["control_auth"], "jwt");
    let resp = request(
        addr,
        "DELETE",
        "/_rustybin/requests",
        &[("Authorization", &auth)],
        "",
    )
    .await;
    assert_eq!(resp.status, 403);

    server.shutdown();
    server.wait().await.expect("clean shutdown");
}
