use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Negotiate the response format based on the Accept header.
/// Returns XML for `application/xml`, JSON otherwise.
pub fn negotiate<T: Serialize>(headers: &HeaderMap, data: &T) -> Response {
    negotiate_with_status(headers, data, StatusCode::OK)
}

/// Negotiate with a custom status code.
pub fn negotiate_with_status<T: Serialize>(
    headers: &HeaderMap,
    data: &T,
    status: StatusCode,
) -> Response {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json");

    if accept.contains("application/xml") {
        match quick_xml::se::to_string(data) {
            Ok(xml) => (
                status,
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/xml"),
                )],
                xml,
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("XML serialization error: {e}"),
            )
                .into_response(),
        }
    } else {
        match serde_json::to_string(data) {
            Ok(json) => (
                status,
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                )],
                json,
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("JSON serialization error: {e}"),
            )
                .into_response(),
        }
    }
}
