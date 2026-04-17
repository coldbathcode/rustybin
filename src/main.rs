mod ai_gateway;
mod auth_apikey;
mod auth_basic;
mod auth_jwt;
mod auth_mtls;
mod cert_state;
mod config;
mod content_negotiation;
mod cookies;
mod echo;
mod flaky;
mod graphql;
mod openapi;
mod health;
mod identity;
mod image;
mod info;
mod jwt_state;
mod logging;
mod oidc;
mod orchestration;
mod random;
mod redirects;
mod response_shaping;
mod soap;
mod status;
mod types;

use config::Config;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    let config = Config::from_env();
    logging::init(&config);

    let config = Arc::new(config);
    let jwt_state = Arc::new(jwt_state::JwtState::generate());
    let cert_state = Arc::new(cert_state::CertState::generate(
        &config.tls_cert,
        &config.tls_key,
    ));
    let identity_state = Arc::new(identity::IdentityState {
        start_time: std::time::Instant::now(),
        request_count: std::sync::atomic::AtomicU64::new(0),
    });
    let flaky_state = Arc::new(flaky::FlakyState::new());

    let app = axum::Router::new()
        .merge(health::router())
        .merge(echo::router())
        .merge(status::router())
        .merge(response_shaping::router())
        .merge(redirects::router())
        .merge(cookies::router())
        .merge(info::router())
        .merge(random::router())
        .merge(image::router())
        .merge(auth_basic::router())
        .merge(auth_apikey::router())
        .merge(auth_jwt::router(jwt_state.clone()))
        .merge(oidc::router(jwt_state))
        .merge(auth_mtls::router(cert_state.clone()))
        .merge(ai_gateway::router())
        .merge(graphql::router())
        .merge(orchestration::router())
        .merge(soap::router())
        .merge(identity::router(identity_state))
        .merge(flaky::router(flaky_state))
        .merge(openapi::router())
        .with_state(config.clone());

    let http_addr: SocketAddr = format!("{}:{}", config.host, config.http_port)
        .parse()
        .expect("invalid HTTP bind address");

    let https_addr: SocketAddr = format!("{}:{}", config.host, config.https_port)
        .parse()
        .expect("invalid HTTPS bind address");

    tracing::info!(
        "rustybin v{} starting | instance={} | http={} | https={}",
        env!("CARGO_PKG_VERSION"),
        config.instance_id,
        http_addr,
        https_addr,
    );

    let http_app = app.clone();
    let http_handle = tokio::spawn(async move {
        let listener = TcpListener::bind(http_addr)
            .await
            .expect("failed to bind HTTP listener");
        tracing::info!("HTTP listening on {http_addr}");
        if let Err(e) = axum::serve(listener, http_app.into_make_service_with_connect_info::<SocketAddr>()).await {
            tracing::error!("HTTP server error: {e}");
        }
    });

    let tls_cert_path = config.tls_cert.clone();
    let tls_key_path = config.tls_key.clone();
    let ca_cert_pem = cert_state.ca_cert_pem.clone();
    let https_app = app;

    let https_handle = tokio::spawn(async move {
        if let Err(e) =
            start_https(https_addr, tls_cert_path, tls_key_path, ca_cert_pem, https_app).await
        {
            tracing::warn!("HTTPS server not started: {e}");
        }
    });

    tokio::select! {
        res = http_handle => {
            if let Err(e) = res {
                tracing::error!("HTTP task failed: {e}");
            }
        }
        res = https_handle => {
            if let Err(e) = res {
                tracing::error!("HTTPS task failed: {e}");
            }
        }
    }
}

async fn start_https(
    addr: SocketAddr,
    cert_path: String,
    key_path: String,
    ca_cert_pem: String,
    app: axum::Router,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use axum_server::tls_rustls::RustlsConfig;

    let cert_path = std::path::Path::new(&cert_path);
    let key_path = std::path::Path::new(&key_path);

    if !cert_path.exists() || !key_path.exists() {
        return Err(format!(
            "TLS cert ({}) or key ({}) not found, skipping HTTPS",
            cert_path.display(),
            key_path.display()
        )
        .into());
    }

    // Load server cert and key
    let cert_pem = std::fs::read(cert_path)?;
    let key_pem = std::fs::read(key_path)?;

    let certs: Vec<_> = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .collect::<Result<Vec<_>, _>>()?;
    let key = rustls_pemfile::private_key(&mut key_pem.as_slice())?
        .ok_or("no private key found in key file")?;

    // Load CA cert for optional client cert verification
    let ca_certs: Vec<_> = rustls_pemfile::certs(&mut ca_cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()?;

    let mut root_store = rustls::RootCertStore::empty();
    for ca_cert in ca_certs {
        root_store.add(ca_cert)?;
    }

    let client_verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(root_store))
        .allow_unauthenticated()
        .build()?;

    let server_config = rustls::ServerConfig::builder()
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(certs, key)?;

    let tls_config = RustlsConfig::from_config(Arc::new(server_config));

    tracing::info!("HTTPS listening on {addr} (optional client cert verification enabled)");
    axum_server::bind_rustls(addr, tls_config)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await?;

    Ok(())
}
