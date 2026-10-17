//! The shared quota store (`RUSTYBIN_QUOTA_STORE`): per-key daily totals
//! shared by every replica of a shared deployment.
//!
//! The PostgreSQL implementation needs the cargo feature `quota-postgres`
//! (off by default). Table, created on startup when missing:
//!
//! ```sql
//! create table if not exists key_usage (
//!   key_id text not null, day date not null,
//!   requests bigint not null default 0, egress_bytes bigint not null default 0,
//!   primary key (key_id, day));
//! ```
//!
//! Each flush is one statement per batch:
//! `insert into key_usage (key_id, day, requests, egress_bytes) values ...
//! on conflict (key_id, day) do update set requests = key_usage.requests +
//! excluded.requests, egress_bytes = key_usage.egress_bytes +
//! excluded.egress_bytes returning key_id, day, requests, egress_bytes`.

use std::future::Future;
use std::pin::Pin;

use chrono::NaiveDate;

use super::ledger::{Delta, Total};

/// Boxed future of the store operations.
pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// Days of rows kept in the store.
pub const RETENTION_DAYS: i64 = 35;
/// Rows per statement (4 parameters each, well under the 65,535 limit).
pub const MAX_BATCH_ROWS: usize = 5_000;

/// A store for per-key daily totals.
pub trait QuotaStore: Send + Sync {
    /// Short name for logs and `/_rustybin/usage` (`postgres`, `memory`).
    fn kind(&self) -> &'static str;
    /// Add the deltas (at most [`MAX_BATCH_ROWS`]) and return the new
    /// totals of the affected rows.
    fn flush<'a>(&'a self, batch: &'a [Delta]) -> StoreFuture<'a, Vec<Total>>;
    /// Totals of one day, sorted by key id, after `after`, at most `limit`.
    fn page<'a>(
        &'a self,
        day: NaiveDate,
        after: Option<&'a str>,
        limit: usize,
    ) -> StoreFuture<'a, Vec<Total>>;
    /// Delete rows of days before `before`; returns the number deleted.
    fn cleanup(&self, before: NaiveDate) -> StoreFuture<'_, u64>;
    /// Connect (if needed) and make sure the table exists.
    fn ping(&self) -> StoreFuture<'_, ()>;
}

/// Open the store named by `RUSTYBIN_QUOTA_STORE`. `None` (with a warning)
/// when the URL is not supported by this build.
pub fn open(
    url: &str,
    ca_file: Option<&std::path::Path>,
) -> Option<std::sync::Arc<dyn QuotaStore>> {
    let lower = url.trim().to_ascii_lowercase();
    if !(lower.starts_with("postgres://") || lower.starts_with("postgresql://")) {
        tracing::warn!(
            "RUSTYBIN_QUOTA_STORE must be a postgres:// URL: quota store disabled, counting locally"
        );
        return None;
    }
    open_postgres(url, ca_file)
}

#[cfg(feature = "quota-postgres")]
fn open_postgres(
    url: &str,
    ca_file: Option<&std::path::Path>,
) -> Option<std::sync::Arc<dyn QuotaStore>> {
    match postgres::PgStore::new(url, ca_file) {
        Ok(store) => Some(std::sync::Arc::new(store)),
        Err(e) => {
            tracing::warn!(
                "RUSTYBIN_QUOTA_STORE is invalid ({e}): quota store disabled, counting locally"
            );
            None
        }
    }
}

#[cfg(not(feature = "quota-postgres"))]
fn open_postgres(
    _url: &str,
    _ca_file: Option<&std::path::Path>,
) -> Option<std::sync::Arc<dyn QuotaStore>> {
    tracing::warn!(
        "RUSTYBIN_QUOTA_STORE is set but this binary was built without the quota-postgres \
         feature: quota store disabled, counting locally"
    );
    None
}

// ── In-memory store (tests, single process) ─────────────────────────

/// A store in process memory: lets several in-process replicas share
/// totals (tests) and can simulate an outage.
#[derive(Default)]
pub struct MemoryStore {
    rows: std::sync::Mutex<std::collections::BTreeMap<(NaiveDate, String), (u64, u64)>>,
    down: std::sync::atomic::AtomicBool,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Simulate an outage (every operation fails) or a recovery.
    pub fn set_down(&self, down: bool) {
        self.down.store(down, std::sync::atomic::Ordering::SeqCst);
    }

    fn check(&self) -> Result<(), String> {
        if self.down.load(std::sync::atomic::Ordering::SeqCst) {
            Err("memory store is down".to_string())
        } else {
            Ok(())
        }
    }

    fn rows(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::BTreeMap<(NaiveDate, String), (u64, u64)>>
    {
        self.rows.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl QuotaStore for MemoryStore {
    fn kind(&self) -> &'static str {
        "memory"
    }

    fn flush<'a>(&'a self, batch: &'a [Delta]) -> StoreFuture<'a, Vec<Total>> {
        Box::pin(async move {
            self.check()?;
            let mut rows = self.rows();
            let mut out = Vec::with_capacity(batch.len());
            for d in batch {
                let row = rows.entry((d.day, d.key_id.to_string())).or_insert((0, 0));
                row.0 = row.0.saturating_add(d.requests);
                row.1 = row.1.saturating_add(d.egress_bytes);
                out.push(Total {
                    day: d.day,
                    key_id: d.key_id.to_string(),
                    requests: row.0,
                    egress_bytes: row.1,
                });
            }
            Ok(out)
        })
    }

    fn page<'a>(
        &'a self,
        day: NaiveDate,
        after: Option<&'a str>,
        limit: usize,
    ) -> StoreFuture<'a, Vec<Total>> {
        Box::pin(async move {
            self.check()?;
            Ok(self
                .rows()
                .iter()
                .filter(|((d, k), _)| *d == day && after.is_none_or(|a| k.as_str() > a))
                .take(limit)
                .map(|((d, k), (r, e))| Total {
                    day: *d,
                    key_id: k.clone(),
                    requests: *r,
                    egress_bytes: *e,
                })
                .collect())
        })
    }

    fn cleanup(&self, before: NaiveDate) -> StoreFuture<'_, u64> {
        Box::pin(async move {
            self.check()?;
            let mut rows = self.rows();
            let n = rows.len();
            rows.retain(|(d, _), _| *d >= before);
            Ok((n - rows.len()) as u64)
        })
    }

    fn ping(&self) -> StoreFuture<'_, ()> {
        Box::pin(async move { self.check() })
    }
}

// ── PostgreSQL ──────────────────────────────────────────────────────

#[cfg(feature = "quota-postgres")]
pub mod postgres {
    //! PostgreSQL quota store (`tokio-postgres`, TLS through rustls).
    //!
    //! `sslmode=disable` connects in plain text; otherwise TLS is offered
    //! (`prefer`, the default, falls back to plain text when the server has
    //! no TLS) and the server certificate is verified against the public
    //! web roots plus the PEM file in `RUSTYBIN_QUOTA_STORE_CA`, if any.

    use std::sync::Arc;
    use std::time::Duration;

    use chrono::NaiveDate;
    use tokio::sync::Mutex;
    use tokio_postgres::types::ToSql;
    use tokio_postgres::{Client, Config, NoTls};

    use super::{QuotaStore, StoreFuture};
    use crate::keys::ledger::{Delta, Total};

    /// Bound on connecting and on each statement.
    pub const TIMEOUT: Duration = Duration::from_secs(5);

    const CREATE_TABLE: &str = "create table if not exists key_usage (\
        key_id text not null, \
        day date not null, \
        requests bigint not null default 0, \
        egress_bytes bigint not null default 0, \
        primary key (key_id, day))";

    pub struct PgStore {
        config: Config,
        tls: Option<tokio_postgres_rustls::MakeRustlsConnect>,
        client: Mutex<Option<Arc<Client>>>,
    }

    impl PgStore {
        /// Parse the URL (no connection yet).
        pub fn new(url: &str, ca_file: Option<&std::path::Path>) -> Result<Self, String> {
            let mut config: Config = url
                .parse()
                .map_err(|e| format!("not a PostgreSQL URL: {e}"))?;
            config.connect_timeout(TIMEOUT);
            if config.get_application_name().is_none() {
                config.application_name("rustybin");
            }
            let tls = match config.get_ssl_mode() {
                tokio_postgres::config::SslMode::Disable => None,
                _ => Some(tokio_postgres_rustls::MakeRustlsConnect::new(tls_config(
                    ca_file,
                )?)),
            };
            Ok(Self {
                config,
                tls,
                client: Mutex::new(None),
            })
        }

        /// A connected client (connects and creates the table when needed).
        async fn client(&self) -> Result<Arc<Client>, String> {
            let mut guard = self.client.lock().await;
            if let Some(c) = guard.as_ref().filter(|c| !c.is_closed()) {
                return Ok(c.clone());
            }
            *guard = None;
            let client = match &self.tls {
                None => {
                    let (client, conn) = tokio::time::timeout(TIMEOUT, self.config.connect(NoTls))
                        .await
                        .map_err(|_| "connect timed out".to_string())?
                        .map_err(|e| format!("connect: {e}"))?;
                    tokio::spawn(async move {
                        if let Err(e) = conn.await {
                            tracing::debug!("quota store connection ended: {e}");
                        }
                    });
                    client
                }
                Some(tls) => {
                    let (client, conn) =
                        tokio::time::timeout(TIMEOUT, self.config.connect(tls.clone()))
                            .await
                            .map_err(|_| "connect timed out".to_string())?
                            .map_err(|e| format!("connect: {e}"))?;
                    tokio::spawn(async move {
                        if let Err(e) = conn.await {
                            tracing::debug!("quota store connection ended: {e}");
                        }
                    });
                    client
                }
            };
            ensure_table(&client).await?;
            let client = Arc::new(client);
            *guard = Some(client.clone());
            Ok(client)
        }

        /// Run one statement with a timeout; drops the connection on error
        /// so the next call reconnects.
        async fn run<T, Fut>(&self, f: impl FnOnce(Arc<Client>) -> Fut) -> Result<T, String>
        where
            Fut: std::future::Future<Output = Result<T, String>>,
        {
            let client = self.client().await?;
            let result = match tokio::time::timeout(TIMEOUT, f(client)).await {
                Ok(r) => r,
                Err(_) => Err("statement timed out".to_string()),
            };
            if result.is_err() {
                *self.client.lock().await = None;
            }
            result
        }
    }

    async fn ensure_table(client: &Client) -> Result<(), String> {
        match tokio::time::timeout(TIMEOUT, client.batch_execute(CREATE_TABLE)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                // Two replicas creating the table at the same moment: one
                // fails on the catalog's unique index; the table exists.
                let code = e.code().map(|c| c.code().to_string());
                if matches!(code.as_deref(), Some("23505") | Some("42P07")) {
                    Ok(())
                } else {
                    Err(format!("create table key_usage: {e}"))
                }
            }
            Err(_) => Err("create table timed out".to_string()),
        }
    }

    fn tls_config(ca_file: Option<&std::path::Path>) -> Result<rustls::ClientConfig, String> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        if let Some(path) = ca_file {
            let pem = std::fs::read(path).map_err(|e| {
                format!(
                    "cannot read RUSTYBIN_QUOTA_STORE_CA {}: {e}",
                    path.display()
                )
            })?;
            for cert in rustls_pemfile::certs(&mut pem.as_slice()) {
                let cert = cert.map_err(|e| format!("RUSTYBIN_QUOTA_STORE_CA: {e}"))?;
                roots
                    .add(cert)
                    .map_err(|e| format!("RUSTYBIN_QUOTA_STORE_CA: {e}"))?;
            }
        }
        Ok(rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth())
    }

    fn to_i64(v: u64) -> i64 {
        i64::try_from(v).unwrap_or(i64::MAX)
    }

    fn to_u64(v: i64) -> u64 {
        u64::try_from(v).unwrap_or(0)
    }

    fn row_total(row: &tokio_postgres::Row) -> Result<Total, String> {
        let get = |e: tokio_postgres::Error| format!("unexpected row: {e}");
        Ok(Total {
            key_id: row.try_get::<_, String>(0).map_err(get)?,
            day: row.try_get::<_, NaiveDate>(1).map_err(get)?,
            requests: to_u64(row.try_get::<_, i64>(2).map_err(get)?),
            egress_bytes: to_u64(row.try_get::<_, i64>(3).map_err(get)?),
        })
    }

    impl QuotaStore for PgStore {
        fn kind(&self) -> &'static str {
            "postgres"
        }

        fn flush<'a>(&'a self, batch: &'a [Delta]) -> StoreFuture<'a, Vec<Total>> {
            Box::pin(async move {
                if batch.is_empty() {
                    return Ok(Vec::new());
                }
                let mut sql = String::from(
                    "insert into key_usage (key_id, day, requests, egress_bytes) values ",
                );
                let mut keys: Vec<&str> = Vec::with_capacity(batch.len());
                let mut nums: Vec<(i64, i64)> = Vec::with_capacity(batch.len());
                for (i, d) in batch.iter().enumerate() {
                    if i > 0 {
                        sql.push_str(", ");
                    }
                    let n = i * 4;
                    sql.push_str(&format!("(${}, ${}, ${}, ${})", n + 1, n + 2, n + 3, n + 4));
                    keys.push(d.key_id.as_ref());
                    nums.push((to_i64(d.requests), to_i64(d.egress_bytes)));
                }
                sql.push_str(
                    " on conflict (key_id, day) do update set \
                     requests = key_usage.requests + excluded.requests, \
                     egress_bytes = key_usage.egress_bytes + excluded.egress_bytes \
                     returning key_id, day, requests, egress_bytes",
                );
                let mut params: Vec<&(dyn ToSql + Sync)> = Vec::with_capacity(batch.len() * 4);
                for (i, d) in batch.iter().enumerate() {
                    params.push(&keys[i]);
                    params.push(&d.day);
                    params.push(&nums[i].0);
                    params.push(&nums[i].1);
                }
                self.run(|c| async move {
                    let rows = c
                        .query(sql.as_str(), &params)
                        .await
                        .map_err(|e| format!("flush: {e}"))?;
                    rows.iter().map(row_total).collect()
                })
                .await
            })
        }

        fn page<'a>(
            &'a self,
            day: NaiveDate,
            after: Option<&'a str>,
            limit: usize,
        ) -> StoreFuture<'a, Vec<Total>> {
            Box::pin(async move {
                let after = after.unwrap_or("");
                let limit = i64::try_from(limit).unwrap_or(1000);
                self.run(|c| async move {
                    let rows = c
                        .query(
                            "select key_id, day, requests, egress_bytes from key_usage \
                             where day = $1 and key_id > $2 order by key_id limit $3",
                            &[&day, &after, &limit],
                        )
                        .await
                        .map_err(|e| format!("usage page: {e}"))?;
                    rows.iter().map(row_total).collect()
                })
                .await
            })
        }

        fn cleanup(&self, before: NaiveDate) -> StoreFuture<'_, u64> {
            Box::pin(async move {
                self.run(|c| async move {
                    c.execute("delete from key_usage where day < $1", &[&before])
                        .await
                        .map_err(|e| format!("cleanup: {e}"))
                })
                .await
            })
        }

        fn ping(&self) -> StoreFuture<'_, ()> {
            Box::pin(async move {
                self.run(|c| async move {
                    c.simple_query("select 1")
                        .await
                        .map(|_| ())
                        .map_err(|e| format!("ping: {e}"))
                })
                .await
            })
        }
    }
}
