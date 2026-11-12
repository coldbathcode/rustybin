//! The inspector as an OAuth client that names itself with a document
//! (client ID metadata documents): `GET /_rustybin/oauth/inspector-client.json`
//! describes the console's MCP inspector, and its URL is the client id the
//! OAuth walk sends to an authorization server that says
//! `client_id_metadata_document_supported`, with no registration call.
//!
//! The document is public and holds nothing secret: who the client says it
//! is and where the authorization code may be sent (the console's own
//! callback). An authorization server fetches it from the internet, so it
//! is usable only when this instance is reached over https on a host that
//! is not this computer; [`status`] says which, and why not.

use axum::extract::State;
use axum::http::{header, Extensions, HeaderMap, HeaderValue, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::state::AppState;

/// The document's path, and so the path of the inspector's client id.
pub const DOCUMENT_PATH: &str = crate::control_oauth::INSPECTOR_CLIENT_PATH;
/// The console's OAuth callback (`ui/app.js`), the walk's redirect URI.
pub const CALLBACK_PATH: &str = "/ui/callback";
pub const CLIENT_NAME: &str = "Rustybin MCP inspector";

/// How the walk names its client to an authorization server: the choice
/// of the console's auth form, kept with a saved connection
/// (`auth.oauth.client`).
pub const CLIENT_WAYS: &[&str] = &["auto", "document", "registration", "given"];

/// The inspector's client id for an instance reached at `base`.
pub fn client_id(base: &str) -> String {
    format!("{base}{DOCUMENT_PATH}")
}

/// The redirect URI the walk sends, and the document's only one.
pub fn redirect_uri(base: &str) -> String {
    format!("{base}{CALLBACK_PATH}")
}

/// The document for an instance reached at `base` (scheme, host and port,
/// no trailing slash).
pub fn document(base: &str) -> Value {
    json!({
        "client_id": client_id(base),
        "client_name": CLIENT_NAME,
        "client_uri": format!("{base}/ui/"),
        "redirect_uris": [redirect_uri(base)],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    })
}

/// Why an authorization server could not use the document of an instance
/// reached at `base`, or `None` when it can: the client id must be an
/// https URL the server can fetch, which a loopback host never is.
pub fn unusable_reason(base: &str) -> Option<String> {
    let Ok(url) = reqwest::Url::parse(base) else {
        return Some(format!("{base} is not a URL"));
    };
    let host = url.host_str().unwrap_or("");
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = bare.eq_ignore_ascii_case("localhost")
        || bare.to_ascii_lowercase().ends_with(".localhost")
        || bare
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if loopback {
        return Some(format!(
            "this instance is reached at {base}, a loopback address: an authorization server cannot fetch a document from this computer"
        ));
    }
    if url.scheme() != "https" {
        return Some(format!(
            "this instance is reached at {base}: a client id must be an https URL"
        ));
    }
    None
}

/// What the console, `list_mcp_servers` and the check need to know: the
/// document, its URL, and whether an authorization server can use it.
pub fn status(base: &str) -> Value {
    let reason = unusable_reason(base);
    json!({
        "url": client_id(base),
        "redirect_uri": redirect_uri(base),
        "usable": reason.is_none(),
        "reason": reason,
        "document": document(base),
        "ways": CLIENT_WAYS,
    })
}

/// The base URL a request reached this instance at.
pub fn base_url(
    headers: &HeaderMap,
    extensions: &Extensions,
    uri: &Uri,
    state: &AppState,
) -> String {
    crate::session::request_origin(headers, extensions, uri, &state.config).base_url()
}

/// `GET /_rustybin/oauth/inspector-client.json`.
pub async fn handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    let base = base_url(&headers, &extensions, &uri, &state);
    let mut resp = Json(document(&base)).into_response();
    // An authorization server may keep it for a while; nothing in it changes
    // between releases.
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    resp
}
