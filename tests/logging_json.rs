//! `RUSTYBIN_LOG_FORMAT=json` end to end: the real server, the real request
//! span, every captured line valid JSON and the request line carrying the
//! request id. A test binary of its own because it installs the global
//! subscriber.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustybin::logging::LogFormat;
use rustybin::{AppState, Config};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing_subscriber::fmt::MakeWriter;

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

#[tokio::test]
async fn json_log_lines_carry_the_request_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = Config::for_tests();
    config.tls_cert = dir.path().join("server.crt").display().to_string();
    config.tls_key = dir.path().join("server.key").display().to_string();
    config.log_level = "info".to_string();
    config.log_format = LogFormat::Json;
    let buffer = Buffer::default();
    tracing::subscriber::set_global_default(rustybin::logging::subscriber(
        &config,
        buffer.clone(),
        false,
    ))
    .expect("first subscriber");

    let server = rustybin::start_with_state(AppState::for_tests(config))
        .await
        .expect("server starts");
    let mut stream = tokio::net::TcpStream::connect(server.http_addr)
        .await
        .expect("connect");
    stream
        .write_all(
            b"GET /uuid HTTP/1.1\r\nHost: test\r\nX-Request-Id: json-log-42\r\nConnection: close\r\n\r\n",
        )
        .await
        .expect("write");
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .expect("in time")
        .expect("read");
    assert!(String::from_utf8_lossy(&raw).starts_with("HTTP/1.1 200"));
    server.shutdown();
    server.wait().await.expect("clean shutdown");

    let raw = buffer.0.lock().map(|b| b.clone()).unwrap_or_default();
    let text = String::from_utf8(raw).expect("utf8");
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}")))
        .collect();
    assert!(lines.len() >= 3, "{text}");
    assert!(lines
        .iter()
        .all(|l| l["timestamp"].is_string() && l["level"].is_string()));
    let request_line = lines
        .iter()
        .find(|l| l["span"]["request_id"] == "json-log-42")
        .unwrap_or_else(|| panic!("no line with the request id:\n{text}"));
    assert_eq!(request_line["span"]["path"], "/uuid");
    assert_eq!(request_line["span"]["method"], "GET");
    assert_eq!(request_line["status"], 200);
}
