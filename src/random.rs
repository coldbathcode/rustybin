use axum::{
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::get,
    Router,
};
use rand::Rng;
use serde::Serialize;
use uuid::Uuid;

use crate::catalog::{category, Endpoint, Example};
use crate::content_negotiation::{negotiate, negotiate_with_status};
use crate::state::AppState;
use crate::types::ErrorResponse;

// ── Response types ───────────────────────────────────────────────────

#[derive(Serialize)]
struct UuidResponse {
    uuid: String,
}

#[derive(Serialize)]
struct GuuidResponse {
    guuid: String,
}

#[derive(Serialize)]
struct IntValueResponse {
    value: i64,
}

#[derive(Serialize)]
struct UintValueResponse {
    value: u64,
}

#[derive(Serialize)]
struct RandomAllResponse {
    int: i64,
    uint: u64,
    uuid: String,
    guuid: String,
    lorem_ipsum: String,
}

#[derive(Serialize)]
struct LoremIpsumResponse {
    paragraphs: Vec<String>,
}

// ── Lorem Ipsum constants ───────────────────────────────────────────

const LOREM_PARAGRAPHS: &[&str] = &[
    "Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.",
    "Curabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra, est eros bibendum elit, nec luctus magna felis sollicitudin mauris. Integer in mauris eu nibh euismod gravida. Duis ac tellus et risus vulputate vehicula. Donec lobortis risus a elit. Etiam tempor augue at sapien pellentesque porttitor. Praesent blandit laoreet nibh.",
    "Fusce lacinia arcu et nulla. Nulla vitae mauris non felis mollis faucibus. Phasellus volutpat, metus eget egestas mollis, lacus lacus blandit dui, id egestas quam mauris ut lacus. Pellentesque habitant morbi tristique senectus et netus et malesuada fames ac turpis egestas. Sed augue ipsum, egestas nec, vestibulum et, malesuada adipiscing, dui.",
    "Vestibulum ante ipsum primis in faucibus orci luctus et ultrices posuere cubilia curae. Aliquam nibh lorem, varius id ultrices eget, tempus vel lectus. Donec id tempus augue. Suspendisse potenti. Maecenas ornare lacus sit amet sapien blandit, at finibus velit tincidunt. Nam scelerisque diam quis congue cursus. Morbi maximus luctus ante.",
    "Aenean lacinia bibendum nulla sed consectetur. Cras mattis consectetur purus sit amet fermentum. Donec sed odio dui. Vestibulum id ligula porta felis euismod semper. Nullam quis risus eget urna mollis ornare vel eu leo. Cras justo odio, dapibus ut facilisis in, egestas eget quam. Praesent commodo cursus magna vel scelerisque nisl consectetur.",
    "Maecenas sed diam eget risus varius blandit sit amet non magna. Integer posuere erat a ante venenatis dapibus posuere velit aliquet. Praesent commodo cursus magna, vel scelerisque nisl consectetur et. Vivamus sagittis lacus vel augue laoreet rutrum faucibus dolor auctor. Morbi leo risus, porta ac consectetur ac, vestibulum at eros.",
    "Etiam porta sem malesuada magna mollis euismod. Nulla vitae elit libero, a pharetra augue. Sed posuere consectetur est at lobortis. Fusce dapibus, tellus ac cursus commodo, tortor mauris condimentum nibh, ut fermentum massa justo sit amet risus. Donec ullamcorper nulla non metus auctor fringilla.",
    "Pellentesque ornare sem lacinia quam venenatis vestibulum. Aenean eu leo quam. Pellentesque ornare sem lacinia quam venenatis vestibulum. Cum sociis natoque penatibus et magnis dis parturient montes, nascetur ridiculus mus. Donec id elit non mi porta gravida at eget metus. Vivamus sagittis lacus vel augue laoreet rutrum faucibus dolor auctor.",
];

// ── Handlers ────────────────────────────────────────────────────────

async fn uuid_handler(headers: HeaderMap) -> Response {
    negotiate(
        &headers,
        &UuidResponse {
            uuid: Uuid::new_v4().to_string(),
        },
    )
}

async fn guuid_handler(headers: HeaderMap) -> Response {
    let id = Uuid::new_v4();
    negotiate(
        &headers,
        &GuuidResponse {
            guuid: format!("{{{id}}}"),
        },
    )
}

async fn random_all_handler(headers: HeaderMap) -> Response {
    let mut rng = rand::thread_rng();
    negotiate(
        &headers,
        &RandomAllResponse {
            int: rng.gen_range(-32000..=32000),
            uint: rng.gen_range(0..=65535),
            uuid: Uuid::new_v4().to_string(),
            guuid: format!("{{{}}}", Uuid::new_v4()),
            lorem_ipsum: LOREM_PARAGRAPHS[0].to_string(),
        },
    )
}

async fn random_int_handler(headers: HeaderMap) -> Response {
    let mut rng = rand::thread_rng();
    negotiate(
        &headers,
        &IntValueResponse {
            value: rng.gen_range(-32000..=32000),
        },
    )
}

async fn random_int_range_handler(
    Path((lower, upper)): Path<(i64, i64)>,
    headers: HeaderMap,
) -> Response {
    if lower >= upper {
        return negotiate_with_status(
            &headers,
            &ErrorResponse {
                error: "invalid_range".to_string(),
                details: Some(format!("lower ({lower}) must be less than upper ({upper})")),
            },
            StatusCode::BAD_REQUEST,
        );
    }

    let mut rng = rand::thread_rng();
    negotiate(
        &headers,
        &IntValueResponse {
            value: rng.gen_range(lower..=upper),
        },
    )
}

async fn random_uint_handler(headers: HeaderMap) -> Response {
    let mut rng = rand::thread_rng();
    negotiate(
        &headers,
        &UintValueResponse {
            value: rng.gen_range(0..=65535),
        },
    )
}

async fn lorem_ipsum_handler(headers: HeaderMap) -> Response {
    negotiate(
        &headers,
        &LoremIpsumResponse {
            paragraphs: vec![LOREM_PARAGRAPHS[0].to_string()],
        },
    )
}

async fn lorem_ipsum_count_handler(Path(count): Path<u32>, headers: HeaderMap) -> Response {
    if count == 0 || count > 32 {
        return negotiate_with_status(
            &headers,
            &ErrorResponse {
                error: "invalid_count".to_string(),
                details: Some("count must be between 1 and 32".to_string()),
            },
            StatusCode::BAD_REQUEST,
        );
    }

    let paragraphs: Vec<String> = (0..count as usize)
        .map(|i| LOREM_PARAGRAPHS[i % LOREM_PARAGRAPHS.len()].to_string())
        .collect();

    negotiate(&headers, &LoremIpsumResponse { paragraphs })
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/uuid", get(uuid_handler))
        .route("/guuid", get(guuid_handler))
        .route("/random", get(random_all_handler))
        .route("/random/int", get(random_int_handler))
        .route("/random/int/{lower}/{upper}", get(random_int_range_handler))
        .route("/random/uint", get(random_uint_handler))
        .route("/random/lorem-ipsum", get(lorem_ipsum_handler))
        .route(
            "/random/lorem-ipsum/{count}",
            get(lorem_ipsum_count_handler),
        )
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new("/uuid", &["GET"], category::INFO, "Random UUID v4")
            .example(Example::get("UUID v4", "/uuid")),
        Endpoint::new("/guuid", &["GET"], category::INFO, "Random braced GUID")
            .example(Example::get("GUID", "/guuid")),
        Endpoint::new(
            "/random",
            &["GET"],
            category::INFO,
            "Bundle of random values",
        )
        .example(Example::get("Random bundle", "/random")),
        Endpoint::new(
            "/random/int",
            &["GET"],
            category::INFO,
            "Random signed integer",
        )
        .example(Example::get("Random int", "/random/int")),
        Endpoint::new(
            "/random/int/{lower}/{upper}",
            &["GET"],
            category::INFO,
            "Random integer in [lower, upper]",
        )
        .example(Example::get("Random int 1..100", "/random/int/1/100")),
        Endpoint::new(
            "/random/uint",
            &["GET"],
            category::INFO,
            "Random unsigned integer",
        )
        .example(Example::get("Random uint", "/random/uint")),
        Endpoint::new(
            "/random/lorem-ipsum",
            &["GET"],
            category::INFO,
            "One paragraph of lorem ipsum",
        )
        .example(Example::get("Lorem Ipsum", "/random/lorem-ipsum")),
        Endpoint::new(
            "/random/lorem-ipsum/{count}",
            &["GET"],
            category::INFO,
            "count paragraphs of lorem ipsum",
        )
        .example(Example::get(
            "Lorem Ipsum (3 paragraphs)",
            "/random/lorem-ipsum/3",
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_app() -> Router {
        crate::test_support::module_app(router)
    }

    fn get_req(uri: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("request")
    }

    async fn json_body(resp: axum::http::Response<Body>) -> serde_json::Value {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&body).expect("json")
    }

    #[tokio::test]
    async fn uuid_returns_valid_uuid() {
        let app = test_app();
        let resp = app.oneshot(get_req("/uuid")).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let json = json_body(resp).await;
        let uuid_str = json["uuid"].as_str().expect("uuid string");
        assert!(Uuid::parse_str(uuid_str).is_ok());
    }

    #[tokio::test]
    async fn guuid_returns_braced_uuid() {
        let app = test_app();
        let resp = app.oneshot(get_req("/guuid")).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let json = json_body(resp).await;
        let guuid = json["guuid"].as_str().expect("guuid string");
        assert!(guuid.starts_with('{'));
        assert!(guuid.ends_with('}'));
        assert!(Uuid::parse_str(&guuid[1..guuid.len() - 1]).is_ok());
    }

    #[tokio::test]
    async fn random_all_returns_all_fields() {
        let app = test_app();
        let resp = app.oneshot(get_req("/random")).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let json = json_body(resp).await;
        assert!(json["int"].is_i64());
        assert!(json["uint"].is_u64());
        assert!(json["uuid"].is_string());
        assert!(json["guuid"].is_string());
        assert!(json["lorem_ipsum"].is_string());
    }

    #[tokio::test]
    async fn random_int_in_default_range() {
        let app = test_app();
        let resp = app.oneshot(get_req("/random/int")).await.expect("response");
        let json = json_body(resp).await;
        let val = json["value"].as_i64().expect("int value");
        assert!((-32000..=32000).contains(&val));
    }

    #[tokio::test]
    async fn random_int_custom_range() {
        let app = test_app();
        let resp = app
            .oneshot(get_req("/random/int/10/20"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        let val = json["value"].as_i64().expect("int value");
        assert!((10..=20).contains(&val));
    }

    #[tokio::test]
    async fn random_int_invalid_range() {
        let app = test_app();
        let resp = app
            .oneshot(get_req("/random/int/20/10"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn random_uint_in_range() {
        let app = test_app();
        let resp = app
            .oneshot(get_req("/random/uint"))
            .await
            .expect("response");
        let json = json_body(resp).await;
        let val = json["value"].as_u64().expect("uint value");
        assert!(val <= 65535);
    }

    #[tokio::test]
    async fn lorem_ipsum_single() {
        let app = test_app();
        let resp = app
            .oneshot(get_req("/random/lorem-ipsum"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        let paragraphs = json["paragraphs"].as_array().expect("array");
        assert_eq!(paragraphs.len(), 1);
    }

    #[tokio::test]
    async fn lorem_ipsum_multiple() {
        let app = test_app();
        let resp = app
            .oneshot(get_req("/random/lorem-ipsum/3"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        let paragraphs = json["paragraphs"].as_array().expect("array");
        assert_eq!(paragraphs.len(), 3);
    }

    #[tokio::test]
    async fn lorem_ipsum_cycles() {
        let app = test_app();
        let resp = app
            .oneshot(get_req("/random/lorem-ipsum/16"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let json = json_body(resp).await;
        let paragraphs = json["paragraphs"].as_array().expect("array");
        assert_eq!(paragraphs.len(), 16);
    }

    #[tokio::test]
    async fn lorem_ipsum_zero_returns_400() {
        let app = test_app();
        let resp = app
            .oneshot(get_req("/random/lorem-ipsum/0"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn lorem_ipsum_too_many_returns_400() {
        let app = test_app();
        let resp = app
            .oneshot(get_req("/random/lorem-ipsum/33"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn uuid_xml_negotiation() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/uuid")
                    .header("accept", "application/xml")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            resp.headers()
                .get("content-type")
                .expect("ct")
                .to_str()
                .expect("str"),
            "application/xml"
        );
    }
}
