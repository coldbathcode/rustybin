//! Batch APIs: OpenAI-shaped files and batches (`/ai/openai/v1/files*`,
//! `/ai/openai/v1/batches*`) and Anthropic-shaped message batches
//! (`/ai/anthropic/v1/messages/batches*`).
//!
//! A batch's answers are the engine's, one per line, made when the batch
//! is created and deterministic for the same input (ids included), so a
//! gateway's caching of batch results can be shown. What a client sees of
//! it follows a compressed clock: `X-Rustybin-Batch-Seconds` (default the
//! `ai.batch.seconds` setting, 10) is the whole life of the batch, through
//! the provider's states, on the injectable [`Clock`]. `X-Rustybin-Batch-
//! Fail-Rate` makes chosen lines fail with the provider's error
//! (`X-Rustybin-Batch-Fail` picks the kind), `X-Rustybin-Batch-Outcome:
//! expired` lets the batch run out of time half way.
//!
//! Bounded: per caller (session, and API key in shared mode)
//! `ai.batch.max_files` files (20, 8 MiB each and never more than the
//! instance's body limit) and `ai.batch.max_batches` batches (10), kept an
//! hour; callers and stored bytes are capped for the instance.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::Path;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::{json, Value};

use super::engine::stable_hash;
use super::faults::{ErrorKind, Fault};
use super::{anthropic, openai, AiCtx, DetailArgs};
use crate::catalog::{category, Endpoint, Example};
use crate::limits::Clock;
use crate::state::AppState;

pub const SECONDS_HEADER: &str = "x-rustybin-batch-seconds";
pub const FAIL_RATE_HEADER: &str = "x-rustybin-batch-fail-rate";
pub const FAIL_HEADER: &str = "x-rustybin-batch-fail";
pub const OUTCOME_HEADER: &str = "x-rustybin-batch-outcome";

pub const MAX_FILES_KEY: &str = "ai.batch.max_files";
pub const MAX_BATCHES_KEY: &str = "ai.batch.max_batches";
pub const SECONDS_KEY: &str = "ai.batch.seconds";
pub const DEFAULT_MAX_FILES: usize = 20;
pub const DEFAULT_MAX_BATCHES: usize = 10;
pub const DEFAULT_SECONDS: usize = 10;
/// The largest file.
pub const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
/// What a caller's files and batches are kept for.
pub const TTL: Duration = Duration::from_secs(3600);
/// The longest a batch's compressed life may be.
const MAX_SECONDS: f64 = 3600.0;
/// The supported endpoint of an OpenAI-shaped batch.
const CHAT_ENDPOINT: &str = "/v1/chat/completions";

/// One stored file.
struct File {
    filename: String,
    purpose: String,
    bytes: Bytes,
    created_at: i64,
}

/// One request of a batch, answered when the batch was created.
struct Line {
    custom_id: String,
    ok: bool,
    /// The provider's result line for it.
    result: Value,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    OpenAi,
    Anthropic,
}

struct Batch {
    shape: Shape,
    /// On the monotonic clock.
    created: Duration,
    created_at: i64,
    /// The whole life of the batch, in seconds.
    total: f64,
    input_file_id: Option<String>,
    metadata: Value,
    lines: Vec<Line>,
    /// Why validation failed (the provider's error objects): the batch
    /// ends `failed`.
    invalid: Vec<Value>,
    cancelled: Option<Duration>,
    expires: bool,
}

/// Where a batch is on its clock.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Validating,
    InProgress,
    Finalizing,
    Completed,
    Failed,
    Expired,
    Cancelling,
    Cancelled,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Phase::Validating => "validating",
            Phase::InProgress => "in_progress",
            Phase::Finalizing => "finalizing",
            Phase::Completed => "completed",
            Phase::Failed => "failed",
            Phase::Expired => "expired",
            Phase::Cancelling => "cancelling",
            Phase::Cancelled => "cancelled",
        }
    }

    fn ended(self) -> bool {
        matches!(
            self,
            Phase::Completed | Phase::Failed | Phase::Expired | Phase::Cancelled
        )
    }
}

impl Batch {
    /// Validation ends at a tenth of the life, the work at eight tenths.
    fn marks(&self) -> (f64, f64) {
        (self.total * 0.1, self.total * 0.8)
    }

    /// Lines done at `t` seconds: evenly through the in-progress phase.
    fn done_at(&self, t: f64) -> usize {
        let (v, p) = self.marks();
        let share = if p <= v {
            1.0
        } else {
            ((t - v) / (p - v)).clamp(0.0, 1.0)
        };
        ((self.lines.len() as f64) * share).floor() as usize
    }

    /// The phase and the lines done, at `now` on the monotonic clock.
    fn at(&self, now: Duration) -> (Phase, usize) {
        let t = now.saturating_sub(self.created).as_secs_f64();
        let (v, p) = self.marks();
        if !self.invalid.is_empty() {
            return if t < v {
                (Phase::Validating, 0)
            } else {
                (Phase::Failed, 0)
            };
        }
        if let Some(c) = self.cancelled {
            let tc = c.saturating_sub(self.created).as_secs_f64();
            let done = self.done_at(tc);
            return if t < tc + self.total * 0.1 {
                (Phase::Cancelling, done)
            } else {
                (Phase::Cancelled, done)
            };
        }
        if t < v {
            return (Phase::Validating, 0);
        }
        if self.expires {
            // Out of time half way through the work.
            let half = self.lines.len() / 2;
            return if t < self.total {
                (Phase::InProgress, self.done_at(t).min(half))
            } else {
                (Phase::Expired, half)
            };
        }
        if t < p {
            (Phase::InProgress, self.done_at(t))
        } else if t < self.total {
            (Phase::Finalizing, self.lines.len())
        } else {
            (Phase::Completed, self.lines.len())
        }
    }

    /// Unix seconds of `offset` seconds into the batch.
    fn stamp(&self, offset: f64) -> i64 {
        self.created_at + offset.ceil() as i64
    }
}

#[derive(Default)]
struct Owner {
    files: BTreeMap<String, File>,
    batches: BTreeMap<String, Batch>,
    touched: Duration,
    bytes: usize,
}

/// Every caller's files and batches.
pub struct Store {
    clock: Arc<dyn Clock>,
    owners: Mutex<HashMap<String, Owner>>,
    max_owners: usize,
    max_bytes: usize,
}

impl Store {
    pub fn new(clock: Arc<dyn Clock>, public: bool) -> Self {
        Self {
            clock,
            owners: Mutex::new(HashMap::new()),
            max_owners: if public { 64 } else { 256 },
            max_bytes: if public {
                32 * 1024 * 1024
            } else {
                256 * 1024 * 1024
            },
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Owner>> {
        self.owners.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Callers with files or batches now (for tests and bounds).
    pub fn owners(&self) -> usize {
        self.lock().len()
    }

    /// Run `f` on the caller's files and batches. Callers idle for
    /// [`TTL`] are forgotten first; `None` when the instance holds as many
    /// callers as it keeps.
    fn with<T>(&self, owner: &str, f: impl FnOnce(&mut Owner, Duration, usize) -> T) -> Option<T> {
        let now = self.clock.mono();
        let mut owners = self.lock();
        owners.retain(|_, o| now.saturating_sub(o.touched) < TTL);
        if !owners.contains_key(owner) && owners.len() >= self.max_owners {
            return None;
        }
        let others: usize = owners
            .iter()
            .filter(|(k, _)| k.as_str() != owner)
            .map(|(_, o)| o.bytes)
            .sum();
        let room = self.max_bytes.saturating_sub(others);
        let entry = owners.entry(owner.to_string()).or_default();
        entry.touched = now;
        let out = f(entry, now, room);
        if entry.files.is_empty() && entry.batches.is_empty() {
            owners.remove(owner);
        }
        Some(out)
    }
}

fn owner_key(ctx: &AiCtx) -> String {
    format!("{}|{}", ctx.key_id.as_deref().unwrap_or(""), ctx.session)
}

fn hex(bytes: &[u8], len: usize) -> String {
    let mut s: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    s.truncate(len);
    s
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok()).map(str::trim)
}

/// What the headers ask of a new batch.
struct Asked {
    total: f64,
    fail_pct: u8,
    fail_kind: ErrorKind,
    expires: bool,
}

fn asked(ctx: &AiCtx, h: &HeaderMap) -> Asked {
    let default = super::setting_usize(&ctx.shared, SECONDS_KEY, DEFAULT_SECONDS) as f64;
    Asked {
        total: header(h, SECONDS_HEADER)
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(default)
            .min(MAX_SECONDS),
        fail_pct: header(h, FAIL_RATE_HEADER)
            .and_then(super::faults::parse_rate)
            .unwrap_or(0),
        fail_kind: match header(h, FAIL_HEADER).and_then(super::faults::parse_kind) {
            Some(Fault::Error(k)) => k,
            _ => ErrorKind::Server,
        },
        expires: header(h, OUTCOME_HEADER).is_some_and(|o| o.eq_ignore_ascii_case("expired")),
    }
}

/// Does this line fail? Chosen by its id, so the same input fails the
/// same lines.
fn fails(custom_id: &str, pct: u8) -> bool {
    pct >= 100 || (pct > 0 && stable_hash(custom_id)[0] % 100 < pct)
}

/// The status and the provider's error body of a line that fails.
fn line_error(shape: Shape, kind: &ErrorKind) -> (u16, Value) {
    let (status, openai_type, anthropic_type, message) = match kind {
        ErrorKind::RateLimit => (
            429,
            "requests",
            "rate_limit_error",
            "Rate limit reached for requests.",
        ),
        ErrorKind::Overloaded => (529, "server_error", "overloaded_error", "Overloaded"),
        ErrorKind::Unavailable => (
            503,
            "server_error",
            "api_error",
            "The service is temporarily unavailable.",
        ),
        ErrorKind::Timeout => (504, "server_error", "api_error", "The request timed out."),
        _ => (
            500,
            "server_error",
            "api_error",
            "The server had an error while processing your request.",
        ),
    };
    match shape {
        Shape::OpenAi => (
            if status == 529 { 503 } else { status },
            json!({ "error": { "message": message, "type": openai_type, "param": null, "code": null } }),
        ),
        Shape::Anthropic => (
            status,
            json!({ "type": "error", "error": { "type": anthropic_type, "message": message } }),
        ),
    }
}

/// The totals a batch adds to the request log.
#[derive(Default)]
struct Totals {
    prompt: u32,
    completion: u32,
    logged: Vec<Value>,
    model: String,
    mode: String,
}

fn record(
    ctx: &AiCtx,
    batch_id: &str,
    endpoint: &str,
    total: usize,
    failed: usize,
    invalid: &[Value],
    t: Totals,
) {
    ctx.record_detail(DetailArgs {
        kind: "batch",
        model: &t.model,
        mode: &t.mode,
        prompt: "",
        prompt_tokens: t.prompt,
        completion_tokens: t.completion,
        new_prompt_tokens: t.prompt,
        new_completion_tokens: t.completion,
        first: true,
        finish: if invalid.is_empty() {
            "batch"
        } else {
            "failed"
        },
        reply: &format!("{total} line(s), {failed} failing"),
        detail: json!({
            "batch_id": batch_id,
            "endpoint": endpoint,
            "total": total,
            "failing": failed,
            "lines": t.logged,
            "validation_errors": invalid,
        }),
    });
}

// ── OpenAI: files ───────────────────────────────────────────────────

fn file_json(id: &str, f: &File) -> Value {
    json!({
        "id": id, "object": "file", "bytes": f.bytes.len(), "created_at": f.created_at,
        "filename": f.filename, "purpose": f.purpose, "status": "processed", "status_details": null,
    })
}

fn full(ctx: &AiCtx) -> Response {
    ctx.error(
        ErrorKind::RateLimit,
        "This instance holds files and batches for as many callers as it keeps: try again later.",
    )
}

#[allow(clippy::result_large_err)]
async fn create_file(
    Extension(ctx): Extension<AiCtx>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let content_type = header(&headers, "content-type").unwrap_or_default();
    let Some(fields) = openai::parse_multipart(content_type, &body) else {
        return ctx.error(
            ErrorKind::BadRequest,
            "Expected multipart/form-data with 'purpose' and 'file'.",
        );
    };
    let purpose = fields
        .iter()
        .find(|f| f.name == "purpose")
        .map(|f| String::from_utf8_lossy(&f.data).trim().to_string())
        .unwrap_or_default();
    if purpose != "batch" {
        return ctx.error(ErrorKind::BadRequest, format!("Invalid value for 'purpose': '{purpose}'. This mock stores files of purpose 'batch'."));
    }
    let Some(file) = fields.into_iter().find(|f| f.name == "file") else {
        return ctx.error(ErrorKind::BadRequest, "Missing required parameter: 'file'.");
    };
    if file.data.len() > MAX_FILE_BYTES {
        return ctx.error(
            ErrorKind::TooLarge,
            format!(
                "The file is larger than {} MiB.",
                MAX_FILE_BYTES / (1024 * 1024)
            ),
        );
    }
    let max = super::setting_usize(&ctx.shared, MAX_FILES_KEY, DEFAULT_MAX_FILES);
    let id = format!("file-{}", super::rand_id(24));
    let now = super::now_secs();
    let stored = ctx.shared.batches.with(&owner_key(&ctx), |o, _, room| {
        if o.files.len() >= max {
            return Err(ctx.error(ErrorKind::RateLimit, format!("You have {max} files, the most this instance keeps for a caller: delete one first.")));
        }
        if o.bytes + file.data.len() > room {
            return Err(ctx.error(ErrorKind::TooLarge, "This instance has no room for the file: delete files, or try again later."));
        }
        let f = File {
            filename: file.filename.unwrap_or_else(|| "batch.jsonl".into()).chars().take(200).collect(),
            purpose: purpose.clone(),
            bytes: Bytes::from(file.data),
            created_at: now,
        };
        o.bytes += f.bytes.len();
        let v = file_json(&id, &f);
        o.files.insert(id.clone(), f);
        Ok(v)
    });
    match stored {
        Some(Ok(v)) => Json(v).into_response(),
        Some(Err(r)) => r,
        None => full(&ctx),
    }
}

fn no_file(ctx: &AiCtx, id: &str) -> Response {
    let mut r = ctx.error(ErrorKind::BadRequest, format!("No such File object: {id}"));
    *r.status_mut() = StatusCode::NOT_FOUND;
    r
}

async fn list_files(Extension(ctx): Extension<AiCtx>) -> Response {
    let data = ctx
        .shared
        .batches
        .with(&owner_key(&ctx), |o, _, _| {
            o.files
                .iter()
                .map(|(id, f)| file_json(id, f))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Json(json!({ "object": "list", "data": data, "has_more": false })).into_response()
}

/// The content of an output or error file of a batch that reached it.
fn virtual_file(o: &Owner, id: &str, now: Duration) -> Option<(String, i64)> {
    for (bid, b) in &o.batches {
        if b.shape != Shape::OpenAi {
            continue;
        }
        let (phase, done) = b.at(now);
        if !matches!(phase, Phase::Completed | Phase::Cancelled | Phase::Expired) {
            continue;
        }
        for (ok, suffix) in [(true, "out"), (false, "err")] {
            if id == format!("file-{}{suffix}", &bid["batch_".len()..]) {
                let text: String = b.lines[..done]
                    .iter()
                    .filter(|l| l.ok == ok)
                    .map(|l| format!("{}\n", l.result))
                    .collect();
                return Some((text, b.stamp(b.total)));
            }
        }
    }
    None
}

async fn get_file(Extension(ctx): Extension<AiCtx>, Path(id): Path<String>) -> Response {
    let found = ctx.shared.batches.with(&owner_key(&ctx), |o, now, _| {
        o.files.get(&id).map(|f| file_json(&id, f)).or_else(|| {
            virtual_file(o, &id, now).map(|(text, at)| {
                json!({ "id": id, "object": "file", "bytes": text.len(), "created_at": at, "filename": format!("{id}.jsonl"), "purpose": "batch_output", "status": "processed", "status_details": null })
            })
        })
    });
    match found.flatten() {
        Some(v) => Json(v).into_response(),
        None => no_file(&ctx, &id),
    }
}

async fn file_content(Extension(ctx): Extension<AiCtx>, Path(id): Path<String>) -> Response {
    let found = ctx.shared.batches.with(&owner_key(&ctx), |o, now, _| {
        o.files
            .get(&id)
            .map(|f| f.bytes.clone())
            .or_else(|| virtual_file(o, &id, now).map(|(text, _)| Bytes::from(text)))
    });
    match found.flatten() {
        Some(bytes) => ([(header::CONTENT_TYPE, "application/jsonl")], bytes).into_response(),
        None => no_file(&ctx, &id),
    }
}

async fn delete_file(Extension(ctx): Extension<AiCtx>, Path(id): Path<String>) -> Response {
    let gone = ctx.shared.batches.with(&owner_key(&ctx), |o, _, _| {
        o.files
            .remove(&id)
            .map(|f| o.bytes = o.bytes.saturating_sub(f.bytes.len()))
    });
    match gone.flatten() {
        Some(()) => Json(json!({ "id": id, "object": "file", "deleted": true })).into_response(),
        None => no_file(&ctx, &id),
    }
}

// ── OpenAI: batches ─────────────────────────────────────────────────

fn batch_json(id: &str, b: &Batch, now: Duration) -> Value {
    let (phase, done) = b.at(now);
    let (v, p) = b.marks();
    let reached = |offset: f64| {
        let t = now.saturating_sub(b.created).as_secs_f64();
        (t >= offset).then(|| b.stamp(offset))
    };
    let failed = b.lines[..done].iter().filter(|l| !l.ok).count();
    let suffix = &id["batch_".len()..];
    let has_output = matches!(phase, Phase::Completed | Phase::Cancelled | Phase::Expired);
    let cancel = b
        .cancelled
        .map(|c| c.saturating_sub(b.created).as_secs_f64());
    json!({
        "id": id,
        "object": "batch",
        "endpoint": CHAT_ENDPOINT,
        "errors": if b.invalid.is_empty() || phase == Phase::Validating { Value::Null } else { json!({ "object": "list", "data": b.invalid }) },
        "input_file_id": b.input_file_id,
        "completion_window": "24h",
        "status": phase.as_str(),
        "output_file_id": (has_output && done > failed).then(|| format!("file-{suffix}out")),
        "error_file_id": (has_output && failed > 0).then(|| format!("file-{suffix}err")),
        "created_at": b.created_at,
        "in_progress_at": if b.invalid.is_empty() { reached(v) } else { None },
        "expires_at": b.created_at + 24 * 3600,
        "finalizing_at": if b.invalid.is_empty() && cancel.is_none() && !b.expires { reached(p) } else { None },
        "completed_at": (phase == Phase::Completed).then(|| b.stamp(b.total)),
        "failed_at": (phase == Phase::Failed).then(|| b.stamp(v)),
        "expired_at": (phase == Phase::Expired).then(|| b.stamp(b.total)),
        "cancelling_at": cancel.map(|c| b.stamp(c)),
        "cancelled_at": match (phase, cancel) { (Phase::Cancelled, Some(c)) => Some(b.stamp(c + b.total * 0.1)), _ => None },
        "request_counts": { "total": b.lines.len(), "completed": done - failed, "failed": failed },
        "metadata": b.metadata,
    })
}

fn max_lines(ctx: &AiCtx) -> usize {
    if ctx.shared.public() {
        100
    } else {
        1000
    }
}

/// The lines of an OpenAI batch file, each answered; what is wrong with
/// the file as the provider's validation errors.
fn openai_lines(
    ctx: &AiCtx,
    text: &str,
    created_at: i64,
    asked: &Asked,
    totals: &mut Totals,
) -> (Vec<Line>, Vec<Value>) {
    let mut lines = Vec::new();
    let mut invalid = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut problem = |n: usize, code: &str, message: String| {
        if invalid.len() < 20 {
            invalid.push(json!({ "code": code, "message": message, "param": null, "line": n }));
        }
    };
    for (i, raw) in text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let n = i + 1;
        if lines.len() >= max_lines(ctx) {
            problem(
                n,
                "too_many_requests",
                format!("The file has more than {} requests.", max_lines(ctx)),
            );
            break;
        }
        let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(raw) else {
            problem(
                n,
                "invalid_json_line",
                "This line is not parseable as valid JSON.".into(),
            );
            continue;
        };
        let Some(custom_id) = v["custom_id"]
            .as_str()
            .filter(|c| !c.is_empty() && c.len() <= 64)
        else {
            problem(
                n,
                "missing_required_parameter",
                "Missing required parameter: 'custom_id'.".into(),
            );
            continue;
        };
        if !seen.insert(custom_id.to_string()) {
            problem(
                n,
                "duplicate_custom_id",
                format!("The custom_id '{custom_id}' is used by an earlier line."),
            );
            continue;
        }
        if v["method"].as_str().unwrap_or("POST") != "POST"
            || v["url"].as_str() != Some(CHAT_ENDPOINT)
        {
            problem(
                n,
                "invalid_url",
                format!(
                    "The URL of this line does not match the batch's endpoint ({CHAT_ENDPOINT})."
                ),
            );
            continue;
        }
        let req = match openai::parse_chat(&v["body"], ctx.shared.max_choices()) {
            Ok(r) => r,
            Err(m) => {
                problem(n, "invalid_request", m);
                continue;
            }
        };
        // Everything of a result follows from the line: the same file
        // gives the same output, ids included.
        let seed = stable_hash(&format!("{custom_id}\u{0}{}", v["body"]));
        let request_id = hex(&seed[8..], 32);
        let id = format!("batch_req_{}", hex(&seed, 24));
        if fails(custom_id, asked.fail_pct) {
            let (status, body) = line_error(Shape::OpenAi, &asked.fail_kind);
            totals
                .logged
                .push(json!({ "custom_id": custom_id, "outcome": "failed", "status": status }));
            lines.push(Line {
                custom_id: custom_id.to_string(),
                ok: false,
                result: json!({ "id": id, "custom_id": custom_id, "response": { "status_code": status, "request_id": request_id, "body": body }, "error": null }),
            });
            continue;
        }
        let replies: Vec<_> = (0..req.n).map(|c| ctx.generate(&req.input, c)).collect();
        let first = &replies[0];
        totals.prompt += replies.iter().map(|r| r.prompt_tokens).sum::<u32>();
        totals.completion += replies.iter().map(|r| r.completion_tokens).sum::<u32>();
        totals.model.clone_from(&req.input.model);
        totals.mode = first.mode.as_str().to_string();
        totals.logged.push(json!({
            "custom_id": custom_id, "outcome": "succeeded", "finish": openai::finish_reason(&first.finish),
            "tokens": first.prompt_tokens + first.completion_tokens,
            "text": first.text.chars().take(120).collect::<String>(),
        }));
        let body = openai::chat_json(
            &format!("chatcmpl-{}", hex(&seed[4..], 29)),
            created_at,
            &req.input.model,
            &replies,
            false,
        );
        lines.push(Line {
            custom_id: custom_id.to_string(),
            ok: true,
            result: json!({ "id": id, "custom_id": custom_id, "response": { "status_code": 200, "request_id": request_id, "body": body }, "error": null }),
        });
    }
    if lines.is_empty() && invalid.is_empty() {
        invalid.push(json!({ "code": "empty_file", "message": "The input file has no requests.", "param": null, "line": 1 }));
    }
    totals.logged.truncate(200);
    (lines, invalid)
}

#[allow(clippy::result_large_err)]
async fn create_batch(
    Extension(ctx): Extension<AiCtx>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return ctx.error(
            ErrorKind::BadRequest,
            "We could not parse the JSON body of your request.",
        );
    };
    let Some(file_id) = v["input_file_id"].as_str() else {
        return ctx.error(
            ErrorKind::BadRequest,
            "Missing required parameter: 'input_file_id'.",
        );
    };
    let endpoint = v["endpoint"].as_str().unwrap_or_default();
    if endpoint != CHAT_ENDPOINT {
        return ctx.error(
            ErrorKind::BadRequest,
            format!("Invalid value: '{endpoint}'. This mock runs batches of '{CHAT_ENDPOINT}'."),
        );
    }
    let asked = asked(&ctx, &headers);
    let owner = owner_key(&ctx);
    let max = super::setting_usize(&ctx.shared, MAX_BATCHES_KEY, DEFAULT_MAX_BATCHES);
    // The file first: its lines are answered off the store's lock.
    let file = ctx.shared.batches.with(&owner, |o, _, _| {
        if o.batches.len() >= max {
            return Err(ctx.error(ErrorKind::RateLimit, format!("You have {max} batches, the most this instance keeps for a caller: they are forgotten an hour after their last use.")));
        }
        o.files
            .get(file_id)
            .map(|f| (f.bytes.clone(), f.created_at))
            .ok_or_else(|| ctx.error(ErrorKind::BadRequest, format!("No such File object: {file_id}")))
    });
    let (bytes, file_created) = match file {
        Some(Ok(f)) => f,
        Some(Err(r)) => return r,
        None => return full(&ctx),
    };
    let mut totals = Totals::default();
    let (lines, invalid) = openai_lines(
        &ctx,
        &String::from_utf8_lossy(&bytes),
        file_created,
        &asked,
        &mut totals,
    );
    let id = format!("batch_{}", super::rand_id(24));
    let (total, failing, problems) = (
        lines.len(),
        lines.iter().filter(|l| !l.ok).count(),
        invalid.clone(),
    );
    let created_at = super::now_secs();
    let made = ctx.shared.batches.with(&owner, |o, now, _| {
        if o.batches.len() >= max {
            return None;
        }
        let b = Batch {
            shape: Shape::OpenAi,
            created: now,
            created_at,
            total: asked.total,
            input_file_id: Some(file_id.to_string()),
            metadata: if v["metadata"].is_object() {
                v["metadata"].clone()
            } else {
                Value::Null
            },
            lines,
            invalid,
            cancelled: None,
            expires: asked.expires,
        };
        let out = batch_json(&id, &b, now);
        o.batches.insert(id.clone(), b);
        Some(out)
    });
    match made.flatten() {
        Some(v) => {
            // Recorded once it is kept (a refused batch is no job).
            record(&ctx, &id, CHAT_ENDPOINT, total, failing, &problems, totals);
            Json(v).into_response()
        }
        None => full(&ctx),
    }
}

fn no_batch(ctx: &AiCtx, id: &str) -> Response {
    let mut r = ctx.error(
        ErrorKind::BadRequest,
        format!("No batch found with id '{id}'."),
    );
    *r.status_mut() = StatusCode::NOT_FOUND;
    r
}

async fn list_batches(Extension(ctx): Extension<AiCtx>) -> Response {
    let data = ctx
        .shared
        .batches
        .with(&owner_key(&ctx), |o, now, _| {
            o.batches
                .iter()
                .filter(|(_, b)| b.shape == Shape::OpenAi)
                .map(|(id, b)| batch_json(id, b, now))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Json(json!({ "object": "list", "data": data, "has_more": false, "first_id": data.first().map(|b| b["id"].clone()), "last_id": data.last().map(|b| b["id"].clone()) })).into_response()
}

async fn get_batch(Extension(ctx): Extension<AiCtx>, Path(id): Path<String>) -> Response {
    let found = ctx.shared.batches.with(&owner_key(&ctx), |o, now, _| {
        o.batches
            .get(&id)
            .filter(|b| b.shape == Shape::OpenAi)
            .map(|b| batch_json(&id, b, now))
    });
    match found.flatten() {
        Some(v) => Json(v).into_response(),
        None => no_batch(&ctx, &id),
    }
}

async fn cancel_batch(Extension(ctx): Extension<AiCtx>, Path(id): Path<String>) -> Response {
    let found = ctx.shared.batches.with(&owner_key(&ctx), |o, now, _| {
        let b = o
            .batches
            .get_mut(&id)
            .filter(|b| b.shape == Shape::OpenAi)?;
        // A batch that ended stays as it ended.
        if b.cancelled.is_none() && !b.at(now).0.ended() {
            b.cancelled = Some(now);
        }
        Some(batch_json(&id, b, now))
    });
    match found.flatten() {
        Some(v) => Json(v).into_response(),
        None => no_batch(&ctx, &id),
    }
}

// ── Anthropic: message batches ──────────────────────────────────────

/// The result of each request at `now`: done lines as they were answered,
/// the rest `canceled` or `expired` once the batch ended.
fn anthropic_results(b: &Batch, now: Duration) -> (Phase, Vec<Value>) {
    let (phase, done) = b.at(now);
    let rest = match phase {
        Phase::Cancelled => "canceled",
        _ => "expired",
    };
    let out = b
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if i < done {
                l.result.clone()
            } else {
                json!({ "custom_id": l.custom_id, "result": { "type": rest } })
            }
        })
        .collect();
    (phase, out)
}

fn message_batch_json(id: &str, b: &Batch, now: Duration) -> Value {
    let (phase, done) = b.at(now);
    let ended = phase.ended();
    let errored = b.lines[..done].iter().filter(|l| !l.ok).count();
    let left = b.lines.len() - done;
    let iso = |secs: i64| {
        chrono::DateTime::from_timestamp(secs, 0)
            .unwrap_or_default()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    };
    let cancel = b
        .cancelled
        .map(|c| c.saturating_sub(b.created).as_secs_f64());
    let ended_at = match (phase, cancel) {
        (Phase::Cancelled, Some(c)) => Some(b.stamp(c + b.total * 0.1)),
        (p, _) if p.ended() => Some(b.stamp(b.total)),
        _ => None,
    };
    json!({
        "id": id,
        "type": "message_batch",
        "processing_status": match phase {
            Phase::Cancelling => "canceling",
            p if p.ended() => "ended",
            _ => "in_progress",
        },
        "request_counts": {
            "processing": if ended { 0 } else { left },
            "succeeded": done - errored,
            "errored": errored,
            "canceled": if phase == Phase::Cancelled { left } else { 0 },
            "expired": if phase == Phase::Expired { left } else { 0 },
        },
        "ended_at": ended_at.map(iso),
        "created_at": iso(b.created_at),
        "expires_at": iso(b.created_at + 24 * 3600),
        "archived_at": null,
        "cancel_initiated_at": cancel.map(|c| iso(b.stamp(c))),
        // A path: the mock does not know the address a gateway serves it at.
        "results_url": ended.then(|| format!("/ai/anthropic/v1/messages/batches/{id}/results")),
    })
}

#[allow(clippy::result_large_err)]
async fn create_message_batch(
    Extension(ctx): Extension<AiCtx>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return ctx.error(
            ErrorKind::BadRequest,
            "There was an issue with your request body: it is not JSON.",
        );
    };
    let Some(requests) = v["requests"].as_array().filter(|r| !r.is_empty()) else {
        return ctx.error(ErrorKind::BadRequest, "requests: Field required");
    };
    if requests.len() > max_lines(&ctx) {
        return ctx.error(
            ErrorKind::BadRequest,
            format!(
                "requests: at most {} requests in a batch on this instance",
                max_lines(&ctx)
            ),
        );
    }
    let asked = asked(&ctx, &headers);
    let mut totals = Totals::default();
    let mut lines = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (i, r) in requests.iter().enumerate() {
        let Some(custom_id) = r["custom_id"]
            .as_str()
            .filter(|c| !c.is_empty() && c.len() <= 64)
        else {
            return ctx.error(
                ErrorKind::BadRequest,
                format!("requests.{i}.custom_id: Field required"),
            );
        };
        if !seen.insert(custom_id) {
            return ctx.error(
                ErrorKind::BadRequest,
                format!("requests.{i}.custom_id: Duplicate custom_id found: {custom_id}"),
            );
        }
        let errored = |ty: &str, message: &str| Line {
            custom_id: custom_id.to_string(),
            ok: false,
            result: json!({ "custom_id": custom_id, "result": { "type": "errored", "error": { "type": "error", "error": { "type": ty, "message": message } } } }),
        };
        if fails(custom_id, asked.fail_pct) {
            let (status, body) = line_error(Shape::Anthropic, &asked.fail_kind);
            totals
                .logged
                .push(json!({ "custom_id": custom_id, "outcome": "failed", "status": status }));
            lines.push(errored(
                body["error"]["type"].as_str().unwrap_or("api_error"),
                body["error"]["message"].as_str().unwrap_or(""),
            ));
            continue;
        }
        // A request whose params are wrong is its own errored result.
        let req = match anthropic::parse(&r["params"], true) {
            Ok(q) => q,
            Err(m) => {
                totals
                    .logged
                    .push(json!({ "custom_id": custom_id, "outcome": "failed", "status": 400 }));
                lines.push(errored("invalid_request_error", &m));
                continue;
            }
        };
        let reply = ctx.generate(&req.input, 0);
        let seed = stable_hash(&format!("{custom_id}\u{0}{}", r["params"]));
        let cu = anthropic::CacheUsage {
            input: reply.prompt_tokens,
            ..Default::default()
        };
        totals.prompt += reply.prompt_tokens;
        totals.completion += reply.completion_tokens;
        totals.model.clone_from(&req.input.model);
        totals.mode = reply.mode.as_str().to_string();
        totals.logged.push(json!({
            "custom_id": custom_id, "outcome": "succeeded", "finish": anthropic::stop_reason(&reply.finish).0,
            "tokens": reply.prompt_tokens + reply.completion_tokens,
            "text": reply.text.chars().take(120).collect::<String>(),
        }));
        let message = anthropic::message_json(
            &format!("msg_{}", hex(&seed, 24)),
            &req.input.model,
            &reply,
            cu,
        );
        lines.push(Line {
            custom_id: custom_id.to_string(),
            ok: true,
            result: json!({ "custom_id": custom_id, "result": { "type": "succeeded", "message": message } }),
        });
    }
    totals.logged.truncate(200);
    let (total, failing) = (lines.len(), lines.iter().filter(|l| !l.ok).count());
    let id = format!("msgbatch_{}", super::rand_id(24));
    let max = super::setting_usize(&ctx.shared, MAX_BATCHES_KEY, DEFAULT_MAX_BATCHES);
    let created_at = super::now_secs();
    let made = ctx.shared.batches.with(&owner_key(&ctx), |o, now, _| {
        if o.batches.len() >= max {
            return Err(ctx.error(ErrorKind::RateLimit, format!("You have {max} batches, the most this instance keeps for a caller: they are forgotten an hour after their last use.")));
        }
        let b = Batch {
            shape: Shape::Anthropic,
            created: now,
            created_at,
            // No validating phase in this shape: the work starts at once.
            total: asked.total,
            input_file_id: None,
            metadata: Value::Null,
            lines,
            invalid: Vec::new(),
            cancelled: None,
            expires: asked.expires,
        };
        let out = message_batch_json(&id, &b, now);
        o.batches.insert(id.clone(), b);
        Ok(out)
    });
    match made {
        Some(Ok(v)) => {
            // Recorded once it is kept (a refused batch is no job).
            record(&ctx, &id, "/v1/messages", total, failing, &[], totals);
            Json(v).into_response()
        }
        Some(Err(r)) => r,
        None => full(&ctx),
    }
}

fn no_message_batch(ctx: &AiCtx, id: &str) -> Response {
    ctx.error(
        ErrorKind::NotFound,
        format!("No message batch found with id '{id}'."),
    )
}

async fn list_message_batches(Extension(ctx): Extension<AiCtx>) -> Response {
    let data = ctx
        .shared
        .batches
        .with(&owner_key(&ctx), |o, now, _| {
            o.batches
                .iter()
                .filter(|(_, b)| b.shape == Shape::Anthropic)
                .map(|(id, b)| message_batch_json(id, b, now))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Json(json!({ "data": data, "has_more": false, "first_id": data.first().map(|b| b["id"].clone()), "last_id": data.last().map(|b| b["id"].clone()) })).into_response()
}

async fn get_message_batch(Extension(ctx): Extension<AiCtx>, Path(id): Path<String>) -> Response {
    let found = ctx.shared.batches.with(&owner_key(&ctx), |o, now, _| {
        o.batches
            .get(&id)
            .filter(|b| b.shape == Shape::Anthropic)
            .map(|b| message_batch_json(&id, b, now))
    });
    match found.flatten() {
        Some(v) => Json(v).into_response(),
        None => no_message_batch(&ctx, &id),
    }
}

async fn cancel_message_batch(
    Extension(ctx): Extension<AiCtx>,
    Path(id): Path<String>,
) -> Response {
    let found = ctx.shared.batches.with(&owner_key(&ctx), |o, now, _| {
        let b = o
            .batches
            .get_mut(&id)
            .filter(|b| b.shape == Shape::Anthropic)?;
        if b.cancelled.is_none() && !b.at(now).0.ended() {
            b.cancelled = Some(now);
        }
        Some(message_batch_json(&id, b, now))
    });
    match found.flatten() {
        Some(v) => Json(v).into_response(),
        None => no_message_batch(&ctx, &id),
    }
}

async fn message_batch_results(
    Extension(ctx): Extension<AiCtx>,
    Path(id): Path<String>,
) -> Response {
    let found = ctx.shared.batches.with(&owner_key(&ctx), |o, now, _| {
        o.batches
            .get(&id)
            .filter(|b| b.shape == Shape::Anthropic)
            .map(|b| anthropic_results(b, now))
    });
    match found.flatten() {
        Some((phase, results)) if phase.ended() => {
            let text: String = results.iter().map(|r| format!("{r}\n")).collect();
            ([(header::CONTENT_TYPE, "application/x-jsonl")], text).into_response()
        }
        Some(_) => ctx.error(ErrorKind::BadRequest, format!("The batch {id} has not ended: its results are available once processing_status is 'ended'.")),
        None => no_message_batch(&ctx, &id),
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ai/openai/v1/files", post(create_file).get(list_files))
        .route(
            "/ai/openai/v1/files/{id}",
            get(get_file).delete(delete_file),
        )
        .route("/ai/openai/v1/files/{id}/content", get(file_content))
        .route(
            "/ai/openai/v1/batches",
            post(create_batch).get(list_batches),
        )
        .route("/ai/openai/v1/batches/{id}", get(get_batch))
        .route("/ai/openai/v1/batches/{id}/cancel", post(cancel_batch))
        .route(
            "/ai/anthropic/v1/messages/batches",
            post(create_message_batch).get(list_message_batches),
        )
        .route(
            "/ai/anthropic/v1/messages/batches/{id}",
            get(get_message_batch),
        )
        .route(
            "/ai/anthropic/v1/messages/batches/{id}/cancel",
            post(cancel_message_batch),
        )
        .route(
            "/ai/anthropic/v1/messages/batches/{id}/results",
            get(message_batch_results),
        )
}

const MESSAGE_BATCH_EX: &str = r#"{"requests":[{"custom_id":"first","params":{"model":"rustybin-claude","max_tokens":256,"messages":[{"role":"user","content":"hello"}]}}]}"#;

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new("/ai/openai/v1/files", &["GET", "POST"], category::AI_OPENAI, "Files: upload a batch input file (multipart, purpose batch, JSONL), or list yours")
            .description("At most ai.batch.max_files files (20) a caller, 8 MiB each and never more than the instance's body limit; kept an hour.")
            .example(Example::get("List files", "/ai/openai/v1/files")),
        Endpoint::new("/ai/openai/v1/files/{id}", &["GET", "DELETE"], category::AI_OPENAI, "One file: its metadata, or delete it")
            .example(Example::get("A file", "/ai/openai/v1/files/file-0000").expect_status(404)),
        Endpoint::new("/ai/openai/v1/files/{id}/content", &["GET"], category::AI_OPENAI, "A file's content: your input file, or a batch's output or error file (JSONL)")
            .example(Example::get("A file's content", "/ai/openai/v1/files/file-0000/content").expect_status(404)),
        Endpoint::new("/ai/openai/v1/batches", &["GET", "POST"], category::AI_OPENAI, "Batches: start one from an input file, or list yours")
            .description("A batch of /v1/chat/completions lines, answered by the engine and deterministic for the same file. It moves through validating, in_progress, finalizing and completed in X-Rustybin-Batch-Seconds (default 10). X-Rustybin-Batch-Fail-Rate makes chosen lines fail (X-Rustybin-Batch-Fail picks the error), X-Rustybin-Batch-Outcome: expired runs it out of time. A malformed line fails validation. At most ai.batch.max_batches (10) a caller.")
            .example(Example::get("List batches", "/ai/openai/v1/batches"))
            .example(Example::post("A batch of a file that is not there", "/ai/openai/v1/batches").json(r#"{"input_file_id":"file-0000","endpoint":"/v1/chat/completions","completion_window":"24h"}"#).expect_status(400)),
        Endpoint::new("/ai/openai/v1/batches/{id}", &["GET"], category::AI_OPENAI, "One batch: its status, counts and output files")
            .example(Example::get("A batch", "/ai/openai/v1/batches/batch_0000").expect_status(404)),
        Endpoint::new("/ai/openai/v1/batches/{id}/cancel", &["POST"], category::AI_OPENAI, "Cancel a batch: cancelling, then cancelled with what was done")
            .example(Example::post("Cancel a batch", "/ai/openai/v1/batches/batch_0000/cancel").expect_status(404)),
        Endpoint::new("/ai/anthropic/v1/messages/batches", &["GET", "POST"], category::AI_ANTHROPIC, "Message batches: start one (requests with a custom_id and params), or list yours")
            .description("Each request is answered by the engine, deterministic for the same input. The batch is in_progress for X-Rustybin-Batch-Seconds (default 10), then ended. The batch headers of the OpenAI shape apply.")
            .example(Example::post("A message batch", "/ai/anthropic/v1/messages/batches").json(MESSAGE_BATCH_EX))
            .example(Example::get("List message batches", "/ai/anthropic/v1/messages/batches")),
        Endpoint::new("/ai/anthropic/v1/messages/batches/{id}", &["GET"], category::AI_ANTHROPIC, "One message batch: processing_status and request_counts")
            .example(Example::get("A message batch", "/ai/anthropic/v1/messages/batches/msgbatch_0000").expect_status(404)),
        Endpoint::new("/ai/anthropic/v1/messages/batches/{id}/cancel", &["POST"], category::AI_ANTHROPIC, "Cancel a message batch: canceling, then ended with the rest canceled")
            .example(Example::post("Cancel a message batch", "/ai/anthropic/v1/messages/batches/msgbatch_0000/cancel").expect_status(404)),
        Endpoint::new("/ai/anthropic/v1/messages/batches/{id}/results", &["GET"], category::AI_ANTHROPIC, "A message batch's results as JSONL, once it ended")
            .example(Example::get("Results", "/ai/anthropic/v1/messages/batches/msgbatch_0000/results").expect_status(404)),
    ]
}

pub fn openapi_paths() -> Value {
    let obj = json!({ "application/json": { "schema": { "type": "object" } } });
    let jsonl = json!({ "application/jsonl": { "schema": { "type": "string" } } });
    let id =
        json!({ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } });
    let batch_headers = json!([
        { "name": "X-Rustybin-Batch-Seconds", "in": "header", "required": false, "description": "The whole life of the batch on the compressed clock (default the ai.batch.seconds setting, 10; at most 3600)", "schema": { "type": "number" } },
        { "name": "X-Rustybin-Batch-Fail-Rate", "in": "header", "required": false, "description": "The share of lines that fail (0 to 1, or a percent), chosen by custom_id", "schema": { "type": "string" } },
        { "name": "X-Rustybin-Batch-Fail", "in": "header", "required": false, "description": "The error of a failing line (500, 429, 503, 529, ...)", "schema": { "type": "string" } },
        { "name": "X-Rustybin-Batch-Outcome", "in": "header", "required": false, "description": "expired: the batch runs out of time half way", "schema": { "type": "string", "enum": ["expired"] } }
    ]);
    let op = |tag: &str, summary: &str, op_id: &str, params: Value, ok: Value| json!({ "tags": [tag], "summary": summary, "operationId": op_id, "parameters": params, "responses": { "200": { "description": "OK", "content": ok }, "404": { "description": "Not yours, or not there" } } });
    let o = "AI: OpenAI-compatible";
    let a = "AI: Anthropic-compatible";
    json!({
        "/ai/openai/v1/files": {
            "get": op(o, "List files", "aiOpenAiListFiles", json!([]), obj.clone()),
            "post": {
                "tags": [o], "summary": "Upload a file", "operationId": "aiOpenAiCreateFile",
                "requestBody": { "required": true, "content": { "multipart/form-data": { "schema": { "type": "object", "required": ["purpose", "file"], "properties": { "purpose": { "type": "string", "enum": ["batch"] }, "file": { "type": "string", "format": "binary" } } } } } },
                "responses": { "200": { "description": "The file object", "content": obj.clone() }, "400": { "description": "Not multipart, or not purpose batch" }, "413": { "description": "Larger than 8 MiB, or no room" }, "429": { "description": "As many files as a caller keeps" } }
            }
        },
        "/ai/openai/v1/files/{id}": {
            "get": op(o, "Retrieve a file", "aiOpenAiGetFile", json!([id]), obj.clone()),
            "delete": op(o, "Delete a file", "aiOpenAiDeleteFile", json!([id]), obj.clone())
        },
        "/ai/openai/v1/files/{id}/content": { "get": op(o, "A file's content", "aiOpenAiFileContent", json!([id]), jsonl.clone()) },
        "/ai/openai/v1/batches": {
            "get": op(o, "List batches", "aiOpenAiListBatches", json!([]), obj.clone()),
            "post": {
                "tags": [o], "summary": "Create a batch", "operationId": "aiOpenAiCreateBatch",
                "parameters": batch_headers,
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["input_file_id", "endpoint"], "properties": { "input_file_id": { "type": "string" }, "endpoint": { "type": "string", "enum": [CHAT_ENDPOINT] }, "completion_window": { "type": "string" }, "metadata": { "type": "object" } } } } } },
                "responses": { "200": { "description": "The batch, validating", "content": obj.clone() }, "400": { "description": "No such file, or another endpoint" }, "429": { "description": "As many batches as a caller keeps" } }
            }
        },
        "/ai/openai/v1/batches/{id}": { "get": op(o, "Retrieve a batch", "aiOpenAiGetBatch", json!([id]), obj.clone()) },
        "/ai/openai/v1/batches/{id}/cancel": { "post": op(o, "Cancel a batch", "aiOpenAiCancelBatch", json!([id]), obj.clone()) },
        "/ai/anthropic/v1/messages/batches": {
            "get": op(a, "List message batches", "aiAnthropicListBatches", json!([]), obj.clone()),
            "post": {
                "tags": [a], "summary": "Create a message batch", "operationId": "aiAnthropicCreateBatch",
                "parameters": batch_headers,
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["requests"], "properties": { "requests": { "type": "array", "items": { "type": "object", "required": ["custom_id", "params"], "properties": { "custom_id": { "type": "string" }, "params": { "type": "object" } } } } } } } } },
                "responses": { "200": { "description": "The batch, in_progress", "content": obj.clone() }, "400": { "description": "No requests, or a custom_id missing or used twice" }, "429": { "description": "As many batches as a caller keeps" } }
            }
        },
        "/ai/anthropic/v1/messages/batches/{id}": { "get": op(a, "Retrieve a message batch", "aiAnthropicGetBatch", json!([id]), obj.clone()) },
        "/ai/anthropic/v1/messages/batches/{id}/cancel": { "post": op(a, "Cancel a message batch", "aiAnthropicCancelBatch", json!([id]), obj) },
        "/ai/anthropic/v1/messages/batches/{id}/results": { "get": op(a, "A message batch's results", "aiAnthropicBatchResults", json!([id]), jsonl) }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::test_util::{app, json as body_json};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: Option<(&str, Vec<u8>)>,
    ) -> (u16, Bytes) {
        let mut b = Request::builder().method(method).uri(uri);
        if !headers.iter().any(|(k, _)| *k == "x-rustybin-session") {
            b = b.header("x-rustybin-session", "batcher");
        }
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let body = match body {
            Some((content_type, bytes)) => {
                b = b.header("content-type", content_type);
                Body::from(bytes)
            }
            None => Body::empty(),
        };
        let resp = app
            .clone()
            .oneshot(b.body(body).expect("request"))
            .await
            .expect("response");
        let status = resp.status().as_u16();
        (status, crate::test_support::body_bytes(resp).await)
    }

    async fn get(app: &Router, uri: &str) -> (u16, Value) {
        let (s, b) = call(app, "GET", uri, &[], None).await;
        (s, body_json(&b))
    }

    async fn post(app: &Router, uri: &str, headers: &[(&str, &str)], body: Value) -> (u16, Value) {
        let (s, b) = call(
            app,
            "POST",
            uri,
            headers,
            Some(("application/json", body.to_string().into_bytes())),
        )
        .await;
        (s, body_json(&b))
    }

    async fn upload(app: &Router, text: &str) -> (u16, Value) {
        let body = format!(
            "--XX\r\nContent-Disposition: form-data; name=\"purpose\"\r\n\r\nbatch\r\n--XX\r\nContent-Disposition: form-data; name=\"file\"; filename=\"requests.jsonl\"\r\nContent-Type: application/jsonl\r\n\r\n{text}\r\n--XX--\r\n"
        );
        let (s, b) = call(
            app,
            "POST",
            "/ai/openai/v1/files",
            &[],
            Some(("multipart/form-data; boundary=XX", body.into_bytes())),
        )
        .await;
        (s, body_json(&b))
    }

    fn line(id: &str, text: &str) -> String {
        json!({ "custom_id": id, "method": "POST", "url": CHAT_ENDPOINT, "body": { "model": "rustybin-echo", "messages": [{ "role": "user", "content": text }] } }).to_string()
    }

    fn three() -> String {
        [line("a", "first"), line("b", "second"), line("c", "third")].join("\n")
    }

    async fn start(app: &Router, file: &str, headers: &[(&str, &str)]) -> (u16, Value) {
        post(app, "/ai/openai/v1/batches", headers, json!({ "input_file_id": file, "endpoint": CHAT_ENDPOINT, "completion_window": "24h", "metadata": { "run": "nightly" } })).await
    }

    fn jsonl(b: &Bytes) -> Vec<Value> {
        String::from_utf8_lossy(b)
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    #[tokio::test]
    async fn a_batch_of_three_lines_completes_on_the_compressed_clock() {
        let app = app();
        let (s, file) = upload(&app, &three()).await;
        assert_eq!(s, 200, "{file}");
        assert_eq!(file["object"], "file");
        assert_eq!(file["purpose"], "batch");
        let file_id = file["id"].as_str().expect("id").to_string();
        let (s, got) = get(&app, &format!("/ai/openai/v1/files/{file_id}")).await;
        assert_eq!((s, &got["filename"]), (200, &json!("requests.jsonl")));

        // A whole life of 0.4 s: validating first, completed after it.
        let (s, b) = start(
            &app,
            &file_id,
            &[(SECONDS_HEADER, "0.4"), ("x-request-id", "batch-one")],
        )
        .await;
        assert_eq!(s, 200, "{b}");
        assert_eq!(b["status"], "validating");
        assert_eq!(
            b["request_counts"],
            json!({ "total": 3, "completed": 0, "failed": 0 })
        );
        assert!(b["output_file_id"].is_null());
        assert_eq!(b["metadata"]["run"], "nightly");
        let id = b["id"].as_str().expect("id").to_string();
        tokio::time::sleep(Duration::from_millis(450)).await;
        let (_, b) = get(&app, &format!("/ai/openai/v1/batches/{id}")).await;
        assert_eq!(b["status"], "completed", "{b}");
        assert_eq!(
            b["request_counts"],
            json!({ "total": 3, "completed": 3, "failed": 0 })
        );
        assert!(
            b["completed_at"].is_number()
                && b["in_progress_at"].is_number()
                && b["finalizing_at"].is_number()
        );
        assert!(b["error_file_id"].is_null());
        let out = b["output_file_id"].as_str().expect("output").to_string();
        let (s, content) = call(
            &app,
            "GET",
            &format!("/ai/openai/v1/files/{out}/content"),
            &[],
            None,
        )
        .await;
        assert_eq!(s, 200);
        let results = jsonl(&content);
        assert_eq!(results.len(), 3);
        assert_eq!(results[1]["custom_id"], "b");
        assert_eq!(results[1]["response"]["status_code"], 200);
        assert!(
            results[1]["response"]["body"]["choices"][0]["message"]["content"]
                .as_str()
                .expect("text")
                .contains("second")
        );
        let (_, list) = get(&app, "/ai/openai/v1/batches").await;
        assert_eq!(list["data"][0]["id"], id);

        // The same file again: the same output, byte for byte.
        let (_, again) = start(&app, &file_id, &[(SECONDS_HEADER, "0")]).await;
        assert_eq!(
            again["status"], "completed",
            "a life of 0 seconds is done at once: {again}"
        );
        let out2 = again["output_file_id"]
            .as_str()
            .expect("output")
            .to_string();
        let (_, content2) = call(
            &app,
            "GET",
            &format!("/ai/openai/v1/files/{out2}/content"),
            &[],
            None,
        )
        .await;
        assert_eq!(content, content2);

        // The request log has the job with its lines.
        let (s, r) = get(&app, "/ai/requests/batch-one").await;
        assert_eq!(s, 200, "{r}");
        assert_eq!(r["kind"], "batch");
        assert_eq!(r["detail"]["batch_id"], id);
        assert_eq!(r["detail"]["lines"].as_array().map(Vec::len), Some(3));
        assert!(r["usage"]["total_tokens"].as_u64().expect("tokens") > 0);

        // Another caller sees none of it.
        let (s, _) = call(
            &app,
            "GET",
            &format!("/ai/openai/v1/batches/{id}"),
            &[("x-rustybin-session", "someone-else")],
            None,
        )
        .await;
        assert_eq!(s, 404);
        let (s, gone) = call(
            &app,
            "DELETE",
            &format!("/ai/openai/v1/files/{file_id}"),
            &[],
            None,
        )
        .await;
        assert_eq!((s, body_json(&gone)["deleted"].clone()), (200, json!(true)));
        let (s, _) = get(&app, &format!("/ai/openai/v1/files/{file_id}")).await;
        assert_eq!(s, 404);
    }

    #[tokio::test]
    async fn a_fail_rate_fails_lines_with_the_native_error_and_a_bad_line_fails_validation() {
        let app = app();
        let (_, file) = upload(&app, &three()).await;
        let file_id = file["id"].as_str().expect("id").to_string();
        let (_, b) = start(
            &app,
            &file_id,
            &[
                (SECONDS_HEADER, "0"),
                (FAIL_RATE_HEADER, "1"),
                (FAIL_HEADER, "429"),
            ],
        )
        .await;
        assert_eq!(b["status"], "completed", "{b}");
        assert_eq!(
            b["request_counts"],
            json!({ "total": 3, "completed": 0, "failed": 3 })
        );
        assert!(b["output_file_id"].is_null());
        let err = b["error_file_id"].as_str().expect("errors").to_string();
        let (_, content) = call(
            &app,
            "GET",
            &format!("/ai/openai/v1/files/{err}/content"),
            &[],
            None,
        )
        .await;
        let failed = jsonl(&content);
        assert_eq!(failed.len(), 3);
        assert_eq!(failed[0]["response"]["status_code"], 429);
        assert_eq!(failed[0]["response"]["body"]["error"]["type"], "requests");
        // A share: the lines that fail are chosen by their id, the same every time.
        let many: String = (0..40)
            .map(|i| line(&format!("req-{i}"), "hi"))
            .collect::<Vec<_>>()
            .join("\n");
        let (_, file) = upload(&app, &many).await;
        let big = file["id"].as_str().expect("id").to_string();
        let mut counts = Vec::new();
        for _ in 0..2 {
            let (_, b) = start(
                &app,
                &big,
                &[(SECONDS_HEADER, "0"), (FAIL_RATE_HEADER, "25%")],
            )
            .await;
            counts.push(b["request_counts"]["failed"].as_u64().expect("failed"));
        }
        assert_eq!(counts[0], counts[1]);
        assert!(counts[0] > 0 && counts[0] < 40, "{counts:?}");

        // A malformed line: validation fails with the provider's error, by line.
        let (_, file) = upload(
            &app,
            &format!(
                "{}\nthis is not json\n{}",
                line("a", "x"),
                json!({ "custom_id": "c", "method": "POST", "url": "/v1/embeddings", "body": {} })
            ),
        )
        .await;
        let bad = file["id"].as_str().expect("id").to_string();
        let (s, b) = start(&app, &bad, &[(SECONDS_HEADER, "0")]).await;
        assert_eq!(s, 200);
        assert_eq!(b["status"], "failed", "{b}");
        assert_eq!(b["errors"]["data"][0]["code"], "invalid_json_line");
        assert_eq!(b["errors"]["data"][0]["line"], 2);
        assert_eq!(b["errors"]["data"][1]["code"], "invalid_url");
        assert!(b["failed_at"].is_number() && b["output_file_id"].is_null());
        // What is not a batch at all is refused at once.
        let (s, v) = start(&app, "file-nope", &[]).await;
        assert_eq!(s, 400, "{v}");
        assert!(v["error"]["message"]
            .as_str()
            .expect("m")
            .contains("No such File object"));
        let (s, _) = post(
            &app,
            "/ai/openai/v1/batches",
            &[],
            json!({ "input_file_id": bad, "endpoint": "/v1/embeddings" }),
        )
        .await;
        assert_eq!(s, 400);
    }

    #[tokio::test]
    async fn a_cancel_moves_through_cancelling_to_cancelled_and_a_batch_expires() {
        let app = app();
        let (_, file) = upload(&app, &three()).await;
        let file_id = file["id"].as_str().expect("id").to_string();
        let (_, b) = start(&app, &file_id, &[(SECONDS_HEADER, "1")]).await;
        let id = b["id"].as_str().expect("id").to_string();
        let (s, c) = post(
            &app,
            &format!("/ai/openai/v1/batches/{id}/cancel"),
            &[],
            json!({}),
        )
        .await;
        assert_eq!(s, 200);
        assert_eq!(c["status"], "cancelling", "{c}");
        assert!(c["cancelling_at"].is_number() && c["cancelled_at"].is_null());
        tokio::time::sleep(Duration::from_millis(150)).await;
        let (_, c) = get(&app, &format!("/ai/openai/v1/batches/{id}")).await;
        assert_eq!(c["status"], "cancelled", "{c}");
        assert!(c["cancelled_at"].is_number());
        // It stays cancelled, however long, and a second cancel changes nothing.
        tokio::time::sleep(Duration::from_millis(900)).await;
        let (_, c) = post(
            &app,
            &format!("/ai/openai/v1/batches/{id}/cancel"),
            &[],
            json!({}),
        )
        .await;
        assert_eq!(c["status"], "cancelled");
        // Out of time half way: what was done is in the output file.
        let (_, e) = start(
            &app,
            &file_id,
            &[(SECONDS_HEADER, "0"), (OUTCOME_HEADER, "expired")],
        )
        .await;
        assert_eq!(e["status"], "expired", "{e}");
        assert_eq!(e["request_counts"]["completed"], 1);
        assert!(e["expired_at"].is_number() && e["output_file_id"].is_string());
        // A completed batch is not cancelled.
        let (_, done) = start(&app, &file_id, &[(SECONDS_HEADER, "0")]).await;
        let (_, still) = post(
            &app,
            &format!(
                "/ai/openai/v1/batches/{}/cancel",
                done["id"].as_str().expect("id")
            ),
            &[],
            json!({}),
        )
        .await;
        assert_eq!(still["status"], "completed");
    }

    #[test]
    fn the_phases_of_a_batch_follow_its_clock() {
        let lines = |n: usize| {
            (0..n)
                .map(|i| Line {
                    custom_id: format!("l{i}"),
                    ok: true,
                    result: Value::Null,
                })
                .collect()
        };
        let mut b = Batch {
            shape: Shape::OpenAi,
            created: Duration::from_secs(100),
            created_at: 1_000,
            total: 10.0,
            input_file_id: None,
            metadata: Value::Null,
            lines: lines(10),
            invalid: Vec::new(),
            cancelled: None,
            expires: false,
        };
        let at =
            |b: &Batch, secs: f64| b.at(Duration::from_secs(100) + Duration::from_secs_f64(secs));
        assert_eq!(at(&b, 0.5), (Phase::Validating, 0));
        assert_eq!(at(&b, 1.0), (Phase::InProgress, 0));
        assert_eq!(at(&b, 4.5), (Phase::InProgress, 5));
        assert_eq!(at(&b, 8.5), (Phase::Finalizing, 10));
        assert_eq!(at(&b, 10.0), (Phase::Completed, 10));
        b.cancelled = Some(Duration::from_secs(104) + Duration::from_millis(500));
        assert_eq!(at(&b, 5.0), (Phase::Cancelling, 5));
        assert_eq!(at(&b, 5.6), (Phase::Cancelled, 5));
        assert_eq!(at(&b, 60.0), (Phase::Cancelled, 5));
        b.cancelled = None;
        b.expires = true;
        assert_eq!(at(&b, 9.0), (Phase::InProgress, 5));
        assert_eq!(at(&b, 10.0), (Phase::Expired, 5));
        b.invalid = vec![json!({})];
        assert_eq!(at(&b, 0.5), (Phase::Validating, 0));
        assert_eq!(at(&b, 1.0), (Phase::Failed, 0));
    }

    #[tokio::test]
    async fn a_message_batch_ends_with_a_result_a_request() {
        let app = app();
        let requests = json!({ "requests": [
            { "custom_id": "one", "params": { "model": "rustybin-echo", "max_tokens": 64, "messages": [{ "role": "user", "content": "first" }] } },
            { "custom_id": "two", "params": { "model": "rustybin-claude", "messages": [{ "role": "user", "content": "no max_tokens" }] } },
            { "custom_id": "three", "params": { "model": "rustybin-echo", "max_tokens": 64, "messages": [{ "role": "user", "content": "third" }] } }
        ] });
        let (s, b) = post(
            &app,
            "/ai/anthropic/v1/messages/batches",
            &[(SECONDS_HEADER, "0.3")],
            requests.clone(),
        )
        .await;
        assert_eq!(s, 200, "{b}");
        assert_eq!(b["type"], "message_batch");
        assert_eq!(b["processing_status"], "in_progress");
        assert_eq!(b["request_counts"]["processing"], 3);
        assert!(b["results_url"].is_null() && b["ended_at"].is_null());
        let id = b["id"].as_str().expect("id").to_string();
        assert!(id.starts_with("msgbatch_"));
        // Results are there once it ended.
        let (s, early) = get(
            &app,
            &format!("/ai/anthropic/v1/messages/batches/{id}/results"),
        )
        .await;
        assert_eq!(s, 400, "{early}");
        assert_eq!(early["error"]["type"], "invalid_request_error");
        tokio::time::sleep(Duration::from_millis(350)).await;
        let (_, b) = get(&app, &format!("/ai/anthropic/v1/messages/batches/{id}")).await;
        assert_eq!(b["processing_status"], "ended", "{b}");
        assert_eq!(
            b["request_counts"],
            json!({ "processing": 0, "succeeded": 2, "errored": 1, "canceled": 0, "expired": 0 })
        );
        assert!(b["results_url"]
            .as_str()
            .expect("url")
            .ends_with("/results"));
        let (s, content) = call(
            &app,
            "GET",
            &format!("/ai/anthropic/v1/messages/batches/{id}/results"),
            &[],
            None,
        )
        .await;
        assert_eq!(s, 200);
        let results = jsonl(&content);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0]["result"]["type"], "succeeded");
        assert_eq!(results[0]["result"]["message"]["type"], "message");
        // A request whose params are wrong is its own errored result.
        assert_eq!(results[1]["result"]["type"], "errored");
        assert_eq!(
            results[1]["result"]["error"]["error"]["type"],
            "invalid_request_error"
        );
        // The same requests: the same results.
        let (_, again) = post(
            &app,
            "/ai/anthropic/v1/messages/batches",
            &[(SECONDS_HEADER, "0")],
            requests.clone(),
        )
        .await;
        let (_, content2) = call(
            &app,
            "GET",
            &format!(
                "/ai/anthropic/v1/messages/batches/{}/results",
                again["id"].as_str().expect("id")
            ),
            &[],
            None,
        )
        .await;
        assert_eq!(content, content2);
        // Cancelled: canceling, then ended with the rest canceled.
        let (_, c) = post(
            &app,
            "/ai/anthropic/v1/messages/batches",
            &[(SECONDS_HEADER, "1")],
            requests.clone(),
        )
        .await;
        let cid = c["id"].as_str().expect("id").to_string();
        let (_, c) = post(
            &app,
            &format!("/ai/anthropic/v1/messages/batches/{cid}/cancel"),
            &[],
            json!({}),
        )
        .await;
        assert_eq!(c["processing_status"], "canceling", "{c}");
        tokio::time::sleep(Duration::from_millis(150)).await;
        let (_, c) = get(&app, &format!("/ai/anthropic/v1/messages/batches/{cid}")).await;
        assert_eq!(c["processing_status"], "ended");
        assert_eq!(c["request_counts"]["canceled"], 3, "{c}");
        let (_, content) = call(
            &app,
            "GET",
            &format!("/ai/anthropic/v1/messages/batches/{cid}/results"),
            &[],
            None,
        )
        .await;
        assert_eq!(jsonl(&content)[2]["result"]["type"], "canceled");
        // A fail rate, with the provider's error.
        let (_, f) = post(
            &app,
            "/ai/anthropic/v1/messages/batches",
            &[
                (SECONDS_HEADER, "0"),
                (FAIL_RATE_HEADER, "100%"),
                (FAIL_HEADER, "529"),
            ],
            requests,
        )
        .await;
        let (_, content) = call(
            &app,
            "GET",
            &format!(
                "/ai/anthropic/v1/messages/batches/{}/results",
                f["id"].as_str().expect("id")
            ),
            &[],
            None,
        )
        .await;
        assert_eq!(
            jsonl(&content)[0]["result"]["error"]["error"]["type"],
            "overloaded_error"
        );
        // What is no batch is refused with the provider's error.
        let (s, v) = post(
            &app,
            "/ai/anthropic/v1/messages/batches",
            &[],
            json!({ "requests": [{ "params": {} }] }),
        )
        .await;
        assert_eq!(s, 400);
        assert_eq!(v["type"], "error");
        let (s, v) = get(&app, "/ai/anthropic/v1/messages/batches/msgbatch_nope").await;
        assert_eq!(s, 404);
        assert_eq!(v["error"]["type"], "not_found_error");
    }

    #[tokio::test]
    async fn the_caps_refuse_with_the_providers_error() {
        let app = app();
        let (_, file) = upload(&app, &line("a", "x")).await;
        let file_id = file["id"].as_str().expect("id").to_string();
        for i in 0..DEFAULT_MAX_BATCHES {
            let (s, b) = start(&app, &file_id, &[(SECONDS_HEADER, "0")]).await;
            assert_eq!(s, 200, "batch {i}: {b}");
        }
        // The eleventh, in either shape.
        let (s, v) = start(&app, &file_id, &[]).await;
        assert_eq!(s, 429, "{v}");
        assert_eq!(v["error"]["code"], "rate_limit_exceeded");
        let (s, v) = post(&app, "/ai/anthropic/v1/messages/batches", &[], json!({ "requests": [{ "custom_id": "a", "params": { "model": "m", "max_tokens": 8, "messages": [{ "role": "user", "content": "x" }] } }] })).await;
        assert_eq!(s, 429, "{v}");
        assert_eq!(v["error"]["type"], "rate_limit_error");
        // Twenty files, then no more; another purpose is not stored.
        for _ in 1..DEFAULT_MAX_FILES {
            assert_eq!(upload(&app, &line("a", "x")).await.0, 200);
        }
        let (s, v) = upload(&app, &line("a", "x")).await;
        assert_eq!(s, 429, "{v}");
        let (_, list) = get(&app, "/ai/openai/v1/files").await;
        assert_eq!(
            list["data"].as_array().map(Vec::len),
            Some(DEFAULT_MAX_FILES)
        );
        let body = "--XX\r\nContent-Disposition: form-data; name=\"purpose\"\r\n\r\nfine-tune\r\n--XX\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a\"\r\n\r\n{}\r\n--XX--\r\n";
        let (s, _) = call(
            &app,
            "POST",
            "/ai/openai/v1/files",
            &[("x-rustybin-session", "other")],
            Some(("multipart/form-data; boundary=XX", body.as_bytes().to_vec())),
        )
        .await;
        assert_eq!(s, 400);
    }

    #[test]
    fn callers_idle_for_an_hour_are_forgotten_and_the_instance_is_bounded() {
        let clock = Arc::new(crate::limits::ManualClock::new(chrono::Utc::now()));
        let store = Store::new(clock.clone(), true);
        let add = |owner: &str, bytes: usize| {
            store.with(owner, |o, _, room| {
                if bytes > room.saturating_sub(o.bytes) {
                    return false;
                }
                o.bytes += bytes;
                o.files.insert(
                    format!("file-{}", o.files.len()),
                    File {
                        filename: "f".into(),
                        purpose: "batch".into(),
                        bytes: Bytes::from(vec![0u8; bytes]),
                        created_at: 0,
                    },
                );
                true
            })
        };
        // A public instance keeps 64 callers and 32 MiB.
        for i in 0..64 {
            assert_eq!(add(&format!("caller-{i}"), 1024), Some(true));
        }
        assert_eq!(add("one-more", 1024), None, "no room for another caller");
        assert_eq!(
            add("caller-0", 40 * 1024 * 1024),
            Some(false),
            "no room for the bytes"
        );
        // A caller that only looks keeps nothing.
        assert_eq!(store.owners(), 64);
        clock.advance(TTL + Duration::from_secs(1));
        assert_eq!(add("one-more", 1024), Some(true));
        assert_eq!(store.owners(), 1, "the idle callers are gone");
    }
}
