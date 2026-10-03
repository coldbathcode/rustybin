use async_graphql::{Enum, InputObject, Object, Schema, SimpleObject, ID};
use axum::{
    extract::Extension,
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;

use crate::catalog::{category, Endpoint, Example};
use crate::state::AppState;

// ── Enums ───────────────────────────────────────────────────────────

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
enum UserRole {
    Admin,
    User,
    Guest,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
enum OrderStatus {
    Pending,
    Processing,
    Shipped,
    Delivered,
    Cancelled,
}

// ── Data structs (internal) ─────────────────────────────────────────

#[derive(Clone, Debug)]
struct UserData {
    id: &'static str,
    name: &'static str,
    email: &'static str,
    role: UserRole,
    created_at: &'static str,
}

#[derive(Clone, Debug)]
struct ProductData {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    price: f64,
    category: &'static str,
    in_stock: bool,
}

#[derive(Clone, Debug)]
struct OrderData {
    id: &'static str,
    user_id: &'static str,
    items: &'static [OrderItemData],
    status: OrderStatus,
    total: f64,
    created_at: &'static str,
}

#[derive(Clone, Debug)]
struct OrderItemData {
    product_id: &'static str,
    quantity: i32,
    unit_price: f64,
}

#[derive(Clone, Debug)]
struct ReviewData {
    id: &'static str,
    product_id: &'static str,
    author_id: &'static str,
    rating: i32,
    comment: &'static str,
    created_at: &'static str,
}

// ── Hardcoded data ──────────────────────────────────────────────────

static USERS: &[UserData] = &[
    UserData {
        id: "u1",
        name: "Alice Chen",
        email: "alice@example.com",
        role: UserRole::Admin,
        created_at: "2024-01-15T09:30:00Z",
    },
    UserData {
        id: "u2",
        name: "Bob Martinez",
        email: "bob@example.com",
        role: UserRole::User,
        created_at: "2024-02-20T14:15:00Z",
    },
    UserData {
        id: "u3",
        name: "Carol Nakamura",
        email: "carol@example.com",
        role: UserRole::User,
        created_at: "2024-03-10T11:00:00Z",
    },
    UserData {
        id: "u4",
        name: "David Okafor",
        email: "david@example.com",
        role: UserRole::Guest,
        created_at: "2024-04-05T16:45:00Z",
    },
    UserData {
        id: "u5",
        name: "Eva Johansson",
        email: "eva@example.com",
        role: UserRole::User,
        created_at: "2024-05-12T08:20:00Z",
    },
];

static PRODUCTS: &[ProductData] = &[
    ProductData {
        id: "p1",
        name: "Mechanical Keyboard",
        description: "Cherry MX Brown switches, RGB backlight, hot-swappable",
        price: 149.99,
        category: "Electronics",
        in_stock: true,
    },
    ProductData {
        id: "p2",
        name: "Wireless Mouse",
        description: "Ergonomic design, 20000 DPI, dual-mode connectivity",
        price: 79.99,
        category: "Electronics",
        in_stock: true,
    },
    ProductData {
        id: "p3",
        name: "USB-C Hub",
        description: "7-in-1 hub with HDMI, USB-A, SD card reader",
        price: 45.99,
        category: "Electronics",
        in_stock: false,
    },
    ProductData {
        id: "p4",
        name: "Espresso Machine",
        description: "15-bar pressure, built-in grinder, milk frother",
        price: 599.99,
        category: "Kitchen",
        in_stock: true,
    },
    ProductData {
        id: "p5",
        name: "Cast Iron Skillet",
        description: "12-inch pre-seasoned, oven-safe to 500°F",
        price: 34.99,
        category: "Kitchen",
        in_stock: true,
    },
    ProductData {
        id: "p6",
        name: "Chef's Knife",
        description: "8-inch German steel, full tang, triple-riveted handle",
        price: 89.99,
        category: "Kitchen",
        in_stock: true,
    },
    ProductData {
        id: "p7",
        name: "Running Shoes",
        description: "Lightweight mesh, responsive cushioning, carbon plate",
        price: 179.99,
        category: "Sports",
        in_stock: true,
    },
    ProductData {
        id: "p8",
        name: "Yoga Mat",
        description: "6mm thick, non-slip surface, carrying strap included",
        price: 29.99,
        category: "Sports",
        in_stock: true,
    },
    ProductData {
        id: "p9",
        name: "Resistance Bands Set",
        description: "5 bands with varying resistance, door anchor, handles",
        price: 24.99,
        category: "Sports",
        in_stock: false,
    },
    ProductData {
        id: "p10",
        name: "Monitor Stand",
        description: "Adjustable height, cable management, holds up to 30 lbs",
        price: 59.99,
        category: "Electronics",
        in_stock: true,
    },
];

static ORDER_ITEMS_1: &[OrderItemData] = &[
    OrderItemData {
        product_id: "p1",
        quantity: 1,
        unit_price: 149.99,
    },
    OrderItemData {
        product_id: "p2",
        quantity: 1,
        unit_price: 79.99,
    },
];

static ORDER_ITEMS_2: &[OrderItemData] = &[OrderItemData {
    product_id: "p4",
    quantity: 1,
    unit_price: 599.99,
}];

static ORDER_ITEMS_3: &[OrderItemData] = &[
    OrderItemData {
        product_id: "p7",
        quantity: 2,
        unit_price: 179.99,
    },
    OrderItemData {
        product_id: "p8",
        quantity: 1,
        unit_price: 29.99,
    },
];

static ORDER_ITEMS_4: &[OrderItemData] = &[
    OrderItemData {
        product_id: "p5",
        quantity: 1,
        unit_price: 34.99,
    },
    OrderItemData {
        product_id: "p6",
        quantity: 1,
        unit_price: 89.99,
    },
];

static ORDER_ITEMS_5: &[OrderItemData] = &[
    OrderItemData {
        product_id: "p3",
        quantity: 2,
        unit_price: 45.99,
    },
    OrderItemData {
        product_id: "p10",
        quantity: 1,
        unit_price: 59.99,
    },
];

static ORDER_ITEMS_6: &[OrderItemData] = &[OrderItemData {
    product_id: "p1",
    quantity: 1,
    unit_price: 149.99,
}];

static ORDER_ITEMS_7: &[OrderItemData] = &[
    OrderItemData {
        product_id: "p9",
        quantity: 3,
        unit_price: 24.99,
    },
    OrderItemData {
        product_id: "p7",
        quantity: 1,
        unit_price: 179.99,
    },
];

static ORDER_ITEMS_8: &[OrderItemData] = &[
    OrderItemData {
        product_id: "p4",
        quantity: 1,
        unit_price: 599.99,
    },
    OrderItemData {
        product_id: "p5",
        quantity: 2,
        unit_price: 34.99,
    },
];

static ORDERS: &[OrderData] = &[
    OrderData {
        id: "o1",
        user_id: "u1",
        items: ORDER_ITEMS_1,
        status: OrderStatus::Delivered,
        total: 229.98,
        created_at: "2024-06-01T10:00:00Z",
    },
    OrderData {
        id: "o2",
        user_id: "u2",
        items: ORDER_ITEMS_2,
        status: OrderStatus::Shipped,
        total: 599.99,
        created_at: "2024-06-05T14:30:00Z",
    },
    OrderData {
        id: "o3",
        user_id: "u3",
        items: ORDER_ITEMS_3,
        status: OrderStatus::Processing,
        total: 389.97,
        created_at: "2024-06-10T09:15:00Z",
    },
    OrderData {
        id: "o4",
        user_id: "u1",
        items: ORDER_ITEMS_4,
        status: OrderStatus::Delivered,
        total: 124.98,
        created_at: "2024-06-15T16:00:00Z",
    },
    OrderData {
        id: "o5",
        user_id: "u4",
        items: ORDER_ITEMS_5,
        status: OrderStatus::Pending,
        total: 151.97,
        created_at: "2024-06-20T11:45:00Z",
    },
    OrderData {
        id: "o6",
        user_id: "u5",
        items: ORDER_ITEMS_6,
        status: OrderStatus::Cancelled,
        total: 149.99,
        created_at: "2024-06-25T13:20:00Z",
    },
    OrderData {
        id: "o7",
        user_id: "u2",
        items: ORDER_ITEMS_7,
        status: OrderStatus::Shipped,
        total: 254.96,
        created_at: "2024-07-01T08:00:00Z",
    },
    OrderData {
        id: "o8",
        user_id: "u3",
        items: ORDER_ITEMS_8,
        status: OrderStatus::Pending,
        total: 669.97,
        created_at: "2024-07-05T15:30:00Z",
    },
];

static REVIEWS: &[ReviewData] = &[
    ReviewData {
        id: "r1",
        product_id: "p1",
        author_id: "u1",
        rating: 5,
        comment: "Best keyboard I've ever used. The switches feel amazing.",
        created_at: "2024-06-15T10:00:00Z",
    },
    ReviewData {
        id: "r2",
        product_id: "p1",
        author_id: "u3",
        rating: 4,
        comment: "Great build quality, slightly loud for office use.",
        created_at: "2024-06-20T14:30:00Z",
    },
    ReviewData {
        id: "r3",
        product_id: "p4",
        author_id: "u2",
        rating: 5,
        comment: "Makes coffee shop quality espresso at home.",
        created_at: "2024-06-25T09:00:00Z",
    },
    ReviewData {
        id: "r4",
        product_id: "p7",
        author_id: "u3",
        rating: 4,
        comment: "Very comfortable for long runs. Carbon plate is noticeable.",
        created_at: "2024-07-01T16:00:00Z",
    },
    ReviewData {
        id: "r5",
        product_id: "p2",
        author_id: "u5",
        rating: 3,
        comment: "Good mouse but the scroll wheel feels cheap.",
        created_at: "2024-07-05T11:00:00Z",
    },
    ReviewData {
        id: "r6",
        product_id: "p5",
        author_id: "u1",
        rating: 5,
        comment: "Perfect heat distribution. Sears beautifully.",
        created_at: "2024-07-10T08:30:00Z",
    },
    ReviewData {
        id: "r7",
        product_id: "p8",
        author_id: "u4",
        rating: 4,
        comment: "Nice thickness, doesn't slip. Strap is a bonus.",
        created_at: "2024-07-12T13:00:00Z",
    },
    ReviewData {
        id: "r8",
        product_id: "p10",
        author_id: "u2",
        rating: 5,
        comment: "Clean desk setup. Cable management is a game changer.",
        created_at: "2024-07-15T10:30:00Z",
    },
];

// ── GraphQL output types ────────────────────────────────────────────

struct User(UserData);

#[Object]
impl User {
    async fn id(&self) -> ID {
        ID(self.0.id.to_string())
    }

    async fn name(&self) -> &str {
        self.0.name
    }

    async fn email(&self) -> &str {
        self.0.email
    }

    async fn role(&self) -> UserRole {
        self.0.role
    }

    async fn created_at(&self) -> &str {
        self.0.created_at
    }

    async fn orders(&self) -> Vec<Order> {
        ORDERS
            .iter()
            .filter(|o| o.user_id == self.0.id)
            .map(|o| Order(o.clone()))
            .collect()
    }
}

struct Product(ProductData);

#[Object]
impl Product {
    async fn id(&self) -> ID {
        ID(self.0.id.to_string())
    }

    async fn name(&self) -> &str {
        self.0.name
    }

    async fn description(&self) -> &str {
        self.0.description
    }

    async fn price(&self) -> f64 {
        self.0.price
    }

    async fn category(&self) -> &str {
        self.0.category
    }

    async fn in_stock(&self) -> bool {
        self.0.in_stock
    }

    async fn reviews(&self) -> Vec<Review> {
        REVIEWS
            .iter()
            .filter(|r| r.product_id == self.0.id)
            .map(|r| Review(r.clone()))
            .collect()
    }
}

struct Order(OrderData);

#[Object]
impl Order {
    async fn id(&self) -> ID {
        ID(self.0.id.to_string())
    }

    async fn user(&self) -> Option<User> {
        USERS
            .iter()
            .find(|u| u.id == self.0.user_id)
            .map(|u| User(u.clone()))
    }

    async fn items(&self) -> Vec<OrderItem> {
        self.0.items.iter().map(|i| OrderItem(i.clone())).collect()
    }

    async fn status(&self) -> OrderStatus {
        self.0.status
    }

    async fn total(&self) -> f64 {
        self.0.total
    }

    async fn created_at(&self) -> &str {
        self.0.created_at
    }
}

struct OrderItem(OrderItemData);

#[Object]
impl OrderItem {
    async fn product(&self) -> Option<Product> {
        PRODUCTS
            .iter()
            .find(|p| p.id == self.0.product_id)
            .map(|p| Product(p.clone()))
    }

    async fn quantity(&self) -> i32 {
        self.0.quantity
    }

    async fn unit_price(&self) -> f64 {
        self.0.unit_price
    }
}

struct Review(ReviewData);

#[Object]
impl Review {
    async fn id(&self) -> ID {
        ID(self.0.id.to_string())
    }

    async fn author(&self) -> Option<User> {
        USERS
            .iter()
            .find(|u| u.id == self.0.author_id)
            .map(|u| User(u.clone()))
    }

    async fn rating(&self) -> i32 {
        self.0.rating
    }

    async fn comment(&self) -> &str {
        self.0.comment
    }

    async fn created_at(&self) -> &str {
        self.0.created_at
    }
}

// ── Input types ─────────────────────────────────────────────────────

#[derive(InputObject)]
struct CreateUserInput {
    name: String,
    email: String,
    role: Option<UserRole>,
}

#[derive(InputObject)]
struct UpdateUserInput {
    name: Option<String>,
    email: Option<String>,
    role: Option<UserRole>,
}

#[derive(InputObject)]
struct CreateOrderInput {
    user_id: ID,
    items: Vec<OrderItemInput>,
}

#[derive(InputObject)]
struct OrderItemInput {
    product_id: ID,
    quantity: i32,
}

// ── Mutation result types ───────────────────────────────────────────

#[derive(SimpleObject)]
struct CreatedUser {
    id: ID,
    name: String,
    email: String,
    role: UserRole,
    created_at: String,
}

#[derive(SimpleObject)]
struct CreatedOrder {
    id: ID,
    user_id: ID,
    items: Vec<CreatedOrderItem>,
    status: OrderStatus,
    total: f64,
    created_at: String,
}

#[derive(SimpleObject)]
struct CreatedOrderItem {
    product_id: ID,
    quantity: i32,
    unit_price: f64,
}

// ── Query root ──────────────────────────────────────────────────────

struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn user(&self, id: ID) -> Option<User> {
        USERS
            .iter()
            .find(|u| u.id == id.as_str())
            .map(|u| User(u.clone()))
    }

    async fn users(
        &self,
        #[graphql(default = 10)] limit: i32,
        #[graphql(default = 0)] offset: i32,
    ) -> Vec<User> {
        USERS
            .iter()
            .skip(offset.max(0) as usize)
            .take(limit.max(0) as usize)
            .map(|u| User(u.clone()))
            .collect()
    }

    async fn product(&self, id: ID) -> Option<Product> {
        PRODUCTS
            .iter()
            .find(|p| p.id == id.as_str())
            .map(|p| Product(p.clone()))
    }

    async fn products(
        &self,
        category: Option<String>,
        #[graphql(default = 10)] limit: i32,
    ) -> Vec<Product> {
        PRODUCTS
            .iter()
            .filter(|p| match &category {
                Some(cat) => p.category.eq_ignore_ascii_case(cat),
                None => true,
            })
            .take(limit.max(0) as usize)
            .map(|p| Product(p.clone()))
            .collect()
    }

    async fn order(&self, id: ID) -> Option<Order> {
        ORDERS
            .iter()
            .find(|o| o.id == id.as_str())
            .map(|o| Order(o.clone()))
    }

    async fn orders(
        &self,
        user_id: Option<ID>,
        status: Option<OrderStatus>,
        #[graphql(default = 10)] limit: i32,
    ) -> Vec<Order> {
        ORDERS
            .iter()
            .filter(|o| match &user_id {
                Some(uid) => o.user_id == uid.as_str(),
                None => true,
            })
            .filter(|o| match &status {
                Some(s) => o.status == *s,
                None => true,
            })
            .take(limit.max(0) as usize)
            .map(|o| Order(o.clone()))
            .collect()
    }
}

// ── Mutation root ───────────────────────────────────────────────────

struct MutationRoot;

#[Object]
impl MutationRoot {
    async fn create_user(&self, input: CreateUserInput) -> CreatedUser {
        CreatedUser {
            id: ID(uuid::Uuid::new_v4().to_string()),
            name: input.name,
            email: input.email,
            role: input.role.unwrap_or(UserRole::User),
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    async fn update_user(&self, id: ID, input: UpdateUserInput) -> Option<CreatedUser> {
        USERS
            .iter()
            .find(|u| u.id == id.as_str())
            .map(|u| CreatedUser {
                id: ID(u.id.to_string()),
                name: input.name.unwrap_or_else(|| u.name.to_string()),
                email: input.email.unwrap_or_else(|| u.email.to_string()),
                role: input.role.unwrap_or(u.role),
                created_at: u.created_at.to_string(),
            })
    }

    async fn create_order(&self, input: CreateOrderInput) -> CreatedOrder {
        let items: Vec<CreatedOrderItem> = input
            .items
            .iter()
            .map(|item| {
                let price = PRODUCTS
                    .iter()
                    .find(|p| p.id == item.product_id.as_str())
                    .map(|p| p.price)
                    .unwrap_or(0.0);
                CreatedOrderItem {
                    product_id: item.product_id.clone(),
                    quantity: item.quantity,
                    unit_price: price,
                }
            })
            .collect();

        let total: f64 = items.iter().map(|i| i.unit_price * i.quantity as f64).sum();

        CreatedOrder {
            id: ID(uuid::Uuid::new_v4().to_string()),
            user_id: input.user_id,
            items,
            status: OrderStatus::Pending,
            total,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

// ── Schema construction ─────────────────────────────────────────────

type GqlSchema = Schema<QueryRoot, MutationRoot, async_graphql::EmptySubscription>;

fn build_schema() -> GqlSchema {
    Schema::build(QueryRoot, MutationRoot, async_graphql::EmptySubscription).finish()
}

// ── GraphQL request type ────────────────────────────────────────────

#[derive(Deserialize)]
struct GraphQLRequest {
    query: String,
    #[serde(default)]
    variables: Option<serde_json::Value>,
    #[allow(dead_code)]
    #[serde(default, rename = "operationName")]
    operation_name: Option<String>,
}

// ── Handlers ────────────────────────────────────────────────────────

async fn graphql_handler(
    Extension(schema): Extension<GqlSchema>,
    Json(req): Json<GraphQLRequest>,
) -> Response {
    let mut gql_req = async_graphql::Request::new(&req.query);
    if let Some(vars) = req.variables {
        gql_req = gql_req.variables(async_graphql::Variables::from_json(vars));
    }
    let resp = schema.execute(gql_req).await;
    let body = serde_json::to_string(&resp).unwrap_or_default();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

async fn graphql_playground() -> Html<String> {
    Html(playground_html("/graphql"))
}

async fn graphql_sdl(Extension(schema): Extension<GqlSchema>) -> Response {
    let sdl = schema.sdl();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        sdl,
    )
        .into_response()
}

fn playground_html(endpoint: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
  <title>Rustybin GraphQL Playground</title>
  <link rel="stylesheet" href="https://unpkg.com/graphiql/graphiql.min.css" />
</head>
<body style="margin: 0;">
  <div id="graphiql" style="height: 100vh;"></div>
  <script crossorigin src="https://unpkg.com/react/umd/react.production.min.js"></script>
  <script crossorigin src="https://unpkg.com/react-dom/umd/react-dom.production.min.js"></script>
  <script crossorigin src="https://unpkg.com/graphiql/graphiql.min.js"></script>
  <script>
    const fetcher = GraphiQL.createFetcher({{ url: '{}' }});
    ReactDOM.render(
      React.createElement(GraphiQL, {{ fetcher }}),
      document.getElementById('graphiql'),
    );
  </script>
</body>
</html>"#,
        endpoint
    )
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    let schema = build_schema();

    Router::new()
        .route("/graphql", get(graphql_playground).post(graphql_handler))
        .route("/graphql/schema", get(graphql_sdl))
        .layer(axum::extract::Extension(schema))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/graphql",
            &["GET", "POST"],
            category::GRAPHQL,
            "GraphQL endpoint (GET: playground, POST: query)",
        )
        .example(
            Example::post("Query users", "/graphql")
                .json(r#"{"query":"{ users { id name email } }"}"#),
        ),
        Endpoint::new(
            "/graphql/schema",
            &["GET"],
            category::GRAPHQL,
            "Schema in SDL",
        )
        .example(Example::get("SDL schema", "/graphql/schema")),
    ]
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_app() -> Router {
        crate::test_support::module_app(router)
    }

    async fn json_body(resp: axum::http::Response<Body>) -> serde_json::Value {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&body).expect("json")
    }

    async fn gql_query(app: Router, query: &str) -> serde_json::Value {
        let body = serde_json::json!({ "query": query });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/graphql")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).expect("json")))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        json_body(resp).await
    }

    #[tokio::test]
    async fn query_users_list() {
        let json = gql_query(test_app(), "{ users { id name email } }").await;
        let users = json["data"]["users"].as_array().expect("users array");
        assert_eq!(users.len(), 5);
        assert_eq!(users[0]["name"], "Alice Chen");
    }

    #[tokio::test]
    async fn query_single_user() {
        let json = gql_query(test_app(), r#"{ user(id: "u2") { name email role } }"#).await;
        assert_eq!(json["data"]["user"]["name"], "Bob Martinez");
        assert_eq!(json["data"]["user"]["role"], "USER");
    }

    #[tokio::test]
    async fn query_user_not_found() {
        let json = gql_query(test_app(), r#"{ user(id: "u999") { name } }"#).await;
        assert!(json["data"]["user"].is_null());
    }

    #[tokio::test]
    async fn query_products_by_category() {
        let json = gql_query(
            test_app(),
            r#"{ products(category: "Kitchen") { id name price } }"#,
        )
        .await;
        let products = json["data"]["products"].as_array().expect("products");
        assert_eq!(products.len(), 3);
    }

    #[tokio::test]
    async fn query_single_product_with_reviews() {
        let json = gql_query(
            test_app(),
            r#"{ product(id: "p1") { name reviews { rating comment author { name } } } }"#,
        )
        .await;
        let reviews = json["data"]["product"]["reviews"]
            .as_array()
            .expect("reviews");
        assert_eq!(reviews.len(), 2);
        assert!(reviews[0]["author"]["name"].is_string());
    }

    #[tokio::test]
    async fn query_orders_by_user() {
        let json = gql_query(
            test_app(),
            r#"{ orders(userId: "u1") { id status total } }"#,
        )
        .await;
        let orders = json["data"]["orders"].as_array().expect("orders");
        assert_eq!(orders.len(), 2);
    }

    #[tokio::test]
    async fn query_orders_by_status() {
        let json = gql_query(test_app(), r#"{ orders(status: PENDING) { id total } }"#).await;
        let orders = json["data"]["orders"].as_array().expect("orders");
        assert_eq!(orders.len(), 2);
    }

    #[tokio::test]
    async fn query_deep_nesting() {
        let json = gql_query(
            test_app(),
            "{ users { orders { items { product { reviews { author { name } } } } } } }",
        )
        .await;
        // Should resolve without error
        assert!(json["data"]["users"].is_array());
        let users = json["data"]["users"].as_array().expect("users");
        // Alice has orders with items
        let alice_orders = users[0]["orders"].as_array().expect("orders");
        assert!(!alice_orders.is_empty());
    }

    #[tokio::test]
    async fn query_order_with_user_back_reference() {
        let json = gql_query(
            test_app(),
            r#"{ order(id: "o1") { id user { name } items { product { name } quantity } total } }"#,
        )
        .await;
        assert_eq!(json["data"]["order"]["user"]["name"], "Alice Chen");
        let items = json["data"]["order"]["items"].as_array().expect("items");
        assert_eq!(items.len(), 2);
    }

    #[tokio::test]
    async fn mutation_create_user() {
        let json = gql_query(
            test_app(),
            r#"mutation { createUser(input: { name: "Test User", email: "test@test.com" }) { id name email role } }"#,
        )
        .await;
        let user = &json["data"]["createUser"];
        assert_eq!(user["name"], "Test User");
        assert_eq!(user["email"], "test@test.com");
        assert_eq!(user["role"], "USER");
        assert!(user["id"].is_string());
    }

    #[tokio::test]
    async fn mutation_update_user() {
        let json = gql_query(
            test_app(),
            r#"mutation { updateUser(id: "u1", input: { name: "Alice Updated" }) { id name email } }"#,
        )
        .await;
        let user = &json["data"]["updateUser"];
        assert_eq!(user["name"], "Alice Updated");
        assert_eq!(user["email"], "alice@example.com");
    }

    #[tokio::test]
    async fn mutation_create_order() {
        let json = gql_query(
            test_app(),
            r#"mutation { createOrder(input: { userId: "u1", items: [{ productId: "p1", quantity: 2 }] }) { id status total items { productId quantity unitPrice } } }"#,
        )
        .await;
        let order = &json["data"]["createOrder"];
        assert_eq!(order["status"], "PENDING");
        assert_eq!(order["total"], 299.98);
        let items = order["items"].as_array().expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["quantity"], 2);
    }

    #[tokio::test]
    async fn query_users_with_limit_offset() {
        let json = gql_query(test_app(), "{ users(limit: 2, offset: 1) { name } }").await;
        let users = json["data"]["users"].as_array().expect("users");
        assert_eq!(users.len(), 2);
        assert_eq!(users[0]["name"], "Bob Martinez");
    }

    #[tokio::test]
    async fn playground_returns_html() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/graphql")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .expect("ct")
            .to_str()
            .expect("str");
        assert!(ct.contains("text/html"));
    }

    #[tokio::test]
    async fn schema_sdl_returns_text() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/graphql/schema")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let sdl = String::from_utf8(body.to_vec()).expect("utf8");
        assert!(sdl.contains("type Query"));
        assert!(sdl.contains("type User"));
        assert!(sdl.contains("type Product"));
        assert!(sdl.contains("type Order"));
        assert!(sdl.contains("enum UserRole"));
    }
}
