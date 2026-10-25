//! Stateful mode: an in-memory CRUD store behind the mock, keyed by the
//! request path. `POST /pets` stores the item, `GET /pets/{id}` returns it,
//! `PUT` / `PATCH` change it, `DELETE` removes it and `GET /pets` lists the
//! collection. A collection starts with the items of its list operation's
//! example (the `GET` on the collection path), or empty.
//!
//! Bounded by a total item count and byte budget per mock: the oldest items
//! are evicted first. Operations that do not look like CRUD (a `POST` on an
//! item, a path ending in a literal action) answer as in `examples` mode.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use super::compile::Compiled;
use super::model::ResponseMode;
use super::respond::{self, Body, BodyChoice, Outgoing, RequestView};
use super::spec::Operation;

/// Size of a stored item (its compact JSON).
fn item_bytes(v: &Value) -> usize {
    serde_json::to_vec(v).map_or(0, |b| b.len())
}

#[derive(Default)]
struct Collection {
    /// id -> (insertion sequence, item).
    items: BTreeMap<String, (u64, Value)>,
}

#[derive(Default)]
struct Inner {
    collections: BTreeMap<String, Collection>,
    items: usize,
    bytes: usize,
    next_seq: u64,
}

/// The state of one mock. Cheap to clone; shared by all its revisions.
#[derive(Clone)]
pub struct MockState {
    inner: Arc<Mutex<Inner>>,
    max_items: usize,
    max_bytes: usize,
}

impl MockState {
    pub fn new(max_items: usize, max_bytes: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            max_items,
            max_bytes,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Items of a collection in insertion order; `None` when it was never used.
    fn list(&self, key: &str) -> Option<Vec<Value>> {
        let inner = self.lock();
        let col = inner.collections.get(key)?;
        let mut items: Vec<&(u64, Value)> = col.items.values().collect();
        items.sort_by_key(|(seq, _)| *seq);
        Some(items.into_iter().map(|(_, v)| v.clone()).collect())
    }

    fn known(&self, key: &str) -> bool {
        self.lock().collections.contains_key(key)
    }

    fn get(&self, key: &str, id: &str) -> Option<Value> {
        let inner = self.lock();
        inner
            .collections
            .get(key)?
            .items
            .get(id)
            .map(|(_, v)| v.clone())
    }

    /// Mark a collection as used, with its first items.
    fn seed(&self, key: &str, items: Vec<(String, Value)>) {
        if self.known(key) {
            return;
        }
        self.open(&mut self.lock(), key);
        for (id, v) in items {
            let _ = self.put(key, &id, v);
        }
    }

    /// Insert or replace an item. Refused when the item alone exceeds the
    /// byte budget; otherwise the oldest items make room.
    fn put(&self, key: &str, id: &str, value: Value) -> Result<(), String> {
        let size = item_bytes(&value) + key.len() + id.len();
        if size > self.max_bytes {
            return Err(format!(
                "the item is larger than this mock's state budget ({} bytes)",
                self.max_bytes
            ));
        }
        let mut inner = self.lock();
        let old = inner
            .collections
            .get_mut(key)
            .and_then(|c| c.items.remove(id));
        if let Some((_, v)) = old {
            inner.items -= 1;
            inner.bytes = inner
                .bytes
                .saturating_sub(item_bytes(&v) + key.len() + id.len());
        }
        while inner.items > 0
            && (inner.items >= self.max_items || inner.bytes + size > self.max_bytes)
        {
            if !evict_oldest(&mut inner) {
                break;
            }
        }
        self.open(&mut inner, key);
        let seq = inner.next_seq;
        inner.next_seq += 1;
        inner
            .collections
            .entry(key.to_string())
            .or_default()
            .items
            .insert(id.to_string(), (seq, value));
        inner.items += 1;
        inner.bytes += size;
        Ok(())
    }

    /// Create the collection `key` (no-op when it exists). Collections are
    /// keyed by request path, so they count toward the budget too: their key
    /// bytes, and at most `max_items` of them. Room is made by dropping empty
    /// collections first, then the oldest items.
    fn open(&self, inner: &mut Inner, key: &str) {
        if inner.collections.contains_key(key) {
            return;
        }
        while !inner.collections.is_empty()
            && (inner.collections.len() >= self.max_items
                || inner.bytes + key.len() > self.max_bytes)
        {
            if !drop_one(inner) {
                break;
            }
        }
        inner
            .collections
            .insert(key.to_string(), Collection::default());
        inner.bytes += key.len();
    }

    fn remove(&self, key: &str, id: &str) -> Option<Value> {
        let mut inner = self.lock();
        let (_, v) = inner.collections.get_mut(key)?.items.remove(id)?;
        inner.items -= 1;
        inner.bytes = inner
            .bytes
            .saturating_sub(item_bytes(&v) + key.len() + id.len());
        Some(v)
    }

    /// Ids already used in a collection.
    fn ids(&self, key: &str) -> Vec<String> {
        self.lock()
            .collections
            .get(key)
            .map(|c| c.items.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn clear(&self) {
        *self.lock() = Inner::default();
    }

    /// Every collection with its items (bounded by the budget).
    pub fn dump(&self) -> Value {
        let inner = self.lock();
        let collections: Map<String, Value> = inner
            .collections
            .iter()
            .map(|(k, c)| {
                let mut items: Vec<&(u64, Value)> = c.items.values().collect();
                items.sort_by_key(|(seq, _)| *seq);
                (
                    k.clone(),
                    Value::Array(items.into_iter().map(|(_, v)| v.clone()).collect()),
                )
            })
            .collect();
        json!({
            "items": inner.items,
            "bytes": inner.bytes,
            "max_items": self.max_items,
            "max_bytes": self.max_bytes,
            "collections": collections,
        })
    }

    pub fn item_count(&self) -> usize {
        self.lock().items
    }
}

/// Drop an empty collection, else the oldest item (whose collection the next
/// call may then drop). False when there is nothing left to drop.
fn drop_one(inner: &mut Inner) -> bool {
    let empty = inner
        .collections
        .iter()
        .find(|(_, c)| c.items.is_empty())
        .map(|(k, _)| k.clone());
    if let Some(key) = empty {
        inner.collections.remove(&key);
        inner.bytes = inner.bytes.saturating_sub(key.len());
        return true;
    }
    evict_oldest(inner)
}

fn evict_oldest(inner: &mut Inner) -> bool {
    let oldest = inner
        .collections
        .iter()
        .flat_map(|(k, c)| c.items.iter().map(move |(id, (seq, _))| (*seq, k, id)))
        .min_by_key(|(seq, _, _)| *seq)
        .map(|(_, k, id)| (k.clone(), id.clone()));
    let Some((key, id)) = oldest else {
        return false;
    };
    if let Some((_, v)) = inner
        .collections
        .get_mut(&key)
        .and_then(|c| c.items.remove(&id))
    {
        inner.items -= 1;
        inner.bytes = inner
            .bytes
            .saturating_sub(item_bytes(&v) + key.len() + id.len());
    }
    true
}

/// The path parameter that names an item (`/pets/{petId}` -> `petId`).
fn item_param(template: &str) -> Option<&str> {
    let last = template.trim_end_matches('/').rsplit('/').next()?;
    last.strip_prefix('{')?.strip_suffix('}')
}

/// A path without its last segment (`/v1/pets/7` -> `/v1/pets`).
fn parent(path: &str) -> &str {
    let p = path.trim_end_matches('/');
    match p.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &p[..i],
    }
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The property that holds an item's id.
fn id_field(sample: Option<&Value>, param: Option<&str>) -> String {
    let Some(Value::Object(m)) = sample else {
        return "id".into();
    };
    if m.contains_key("id") {
        return "id".into();
    }
    if let Some(p) = param.filter(|p| m.contains_key(*p)) {
        return p.to_string();
    }
    m.keys()
        .find(|k| {
            let l = k.to_ascii_lowercase();
            l.ends_with("id") || l == "uuid" || l == "key"
        })
        .cloned()
        .unwrap_or_else(|| "id".into())
}

/// The default response body of an operation (as in `examples` mode).
fn default_body(c: &Compiled, op: &Operation, status: u16) -> Option<Value> {
    let choice = BodyChoice {
        body: None,
        example: None,
        mode: ResponseMode::Examples,
    };
    let (_, body, _) = respond::render_body(c, op, status, choice, None, &[]);
    body.as_ref().and_then(Body::as_json)
}

/// The items array of a list response (the body itself, or its first array
/// property).
fn items_of(body: &Value) -> Option<&Vec<Value>> {
    match body {
        Value::Array(a) => Some(a),
        Value::Object(m) => m.values().find_map(Value::as_array),
        _ => None,
    }
}

/// The list operation of a collection template.
fn list_op<'c>(c: &'c Compiled, template: &str) -> Option<&'c Operation> {
    c.spec
        .operations
        .iter()
        .find(|o| o.method == "GET" && o.path.trim_end_matches('/') == template)
}

/// Seed a collection from the list operation's example the first time it is
/// used.
fn ensure_seeded(state: &MockState, c: &Compiled, key: &str, list: Option<&Operation>) {
    if state.known(key) {
        return;
    }
    let mut items = Vec::new();
    if let Some(op) = list {
        if let Some(body) = default_body(c, op, op.default_status()) {
            if let Some(list) = items_of(&body) {
                let field = id_field(list.first(), None);
                for (i, item) in list.iter().enumerate() {
                    if !item.is_object() {
                        continue;
                    }
                    let mut item = item.clone();
                    let id = match item.get(&field) {
                        Some(v) if !v.is_null() => text_of(v),
                        _ => {
                            let id = (i + 1).to_string();
                            if let Value::Object(m) = &mut item {
                                m.insert(field.clone(), json!(i + 1));
                            }
                            id
                        }
                    };
                    items.push((id, item));
                }
            }
        }
    }
    state.seed(key, items);
}

fn json_out(status: u16, body: Option<Value>, action: &str, delay_ms: u64) -> Outgoing {
    let (content_type, body) = match body {
        Some(v) if status != 204 => (Some("application/json".to_string()), Some(Body::Json(v))),
        _ => (None, None),
    };
    Outgoing {
        status,
        headers: Vec::new(),
        content_type,
        body,
        source: format!("state {action}"),
        delay_ms,
    }
}

/// 404 for a missing item: the documented 404 body, else problem+json.
fn missing(c: &Compiled, op: &Operation, id: &str, delay_ms: u64) -> Outgoing {
    if op.response_for(404).is_some_and(|r| r.code() == Some(404)) {
        let body = default_body(c, op, 404);
        return json_out(404, body, "miss", delay_ms);
    }
    let mut out = json_out(
        404,
        Some(respond::problem(
            404,
            &format!("no item with id {id:?} in this mock's state"),
        )),
        "miss",
        delay_ms,
    );
    out.content_type = Some("application/problem+json".into());
    out
}

/// A new id: the next integer when the collection uses integers (and the
/// schema does not make ids strings), else a UUID.
fn new_id(state: &MockState, key: &str, template: Option<&Value>) -> Value {
    let ids = state.ids(key);
    let numeric =
        !matches!(template, Some(Value::String(_))) && ids.iter().all(|i| i.parse::<i64>().is_ok());
    if numeric {
        let max = ids.iter().filter_map(|i| i.parse::<i64>().ok()).max();
        // A client may have stored `i64::MAX` itself: then fall back to a UUID.
        if let Some(next) = max.unwrap_or(0).checked_add(1) {
            return json!(next);
        }
    }
    json!(uuid::Uuid::new_v4().to_string())
}

/// Keep the id's JSON type in line with the stored items.
fn typed_id(id: &str, like: Option<&Value>) -> Value {
    match like {
        Some(Value::Number(_)) => id
            .parse::<i64>()
            .map(|n| json!(n))
            .unwrap_or_else(|_| json!(id)),
        _ => json!(id),
    }
}

/// Answer a CRUD-shaped operation from the state; `None` means the
/// operation is not CRUD-shaped (answer it normally).
pub fn respond(
    c: &Compiled,
    op: &Operation,
    req: &RequestView,
    path: &str,
    state: &MockState,
    delay_ms: u64,
) -> Option<Outgoing> {
    let path = match path.trim_end_matches('/') {
        "" => "/",
        p => p,
    };
    let template = op.path.trim_end_matches('/');
    let status = op.default_status();
    match (op.method.as_str(), item_param(template)) {
        ("GET", None) => {
            ensure_seeded(state, c, path, Some(op));
            let items = state.list(path).unwrap_or_default();
            let total = items.len();
            let num = |k: &str| {
                req.query
                    .iter()
                    .find(|(n, _)| n == k)
                    .and_then(|(_, v)| v.parse::<usize>().ok())
            };
            let offset = num("offset").unwrap_or(0);
            let page: Vec<Value> = match num("limit") {
                Some(limit) => items.into_iter().skip(offset).take(limit).collect(),
                None => items.into_iter().skip(offset).collect(),
            };
            let body = match default_body(c, op, status) {
                Some(Value::Object(mut m)) => {
                    match m.iter_mut().find(|(_, v)| v.is_array()) {
                        Some((_, v)) => *v = Value::Array(page),
                        None => return None,
                    }
                    for key in ["total", "count", "total_count", "totalCount"] {
                        if m.get(key).is_some_and(Value::is_number) {
                            m.insert(key.into(), json!(total));
                        }
                    }
                    Value::Object(m)
                }
                _ => Value::Array(page),
            };
            Some(json_out(status, Some(body), "list", delay_ms))
        }
        ("POST", None) => {
            let Some(Value::Object(fields)) = req.body_json else {
                return None;
            };
            ensure_seeded(state, c, path, list_op(c, template));
            let base = default_body(c, op, status);
            let mut item = match base {
                Some(Value::Object(m)) => m,
                _ => Map::new(),
            };
            let sample = Value::Object(item.clone());
            let field = id_field(Some(&sample), None);
            let template_id = item.get(&field).cloned();
            for (k, v) in fields {
                item.insert(k.clone(), v.clone());
            }
            let id = match fields.get(&field) {
                Some(v) if !v.is_null() => v.clone(),
                _ => new_id(state, path, template_id.as_ref()),
            };
            item.insert(field, id.clone());
            let item = Value::Object(item);
            if let Err(e) = state.put(path, &text_of(&id), item.clone()) {
                let mut out = json_out(507, Some(respond::problem(507, &e)), "full", delay_ms);
                out.content_type = Some("application/problem+json".into());
                return Some(out);
            }
            Some(json_out(status, Some(item), "create", delay_ms))
        }
        (method, Some(param)) => {
            let id = req
                .path_params
                .iter()
                .find(|(k, _)| k == param)
                .map(|(_, v)| v.clone())?;
            let key = parent(path);
            let parent_template = parent(template);
            ensure_seeded(state, c, key, list_op(c, parent_template));
            let existing = state.get(key, &id);
            match method {
                "GET" => Some(match existing {
                    Some(item) => json_out(status, Some(item), "read", delay_ms),
                    None => missing(c, op, &id, delay_ms),
                }),
                "PUT" | "PATCH" => {
                    let Some(Value::Object(fields)) = req.body_json else {
                        return None;
                    };
                    if method == "PATCH" && existing.is_none() {
                        return Some(missing(c, op, &id, delay_ms));
                    }
                    let like = state.list(key).and_then(|l| l.into_iter().next());
                    let field = id_field(existing.as_ref().or(like.as_ref()), Some(param));
                    let mut item = match (method, existing) {
                        ("PATCH", Some(Value::Object(m))) => m,
                        _ => Map::new(),
                    };
                    for (k, v) in fields {
                        item.insert(k.clone(), v.clone());
                    }
                    let id_like = like.as_ref().and_then(|l| l.get(&field));
                    item.insert(field, typed_id(&id, id_like));
                    let item = Value::Object(item);
                    if let Err(e) = state.put(key, &id, item.clone()) {
                        let mut out =
                            json_out(507, Some(respond::problem(507, &e)), "full", delay_ms);
                        out.content_type = Some("application/problem+json".into());
                        return Some(out);
                    }
                    Some(json_out(status, Some(item), "update", delay_ms))
                }
                "DELETE" => Some(match state.remove(key, &id) {
                    Some(item) => {
                        let documented = op
                            .response_for(status)
                            .is_some_and(|r| !r.content.is_empty());
                        json_out(status, documented.then_some(item), "delete", delay_ms)
                    }
                    None => missing(c, op, &id, delay_ms),
                }),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::compile::tests::compile;
    use crate::mocks::model::MockSettings;
    use axum::http::HeaderMap;

    const SPEC: &str = r##"{
      "openapi": "3.0.3", "info": {"title": "t", "version": "1"},
      "paths": {
        "/pets": {
          "get": {"operationId": "list", "responses": {"200": {"description": "ok", "content": {"application/json":
            {"schema": {"type": "array", "items": {"$ref": "#/components/schemas/Pet"}},
             "example": [{"id": 1, "name": "Rex"}, {"id": 2, "name": "Tom"}]}}}}},
          "post": {"operationId": "create", "responses": {"201": {"description": "ok", "content": {"application/json":
            {"schema": {"$ref": "#/components/schemas/Pet"}}}}}}
        },
        "/pets/{petId}": {
          "parameters": [{"name": "petId", "in": "path", "required": true, "schema": {"type": "integer"}}],
          "get": {"operationId": "read", "responses": {"200": {"description": "ok", "content": {"application/json":
            {"schema": {"$ref": "#/components/schemas/Pet"}}}}}},
          "patch": {"operationId": "patch", "responses": {"200": {"description": "ok", "content": {"application/json":
            {"schema": {"$ref": "#/components/schemas/Pet"}}}}}},
          "delete": {"operationId": "remove", "responses": {"204": {"description": "gone"}}}
        },
        "/pets/{petId}/feed": {"post": {"operationId": "feed", "responses": {"200": {"description": "ok"}}}},
        "/pages": {"get": {"operationId": "pages", "responses": {"200": {"description": "ok", "content": {"application/json":
            {"schema": {"type": "object", "properties": {"total": {"type": "integer"}, "data": {"type": "array", "items": {"type": "object"}}}},
             "example": {"total": 9, "data": [{"key": "a"}]}}}}}}}
      },
      "components": {"schemas": {"Pet": {"type": "object", "required": ["id", "name"],
        "properties": {"id": {"type": "integer", "readOnly": true}, "name": {"type": "string"}}}}}
    }"##;

    fn call(
        c: &Compiled,
        state: &MockState,
        op: &str,
        path: &str,
        params: &[(&str, &str)],
        body: Option<Value>,
    ) -> Option<Outgoing> {
        let op = c.spec.operation(op).expect("op");
        let params: Vec<(String, String)> = params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let headers = HeaderMap::new();
        let req = RequestView {
            path_params: &params,
            query: &[],
            headers: &headers,
            body_json: body.as_ref(),
            body_text: None,
        };
        respond(c, op, &req, path, state, 0)
    }

    fn body(out: &Outgoing) -> Value {
        out.body
            .as_ref()
            .and_then(Body::as_json)
            .unwrap_or(Value::Null)
    }

    #[test]
    fn collections_per_path_stay_within_the_budget() {
        // Stateful data is keyed by request path, so a parameter above the
        // collection (`/u/{uid}/pets`) opens one collection per URL.
        let s = MockState::new(50, 100_000);
        for n in 0..1_400 {
            let key = format!("/u/user{n}/pets");
            s.seed(&key, vec![]);
            if n % 3 == 0 {
                s.put(&key, "1", json!({"id": 1})).expect("put");
            }
        }
        let dump = s.dump();
        let collections = dump["collections"].as_object().map_or(0, |c| c.len());
        assert!(collections <= 50, "{collections} collections");
        assert!(s.item_count() <= 50);
        assert!(
            dump["bytes"].as_u64().unwrap_or(u64::MAX) <= 100_000,
            "{dump}"
        );
        // The newest collection is the one kept.
        assert!(dump["collections"].get("/u/user1399/pets").is_some());
    }

    #[test]
    fn next_id_after_the_largest_integer_falls_back_to_a_uuid() {
        let c = compile(SPEC, MockSettings::default());
        let s = MockState::new(100, 100_000);
        let max = json!({"id": i64::MAX, "name": "Max"});
        call(&c, &s, "create", "/pets", &[], Some(max)).expect("create max");
        let next = call(
            &c,
            &s,
            "create",
            "/pets",
            &[],
            Some(json!({"name": "Next"})),
        )
        .expect("create");
        assert_eq!(next.status, 201);
        assert!(body(&next)["id"].is_string(), "{}", body(&next));
    }

    #[test]
    fn crud_round_trip() {
        let c = compile(SPEC, MockSettings::default());
        let s = MockState::new(100, 100_000);
        let list = call(&c, &s, "list", "/pets", &[], None).expect("list");
        assert_eq!(body(&list).as_array().map(Vec::len), Some(2));

        let created =
            call(&c, &s, "create", "/pets", &[], Some(json!({"name": "Bo"}))).expect("create");
        assert_eq!(created.status, 201);
        assert_eq!(body(&created), json!({"id": 3, "name": "Bo"}));

        let read = call(&c, &s, "read", "/pets/3", &[("petId", "3")], None).expect("read");
        assert_eq!(body(&read)["name"], "Bo");

        let patched = call(
            &c,
            &s,
            "patch",
            "/pets/3",
            &[("petId", "3")],
            Some(json!({"name": "Bob"})),
        )
        .expect("patch");
        assert_eq!(body(&patched), json!({"id": 3, "name": "Bob"}));

        let gone = call(&c, &s, "remove", "/pets/3", &[("petId", "3")], None).expect("delete");
        assert_eq!(gone.status, 204);
        assert!(gone.body.is_none());
        let miss = call(&c, &s, "read", "/pets/3", &[("petId", "3")], None).expect("miss");
        assert_eq!(miss.status, 404);
        assert_eq!(miss.source, "state miss");

        // Not CRUD-shaped: answered normally.
        assert!(call(&c, &s, "feed", "/pets/1/feed", &[("petId", "1")], None).is_none());

        s.clear();
        assert_eq!(s.item_count(), 0);
    }

    #[test]
    fn wrapped_lists_and_budget() {
        let c = compile(SPEC, MockSettings::default());
        let s = MockState::new(2, 100_000);
        let page = call(&c, &s, "pages", "/pages", &[], None).expect("pages");
        assert_eq!(body(&page), json!({"total": 1, "data": [{"key": "a"}]}));
        call(&c, &s, "create", "/pets", &[], Some(json!({"name": "A"}))).expect("a");
        call(&c, &s, "create", "/pets", &[], Some(json!({"name": "B"}))).expect("b");
        assert_eq!(s.item_count(), 2);
        let dump = s.dump();
        let pets = dump["collections"]["/pets"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(pets.len(), 2);
        assert_eq!(pets[1]["name"], "B");
        let tiny = MockState::new(10, 10);
        assert!(tiny.put("/x", "1", json!({"long": "value"})).is_err());
    }

    #[test]
    fn helpers() {
        assert_eq!(item_param("/pets/{petId}"), Some("petId"));
        assert_eq!(item_param("/pets"), None);
        assert_eq!(parent("/v1/pets/7"), "/v1/pets");
        assert_eq!(parent("/pets"), "/");
        assert_eq!(id_field(Some(&json!({"petId": 1})), Some("petId")), "petId");
        assert_eq!(
            id_field(Some(&json!({"sku": 1, "orderId": 2})), None),
            "orderId"
        );
        assert_eq!(id_field(None, None), "id");
    }
}
