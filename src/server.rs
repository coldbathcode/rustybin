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

/// How long a client may take to send a request head (and how long an idle
/// keep-alive connection waits for the next one): a slow client cannot hold
/// a connection open without sending a request.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 2 } else { 30 });
/// Connections the HTTP and HTTPS listeners hold at once, together; one
/// more is closed as soon as it is accepted.
pub const MAX_CONNECTIONS: usize = 16_384;

/// The hyper settings every listener uses: a header read timeout (it needs
/// a timer).
fn tune(builder: &mut hyper_util::server::conn::auto::Builder<hyper_util::rt::TokioExecutor>) {
    builder
        .http1()
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT);
    builder.http2().timer(hyper_util::rt::TokioTimer::new());
}

/// An acceptor that holds one of [`MAX_CONNECTIONS`] permits per connection
/// (released when the connection closes) and refuses a connection when none
/// is left.
#[derive(Clone)]
struct Limited<A> {
    inner: A,
    permits: Arc<tokio::sync::Semaphore>,
}

impl<A, I, S> axum_server::accept::Accept<I, S> for Limited<A>
where
    A: axum_server::accept::Accept<I, S> + Clone + Send + Sync + 'static,
    A::Stream: Unpin,
    A::Future: Send + 'static,
    I: Send + 'static,
    S: Send + 'static,
{
    type Stream = Permitted<A::Stream>;
    type Service = A::Service;
    type Future = std::pin::Pin<
        Box<
            dyn std::future::Future<Output = std::io::Result<(Self::Stream, Self::Service)>> + Send,
        >,
    >;

    fn accept(&self, stream: I, service: S) -> Self::Future {
        let permit = self.permits.clone().try_acquire_owned();
        let inner = self.inner.accept(stream, service);
        Box::pin(async move {
            let Ok(permit) = permit else {
                return Err(std::io::Error::other("connection limit reached"));
            };
            let (stream, service) = inner.await?;
            Ok((
                Permitted {
                    inner: stream,
                    _permit: permit,
                },
                service,
            ))
        })
    }
}

/// A connection's stream with its permit.
struct Permitted<S> {
    inner: S,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for Permitted<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for Permitted<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

/// How long in-flight connections (SSE, WebSocket, slow requests) get to
/// finish after a shutdown signal before they are dropped.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// A started server. Call [`RunningServer::shutdown`] and
/// [`RunningServer::wait`] to stop it. Dropping it also starts a graceful
/// shutdown (the listeners stop when the last shutdown sender, this value or a
/// [`ShutdownTrigger`], is gone), so keep it alive while the server should run.
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
    limits: Arc<crate::limits::Limiter>,
    audit: Arc<crate::audit::Audit>,
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
        // Final usage file write, after in-flight responses were counted.
        self.limits.save();
        // The audit entries still on their way to a file.
        let audit = self.audit.clone();
        let _ = tokio::task::spawn_blocking(move || audit.flush()).await;
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
    run_with_exit(config).await.map(|_| ())
}

/// [`run`] with the process exit code: 0, or the result of a headless
/// harness run (`RUSTYBIN_HARNESS_SCENARIO`, `RUSTYBIN_HARNESS_START`,
/// `RUSTYBIN_HARNESS_EXIT`: 0 when the thresholds pass, 1 when they fail,
/// 2 on an error).
pub async fn run_with_exit(config: Config) -> Result<u8, Error> {
    if let Some(path) = &config.oidc_users_file {
        let n = crate::oidc::users::install_file(path)
            .map_err(|e| format!("RUSTYBIN_OIDC_USERS_FILE: {e}"))?;
        tracing::info!(
            "OIDC provider: {n} configured users from {} (demo users off)",
            path.display()
        );
    }
    let certs = Arc::new(crate::cert_state::CertState::for_config(&config));
    let jwt = Arc::new(crate::jwt_state::JwtState::generate());
    let state = AppState::from_parts(config, jwt, certs);
    let server = start_with_state(state.clone()).await?;
    let trigger = server.shutdown_trigger();
    tokio::spawn(async move {
        shutdown_signal().await;
        tracing::info!("shutdown signal received, draining connections");
        trigger.trigger();
    });
    let mut code = 0;
    if state.config.harness.headless() {
        if let Some(harness) = state.harness.clone() {
            code = crate::harness::run_headless(&harness).await;
            if state.config.harness.exit {
                tracing::info!("headless harness run ended with exit code {code}, shutting down");
                server.shutdown();
            }
        }
    }
    server.wait().await?;
    Ok(code)
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
    // Harness mode in public mode, with self-service, or a client without
    // targets: fatal, never a warning.
    crate::harness::check_startup(&config)?;
    // A public instance never runs with the default console password.
    crate::login::check_startup(&state)?;
    let (shutdown_tx, _) = watch::channel(false);
    let shutdown = Arc::new(shutdown_tx);
    let mut app = crate::build_app(state.clone());
    let mut optional_tasks = Vec::new();

    // RUSTYBIN_GRPC_ON_HTTP: gRPC (content-type application/grpc*) on the
    // HTTP and HTTPS listeners too.
    if config.grpc_on_http {
        match crate::grpc::routes(&state) {
            Ok((routes, health_task)) => {
                app = crate::grpc::GrpcOnHttp::new(app, routes, &state).into_router();
                let rx = shutdown.subscribe();
                let task = tokio::spawn(async move {
                    wait_for(rx).await;
                    health_task.abort();
                });
                optional_tasks.push(("gRPC on HTTP health", task));
            }
            Err(e) => {
                tracing::warn!("gRPC on the HTTP listeners not enabled: {e}");
                state.diagnostics.note("grpc_on_http", e.to_string());
            }
        }
    }

    // HTTP: fatal on failure.
    let http_bind = SocketAddr::new(config.host, config.http_port);
    let http_listener = TcpListener::bind(http_bind)
        .await
        .map_err(|e| format!("failed to bind HTTP listener on {http_bind}: {e}"))?;
    let http_addr = http_listener.local_addr()?;
    let connections = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    // Harness clusters (RUSTYBIN_HARNESS_CLUSTER, RUSTYBIN_HARNESS_JOIN):
    // a coordinator creates its cluster, a member starts joining. A bad
    // setting is fatal like the other harness startup checks.
    if let Some(h) = &state.harness {
        h.set_bound(http_addr);
        crate::harness::start_cluster_from_config(h).await?;
        let h = h.clone();
        let rx = shutdown.subscribe();
        tokio::spawn(async move {
            wait_for(rx).await;
            h.shutdown_clusters().await;
        });
    }

    tracing::info!(
        "rustybin v{} starting | instance={} | public_mode={} | keys={} | plan={} | control_auth={} | grpc_on_http={}",
        env!("CARGO_PKG_VERSION"),
        config.instance_id,
        config.public_mode,
        state.keys.mode().as_str(),
        config.limits.plan_name(),
        config.control_auth.as_str(),
        config.grpc_on_http,
    );

    crate::login::warn_default_password(&state);
    crate::rbac::warn_in_memory(&state);
    crate::login::warn_untrusted_forwarding(&state);

    match &config.state_dir {
        Some(dir) => tracing::info!(
            "state directory {}: settings.json, secrets.key ({}), mocks/, harness/{}",
            dir.display(),
            if config.secrets_key.is_some() {
                "key from RUSTYBIN_SECRETS_KEY"
            } else {
                "generated once"
            },
            if config.settings_lock {
                "; RUSTYBIN_SETTINGS_LOCK: the environment is authoritative"
            } else {
                ""
            },
        ),
        None => tracing::info!(
            "no RUSTYBIN_STATE_DIR: settings and secrets are kept in memory until restart"
        ),
    }

    // Mocks from RUSTYBIN_CONFIG_FILE / RUSTYBIN_MOCKS_DIR load before any
    // request is served: the socket is bound (a busy port still fails fast)
    // but nothing answers until they are in place, so no early 404s.
    crate::mocks::load_startup(&state);
    // RUSTYBIN_CONFIG_GIT: clone the configuration repository and apply it
    // before serving (fatal only with RUSTYBIN_CONFIG_GIT_REQUIRED=true).
    state.git.startup(&state).await?;

    let http_task = {
        // Tag requests with the listener so handlers report scheme and port.
        let app = app
            .clone()
            .layer(axum::Extension(crate::session::ListenerInfo::http(
                http_addr.port(),
            )));
        let rx = shutdown.subscribe();
        let handle = axum_server::Handle::new();
        let shutdown_handle = handle.clone();
        tokio::spawn(async move {
            wait_for(rx).await;
            // RunningServer bounds the wait (SHUTDOWN_GRACE), then aborts.
            shutdown_handle.graceful_shutdown(None);
        });
        let listener = http_listener.into_std()?;
        listener.set_nonblocking(true)?;
        let mut server = axum_server::from_tcp(listener)?.acceptor(Limited {
            inner: axum_server::accept::DefaultAcceptor::new(),
            permits: connections.clone(),
        });
        tune(server.http_builder());
        tokio::spawn(async move {
            tracing::info!("HTTP listening on {http_addr}");
            server
                .handle(handle)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await
        })
    };

    // Mocks from RUSTYBIN_CONFIG_FILE / RUSTYBIN_MOCKS_DIR (loaded before the
    // HTTP listener serves, above), reloaded on SIGHUP.
    #[cfg(unix)]
    if config.config_file.is_some() || config.mocks_dir.is_some() {
        let mocks_state = state.clone();
        let rx = shutdown.subscribe();
        let task = tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            let Ok(mut hup) = signal(SignalKind::hangup()) else {
                tracing::warn!(
                    "SIGHUP handler not installed: mocks reload with POST /_rustybin/mocks/reload"
                );
                return;
            };
            let stop = wait_for(rx);
            tokio::pin!(stop);
            loop {
                tokio::select! {
                    _ = &mut stop => break,
                    received = hup.recv() => {
                        if received.is_none() {
                            break;
                        }
                        let (report, problems) =
                            crate::mocks::reload_files(&mocks_state);
                        tracing::info!(
                            "mocks reloaded: {} created, {} updated, {} deleted, {} problem(s)",
                            report.created.len(),
                            report.updated.len(),
                            report.deleted.len(),
                            problems.len()
                        );
                    }
                }
            }
        });
        optional_tasks.push(("mocks reload", task));
    }

    // Source control's automatic mode (git.auto): every 30 s, push the
    // instance's changes or pull the repository at its interval.
    {
        let git_state = state.clone();
        let rx = shutdown.subscribe();
        let task = tokio::spawn(async move {
            let stop = wait_for(rx);
            tokio::pin!(stop);
            let mut tick = tokio::time::interval(crate::sourcecontrol::AUTO_TICK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await;
            loop {
                tokio::select! {
                    _ = &mut stop => break,
                    _ = tick.tick() => git_state.git.auto_tick(&git_state).await,
                }
            }
        });
        optional_tasks.push(("git auto", task));
    }

    // Saved tests on a schedule: the scheduler, and the delivery of the
    // failure notices to the schedules' webhooks.
    let scheduler = tokio::spawn(crate::replay::schedule::scheduler_loop(
        state.clone(),
        wait_for(shutdown.subscribe()),
    ));
    optional_tasks.push(("test schedules", scheduler));
    let notices = tokio::spawn(crate::replay::schedule::notice_loop(
        state.clone(),
        wait_for(shutdown.subscribe()),
    ));
    optional_tasks.push(("test schedule notices", notices));

    // Plan usage persistence (RUSTYBIN_USAGE_FILE); returns at once when off.
    let persist = tokio::spawn(
        state
            .limits
            .clone()
            .persist_loop(wait_for(shutdown.subscribe())),
    );
    optional_tasks.push(("usage persistence", persist));

    // Self-service mocks: end visits on time and deliver captured leads.
    if let Some(visits) = state.mocks.visits().cloned() {
        let store = state.mocks.clone();
        let metrics = state.metrics.clone();
        let stop = wait_for(shutdown.subscribe());
        let sweep = tokio::spawn(async move {
            tokio::pin!(stop);
            let mut tick = tokio::time::interval(Duration::from_secs(15));
            loop {
                tokio::select! {
                    _ = &mut stop => return,
                    _ = tick.tick() => {
                        let gone = store.sweep();
                        if gone > 0 {
                            tracing::info!("visits ended: {gone} visitor mock(s) deleted");
                        }
                        metrics.set_mock_visits(visits.running() as i64);
                    }
                }
            }
        });
        optional_tasks.push(("visit sweep", sweep));
        let leads = state
            .mocks
            .visits()
            .map(|v| v.leads().clone())
            .map(|l| tokio::spawn(l.run(wait_for(shutdown.subscribe()))));
        if let Some(task) = leads {
            optional_tasks.push(("lead delivery", task));
        }
    }

    // API keys: registry reloads and quota store flushes (no-ops when off).
    if state.keys.enabled() {
        let registry = tokio::spawn(
            state
                .keys
                .clone()
                .registry_loop(wait_for(shutdown.subscribe())),
        );
        optional_tasks.push(("key registry", registry));
        let flush = tokio::spawn(
            state
                .keys
                .clone()
                .flush_loop(wait_for(shutdown.subscribe())),
        );
        optional_tasks.push(("quota store", flush));
    }

    // HTTPS: optional.
    let https_addr = match start_https(&state, app, shutdown.subscribe(), connections.clone()) {
        Ok((addr, task)) => {
            optional_tasks.push(("HTTPS", task));
            Some(addr)
        }
        Err(e) => {
            tracing::warn!("HTTPS listener not started: {e}");
            state.diagnostics.note("https", e.to_string());
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
                let notes = grpc_state.diagnostics.clone();
                if let Err(e) = crate::grpc::serve(listener, grpc_state, wait_for(rx)).await {
                    tracing::warn!("gRPC listener stopped: {e} (HTTP keeps serving)");
                    notes.note("grpc", format!("stopped: {e}"));
                }
            });
            optional_tasks.push(("gRPC", task));
            Some(addr)
        }
        Err(e) => {
            tracing::warn!("gRPC listener not started on {grpc_bind}: {e}");
            state
                .diagnostics
                .note("grpc", format!("not started on {grpc_bind}: {e}"));
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
        limits: state.limits.clone(),
        audit: state.audit.clone(),
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
    connections: Arc<tokio::sync::Semaphore>,
) -> Result<(SocketAddr, JoinHandle<()>), Error> {
    use axum_server::tls_rustls::{RustlsAcceptor, RustlsConfig};

    let config = &state.config;
    let (cert_pem, key_pem) = state
        .certs
        .server_tls_material(&config.tls_cert, &config.tls_key)?;
    use rustls::pki_types::pem::PemObject;
    let certs: Vec<_> = rustls::pki_types::CertificateDer::pem_slice_iter(&cert_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("TLS cert file: {e:?}"))?;
    if certs.is_empty() {
        return Err("no certificate found in TLS cert file".into());
    }
    let key = rustls::pki_types::PrivateKeyDer::from_pem_slice(&key_pem)
        .map_err(|_| "no private key found in TLS key file")?;

    // Optional client certificate verification against the demo CA.
    let mut server_config = rustls::ServerConfig::builder()
        .with_client_cert_verifier(state.certs.client_verifier())
        .with_single_cert(certs, key)?;
    if config.grpc_on_http {
        // gRPC needs HTTP/2: offer it through ALPN (HTTP/1.1 stays available).
        server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    }
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

    let mut server = axum_server::from_tcp(listener)?.acceptor(Limited {
        inner: acceptor,
        permits: connections,
    });
    tune(server.http_builder());
    let task = tokio::spawn(async move {
        tracing::info!("HTTPS listening on {addr} (optional client cert verification enabled)");
        let result = server
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn a_client_that_never_finishes_its_request_head_is_dropped() {
        let server = super::start_with_state(crate::state::AppState::for_tests(
            crate::config::Config::for_tests(),
        ))
        .await
        .expect("start");
        let mut tcp = tokio::net::TcpStream::connect(server.http_addr)
            .await
            .expect("connect");
        tcp.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n")
            .await
            .expect("write");
        let started = Instant::now();
        let mut buf = [0u8; 512];
        // The server answers 408 or just closes: either way the read ends.
        let read = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match tcp.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => continue,
                }
            }
        })
        .await;
        assert!(read.is_ok(), "the half-open request was kept past 10 s");
        assert!(started.elapsed() >= super::HEADER_READ_TIMEOUT - Duration::from_millis(200));
        // A complete request still works.
        let resp = reqwest::get(format!("http://{}/", server.http_addr))
            .await
            .expect("get");
        assert_eq!(resp.status(), 200);
        server.shutdown();
        let _ = server.wait().await;
    }
}
