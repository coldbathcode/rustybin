//! Listener supervision: HTTP (fatal on failure), HTTPS and gRPC (optional,
//! failures only log a warning), graceful shutdown on SIGTERM/SIGINT.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::state::AppState;

/// Boxed error returned by the server entry points.
pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// How long in-flight connections (SSE, WebSocket, slow requests) get to
/// finish after a shutdown signal before they are dropped.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// A started server. Dropping it does not stop the listeners; call
/// [`RunningServer::shutdown`] and [`RunningServer::wait`].
pub struct RunningServer {
    /// Bound HTTP address (useful with port 0).
    pub http_addr: SocketAddr,
    /// Bound HTTPS address, if the HTTPS listener started.
    pub https_addr: Option<SocketAddr>,
    /// Bound gRPC address, if the gRPC listener started.
    pub grpc_addr: Option<SocketAddr>,
    shutdown: Arc<watch::Sender<bool>>,
    http_task: JoinHandle<std::io::Result<()>>,
    optional_tasks: Vec<(&'static str, JoinHandle<()>)>,
}

/// Cloneable trigger for a graceful shutdown.
#[derive(Clone)]
pub struct ShutdownTrigger(Arc<watch::Sender<bool>>);

impl ShutdownTrigger {
    pub fn trigger(&self) {
        let _ = self.0.send(true);
    }
}

async fn wait_for(mut rx: watch::Receiver<bool>) {
    // Resolves when `true` is sent or the sender is dropped.
    let _ = rx.wait_for(|v| *v).await;
}

impl RunningServer {
    pub fn shutdown_trigger(&self) -> ShutdownTrigger {
        ShutdownTrigger(self.shutdown.clone())
    }

    /// Ask every listener to stop accepting and drain.
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    /// Wait until the HTTP listener stops (after a shutdown, or because it
    /// failed). Optional listeners ending on their own never end the server.
    /// Returns an error when HTTP stopped because of a failure.
    pub async fn wait(mut self) -> Result<(), Error> {
        let mut rx = self.shutdown.subscribe();
        let http_result = tokio::select! {
            res = &mut self.http_task => Some(res),
            _ = rx.wait_for(|v| *v) => None,
        };
        let http_result = match http_result {
            Some(res) => {
                // HTTP ended on its own: stop the others too.
                let _ = self.shutdown.send(true);
                res
            }
            None => match tokio::time::timeout(SHUTDOWN_GRACE, &mut self.http_task).await {
                Ok(res) => res,
                Err(_) => {
                    tracing::warn!("HTTP connections still open after {SHUTDOWN_GRACE:?}, closing");
                    self.http_task.abort();
                    Ok(Ok(()))
                }
            },
        };
        for (name, mut task) in self.optional_tasks {
            if tokio::time::timeout(SHUTDOWN_GRACE, &mut task)
                .await
                .is_err()
            {
                tracing::warn!("{name} listener did not stop in time, aborting");
                task.abort();
            }
        }
        let shutting_down = *self.shutdown.borrow();
        match http_result {
            Ok(Ok(())) if shutting_down => {
                tracing::info!("shutdown complete");
                Ok(())
            }
            Ok(Ok(())) => Err("HTTP listener stopped unexpectedly".into()),
            Ok(Err(e)) => Err(format!("HTTP server error: {e}").into()),
            Err(e) => Err(format!("HTTP task failed: {e}").into()),
        }
    }
}

/// Run the server until SIGTERM/SIGINT. Errors only when the HTTP listener
/// cannot bind or fails; HTTPS and gRPC problems are logged and tolerated.
pub async fn run(config: Config) -> Result<(), Error> {
    let server = start(config).await?;
    let trigger = server.shutdown_trigger();
    tokio::spawn(async move {
        shutdown_signal().await;
        tracing::info!("shutdown signal received, draining connections");
        trigger.trigger();
    });
    server.wait().await
}

/// Generate the application state (keys, PKI) and start all listeners.
/// The demo CA is loaded from / persisted next to the TLS certificate.
pub async fn start(config: Config) -> Result<RunningServer, Error> {
    let certs = Arc::new(crate::cert_state::CertState::for_config(&config));
    let jwt = Arc::new(crate::jwt_state::JwtState::generate());
    start_with_state(AppState::from_parts(config, jwt, certs)).await
}

/// Start all listeners for an existing state. Ports may be 0 (ephemeral);
/// the bound addresses are reported on the returned [`RunningServer`].
pub async fn start_with_state(state: AppState) -> Result<RunningServer, Error> {
    let config = state.config.clone();
    let (shutdown_tx, _) = watch::channel(false);
    let shutdown = Arc::new(shutdown_tx);
    let app = crate::build_app(state.clone());

    // HTTP: fatal on failure.
    let http_bind = SocketAddr::new(config.host, config.http_port);
    let http_listener = TcpListener::bind(http_bind)
        .await
        .map_err(|e| format!("failed to bind HTTP listener on {http_bind}: {e}"))?;
    let http_addr = http_listener.local_addr()?;

    tracing::info!(
        "rustybin v{} starting | instance={} | public_mode={}",
        env!("CARGO_PKG_VERSION"),
        config.instance_id,
        config.public_mode,
    );

    let http_task = {
        // Tag requests with the listener so handlers report scheme and port.
        let app = app
            .clone()
            .layer(axum::Extension(crate::session::ListenerInfo::http(
                http_addr.port(),
            )));
        let rx = shutdown.subscribe();
        tokio::spawn(async move {
            tracing::info!("HTTP listening on {http_addr}");
            axum::serve(
                http_listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(wait_for(rx))
            .await
        })
    };

    let mut optional_tasks = Vec::new();

    // HTTPS: optional.
    let https_addr = match start_https(&state, app, shutdown.subscribe()) {
        Ok((addr, task)) => {
            optional_tasks.push(("HTTPS", task));
            Some(addr)
        }
        Err(e) => {
            tracing::warn!("HTTPS listener not started: {e}");
            None
        }
    };

    // gRPC: optional.
    let grpc_bind = SocketAddr::new(config.host, config.grpc_port);
    let grpc_addr = match TcpListener::bind(grpc_bind).await {
        Ok(listener) => {
            let addr = listener.local_addr()?;
            let grpc_state = state.clone();
            let rx = shutdown.subscribe();
            let task = tokio::spawn(async move {
                if let Err(e) = crate::grpc::serve(listener, grpc_state, wait_for(rx)).await {
                    tracing::warn!("gRPC listener stopped: {e} (HTTP keeps serving)");
                }
            });
            optional_tasks.push(("gRPC", task));
            Some(addr)
        }
        Err(e) => {
            tracing::warn!("gRPC listener not started on {grpc_bind}: {e}");
            None
        }
    };

    Ok(RunningServer {
        http_addr,
        https_addr,
        grpc_addr,
        shutdown,
        http_task,
        optional_tasks,
    })
}

/// Connection facts of the HTTPS listener, inserted as a request extension
/// on every request it serves (absent on plain HTTP).
///
/// Handlers read it with `Option<Extension<TlsConnectionInfo>>`.
#[derive(Clone, Debug, Default)]
pub struct TlsConnectionInfo {
    /// DER of the client (leaf) certificate, when the client presented one.
    /// The listener only accepts certificates that chain to the demo CA.
    pub peer_certificate: Option<Arc<Vec<u8>>>,
}

/// Wraps the per-connection service and inserts [`TlsConnectionInfo`].
#[derive(Clone)]
struct WithTlsInfo<S> {
    inner: S,
    info: TlsConnectionInfo,
}

impl<S, B> tower::Service<axum::http::Request<B>> for WithTlsInfo<S>
where
    S: tower::Service<axum::http::Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: axum::http::Request<B>) -> Self::Future {
        req.extensions_mut().insert(self.info.clone());
        self.inner.call(req)
    }
}

/// rustls acceptor that exposes the verified peer certificate to handlers.
#[derive(Clone)]
struct PeerCertAcceptor {
    inner: axum_server::tls_rustls::RustlsAcceptor,
}

impl<S> axum_server::accept::Accept<tokio::net::TcpStream, S> for PeerCertAcceptor
where
    S: Send + 'static,
{
    type Stream = <axum_server::tls_rustls::RustlsAcceptor as axum_server::accept::Accept<
        tokio::net::TcpStream,
        S,
    >>::Stream;
    type Service = WithTlsInfo<S>;
    type Future = std::pin::Pin<
        Box<
            dyn std::future::Future<Output = std::io::Result<(Self::Stream, Self::Service)>> + Send,
        >,
    >;

    fn accept(&self, stream: tokio::net::TcpStream, service: S) -> Self::Future {
        let handshake = self.inner.accept(stream, service);
        Box::pin(async move {
            let (stream, service) = handshake.await?;
            let (_, connection) = stream.get_ref();
            let peer_certificate = connection
                .peer_certificates()
                .and_then(|chain| chain.first())
                .map(|leaf| Arc::new(leaf.as_ref().to_vec()));
            let info = TlsConnectionInfo { peer_certificate };
            Ok((
                stream,
                WithTlsInfo {
                    inner: service,
                    info,
                },
            ))
        })
    }
}

fn start_https(
    state: &AppState,
    app: Router,
    rx: watch::Receiver<bool>,
) -> Result<(SocketAddr, JoinHandle<()>), Error> {
    use axum_server::tls_rustls::{RustlsAcceptor, RustlsConfig};

    let config = &state.config;
    let (cert_pem, key_pem) = state
        .certs
        .server_tls_material(&config.tls_cert, &config.tls_key)?;
    let certs: Vec<_> =
        rustls_pemfile::certs(&mut cert_pem.as_slice()).collect::<Result<Vec<_>, _>>()?;
    if certs.is_empty() {
        return Err("no certificate found in TLS cert file".into());
    }
    let key = rustls_pemfile::private_key(&mut key_pem.as_slice())?
        .ok_or("no private key found in TLS key file")?;

    // Optional client certificate verification against the demo CA.
    let server_config = rustls::ServerConfig::builder()
        .with_client_cert_verifier(state.certs.client_verifier())
        .with_single_cert(certs, key)?;
    let tls_config = RustlsConfig::from_config(Arc::new(server_config));
    let acceptor = PeerCertAcceptor {
        inner: RustlsAcceptor::new(tls_config),
    };

    let bind = SocketAddr::new(state.config.host, state.config.https_port);
    let listener = std::net::TcpListener::bind(bind)
        .map_err(|e| format!("failed to bind HTTPS listener on {bind}: {e}"))?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let app = app.layer(axum::Extension(crate::session::ListenerInfo::https(
        addr.port(),
    )));

    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();
    tokio::spawn(async move {
        wait_for(rx).await;
        shutdown_handle.graceful_shutdown(Some(SHUTDOWN_GRACE));
    });

    let task = tokio::spawn(async move {
        tracing::info!("HTTPS listening on {addr} (optional client cert verification enabled)");
        let result = axum_server::from_tcp(listener)
            .acceptor(acceptor)
            .handle(handle)
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await;
        if let Err(e) = result {
            tracing::warn!("HTTPS listener stopped: {e} (HTTP keeps serving)");
        }
    });
    Ok((addr, task))
}

/// Resolves on SIGINT (Ctrl-C) or SIGTERM.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!("failed to listen for Ctrl-C: {e}");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::warn!("failed to listen for SIGTERM: {e}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
