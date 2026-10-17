//! Shared mode end to end: API keys (header and `rb_key` query) never
//! appear in the logs (JSON format, debug level). A test binary
//! of its own because it installs the global subscriber.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustybin::logging::LogFormat;
use rustybin::{AppState, Config};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing_subscriber::fmt::MakeWriter;

const KEY_A: &str = "rbk_ka1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1";
const KEY_B: &str = "rbk_kb2_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb2";
const UNKNOWN: &str = "rbk_unknown_secret_cccccccccccccccc";

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut b) = self.0.lock() {
            b.extend_from_slice(buf);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Buffer {
    type Writer = Buffer;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

async fn get(addr: std::net::SocketAddr, path: &str, headers: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let req = format!("GET {path} HTTP/1.1\r\nHost: test\r\n{headers}Connection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .expect("in time")
        .expect("read");
    String::from_utf8_lossy(&raw).to_string()
}

fn registry() -> String {
    let entry = |id: &str, key: &str| serde_json::json!({ "id": id, "hash": rustybin::keys::key_hash_hex(key), "status": "active" });
    serde_json::json!({ "version": "1", "keys": [entry("ka1", KEY_A), entry("kb2", KEY_B)] })
        .to_string()
}

async fn run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let keys = dir.path().join("keys.json");
    std::fs::write(&keys, registry()).expect("write keys");
    let mut config = Config::for_tests();
    config.tls_cert = dir.path().join("server.crt").display().to_string();
    config.tls_key = dir.path().join("server.key").display().to_string();
    config.shared_mode = true;
    config.public_mode = true;
    config.keys_file = Some(keys);
    config.admin_token = Some("admin-secret".to_string());

    let server = rustybin::start_with_state(AppState::for_tests(config))
        .await
        .expect("server starts");
    let addr = server.http_addr;
    let ok = get(addr, &format!("/echo?x=1&rb_key={KEY_A}"), "").await;
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
    let ok = get(addr, "/echo", &format!("X-Rustybin-Key: {KEY_B}\r\n")).await;
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
    let denied = get(addr, "/echo", &format!("X-Rustybin-Key: {UNKNOWN}\r\n")).await;
    assert!(denied.starts_with("HTTP/1.1 401"), "{denied}");
    let listed = get(addr, &format!("/_rustybin/requests?rb_key={KEY_A}"), "").await;
    assert!(listed.starts_with("HTTP/1.1 200"), "{listed}");
    assert!(
        listed.contains("rbk_...aaa1") && !listed.contains(KEY_A),
        "{listed}"
    );
    server.shutdown();
    server.wait().await.expect("clean shutdown");
}

#[tokio::test]
async fn api_keys_never_appear_in_logs() {
    let mut config = Config::for_tests();
    config.log_level = "debug".to_string();
    config.log_format = LogFormat::Json;
    let buffer = Buffer::default();
    tracing::subscriber::set_global_default(rustybin::logging::subscriber(
        &config,
        buffer.clone(),
        false,
    ))
    .expect("first subscriber");

    run().await;
    let raw = buffer.0.lock().map(|b| b.clone()).unwrap_or_default();
    let text = String::from_utf8(raw).expect("utf8");
    assert!(text.contains("/echo"), "request lines are logged:\n{text}");
    assert!(text.contains("key registry loaded"), "{text}");
    for secret in [KEY_A, KEY_B, UNKNOWN] {
        assert!(!text.contains(secret), "{secret} in the logs:\n{text}");
    }
}
