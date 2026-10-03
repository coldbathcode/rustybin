//! Task store and the binding independent operations (send, get, list,
//! cancel, subscribe, push config CRUD).
//!
//! The store is bounded (global cap, per-session cap, idle TTL) and every
//! task is scoped to the session that created it (`session::session_key`)
//! and to its agent, so clients never see each other's tasks.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::Value;
use tokio::sync::broadcast;

use super::agents::{self, AgentDef, AuthOutcome, Plan, PlanInput, Step, StepKind};
use super::errors::{A2aError, Kind};
use super::model::*;
use super::push::{PushService, Target};
use super::wire::{self, PushInput, SendRequest, TaskView};

/// History entries kept per task.
const MAX_HISTORY: usize = 100;
/// Artifacts kept per task and parts per artifact.
const MAX_ARTIFACTS: usize = 32;
const MAX_ARTIFACT_PARTS: usize = 256;
/// Push configs per task.
pub const MAX_PUSH_CONFIGS: usize = 10;
/// Events buffered per subscriber.
const CHANNEL_CAPACITY: usize = 64;

pub struct Entry {
    pub owner: String,
    pub agent: &'static str,
    pub task: Task,
    pub touched: Instant,
    pub tx: broadcast::Sender<StreamEvent>,
    pub push: Vec<PushConfig>,
    pub running: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_tasks: usize,
    pub max_tasks_per_session: usize,
    pub ttl: Duration,
    pub max_running: usize,
}

impl Limits {
    pub fn for_mode(public_mode: bool) -> Self {
        if public_mode {
            Limits {
                max_tasks: 2000,
                max_tasks_per_session: 50,
                ttl: Duration::from_secs(15 * 60),
                max_running: 100,
            }
        } else {
            Limits {
                max_tasks: 5000,
                max_tasks_per_session: 1000,
                ttl: Duration::from_secs(60 * 60),
                max_running: 500,
            }
        }
    }
}

/// The module service: tasks plus push delivery.
pub struct Service {
    tasks: Mutex<HashMap<String, Entry>>,
    pub limits: Limits,
    running: AtomicUsize,
    pub push: PushService,
}

/// Result of a send.
pub enum Started {
    /// Direct message reply (no task).
    Reply(Message),
    /// A task was created or continued. `rx` was subscribed before any
    /// update was produced, so no event is missed.
    Task {
        snapshot: Task,
        rx: broadcast::Receiver<StreamEvent>,
    },
}

/// Who is asking, and how.
pub struct Caller<'a> {
    pub owner: &'a str,
    pub agent: &'static AgentDef,
    pub base_url: &'a str,
    pub authority: &'a str,
    pub auth: &'a AuthOutcome,
    pub version: Version,
}

/// Filters of ListTasks.
#[derive(Default, Debug)]
pub struct ListQuery {
    pub context_id: Option<String>,
    pub status: Option<TaskState>,
    pub page_size: Option<i64>,
    pub page_token: Option<String>,
    pub history_length: Option<i64>,
    pub status_timestamp_after: Option<String>,
    pub include_artifacts: bool,
}

pub struct ListResult {
    pub tasks: Vec<Task>,
    pub next_page_token: String,
    pub page_size: usize,
    pub total_size: usize,
    pub view: TaskView,
}

struct RunningGuard<'a>(&'a AtomicUsize);
impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn status_event(task: &Task, state: TaskState, text: Option<String>) -> StreamEvent {
    let message =
        text.map(|t| Message::agent(vec![Part::text(t)], &task.context_id, Some(&task.id)));
    StreamEvent::Status(StatusUpdate {
        task_id: task.id.clone(),
        context_id: task.context_id.clone(),
        status: TaskStatus::new(state, message),
    })
}

fn push_history(task: &mut Task, m: Message) {
    task.history.push(m);
    if task.history.len() > MAX_HISTORY {
        let extra = task.history.len() - MAX_HISTORY;
        task.history.drain(..extra);
    }
}

/// Apply an update event to a task (status replaces, artifacts append or
/// replace by id).
fn apply(task: &mut Task, ev: &StreamEvent) {
    match ev {
        StreamEvent::Status(s) => {
            if let Some(old) = task.status.message.take() {
                push_history(task, old);
            }
            task.status = s.status.clone();
        }
        StreamEvent::Artifact(a) => {
            match task
                .artifacts
                .iter_mut()
                .find(|x| x.artifact_id == a.artifact.artifact_id)
            {
                Some(existing) if a.append => {
                    existing.parts.extend(a.artifact.parts.iter().cloned());
                    existing.parts.truncate(MAX_ARTIFACT_PARTS);
                }
                Some(existing) => *existing = a.artifact.clone(),
                None => {
                    if task.artifacts.len() < MAX_ARTIFACTS {
                        task.artifacts.push(a.artifact.clone());
                    }
                }
            }
        }
        StreamEvent::Task(t) => *task = t.clone(),
        StreamEvent::Message(_) => {}
    }
}

impl Service {
    pub fn new(limits: Limits, push: PushService) -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
            limits,
            running: AtomicUsize::new(0),
            push,
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        self.tasks.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Number of stored tasks (for tests and diagnostics).
    pub fn task_count(&self) -> usize {
        self.lock().len()
    }

    fn evict(&self, map: &mut HashMap<String, Entry>, owner: &str) {
        let ttl = self.limits.ttl;
        map.retain(|_, e| e.touched.elapsed() < ttl);
        while map.values().filter(|e| e.owner == owner).count() >= self.limits.max_tasks_per_session
        {
            let oldest = map
                .iter()
                .filter(|(_, e)| e.owner == owner)
                .min_by_key(|(_, e)| e.touched)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => map.remove(&k),
                None => break,
            };
        }
        while map.len() >= self.limits.max_tasks {
            let oldest = map
                .iter()
                .min_by_key(|(_, e)| e.touched)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => map.remove(&k),
                None => break,
            };
        }
    }

    fn visible<'m>(
        &self,
        map: &'m mut HashMap<String, Entry>,
        caller: &Caller,
        id: &str,
    ) -> Result<&'m mut Entry, A2aError> {
        match map.get_mut(id) {
            Some(e)
                if e.owner == caller.owner
                    && e.agent == caller.agent.id
                    && e.touched.elapsed() < self.limits.ttl =>
            {
                Ok(e)
            }
            _ => Err(A2aError::task_not_found(id)),
        }
    }

    /// Validate a push URL against the policy and build the stored config.
    pub fn make_push_config(
        &self,
        caller: &Caller,
        task_id: &str,
        input: PushInput,
    ) -> Result<PushConfig, A2aError> {
        let target = self
            .push
            .policy
            .check(&input.url, caller.authority)
            .map_err(A2aError::invalid_params)?;
        Ok(PushConfig {
            id: input.id.unwrap_or_else(new_id),
            task_id: task_id.to_string(),
            url: input.url,
            token: input.token,
            authentication: input.authentication,
            version: caller.version,
            sink_id: match target {
                Target::Sink(id) => Some(id),
                Target::Remote(_) => None,
            },
        })
    }

    /// Check the request's media types against the agent's modes.
    fn check_modes(agent: &AgentDef, req: &SendRequest) -> Result<(), A2aError> {
        for p in &req.message.parts {
            let mt = p.effective_media_type();
            if !agent
                .input_modes
                .iter()
                .any(|m| agents::media_matches(&mt, m))
            {
                return Err(A2aError::new(
                    Kind::ContentTypeNotSupported,
                    format!(
                        "media type {mt} is not accepted by the {} agent (input modes: {})",
                        agent.id,
                        agent.input_modes.join(", ")
                    ),
                ));
            }
        }
        if !req.accepted_output_modes.is_empty()
            && !req.accepted_output_modes.iter().any(|w| {
                agent
                    .output_modes
                    .iter()
                    .any(|o| agents::media_matches(w, o))
            })
        {
            return Err(A2aError::new(
                Kind::ContentTypeNotSupported,
                format!(
                    "none of the accepted output modes is produced by the {} agent (output modes: {})",
                    agent.id,
                    agent.output_modes.join(", ")
                ),
            ));
        }
        Ok(())
    }

    /// SendMessage / SendStreamingMessage / message/send / message/stream.
    pub fn send(self: &Arc<Self>, caller: &Caller, req: SendRequest) -> Result<Started, A2aError> {
        Self::check_modes(caller.agent, &req)?;
        let mut message = req.message;
        match message.task_id.clone() {
            Some(task_id) => self.continue_task(caller, &task_id, message, req.push),
            None => {
                let context_id = message.context_id.clone().unwrap_or_else(new_id);
                let task_id = new_id();
                message.context_id = Some(context_id.clone());
                let mut map = self.lock();
                let turn = 1 + map
                    .values()
                    .filter(|e| {
                        e.owner == caller.owner
                            && e.agent == caller.agent.id
                            && e.task.context_id == context_id
                    })
                    .count();
                let plan = agents::plan(
                    caller.agent,
                    &PlanInput {
                        message: &message,
                        task_id: &task_id,
                        existing: None,
                        auth: caller.auth,
                        base_url: caller.base_url,
                        turn,
                        context_id: &context_id,
                    },
                );
                let steps = match plan {
                    Plan::Reply(parts) => {
                        drop(map);
                        let mut reply = Message::agent(parts, &context_id, None);
                        reply.reference_task_ids = message.reference_task_ids.clone();
                        return Ok(Started::Reply(reply));
                    }
                    Plan::Run(steps) => steps,
                };
                if self.running.load(Ordering::SeqCst) >= self.limits.max_running {
                    return Err(A2aError::new(
                        Kind::Internal,
                        "too many tasks are running on this server, retry later",
                    ));
                }
                let push = match req.push {
                    Some(p) => Some(self.make_push_config(caller, &task_id, p)?),
                    None => None,
                };
                message.task_id = Some(task_id.clone());
                let task = Task {
                    id: task_id.clone(),
                    context_id: context_id.clone(),
                    status: TaskStatus::new(TaskState::Submitted, None),
                    artifacts: Vec::new(),
                    history: vec![message],
                    metadata: None,
                };
                self.evict(&mut map, caller.owner);
                let (tx, rx) = broadcast::channel(CHANNEL_CAPACITY);
                map.insert(
                    task_id.clone(),
                    Entry {
                        owner: caller.owner.to_string(),
                        agent: caller.agent.id,
                        task: task.clone(),
                        touched: Instant::now(),
                        tx,
                        push: push.into_iter().collect(),
                        running: true,
                    },
                );
                drop(map);
                self.spawn_runner(task_id, steps);
                Ok(Started::Task { snapshot: task, rx })
            }
        }
    }

    fn continue_task(
        self: &Arc<Self>,
        caller: &Caller,
        task_id: &str,
        mut message: Message,
        push: Option<PushInput>,
    ) -> Result<Started, A2aError> {
        let push = match push {
            Some(p) => Some(self.make_push_config(caller, task_id, p)?),
            None => None,
        };
        let mut map = self.lock();
        let entry = self.visible(&mut map, caller, task_id)?;
        if let Some(ctx) = &message.context_id {
            if *ctx != entry.task.context_id {
                return Err(A2aError::invalid_params(format!(
                    "contextId {ctx} does not match the context of task {task_id}"
                )));
            }
        }
        let state = entry.task.status.state;
        if state.is_terminal() {
            return Err(A2aError::unsupported(format!(
                "task {task_id} is in terminal state {} and cannot accept further messages",
                state.v1()
            ))
            .with("taskId", task_id));
        }
        if let Some(p) = push {
            if entry.push.len() >= MAX_PUSH_CONFIGS {
                return Err(A2aError::invalid_params(
                    "too many push notification configs for this task",
                ));
            }
            entry.push.push(p);
        }
        message.context_id = Some(entry.task.context_id.clone());
        let previous = entry.task.clone();
        push_history(&mut entry.task, message.clone());
        entry.touched = Instant::now();
        let rx = entry.tx.subscribe();
        let snapshot = entry.task.clone();
        if entry.running {
            // A runner is already producing updates: just record the input.
            return Ok(Started::Task { snapshot, rx });
        }
        if self.running.load(Ordering::SeqCst) >= self.limits.max_running {
            return Err(A2aError::new(
                Kind::Internal,
                "too many tasks are running on this server, retry later",
            ));
        }
        let plan = agents::plan(
            caller.agent,
            &PlanInput {
                message: &message,
                task_id,
                existing: Some(&previous),
                auth: caller.auth,
                base_url: caller.base_url,
                turn: 1,
                context_id: &previous.context_id,
            },
        );
        let steps = match plan {
            Plan::Run(steps) => steps,
            Plan::Reply(parts) => vec![Step {
                delay_ms: 0,
                kind: StepKind::Status(
                    TaskState::Completed,
                    Some(Message::agent(parts, "", None).text()),
                ),
            }],
        };
        entry.running = true;
        drop(map);
        self.spawn_runner(task_id.to_string(), steps);
        Ok(Started::Task { snapshot, rx })
    }

    fn spawn_runner(self: &Arc<Self>, task_id: String, steps: Vec<Step>) {
        self.running.fetch_add(1, Ordering::SeqCst);
        let svc = Arc::clone(self);
        tokio::spawn(async move {
            let _guard = RunningGuard(&svc.running);
            for step in steps {
                if step.delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(step.delay_ms)).await;
                }
                let applied = svc.emit_step(&task_id, step.kind);
                if !applied {
                    break;
                }
            }
            if let Some(e) = svc.lock().get_mut(&task_id) {
                e.running = false;
            }
        });
    }

    fn emit_step(&self, task_id: &str, kind: StepKind) -> bool {
        let ev = {
            let map = self.lock();
            let Some(e) = map.get(task_id) else {
                return false;
            };
            match kind {
                StepKind::Status(state, text) => status_event(&e.task, state, text),
                StepKind::Artifact {
                    artifact,
                    append,
                    last_chunk,
                } => StreamEvent::Artifact(ArtifactUpdate {
                    task_id: e.task.id.clone(),
                    context_id: e.task.context_id.clone(),
                    artifact,
                    append,
                    last_chunk,
                }),
            }
        };
        self.emit(task_id, ev)
    }

    /// Apply, broadcast and push one update. Returns false when the task is
    /// gone or already terminal (the update is then dropped).
    pub fn emit(&self, task_id: &str, ev: StreamEvent) -> bool {
        let (configs, snapshot) = {
            let mut map = self.lock();
            let Some(e) = map.get_mut(task_id) else {
                return false;
            };
            if e.task.status.state.is_terminal() {
                return false;
            }
            apply(&mut e.task, &ev);
            e.touched = Instant::now();
            let _ = e.tx.send(ev.clone());
            (e.push.clone(), e.task.clone())
        };
        for cfg in &configs {
            let payload = match cfg.version {
                Version::V10 => wire::event(&ev, Wire::V1, TaskView::FULL),
                // v0.3 push notifications carry the whole Task object.
                Version::V03 => wire::task(&snapshot, Wire::V03Rpc, TaskView::FULL),
            };
            self.push.deliver(cfg, payload);
        }
        true
    }

    pub fn get(&self, caller: &Caller, id: &str) -> Result<Task, A2aError> {
        let mut map = self.lock();
        self.visible(&mut map, caller, id).map(|e| e.task.clone())
    }

    pub fn cancel(&self, caller: &Caller, id: &str) -> Result<Task, A2aError> {
        let ev = {
            let mut map = self.lock();
            let e = self.visible(&mut map, caller, id)?;
            let state = e.task.status.state;
            if state.is_terminal() {
                return Err(A2aError::new(
                    Kind::TaskNotCancelable,
                    format!("task {id} is already {} and cannot be canceled", state.v1()),
                )
                .with("taskId", id));
            }
            status_event(
                &e.task,
                TaskState::Canceled,
                Some("Canceled at the client's request".into()),
            )
        };
        if !self.emit(id, ev) {
            return Err(A2aError::new(
                Kind::TaskNotCancelable,
                "task finished before it could be canceled",
            ));
        }
        self.get(caller, id)
    }

    /// SubscribeToTask: current snapshot plus a live receiver.
    pub fn subscribe(
        &self,
        caller: &Caller,
        id: &str,
    ) -> Result<(Task, broadcast::Receiver<StreamEvent>), A2aError> {
        let mut map = self.lock();
        let e = self.visible(&mut map, caller, id)?;
        if e.task.status.state.is_terminal() {
            return Err(A2aError::unsupported(format!(
                "task {id} is in terminal state {}; subscriptions are only possible for active tasks",
                e.task.status.state.v1()
            ))
            .with("taskId", id));
        }
        Ok((e.task.clone(), e.tx.subscribe()))
    }

    pub fn list(&self, caller: &Caller, q: &ListQuery) -> Result<ListResult, A2aError> {
        let mut problems = Vec::new();
        let page_size = match q.page_size {
            None => 50,
            Some(n) if (1..=100).contains(&n) => n as usize,
            Some(n) => {
                problems.push(format!(
                    "pageSize must be between 1 and 100 inclusive, got {n}"
                ));
                50
            }
        };
        let history_length = match wire::parse_history_length(q.history_length) {
            Ok(h) => h,
            Err(e) => {
                problems.push(e);
                None
            }
        };
        let after = match &q.status_timestamp_after {
            None => None,
            Some(s) => match chrono::DateTime::parse_from_rfc3339(s) {
                Ok(t) => Some(t),
                Err(_) => {
                    problems.push(format!(
                        "statusTimestampAfter must be an ISO 8601 timestamp, got {s:?}"
                    ));
                    None
                }
            },
        };
        let offset = match &q.page_token {
            None => 0,
            Some(t) if t.is_empty() => 0,
            Some(t) => match decode_page_token(t) {
                Some(n) => n,
                None => {
                    problems.push("pageToken is invalid".to_string());
                    0
                }
            },
        };
        if !problems.is_empty() {
            return Err(A2aError::invalid_params(format!(
                "Invalid parameters: {}",
                problems.join("; ")
            )));
        }
        let map = self.lock();
        let mut tasks: Vec<Task> = map
            .values()
            .filter(|e| e.owner == caller.owner && e.agent == caller.agent.id)
            .filter(|e| e.touched.elapsed() < self.limits.ttl)
            .filter(|e| {
                q.context_id
                    .as_ref()
                    .is_none_or(|c| *c == e.task.context_id)
            })
            .filter(|e| q.status.is_none_or(|s| s == e.task.status.state))
            .filter(|e| {
                after.is_none_or(|a| {
                    chrono::DateTime::parse_from_rfc3339(&e.task.status.timestamp)
                        .map(|t| t >= a)
                        .unwrap_or(false)
                })
            })
            .map(|e| e.task.clone())
            .collect();
        drop(map);
        tasks.sort_by(|a, b| {
            b.status
                .timestamp
                .cmp(&a.status.timestamp)
                .then_with(|| a.id.cmp(&b.id))
        });
        let total = tasks.len();
        let page: Vec<Task> = tasks.into_iter().skip(offset).take(page_size).collect();
        let next = if offset + page.len() < total {
            encode_page_token(offset + page.len())
        } else {
            String::new()
        };
        Ok(ListResult {
            tasks: page,
            next_page_token: next,
            page_size,
            total_size: total,
            view: TaskView {
                history_length,
                include_artifacts: q.include_artifacts,
            },
        })
    }

    // ── Push config CRUD ────────────────────────────────────────────

    pub fn push_create(
        &self,
        caller: &Caller,
        task_id: &str,
        input: PushInput,
    ) -> Result<PushConfig, A2aError> {
        let cfg = self.make_push_config(caller, task_id, input)?;
        let mut map = self.lock();
        let e = self.visible(&mut map, caller, task_id)?;
        if let Some(existing) = e.push.iter_mut().find(|c| c.id == cfg.id) {
            *existing = cfg.clone();
            return Ok(cfg);
        }
        if e.push.len() >= MAX_PUSH_CONFIGS {
            return Err(A2aError::invalid_params(format!(
                "at most {MAX_PUSH_CONFIGS} push notification configs per task"
            )));
        }
        e.push.push(cfg.clone());
        Ok(cfg)
    }

    /// `config_id` None returns the first config (v0.3 tasks/pushNotificationConfig/get without an id).
    pub fn push_get(
        &self,
        caller: &Caller,
        task_id: &str,
        config_id: Option<&str>,
    ) -> Result<PushConfig, A2aError> {
        let mut map = self.lock();
        let e = self.visible(&mut map, caller, task_id)?;
        let found = match config_id {
            Some(id) => e.push.iter().find(|c| c.id == id),
            None => e.push.first(),
        };
        found.cloned().ok_or_else(|| {
            A2aError::new(Kind::TaskNotFound, "push notification config not found")
                .with("taskId", task_id)
                .with("configId", config_id.unwrap_or(""))
        })
    }

    pub fn push_list(&self, caller: &Caller, task_id: &str) -> Result<Vec<PushConfig>, A2aError> {
        let mut map = self.lock();
        self.visible(&mut map, caller, task_id)
            .map(|e| e.push.clone())
    }

    /// Idempotent delete.
    pub fn push_delete(
        &self,
        caller: &Caller,
        task_id: &str,
        config_id: &str,
    ) -> Result<(), A2aError> {
        let mut map = self.lock();
        let e = self.visible(&mut map, caller, task_id)?;
        e.push.retain(|c| c.id != config_id);
        Ok(())
    }
}

fn encode_page_token(offset: usize) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("offset:{offset}"))
}

fn decode_page_token(t: &str) -> Option<usize> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(t)
        .ok()?;
    let s = String::from_utf8(bytes).ok()?;
    s.strip_prefix("offset:")?.parse().ok()
}

/// Wait until `rx` delivers an event that ends a stream (terminal or
/// interrupted state), with an upper bound.
pub async fn wait_until_settled(rx: &mut broadcast::Receiver<StreamEvent>, limit: Duration) {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(ev)) if ev.ends_stream() => return,
            Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => return,
        }
    }
}

/// Raw value helper used by the bindings for push config output lists.
pub fn configs_json(configs: &[PushConfig], w: Wire) -> Vec<Value> {
    configs.iter().map(|c| wire::push_config(c, w)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a::push::{PushPolicy, WebhookSink};

    fn svc(limits: Limits) -> Arc<Service> {
        Arc::new(Service::new(
            limits,
            PushService::new(
                PushPolicy::default(),
                Arc::new(WebhookSink::new(10, 10, Duration::from_secs(60))),
            ),
        ))
    }

    fn caller<'a>(owner: &'a str, agent: &'static str, auth: &'a AuthOutcome) -> Caller<'a> {
        Caller {
            owner,
            agent: agents::find(agent).expect("agent"),
            base_url: "http://h",
            authority: "h",
            auth,
            version: Version::V10,
        }
    }

    fn req(text: &str) -> SendRequest {
        wire::parse_send(
            &serde_json::json!({"message": {"messageId": new_id(), "role": "ROLE_USER", "parts": [{"text": text}]}}),
            Wire::V1,
        )
        .expect("request")
    }

    #[tokio::test]
    async fn store_is_bounded_per_session_and_globally() {
        let s = svc(Limits {
            max_tasks: 5,
            max_tasks_per_session: 3,
            ttl: Duration::from_secs(60),
            max_running: 100,
        });
        let auth = AuthOutcome::Missing;
        for i in 0..6 {
            let owner = if i % 2 == 0 { "a" } else { "b" };
            let c = caller(owner, "reject", &auth);
            assert!(matches!(s.send(&c, req("x")), Ok(Started::Task { .. })));
        }
        assert!(s.task_count() <= 5);
        for i in 0..10 {
            let _ = i;
            let c = caller("a", "reject", &auth);
            let _ = s.send(&c, req("x"));
        }
        let c = caller("a", "reject", &auth);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let listed = s.list(&c, &ListQuery::default()).expect("list");
        assert!(listed.total_size <= 3, "per-session cap");
    }

    #[tokio::test]
    async fn tasks_are_scoped_to_owner_and_agent() {
        let s = svc(Limits::for_mode(false));
        let auth = AuthOutcome::Missing;
        let c = caller("a", "reject", &auth);
        let Ok(Started::Task { snapshot, .. }) = s.send(&c, req("x")) else {
            panic!("task expected");
        };
        assert!(s.get(&c, &snapshot.id).is_ok());
        let other = caller("b", "reject", &auth);
        assert_eq!(
            s.get(&other, &snapshot.id).map_err(|e| e.kind),
            Err(Kind::TaskNotFound)
        );
        let other_agent = caller("a", "echo", &auth);
        assert_eq!(
            s.get(&other_agent, &snapshot.id).map_err(|e| e.kind),
            Err(Kind::TaskNotFound)
        );
    }

    #[test]
    fn page_tokens_round_trip() {
        assert_eq!(decode_page_token(&encode_page_token(42)), Some(42));
        assert_eq!(decode_page_token("garbage!"), None);
    }
}
