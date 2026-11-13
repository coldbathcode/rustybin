//! The inspector as an OAuth client that names itself with a document
//! (client ID metadata documents): `GET /.well-known/rustybin-inspector-client.json`
//! describes the console's MCP inspector, and its URL is the client id the
//! OAuth walk sends to an authorization server that says
//! `client_id_metadata_document_supported`, with no registration call.
//!
//! The document has two addresses. The one the walk sends is on the data
//! plane ([`DOCUMENT_PATH`]): an authorization server that refuses to call
//! a Rustybin control plane, as another Rustybin's outbound policy does,
//! can still fetch it. The address 0.7.0 used ([`LEGACY_DOCUMENT_PATH`],
//! under `/_rustybin`) keeps answering, for saved connections and for
//! servers that know the inspector by it. Each address serves the same
//! document but for its `client_id`, which is the address itself (a server
//! refuses a document whose `client_id` is not the URL it fetched).
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
/// The document's first address (0.7.0), under the control plane: never
/// sent by the walk, served for those who kept it.
pub const LEGACY_DOCUMENT_PATH: &str = crate::control_oauth::INSPECTOR_CLIENT_LEGACY_PATH;
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

/// The inspector's client id of 0.7.0 for an instance reached at `base`:
/// the document's address under the control plane.
pub fn legacy_client_id(base: &str) -> String {
    format!("{base}{LEGACY_DOCUMENT_PATH}")
}

/// The document for an instance reached at `base` (scheme, host and port,
/// no trailing slash), as its address on the data plane serves it.
pub fn document(base: &str) -> Value {
    document_at(base, DOCUMENT_PATH)
}

/// The document as the address `path` of an instance reached at `base`
/// serves it: one content, and the `client_id` of the address asked.
fn document_at(base: &str, path: &str) -> Value {
    json!({
        "client_id": format!("{base}{path}"),
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
        "legacy_url": legacy_client_id(base),
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

/// `GET /.well-known/rustybin-inspector-client.json`, the address the walk
/// sends as its client id.
pub async fn handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    serve(&state, &headers, &extensions, &uri, DOCUMENT_PATH)
}

/// `GET /_rustybin/oauth/inspector-client.json`, the address of 0.7.0.
pub async fn legacy_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    uri: Uri,
) -> Response {
    serve(&state, &headers, &extensions, &uri, LEGACY_DOCUMENT_PATH)
}

fn serve(
    state: &AppState,
    headers: &HeaderMap,
    extensions: &Extensions,
    uri: &Uri,
    path: &str,
) -> Response {
    let base = base_url(headers, extensions, uri, state);
    let mut resp = Json(document_at(&base, path)).into_response();
    // An authorization server may keep it for a while; nothing in it changes
    // between releases.
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    resp
}
