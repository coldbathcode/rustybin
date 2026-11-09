//! HTTP/3 to a run's target (the cargo feature `harness-http3`; step 13 of
//! `docs/plans/core-next`).
//!
//! Nothing here chooses where load goes. A target is parsed, checked against
//! the allowlist and pinned by [`super::safety::pin`] exactly as for HTTP/1
//! and 2, and this client is handed the pinned socket addresses: every QUIC
//! connection goes to one of them and nowhere else.
//!
//! - no name is resolved here (the server name is only the TLS name);
//! - no `Alt-Svc`, no redirect, no retry to another address;
//! - a server's preferred address is never moved to, and a packet from an
//!   address other than the pinned one is dropped (the QUIC stack does both
//!   for a client; `http3_tests` proves the first with a server that asks);
//! - TLS is the configuration of [`super::engine::tls_config_with`]: web PKI
//!   roots, or no verification only with the target's `insecure: true`.
//!
//! A target keeps a small fixed set of connections, round robin over the
//! pinned addresses; a connection that closed is replaced on its next use.
//! Requests are streams on them, bounded by the run's in-flight cap.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::{Buf, Bytes};
use http_body_util::BodyExt;
use tokio::runtime::Handle;

use super::engine::{ReqBody, CONNECT_TIMEOUT, MAX_RESPONSE_BYTES};

/// QUIC connections kept per target.
pub const CONNECTIONS: usize = 8;
/// A connection without traffic for this long is closed.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Keep-alive pings, so a connection outlives a quiet stage of a pattern.
const KEEP_ALIVE: Duration = Duration::from_secs(15);
/// The most of an answer kept (a created id, the verification token).
const MAX_KEPT_BODY: usize = 64 * 1024;
/// Headers HTTP/3 forbids (connection-specific): never sent.
const CONNECTION_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "upgrade",
    "te",
    "host",
];

type Sender = h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>;

/// One QUIC connection with its HTTP/3 driver.
struct Conn {
    quic: quinn::Connection,
    send: Sender,
    driver: tokio::task::JoinHandle<()>,
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.driver.abort();
        self.quic.close(0u32.into(), b"done");
    }
}

/// What an HTTP/3 exchange came to.
pub struct Answer {
    pub status: u16,
    pub headers: hyper::http::HeaderMap,
    /// Body bytes read.
    pub bytes: u64,
    /// The first bytes of the body, when asked for.
    pub kept: Option<Vec<u8>>,
}

/// The HTTP/3 client of one pinned target.
pub struct H3Client {
    /// One endpoint (a UDP socket) per address family in use.
    v4: Option<quinn::Endpoint>,
    v6: Option<quinn::Endpoint>,
    addrs: Vec<SocketAddr>,
    server_name: String,
    slots: Vec<tokio::sync::Mutex<Option<Conn>>>,
    next: AtomicUsize,
    /// The engine runtime: connections and their drivers live on it.
    handle: Handle,
}

impl H3Client {
    /// A client for the pinned `addrs` (never empty), with the TLS
    /// configuration of the target (ALPN `h3`). Sockets are bound inside the
    /// engine runtime.
    pub fn new(
        handle: &Handle,
        addrs: Vec<SocketAddr>,
        tls: rustls::ClientConfig,
        server_name: &str,
    ) -> Result<Self, String> {
        if addrs.is_empty() {
            return Err("HTTP/3: no pinned address".into());
        }
        let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
            .map_err(|e| format!("HTTP/3 TLS configuration: {e}"))?;
        let mut config = quinn::ClientConfig::new(Arc::new(crypto));
        let mut transport = quinn::TransportConfig::default();
        transport.max_idle_timeout(Some(
            IDLE_TIMEOUT
                .try_into()
                .map_err(|_| "HTTP/3: idle timeout".to_string())?,
        ));
        transport.keep_alive_interval(Some(KEEP_ALIVE));
        config.transport_config(Arc::new(transport));
        let _enter = handle.enter();
        let endpoint = |bind: &str| -> Result<quinn::Endpoint, String> {
            let bind: SocketAddr = bind
                .parse()
                .map_err(|_| "HTTP/3: bind address".to_string())?;
            let mut ep = quinn::Endpoint::client(bind)
                .map_err(|e| format!("HTTP/3: cannot open a UDP socket: {e}"))?;
            ep.set_default_client_config(config.clone());
            Ok(ep)
        };
        let v4 = if addrs.iter().any(SocketAddr::is_ipv4) {
            Some(endpoint("0.0.0.0:0")?)
        } else {
            None
        };
        let v6 = if addrs.iter().any(SocketAddr::is_ipv6) {
            Some(endpoint("[::]:0")?)
        } else {
            None
        };
        Ok(Self {
            v4,
            v6,
            addrs,
            server_name: server_name.to_string(),
            slots: (0..CONNECTIONS)
                .map(|_| tokio::sync::Mutex::new(None))
                .collect(),
            next: AtomicUsize::new(0),
            handle: handle.clone(),
        })
    }

    /// A request sender on one of the target's connections, connecting to
    /// a pinned address when the slot has none (or a closed one).
    async fn sender(&self) -> Result<Sender, &'static str> {
        let i = self.next.fetch_add(1, Ordering::Relaxed);
        let slot = self.slots.get(i % self.slots.len()).ok_or("connect")?;
        let mut guard = slot.lock().await;
        if let Some(conn) = guard.as_ref() {
            if conn.quic.close_reason().is_none() {
                return Ok(conn.send.clone());
            }
        }
        *guard = None;
        // Only ever a pinned address.
        let addr = *self.addrs.get(i % self.addrs.len()).ok_or("connect")?;
        let endpoint = if addr.is_ipv4() { &self.v4 } else { &self.v6 }
            .as_ref()
            .ok_or("connect")?;
        let connecting = endpoint
            .connect(addr, &self.server_name)
            .map_err(|_| "connect")?;
        let quic = tokio::time::timeout(CONNECT_TIMEOUT, connecting)
            .await
            .map_err(|_| "connect")?
            .map_err(|e| classify_connect(&e))?;
        let (mut driver, send) = h3::client::new(h3_quinn::Connection::new(quic.clone()))
            .await
            .map_err(|_| "protocol")?;
        let driver = tokio::spawn(async move {
            let _ = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
        });
        *guard = Some(Conn {
            quic,
            send: send.clone(),
            driver,
        });
        Ok(send)
    }

    /// Send one request and read its answer whole (bounded). The error is
    /// the engine's transport kind: `connect`, `tls`, `timeout`, `reset` or
    /// `protocol`. `keep`: also the first bytes of the body.
    pub async fn request(
        &self,
        req: hyper::Request<ReqBody>,
        keep: bool,
    ) -> Result<Answer, &'static str> {
        let mut send = self.sender().await?;
        let (mut parts, mut body) = req.into_parts();
        for name in CONNECTION_HEADERS {
            parts.headers.remove(*name);
        }
        let head = hyper::http::Request::from_parts(parts, ());
        let mut stream = send
            .send_request(head)
            .await
            .map_err(|e| classify_stream(&e))?;
        // The body as it comes, so a slow body is slow here too.
        while let Some(frame) = body.frame().await {
            let Ok(frame) = frame;
            if let Ok(data) = frame.into_data() {
                if !data.is_empty() {
                    stream
                        .send_data(data)
                        .await
                        .map_err(|e| classify_stream(&e))?;
                }
            }
        }
        stream.finish().await.map_err(|e| classify_stream(&e))?;
        let resp = stream
            .recv_response()
            .await
            .map_err(|e| classify_stream(&e))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let mut bytes = 0u64;
        let mut kept = keep.then(Vec::new);
        while let Some(mut chunk) = stream.recv_data().await.map_err(|e| classify_stream(&e))? {
            let n = chunk.remaining();
            bytes += n as u64;
            if bytes > MAX_RESPONSE_BYTES as u64 {
                return Err("protocol");
            }
            if let Some(k) = kept.as_mut() {
                let take = n.min(MAX_KEPT_BODY.saturating_sub(k.len()));
                if take > 0 {
                    k.extend_from_slice(&chunk.copy_to_bytes(take));
                }
            }
        }
        Ok(Answer {
            status,
            headers,
            bytes,
            kept,
        })
    }

    /// [`H3Client::request`] run on the engine runtime, for a caller on
    /// another one (target verification).
    pub async fn request_on_engine(
        self: &Arc<Self>,
        req: hyper::Request<ReqBody>,
        keep: bool,
    ) -> Result<Answer, &'static str> {
        let me = self.clone();
        self.handle
            .spawn(async move { me.request(req, keep).await })
            .await
            .map_err(|_| "reset")?
    }
}

/// Why a QUIC handshake failed, as one of the engine's kinds.
fn classify_connect(e: &quinn::ConnectionError) -> &'static str {
    match e {
        // No answer to the handshake: nobody listens on the UDP port.
        quinn::ConnectionError::TimedOut => "connect",
        quinn::ConnectionError::TransportError(t) => {
            let text = t.to_string().to_ascii_lowercase();
            if text.contains("certificate") || text.contains("crypto") || text.contains("tls") {
                "tls"
            } else {
                "protocol"
            }
        }
        quinn::ConnectionError::ConnectionClosed(c) => {
            let text = c.to_string().to_ascii_lowercase();
            if text.contains("certificate") || text.contains("crypto") || text.contains("tls") {
                "tls"
            } else {
                "connect"
            }
        }
        quinn::ConnectionError::VersionMismatch => "protocol",
        _ => "connect",
    }
}

/// Why a stream failed, as one of the engine's kinds.
fn classify_stream(e: &h3::error::StreamError) -> &'static str {
    use h3::error::StreamError;
    match e {
        StreamError::HeaderTooBig { .. } | StreamError::StreamError { .. } => "protocol",
        // The other variants cannot be matched from outside the crate: a
        // connection that timed out says so, anything else ended the stream.
        other => {
            let text = other.to_string().to_ascii_lowercase();
            if text.contains("timeout") || text.contains("timed out") {
                "timeout"
            } else {
                "reset"
            }
        }
    }
}

#[cfg(test)]
#[path = "http3_tests.rs"]
mod tests;
