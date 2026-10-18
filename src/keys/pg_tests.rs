//! Quota store tests with the `quota-postgres` feature.
//!
//! The tests that need a running PostgreSQL only run when
//! `RUSTYBIN_TEST_QUOTA_STORE` is set to a URL, for example:
//!
//! ```sh
//! docker run -d --rm --name rustybin-pg -e POSTGRES_PASSWORD=dev -p 127.0.0.1:25433:5432 postgres:16-alpine
//! RUSTYBIN_TEST_QUOTA_STORE=postgres://postgres:dev@127.0.0.1:25433/postgres \
//!   cargo test --features quota-postgres keys::pg_tests
//! ```

use super::tests::{doc, entry, send, shared_config};
use super::*;

use std::time::Duration;

use crate::test_support::{body_json, test_app_with, test_state_with};

fn store_url() -> Option<String> {
    std::env::var("RUSTYBIN_TEST_QUOTA_STORE")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn unique(prefix: &str) -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}{}", &id[..12])
}

fn replica(url: &str, registry: &str, flush_ms: u64) -> AppState {
    let mut config = shared_config();
    config.quota_store = Some(url.to_string());
    config.quota_flush_ms = flush_ms;
    let state = test_state_with(config);
    state
        .keys
        .load_bytes(registry.as_bytes())
        .expect("registry");
    state
}

async fn wait_store_up(state: &AppState) {
    for _ in 0..250 {
        if state.metrics.quota_store_up() == Some(true) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the quota store did not come up");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_replicas_enforce_one_daily_quota_through_postgres() {
    let Some(url) = store_url() else {
        eprintln!("skipped: set RUSTYBIN_TEST_QUOTA_STORE to a PostgreSQL URL");
        return;
    };
    let id = unique("k");
    let key = format!("rbk_{id}_{}", "s".repeat(32));
    let limit = 6_000u64;
    let registry = doc(vec![entry(
        &id,
        &key,
        "starter",
        "active",
        json!({ "requests": limit, "rps": 0, "concurrency": 0 }),
    )]);
    // Flush every 5 ms; each replica sends about 2,000 requests a second.
    let replicas = [replica(&url, &registry, 5), replica(&url, &registry, 5)];
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let mut flushers = Vec::new();
    for state in &replicas {
        let mut rx = stop_rx.clone();
        flushers.push(tokio::spawn(state.keys.clone().flush_loop(async move {
            let _ = rx.wait_for(|v| *v).await;
        })));
    }
    for state in &replicas {
        wait_store_up(state).await;
    }

    let mut senders = Vec::new();
    for state in &replicas {
        let app = test_app_with(state.clone());
        let key = key.clone();
        senders.push(tokio::spawn(async move {
            let mut admitted = 0u64;
            loop {
                for _ in 0..10 {
                    let resp = send(&app, "GET", "/uuid", &[("x-rustybin-key", &key)]).await;
                    match resp.status() {
                        StatusCode::OK => admitted += 1,
                        StatusCode::TOO_MANY_REQUESTS => return admitted,
                        other => panic!("unexpected status {other}"),
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }));
    }
    let mut total = 0u64;
    for s in senders {
        total += s.await.expect("sender");
    }
    let _ = stop_tx.send(true);
    for f in flushers {
        f.await.expect("flusher");
    }
    eprintln!("two replicas admitted {total} requests for a daily quota of {limit}");
    assert!(total >= limit, "{total} < {limit}");
    // Each replica admits for one flush interval plus the store round trip
    // before it sees the other's counts; on a busy CI runner that round trip
    // is tens of milliseconds at about 2,000 requests a second.
    assert!(
        total as f64 <= limit as f64 * 1.05,
        "{total} is more than 5% over {limit}"
    );

    // The store holds the exact total of both replicas.
    let day = replicas[0].keys.today();
    let mut after: Option<String> = None;
    let mut found = None;
    loop {
        let (rows, next) = replicas[1]
            .keys
            .usage_page(day, after.as_deref(), MAX_USAGE_PAGE)
            .await
            .expect("page");
        if let Some(r) = rows.iter().find(|r| r.key_id == id) {
            found = Some(r.requests);
            break;
        }
        match next {
            Some(n) => after = Some(n),
            None => break,
        }
    }
    assert_eq!(found, Some(total));
}

#[tokio::test]
async fn usage_endpoint_pages_and_old_rows_are_deleted() {
    let Some(url) = store_url() else {
        eprintln!("skipped: set RUSTYBIN_TEST_QUOTA_STORE to a PostgreSQL URL");
        return;
    };
    let state = replica(&url, &doc(vec![]), 2_000);
    let store = state.keys.store.clone().expect("store configured");
    // A day of its own, far in the future, so other runs do not interfere.
    let offset = uuid::Uuid::new_v4().as_u128() % 20_000;
    let day = NaiveDate::from_ymd_opt(2100, 1, 1).expect("day")
        + chrono::Duration::days(i64::try_from(offset).unwrap_or(0));
    let deltas: Vec<ledger::Delta> = (1..=5)
        .map(|n| ledger::Delta {
            day,
            key_id: Arc::from(format!("p{n}").as_str()),
            requests: n,
            egress_bytes: n * 10,
        })
        .collect();
    let totals = store.flush(&deltas).await.expect("flush");
    assert_eq!(totals.len(), 5);
    // A second flush adds up and returns the new totals.
    let totals = store.flush(&deltas[..1]).await.expect("flush");
    assert_eq!(totals[0].requests, 2);
    assert_eq!(totals[0].egress_bytes, 20);

    let app = test_app_with(state.clone());
    let admin = [("authorization", "Bearer admin-secret")];
    let page = |query: String| {
        let app = app.clone();
        async move {
            let resp = send(&app, "GET", &format!("{KEYS_USAGE_PATH}?{query}"), &admin).await;
            assert_eq!(resp.status(), StatusCode::OK, "{query}");
            body_json(resp).await
        }
    };
    let v = page(format!("day={day}&limit=2")).await;
    assert_eq!(
        v,
        json!({
            "day": day.to_string(),
            "keys": [
                { "key_id": "p1", "requests": 2, "egress_bytes": 20 },
                { "key_id": "p2", "requests": 2, "egress_bytes": 20 },
            ],
            "next_after": "p2",
        })
    );
    let v = page(format!("day={day}&limit=2&after=p2")).await;
    assert_eq!(v["keys"][0]["key_id"], "p3");
    assert_eq!(v["next_after"], "p4");
    let v = page(format!("day={day}&limit=2&after=p4")).await;
    assert_eq!(
        v["keys"],
        json!([{ "key_id": "p5", "requests": 5, "egress_bytes": 50 }])
    );
    assert_eq!(v["next_after"], Value::Null);

    // Cleanup deletes rows before the given day.
    let old_day = state.keys.today() - chrono::Duration::days(store::RETENTION_DAYS + 5);
    let old_key = unique("old");
    store
        .flush(&[ledger::Delta {
            day: old_day,
            key_id: Arc::from(old_key.as_str()),
            requests: 1,
            egress_bytes: 1,
        }])
        .await
        .expect("flush");
    let before = state.keys.today() - chrono::Duration::days(store::RETENTION_DAYS);
    // Not counted: the flush loops of a concurrent test run the same
    // cleanup and may delete the row first.
    store.cleanup(before).await.expect("cleanup");
    let rows = store.page(old_day, None, 1000).await.expect("page");
    assert!(rows.iter().all(|r| r.key_id != old_key));
}

/// An unreachable store: requests keep working on local counters, the
/// metric says the store is down. Needs no running PostgreSQL.
#[tokio::test]
async fn unreachable_postgres_never_fails_requests() {
    let registry = doc(vec![entry(
        "ka1",
        super::tests::KEY_A,
        "starter",
        "active",
        json!({ "requests": 2 }),
    )]);
    let state = replica("postgres://postgres:dev@127.0.0.1:1/none", &registry, 2_000);
    assert_eq!(state.keys.store_kind(), "postgres");
    let app = test_app_with(state.clone());
    for _ in 0..2 {
        let resp = send(
            &app,
            "GET",
            "/uuid",
            &[("x-rustybin-key", super::tests::KEY_A)],
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }
    // The local counters still enforce the quota.
    let resp = send(
        &app,
        "GET",
        "/uuid",
        &[("x-rustybin-key", super::tests::KEY_A)],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(state.keys.flush_now().await.is_err());
    assert_eq!(state.metrics.quota_store_up(), Some(false));
    assert!(state.metrics.render().contains("rustybin_quota_store_up 0"));
}
