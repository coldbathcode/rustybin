//! A spec fetched from a URL on the server, for a caller that holds a link
//! rather than the file (an agent, above all): `POST
//! /_rustybin/mocks/import/fetch`, `spec_url` on a create and on `create_mock`.
//!
//! The instance reaches out on someone's word, so three rules hold. Only a
//! caller with `mocks.write` may ask: never a visitor of a public instance.
//! It goes through the outbound policy under `Purpose::SpecFetch`, which has
//! no default: a host is reachable only when the operator named it. And what
//! comes back is bounded by the store's spec limit, read as it arrives, with
//! no redirects followed (a redirect could leave the allowed host).

use std::time::Duration;

use axum::http::StatusCode;
use serde::Serialize;
use serde_json::json;

use super::service::{ServiceError, SvcResult};
use crate::egress::Purpose;
use crate::state::AppState;

/// How long the whole fetch may take.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// What came back.
#[derive(Clone, Debug, Serialize)]
pub struct Fetched {
    pub url: String,
    pub spec: String,
    pub bytes: usize,
    pub content_type: Option<String>,
}

/// Fetch `url` as text, at most `max_bytes`.
pub async fn fetch_spec(state: &AppState, url: &str, max_bytes: usize) -> SvcResult<Fetched> {
    let pinned = state
        .egress
        .resolve(url, Purpose::SpecFetch)
        .await
        .map_err(|e| {
            ServiceError::with_body(
                StatusCode::FORBIDDEN,
                json!({ "error": e.0, "reason": "egress_refused", "purpose": "spec_fetch" }),
            )
        })?;
    let client = state
        .egress
        .client(&pinned, FETCH_TIMEOUT)
        .map_err(|e| ServiceError::new(StatusCode::BAD_GATEWAY, e))?;
    let unreachable =
        |e: reqwest::Error| ServiceError::new(StatusCode::BAD_GATEWAY, format!("{url}: {e}"));
    let mut resp = client
        .get(&pinned.url)
        .header(
            "accept",
            "application/yaml, application/json, text/yaml, text/plain",
        )
        .send()
        .await
        .map_err(unreachable)?;
    let status = resp.status();
    if status.is_redirection() {
        return Err(ServiceError::new(
            StatusCode::BAD_GATEWAY,
            format!("{url} answered {status}: a redirect is not followed, name where it leads"),
        ));
    }
    if !status.is_success() {
        return Err(ServiceError::new(
            StatusCode::BAD_GATEWAY,
            format!("{url} answered {status}"),
        ));
    }
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let too_large = || {
        ServiceError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("{url} is larger than the spec limit of {max_bytes} bytes"),
        )
    };
    if resp.content_length().is_some_and(|n| n > max_bytes as u64) {
        return Err(too_large());
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(unreachable)? {
        if body.len() + chunk.len() > max_bytes {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    let bytes = body.len();
    let spec = String::from_utf8(body)
        .map_err(|_| ServiceError::bad_request(format!("{url} is not text: expected UTF-8")))?;
    Ok(Fetched {
        url: url.to_string(),
        spec,
        bytes,
        content_type,
    })
}

#[cfg(test)]
mod tests;
