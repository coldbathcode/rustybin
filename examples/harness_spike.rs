//! Harness mode, phase 0 spike (docs/plans/harness.md, section 15): how much
//! load can one Rustybin process generate against itself?
//!
//! An in-process Rustybin serves on its own tokio runtime, and a hyper client
//! on a second runtime sends an open-model load: requests leave at their
//! intended times, not when the previous answer allows. Latency is measured
//! from the intended send time (coordinated omission corrected) and from the
//! actual send time. When the in-flight cap is reached a request is counted as
//! `not_sent` instead of being delayed, and the run is flagged as saturated,
//! so a slow client is never read as a slow server.
//!
//! It only ever targets the server it starts: load generation against other
//! hosts waits for the harness safety module (phase 1).
//!
//! ```bash
//! cargo run --release --example harness_spike -- --rate 10000 --duration 5
//! cargo run --release --example harness_spike -- --sweep --client-threads 2 --server-threads 2
//! ```

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hdrhistogram::Histogram;
use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;

/// One load run against `target` + `path`.
#[derive(Clone, Debug)]
struct RunConfig {
    target: String,
    path: String,
    rate: u64,
    duration: Duration,
    max_inflight: usize,
    timeout: Duration,
}

/// Microsecond histograms, 1 us to 60 s, 3 significant digits.
fn histogram() -> Histogram<u64> {
    Histogram::new_with_bounds(1, 60_000_000, 3).expect("histogram bounds")
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX).max(1)
}

#[derive(Debug)]
struct Report {
    rate: u64,
    ok: u64,
    errors: u64,
    not_sent: u64,
    /// From the first intended send to the last answer.
    elapsed: Duration,
    /// Latency from the intended send time (what a user would have seen).
    corrected: Histogram<u64>,
    /// Latency from the actual send time.
    service: Histogram<u64>,
    /// How late the scheduler dispatched each request.
    lag: Histogram<u64>,
    /// Process CPU seconds (server and client together), Linux only.
    cpu_secs: Option<f64>,
}

impl Report {
    fn answered(&self) -> u64 {
        self.ok + self.errors
    }

    /// Answers per second. The last requests finish one latency after the
    /// send window, so the median latency is taken off the elapsed time
    /// (otherwise a 100 ms upstream reads as 3 percent short over 3 s).
    fn achieved(&self) -> f64 {
        let tail = Duration::from_micros(self.corrected.value_at_quantile(0.5));
        let window = self.elapsed.saturating_sub(tail).as_secs_f64().max(1e-9);
        self.answered() as f64 / window
    }

    /// Why this run does not count as sustained, if it does not.
    fn saturated(&self) -> Option<&'static str> {
        if self.not_sent > 0 {
            Some("in-flight cap reached")
        } else if self.errors > 0 {
            Some("errors")
        } else if self.lag.value_at_quantile(0.99) > 5_000 {
            Some("scheduler lag p99 over 5 ms")
        } else if self.achieved() < self.rate as f64 * 0.98 {
            Some("achieved rate under 98 percent")
        } else {
            None
        }
    }
}

struct Shared {
    inflight: AtomicUsize,
    ok: AtomicU64,
    errors: AtomicU64,
    /// (corrected, service). A lock per answer is fine for a spike; the
    /// engine keeps one histogram per worker and merges them.
    hist: Mutex<(Histogram<u64>, Histogram<u64>)>,
}

type HttpClient = Client<HttpConnector, Empty<Bytes>>;

/// One client for every run, so its connection pool stays warm (a fresh pool
/// per run showed up as p99 spikes from connection setup at each step).
fn client(max_idle: usize) -> HttpClient {
    let mut connector = HttpConnector::new();
    connector.set_nodelay(true);
    Client::builder(TokioExecutor::new())
        .pool_max_idle_per_host(max_idle)
        .pool_idle_timeout(Duration::from_secs(30))
        .build(connector)
}

async fn request(
    client: HttpClient,
    uri: hyper::Uri,
    intended: Instant,
    timeout: Duration,
    shared: Arc<Shared>,
) {
    let sent = Instant::now();
    let outcome = tokio::time::timeout(timeout, async {
        let resp = client.get(uri).await.map_err(|e| e.to_string())?;
        let ok = resp.status().is_success();
        resp.into_body()
            .collect()
            .await
            .map_err(|e| e.to_string())?;
        Ok::<bool, String>(ok)
    })
    .await;
    let done = Instant::now();
    if matches!(outcome, Ok(Ok(true))) {
        shared.ok.fetch_add(1, Ordering::Relaxed);
    } else {
        shared.errors.fetch_add(1, Ordering::Relaxed);
    }
    {
        let mut h = shared.hist.lock().unwrap_or_else(|p| p.into_inner());
        h.0.saturating_record(micros(done - intended));
        h.1.saturating_record(micros(done - sent));
    }
    shared.inflight.fetch_sub(1, Ordering::Relaxed);
}

/// Process CPU time from /proc/self/stat (utime + stime, 100 ticks per second).
fn cpu_seconds() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let after = stat.rsplit_once(") ")?.1;
    let fields: Vec<&str> = after.split_whitespace().collect();
    let utime: f64 = fields.get(11)?.parse().ok()?;
    let stime: f64 = fields.get(12)?.parse().ok()?;
    Some((utime + stime) / 100.0)
}

/// Send `rate` requests per second for `duration`. The scheduler runs on the
/// calling thread: `std::thread::sleep` is finer than tokio's 1 ms timer, and
/// a dedicated thread is never queued behind the request tasks, which run on
/// `workers`. (Sharing the workers' runtime gave 10 ms of dispatch lag at
/// 5,000 rps on four cores.)
fn run(cfg: RunConfig, client: &HttpClient, workers: &tokio::runtime::Handle) -> Report {
    let _enter = workers.enter();
    let uri: hyper::Uri = format!("{}{}", cfg.target, cfg.path)
        .parse()
        .expect("target URI");
    let shared = Arc::new(Shared {
        inflight: AtomicUsize::new(0),
        ok: AtomicU64::new(0),
        errors: AtomicU64::new(0),
        hist: Mutex::new((histogram(), histogram())),
    });
    let total = (cfg.rate as f64 * cfg.duration.as_secs_f64()).round() as u64;
    let interval_ns = 1e9 / cfg.rate.max(1) as f64;
    let cpu_before = cpu_seconds();
    let start = Instant::now() + Duration::from_millis(20);
    let at = |i: u64| start + Duration::from_nanos((i as f64 * interval_ns) as u64);
    let mut lag = histogram();
    let mut not_sent = 0u64;
    let mut i = 0u64;
    while i < total {
        let next = at(i);
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
        // Everything due by now goes out.
        let now = Instant::now();
        while i < total && at(i) <= now {
            let intended = at(i);
            lag.saturating_record(micros(now - intended));
            if shared.inflight.load(Ordering::Relaxed) >= cfg.max_inflight {
                not_sent += 1;
            } else {
                shared.inflight.fetch_add(1, Ordering::Relaxed);
                workers.spawn(request(
                    client.clone(),
                    uri.clone(),
                    intended,
                    cfg.timeout,
                    shared.clone(),
                ));
            }
            i += 1;
        }
    }
    let drain_until = Instant::now() + cfg.timeout + Duration::from_secs(1);
    while shared.inflight.load(Ordering::Relaxed) > 0 && Instant::now() < drain_until {
        std::thread::sleep(Duration::from_millis(1));
    }
    let elapsed = start.elapsed();
    let cpu_secs = cpu_before.zip(cpu_seconds()).map(|(a, b)| b - a);
    let (corrected, service) = {
        let h = shared.hist.lock().unwrap_or_else(|p| p.into_inner());
        (h.0.clone(), h.1.clone())
    };
    Report {
        rate: cfg.rate,
        ok: shared.ok.load(Ordering::Relaxed),
        errors: shared.errors.load(Ordering::Relaxed),
        not_sent,
        elapsed,
        corrected,
        service,
        lag,
        cpu_secs,
    }
}

// ── Command line ────────────────────────────────────────────────────

struct Args {
    rate: u64,
    duration: f64,
    step: f64,
    client_threads: usize,
    server_threads: usize,
    inflight: usize,
    path: String,
    sweep: bool,
    /// `rustybin` (default) or `bare`: a minimal in-process axum server, to
    /// tell the client's cost from Rustybin's.
    upstream: String,
    /// `bare` only: answer after this many milliseconds (many requests in flight).
    delay_ms: u64,
}

fn parse_args() -> Args {
    let mut a = Args {
        rate: 5_000,
        duration: 5.0,
        step: 3.0,
        client_threads: 2,
        server_threads: 2,
        inflight: 10_000,
        path: "/status/200".into(),
        sweep: false,
        upstream: "rustybin".into(),
        delay_ms: 0,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--rate" => a.rate = value().parse().expect("--rate"),
            "--duration" => a.duration = value().parse().expect("--duration"),
            "--step" => a.step = value().parse().expect("--step"),
            "--client-threads" => a.client_threads = value().parse().expect("--client-threads"),
            "--server-threads" => a.server_threads = value().parse().expect("--server-threads"),
            "--inflight" => a.inflight = value().parse().expect("--inflight"),
            "--path" => a.path = value(),
            "--sweep" => a.sweep = true,
            "--upstream" => a.upstream = value(),
            "--delay-ms" => a.delay_ms = value().parse().expect("--delay-ms"),
            other => panic!("unknown flag {other} (see the module docs)"),
        }
    }
    a
}

fn ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

fn header() {
    println!(
        "| target rps | achieved | not_sent | errors | lag p99 ms | p50 ms | p99 ms | p99.9 ms | max ms | service p99 ms | CPU s | req per CPU s | sustained |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|");
}

fn row(r: &Report) {
    let cpu = r.cpu_secs.map_or("n/a".into(), |c| format!("{c:.2}"));
    let per_cpu = r
        .cpu_secs
        .filter(|c| *c > 0.0)
        .map_or("n/a".into(), |c| format!("{:.0}", r.answered() as f64 / c));
    println!(
        "| {} | {:.0} | {} | {} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {} | {} | {} |",
        r.rate,
        r.achieved(),
        r.not_sent,
        r.errors,
        ms(r.lag.value_at_quantile(0.99)),
        ms(r.corrected.value_at_quantile(0.50)),
        ms(r.corrected.value_at_quantile(0.99)),
        ms(r.corrected.value_at_quantile(0.999)),
        ms(r.corrected.max()),
        ms(r.service.value_at_quantile(0.99)),
        cpu,
        per_cpu,
        r.saturated()
            .map_or("yes".to_string(), |why| format!("no ({why})")),
    );
}

fn main() {
    let args = parse_args();
    let server_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(args.server_threads)
        .thread_name("server")
        .enable_all()
        .build()
        .expect("server runtime");
    // Kept until the end of main: dropping it stops the server.
    let mut server = None;
    let target = match args.upstream.as_str() {
        "bare" => server_rt.block_on(bare_upstream(Duration::from_millis(args.delay_ms))),
        "rustybin" => {
            let config = rustybin::config::Config::for_tests();
            let s = server_rt
                .block_on(rustybin::server::start(config))
                .expect("start rustybin");
            let target = format!("http://{}", s.http_addr);
            server = Some(s);
            target
        }
        other => panic!("unknown --upstream {other} (rustybin or bare)"),
    };
    let client_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(args.client_threads)
        .thread_name("harness-client")
        .enable_all()
        .build()
        .expect("client runtime");
    let workers = client_rt.handle();
    let http = {
        let _enter = workers.enter();
        client(args.inflight)
    };
    println!(
        "{} at {target}{} | {} cores | server threads {} | client threads {} (+1 scheduler) | in-flight cap {}",
        args.upstream,
        args.path,
        std::thread::available_parallelism().map_or(0, |n| n.get()),
        args.server_threads,
        args.client_threads,
        args.inflight,
    );
    let cfg = |rate: u64, secs: f64| RunConfig {
        target: target.clone(),
        path: args.path.clone(),
        rate,
        duration: Duration::from_secs_f64(secs),
        max_inflight: args.inflight,
        timeout: Duration::from_secs(10),
    };
    // Warm up connections and caches.
    run(cfg(500, 1.0), &http, workers);
    header();
    if args.sweep {
        let mut last_good = None;
        for rate in [
            1_000u64, 2_000, 5_000, 10_000, 15_000, 20_000, 25_000, 30_000, 40_000, 50_000, 75_000,
            100_000,
        ] {
            let r = run(cfg(rate, args.step), &http, workers);
            row(&r);
            if r.saturated().is_some() {
                break;
            }
            last_good = Some(rate);
        }
        match last_good {
            Some(rate) => println!("\nlast sustained rate: {rate} rps"),
            None => println!("\nno rate was sustained"),
        }
    } else {
        row(&run(cfg(args.rate, args.duration), &http, workers));
    }
    drop(server);
}

/// A minimal upstream: every path answers `ok` after `delay`.
async fn bare_upstream(delay: Duration) -> String {
    let app = axum::Router::new().fallback(move || async move {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        "ok"
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(target: String, rate: u64, millis: u64, max_inflight: usize) -> RunConfig {
        RunConfig {
            target,
            path: "/slow".into(),
            rate,
            duration: Duration::from_millis(millis),
            max_inflight,
            timeout: Duration::from_secs(5),
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime")
    }

    #[test]
    fn the_in_flight_cap_is_reported_as_saturation() {
        // 400 rps against 5 slots of 50 ms each: about 100 rps of capacity.
        let rt = runtime();
        let target = rt.block_on(bare_upstream(Duration::from_millis(50)));
        let r = run(cfg(target, 400, 500, 5), &client(5), rt.handle());
        assert!(r.not_sent > 0, "{r:?}");
        assert_eq!(r.saturated(), Some("in-flight cap reached"));
        assert_eq!(r.errors, 0);
        assert_eq!(r.ok + r.not_sent, 200);
    }

    #[test]
    fn an_unsaturated_run_sends_everything_on_schedule() {
        let rt = runtime();
        let target = rt.block_on(bare_upstream(Duration::ZERO));
        let r = run(cfg(target, 200, 300, 1_000), &client(1_000), rt.handle());
        assert_eq!((r.ok, r.errors, r.not_sent), (60, 0, 0), "{r:?}");
        // Measured from the intended send, latency never undercuts service time.
        assert!(r.corrected.max() >= r.service.max());
    }
}
