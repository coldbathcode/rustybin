//! Testing tools: `try_request` (an in-process request through the whole
//! application) and `issue_token` (a token from the built-in identity
//! provider), plus the in-process request helper the traffic tools use.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, HeaderName, HeaderValue, Method, Request, StatusCode};
use base64::Engine;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::access::Access;
use super::tools::{arg_str, err, need_str, ok, truncate, untrusted_headers, MAX_TRY_BODY};
use crate::mcp::core::CallCtx;

/// Wall time an in-process request may take (headers plus body).
const TIMEOUT: Duration = Duration::from_secs(30);
/// How long a streaming body is read before it is cut.
const STREAM_CAP: Duration = Duration::from_secs(5);

/// What an in-process request answered.
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub content_type: Option<String>,
    /// The body as text (lossy for binary), cut at the cap.
    pub body: String,
    pub body_bytes: usize,
    pub truncated: bool,
    pub elapsed_ms: u64,
    pub session: String,
}

impl Answer {
    /// The body parsed as JSON when it is.
    pub fn json(&self) -> Option<Value> {
        let ct = self.content_type.as_deref().unwrap_or("");
        if !self.truncated
            && (ct.contains("json") || self.body.trim_start().starts_with(['{', '[']))
        {
            serde_json::from_str(&self.body).ok()
        } else {
            None
        }
    }

    /// `untrusted_*` fields of a result.
    pub fn untrusted(&self) -> Value {
        let mut v = json!({
            "status": self.status,
            "content_type": self.content_type,
            "body_bytes": self.body_bytes,
            "body_truncated": self.truncated,
            "elapsed_ms": self.elapsed_ms,
            "session": self.session,
            "untrusted_headers": untrusted_headers(&self.headers),
            "untrusted_body": self.body,
        });
        if let Some(j) = self.json() {
            v["untrusted_body_json"] = j;
        }
        v
    }
}

/// Build and send a request through the application, in process. The
/// caller's API key is forwarded (unless the request sets it), with
/// `as_admin` also the caller's admin credential ([`Access::admin_credential`]), and
/// `X-Rustybin-Session` is set to the caller's test session (unless given),
/// so the call is captured like any other and attributable.
#[allow(clippy::too_many_arguments)]
pub async fn in_process(
    ctx: &CallCtx,
    access: &Access,
    method: Method,
    path_and_query: &str,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    session: Option<&str>,
    as_admin: bool,
) -> Result<Answer, Value> {
    let Some(router) = ctx.app.router.get().cloned() else {
        return Err(err(
            "in-process requests are not available on this instance (no application router)",
        ));
    };
    if !path_and_query.starts_with('/') || path_and_query.starts_with("//") {
        return Err(err(
            "path must start with / (a path on this instance, never a URL)",
        ));
    }
    let path_only = path_and_query.split('?').next().unwrap_or("");
    if crate::control::is_control_path(path_only) {
        return Err(err(
            "control-plane paths (/_rustybin/*, /ui/*) are not reachable through try_request: \
             use the management tools instead",
        ));
    }
    let mut b = Request::builder().method(method).uri(path_and_query);
    let mut set: Vec<HeaderName> = Vec::new();
    for (name, value) in &headers {
        let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) else {
            return Err(err(format!("invalid header {name:?}")));
        };
        if matches!(
            n.as_str(),
            "content-length" | "transfer-encoding" | "connection" | "host" | "upgrade"
        ) {
            continue;
        }
        set.push(n.clone());
        b = b.header(n, v);
    }
    for (n, v) in &access.forward {
        if !set.contains(n) {
            b = b.header(n.clone(), v.clone());
        }
    }
    if as_admin {
        if let Some(v) = &access.admin_credential {
            b = b.header(crate::admin::ADMIN_TOKEN_HEADER, v.clone());
        }
    }
    let session_value = session
        .map(str::to_string)
        .unwrap_or_else(|| access.test_session());
    if !crate::session::is_valid_session(&session_value) {
        return Err(err(
            "invalid session value (1 to 128 characters of A-Z a-z 0-9 . _ : -)",
        ));
    }
    if !set.iter().any(|n| n == "x-rustybin-session") {
        b = b.header("x-rustybin-session", &session_value);
    }
    let mut req = b
        .body(match body {
            Some(bytes) => Body::from(bytes),
            None => Body::empty(),
        })
        .map_err(|e| err(format!("invalid request: {e}")))?;
    // A peer address for the handlers that need one (never a real socket).
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))));

    let started = Instant::now();
    let resp = match tokio::time::timeout(TIMEOUT, router.oneshot(req)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return Err(err(format!("the request failed: {e}"))),
        Err(_) => {
            return Err(err(format!(
                "no response headers within {} seconds",
                TIMEOUT.as_secs()
            )))
        }
    };
    let status = resp.status().as_u16();
    let headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut body = resp.into_body();
    let mut collected: Vec<u8> = Vec::new();
    let mut total = 0usize;
    let mut truncated = false;
    let deadline = tokio::time::Instant::now() + STREAM_CAP.min(TIMEOUT);
    loop {
        let next = tokio::select! {
            f = body.frame() => f,
            _ = tokio::time::sleep_until(deadline) => { truncated = true; break; }
        };
        match next {
            Some(Ok(frame)) => {
                if let Some(data) = frame.data_ref() {
                    total += data.len();
                    if collected.len() < MAX_TRY_BODY {
                        let room = MAX_TRY_BODY - collected.len();
                        collected.extend_from_slice(&data[..data.len().min(room)]);
                    }
                    if total > MAX_TRY_BODY {
                        truncated = true;
                        break;
                    }
                }
            }
            Some(Err(_)) | None => break,
        }
    }
    drop(body);
    Ok(Answer {
        status,
        headers,
        content_type,
        body: String::from_utf8_lossy(&collected).into_owned(),
        body_bytes: total,
        truncated,
        elapsed_ms: started.elapsed().as_millis() as u64,
        session: session_value,
    })
}

/// A request body argument: a string verbatim, any other JSON serialised
/// (with `Content-Type: application/json` unless set).
pub fn body_arg(args: &Value, key: &str, headers: &mut Vec<(String, String)>) -> Option<Vec<u8>> {
    match args.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone().into_bytes()),
        Some(other) => {
            if !headers
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case("content-type"))
            {
                headers.push(("content-type".into(), "application/json".into()));
            }
            Some(other.to_string().into_bytes())
        }
    }
}

/// `headers` argument as a list.
pub fn headers_arg(args: &Value) -> Result<Vec<(String, String)>, Value> {
    match args.get("headers") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Object(m)) => {
            if m.len() > 50 {
                return Err(err("at most 50 headers"));
            }
            m.iter()
                .map(|(k, v)| match v {
                    Value::String(s) => Ok((k.clone(), s.clone())),
                    other => Ok((k.clone(), other.to_string())),
                })
                .collect()
        }
        Some(_) => Err(err("headers must be an object of header name to value")),
    }
}

/// `try_request`.
pub async fn try_request(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let path = match need_str(args, "path") {
        Ok(p) => p,
        Err(e) => return e,
    };
    if path.len() > 4096 {
        return err("path is too long");
    }
    let method = match Method::from_bytes(
        arg_str(args, "method")
            .unwrap_or("GET")
            .to_ascii_uppercase()
            .as_bytes(),
    ) {
        Ok(m) => m,
        Err(_) => return err("invalid method"),
    };
    let mut headers = match headers_arg(args) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let body = body_arg(args, "body", &mut headers);
    if body
        .as_ref()
        .is_some_and(|b| b.len() > ctx.app.config.body_limit)
    {
        return err(format!(
            "the body exceeds this instance's limit of {} bytes",
            ctx.app.config.body_limit
        ));
    }
    let session = arg_str(args, "session");
    let method_text = method.to_string();
    match in_process(ctx, access, method, path, headers, body, session, false).await {
        Ok(a) => {
            let summary = format!(
                "{method_text} {path} answered {} in {} ms ({}, {} bytes{}); sent as session {}. The \
                 response content is under untrusted_* fields.",
                a.status,
                a.elapsed_ms,
                a.content_type.as_deref().unwrap_or("no content type"),
                a.body_bytes,
                if a.truncated { ", truncated" } else { "" },
                a.session
            );
            let mut structured = a.untrusted();
            structured["method"] = json!(method_text);
            structured["path"] = json!(path);
            ok(ctx, summary, structured)
        }
        Err(e) => e,
    }
}

/// `issue_token`: client credentials of the demo client at `/oauth/token`.
pub async fn issue_token(ctx: &CallCtx, access: &Access, args: &Value) -> Value {
    let scope = arg_str(args, "scope").unwrap_or("").trim().to_string();
    if scope.len() > 512 {
        return err("scope is too long");
    }
    let mut audiences: Vec<String> = Vec::new();
    match args.get("audience") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) if !s.trim().is_empty() => audiences.push(s.trim().to_string()),
        Some(Value::String(_)) => {}
        Some(Value::Array(items)) => {
            for i in items {
                match i.as_str() {
                    Some(s) if !s.trim().is_empty() => audiences.push(s.trim().to_string()),
                    _ => return err("audience entries must be non-empty strings"),
                }
            }
        }
        Some(_) => return err("audience must be a string or an array of strings"),
    }
    if audiences.len() > 5 {
        return err("at most 5 audiences");
    }
    let form = {
        let mut form = form_urlencoded::Serializer::new(String::new());
        form.append_pair("grant_type", "client_credentials")
            .append_pair("client_id", crate::oidc::clients::DEMO_CLIENT_ID)
            .append_pair("client_secret", crate::oidc::clients::DEMO_CLIENT_SECRET);
        if !scope.is_empty() {
            form.append_pair("scope", &scope);
        }
        for a in &audiences {
            form.append_pair("resource", a);
        }
        form.finish()
    };
    let headers = vec![(
        "content-type".to_string(),
        "application/x-www-form-urlencoded".to_string(),
    )];
    let answer = match in_process(
        ctx,
        access,
        Method::POST,
        "/oauth/token",
        headers,
        Some(form.into_bytes()),
        None,
        false,
    )
    .await
    {
        Ok(a) => a,
        Err(e) => return e,
    };
    let body = answer.json().unwrap_or(Value::Null);
    if answer.status != StatusCode::OK.as_u16() {
        return err(format!(
            "the identity provider refused ({}): {}",
            answer.status,
            body.get("error_description")
                .or_else(|| body.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("no details")
        ));
    }
    let Some(token) = body.get("access_token").and_then(Value::as_str) else {
        return err("the identity provider answered without an access token");
    };
    let claims = decode_claims(token).unwrap_or(Value::Null);
    let expires_in = body.get("expires_in").cloned().unwrap_or(Value::Null);
    let summary = format!(
        "Issued a {} token for the demo client (scope {:?}, audience {}), valid {} seconds. Send it \
         as Authorization: Bearer <access_token>.",
        body.get("token_type").and_then(Value::as_str).unwrap_or("Bearer"),
        claims.get("scope").and_then(Value::as_str).unwrap_or(""),
        claims
            .get("aud")
            .map(|a| a.to_string())
            .unwrap_or_else(|| "default".into()),
        expires_in
    );
    ok(
        ctx,
        summary,
        json!({
            "access_token": token,
            "token_type": body.get("token_type"),
            "expires_in": expires_in,
            "scope": body.get("scope"),
            "claims": claims,
            "header": format!("Authorization: Bearer {token}"),
            "jwks_url": format!("{}/oauth/jwks", access.links.base_url()),
            "issuer": claims.get("iss"),
        }),
    )
}

/// The claims of a JWT, undecoded by signature (the IdP just signed it).
fn decode_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// A body cut for a result summary.
pub fn short(text: &str) -> String {
    truncate(text, 200).0
}
