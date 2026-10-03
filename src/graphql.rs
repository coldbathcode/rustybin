//! GraphQL API (async-graphql) with a GraphQL-over-HTTP transport.
//!
//! - `POST /graphql` (application/json or application/graphql) and
//!   `GET /graphql?query=...` (queries only; mutations over GET are 405).
//!   Browsers (Accept: text/html, no query) get a pinned GraphiQL page.
//! - `operationName` selects the operation in multi-operation documents.
//! - `Accept: application/graphql-response+json` switches to the
//!   GraphQL-over-HTTP status codes (400 for parse/validation/limit errors);
//!   legacy `application/json` answers 200 for every well-formed request.
//! - Limits: depth 10, complexity 500, 30 aliases (fragments expanded).
//! - Automatic persisted queries (`extensions.persistedQuery.sha256Hash`,
//!   `PersistedQueryNotFound`), bounded cache.
//! - Subscriptions over WebSocket at `/graphql/ws` (graphql-transport-ws and
//!   legacy graphql-ws): `ticker`, `orderUpdates`.

use async_graphql::parser::types::{
    DocumentOperations, ExecutableDocument, OperationType, Selection, SelectionSet,
};
use async_graphql::{Enum, InputObject, Object, Schema, SimpleObject, Subscription, ID};
use async_graphql_axum::{GraphQLProtocol, GraphQLWebSocket};
use axum::{
    body::Bytes,
    extract::{ws::WebSocketUpgrade, Extension, State},
    http::{header, HeaderMap, HeaderValue, StatusCode, Uri},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::catalog::{category, Endpoint, Example};
use crate::config::Config;
use crate::content_negotiation::{add_vary, choose};
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

// ── Subscription root ───────────────────────────────────────────────

/// Maximum events one subscription emits.
const MAX_SUB_EVENTS: i32 = 100;
/// Subscription interval bounds (ms).
const MIN_INTERVAL_MS: i32 = 50;
const MAX_INTERVAL_MS: i32 = 10_000;

#[derive(SimpleObject, Clone)]
struct Tick {
    sequence: i32,
    timestamp: String,
}

#[derive(SimpleObject, Clone)]
struct OrderUpdate {
    order_id: ID,
    sequence: i32,
    status: OrderStatus,
    timestamp: String,
}

struct SubscriptionRoot;

#[Subscription]
impl SubscriptionRoot {
    /// Emit `count` ticks (1-100, default 5), one every `intervalMs`
    /// (50-10000, default 1000), then complete.
    async fn ticker(
        &self,
        #[graphql(default = 5)] count: i32,
        #[graphql(default = 1000)] interval_ms: i32,
    ) -> impl Stream<Item = Tick> {
        let count = count.clamp(1, MAX_SUB_EVENTS);
        let interval =
            Duration::from_millis(interval_ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS) as u64);
        async_stream::stream! {
            for sequence in 0..count {
                if sequence > 0 {
                    tokio::time::sleep(interval).await;
                }
                yield Tick { sequence, timestamp: chrono::Utc::now().to_rfc3339() };
            }
        }
    }

    /// Status changes of an order (PENDING, PROCESSING, SHIPPED,
    /// DELIVERED), one every `intervalMs` (50-10000, default 500), then
    /// complete.
    async fn order_updates(
        &self,
        order_id: ID,
        #[graphql(default = 500)] interval_ms: i32,
    ) -> impl Stream<Item = OrderUpdate> {
        let interval =
            Duration::from_millis(interval_ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS) as u64);
        let steps = [
            OrderStatus::Pending,
            OrderStatus::Processing,
            OrderStatus::Shipped,
            OrderStatus::Delivered,
        ];
        async_stream::stream! {
            for (i, status) in steps.into_iter().enumerate() {
                if i > 0 {
                    tokio::time::sleep(interval).await;
                }
                yield OrderUpdate {
                    order_id: order_id.clone(),
                    sequence: i as i32,
                    status,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                };
            }
        }
    }
}

// ── Schema construction ─────────────────────────────────────────────

type GqlSchema = Schema<QueryRoot, MutationRoot, SubscriptionRoot>;

/// Maximum selection depth.
pub const MAX_DEPTH: usize = 10;
/// Maximum query complexity (1 per field by default).
pub const MAX_COMPLEXITY: usize = 500;
/// Maximum aliases per operation (fragments expanded).
pub const MAX_ALIASES: usize = 30;
/// Automatic persisted queries kept.
pub const APQ_CAPACITY: usize = 1_000;
/// Automatic persisted queries idle TTL.
pub const APQ_TTL: Duration = Duration::from_secs(24 * 3600);
/// Largest query stored in the APQ cache.
const APQ_MAX_QUERY_LEN: usize = 64 * 1024;

fn build_schema() -> GqlSchema {
    Schema::build(QueryRoot, MutationRoot, SubscriptionRoot)
        .limit_depth(MAX_DEPTH)
        .limit_complexity(MAX_COMPLEXITY)
        .finish()
}

// ── Automatic persisted queries ─────────────────────────────────────

/// Bounded hash -> query cache (capacity cap, idle TTL, oldest evicted).
struct ApqCache {
    entries: Mutex<HashMap<String, (Arc<str>, Instant)>>,
    capacity: usize,
    ttl: Duration,
}

impl ApqCache {
    fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            capacity: capacity.max(1),
            ttl,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, (Arc<str>, Instant)>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn get(&self, hash: &str) -> Option<Arc<str>> {
        let now = Instant::now();
        let mut map = self.lock();
        match map.get_mut(hash) {
            Some((q, used)) if now.duration_since(*used) <= self.ttl => {
                *used = now;
                Some(q.clone())
            }
            Some(_) => {
                map.remove(hash);
                None
            }
            None => None,
        }
    }

    fn insert(&self, hash: String, query: &str) {
        if query.len() > APQ_MAX_QUERY_LEN {
            return;
        }
        let now = Instant::now();
        let mut map = self.lock();
        if !map.contains_key(&hash) && map.len() >= self.capacity {
            let ttl = self.ttl;
            map.retain(|_, (_, used)| now.duration_since(*used) <= ttl);
            if map.len() >= self.capacity {
                if let Some(oldest) = map
                    .iter()
                    .min_by_key(|(_, (_, used))| *used)
                    .map(|(k, _)| k.clone())
                {
                    map.remove(&oldest);
                }
            }
        }
        map.insert(hash, (Arc::from(query), now));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().len()
    }
}

fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ── Static analysis (aliases, operation type) ───────────────────────

fn count_aliases(doc: &ExecutableDocument, limit: usize) -> usize {
    fn walk(
        set: &SelectionSet,
        doc: &ExecutableDocument,
        stack: &mut Vec<String>,
        count: &mut usize,
        limit: usize,
    ) {
        for item in &set.items {
            if *count > limit {
                return;
            }
            match &item.node {
                Selection::Field(f) => {
                    if f.node.alias.is_some() {
                        *count += 1;
                    }
                    walk(&f.node.selection_set.node, doc, stack, count, limit);
                }
                Selection::InlineFragment(fr) => {
                    walk(&fr.node.selection_set.node, doc, stack, count, limit);
                }
                Selection::FragmentSpread(spread) => {
                    let name = spread.node.fragment_name.node.to_string();
                    if stack.contains(&name) || stack.len() > MAX_DEPTH {
                        continue;
                    }
                    if let Some(fragment) = doc.fragments.get(&spread.node.fragment_name.node) {
                        stack.push(name);
                        walk(&fragment.node.selection_set.node, doc, stack, count, limit);
                        stack.pop();
                    }
                }
            }
        }
    }
    let mut max = 0;
    for (_, op) in doc.operations.iter() {
        let mut count = 0;
        walk(
            &op.node.selection_set.node,
            doc,
            &mut Vec::new(),
            &mut count,
            limit,
        );
        max = max.max(count);
    }
    max
}

/// The type of the operation that would run (by name, or the only one).
fn selected_operation_type(doc: &ExecutableDocument, name: Option<&str>) -> Option<OperationType> {
    match (&doc.operations, name) {
        (DocumentOperations::Single(op), _) => Some(op.node.ty),
        (DocumentOperations::Multiple(ops), Some(name)) => ops
            .iter()
            .find(|(n, _)| n.as_str() == name)
            .map(|(_, op)| op.node.ty),
        (DocumentOperations::Multiple(_), None) => None,
    }
}

// ── HTTP transport ──────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ResponseMode {
    /// `application/graphql-response+json`: spec status codes (4xx for
    /// request errors).
    GraphqlResponse,
    /// `application/json`: legacy, 200 for every well-formed request.
    Json,
}

impl ResponseMode {
    fn from_headers(headers: &HeaderMap) -> Self {
        match choose(
            headers,
            &[
                ("application", "graphql-response+json"),
                ("application", "json"),
            ],
        ) {
            Some(0) => ResponseMode::GraphqlResponse,
            _ => ResponseMode::Json,
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            ResponseMode::GraphqlResponse => "application/graphql-response+json; charset=utf-8",
            ResponseMode::Json => "application/json",
        }
    }
}

#[derive(Deserialize, Default, Debug)]
struct GqlHttpRequest {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    variables: Option<serde_json::Value>,
    #[serde(default, rename = "operationName")]
    operation_name: Option<String>,
    #[serde(default)]
    extensions: Option<serde_json::Value>,
}

#[derive(Clone)]
struct GqlState {
    schema: GqlSchema,
    apq: Arc<ApqCache>,
}

fn gql_json(mode: ResponseMode, status: StatusCode, body: serde_json::Value) -> Response {
    let mut resp = (
        status,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static(mode.content_type()),
        )],
        body.to_string(),
    )
        .into_response();
    add_vary(resp.headers_mut(), "Accept");
    resp
}

fn error_body(message: &str, code: &str) -> serde_json::Value {
    json!({ "errors": [{ "message": message, "extensions": { "code": code } }] })
}

/// A request-level error: 400 in both modes (the request could not be
/// understood at all).
fn bad_request(mode: ResponseMode, message: &str, code: &str) -> Response {
    gql_json(mode, StatusCode::BAD_REQUEST, error_body(message, code))
}

/// A document-level error (parse / validation / limits): 400 with
/// `application/graphql-response+json`, 200 with legacy `application/json`.
fn document_error(mode: ResponseMode, message: &str, code: &str) -> Response {
    let status = match mode {
        ResponseMode::GraphqlResponse => StatusCode::BAD_REQUEST,
        ResponseMode::Json => StatusCode::OK,
    };
    gql_json(mode, status, error_body(message, code))
}

async fn execute(
    state: &GqlState,
    req: GqlHttpRequest,
    is_get: bool,
    mode: ResponseMode,
) -> Response {
    let variables = match req.variables {
        None | Some(serde_json::Value::Null) => None,
        Some(v @ serde_json::Value::Object(_)) => Some(v),
        Some(_) => return bad_request(mode, "variables must be a JSON object", "BAD_REQUEST"),
    };

    // Automatic persisted queries (Apollo protocol).
    let mut query = req.query.filter(|q| !q.trim().is_empty());
    if let Some(pq) = req
        .extensions
        .as_ref()
        .and_then(|e| e.get("persistedQuery"))
    {
        if pq.get("version").and_then(|v| v.as_i64()) != Some(1) {
            return bad_request(mode, "Unsupported persisted query version", "BAD_REQUEST");
        }
        let Some(hash) = pq
            .get("sha256Hash")
            .and_then(|h| h.as_str())
            .map(str::to_ascii_lowercase)
            .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        else {
            return bad_request(
                mode,
                "persistedQuery.sha256Hash must be a SHA-256 hex digest",
                "BAD_REQUEST",
            );
        };
        match &query {
            Some(q) => {
                if sha256_hex(q) != hash {
                    return bad_request(
                        mode,
                        "provided sha does not match query",
                        "PERSISTED_QUERY_HASH_MISMATCH",
                    );
                }
                state.apq.insert(hash, q);
            }
            None => match state.apq.get(&hash) {
                Some(q) => query = Some(q.to_string()),
                None => {
                    return gql_json(
                        mode,
                        StatusCode::OK,
                        error_body("PersistedQueryNotFound", "PERSISTED_QUERY_NOT_FOUND"),
                    );
                }
            },
        }
    }
    let Some(query) = query else {
        return bad_request(mode, "Must provide query string.", "BAD_REQUEST");
    };

    if let Ok(doc) = async_graphql::parser::parse_query(&query) {
        if is_get
            && selected_operation_type(&doc, req.operation_name.as_deref())
                == Some(OperationType::Mutation)
        {
            let mut resp = gql_json(
                mode,
                StatusCode::METHOD_NOT_ALLOWED,
                error_body("Mutations are only allowed over POST", "METHOD_NOT_ALLOWED"),
            );
            resp.headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("POST"));
            return resp;
        }
        let aliases = count_aliases(&doc, MAX_ALIASES);
        if aliases > MAX_ALIASES {
            return document_error(
                mode,
                &format!("Query has too many aliases (more than {MAX_ALIASES})"),
                "ALIAS_LIMIT_EXCEEDED",
            );
        }
    }

    let mut gql_req = async_graphql::Request::new(query);
    if let Some(vars) = variables {
        gql_req = gql_req.variables(async_graphql::Variables::from_json(vars));
    }
    if let Some(name) = req.operation_name.filter(|n| !n.is_empty()) {
        gql_req = gql_req.operation_name(name);
    }
    let resp = state.schema.execute(gql_req).await;

    // No data and only errors without a path: the document never executed
    // (parse, validation, limits, unknown operation).
    let request_error = matches!(resp.data, async_graphql::Value::Null)
        && !resp.errors.is_empty()
        && resp.errors.iter().all(|e| e.path.is_empty());
    let status = if request_error && mode == ResponseMode::GraphqlResponse {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::OK
    };
    let body =
        serde_json::to_value(&resp).unwrap_or_else(|e| error_body(&e.to_string(), "INTERNAL"));
    gql_json(mode, status, body)
}

async fn graphql_post(
    Extension(state): Extension<GqlState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mode = ResponseMode::from_headers(&headers);
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| {
            ct.split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        });
    let req = match content_type.as_deref() {
        None | Some("application/json") | Some("application/graphql+json") => {
            match serde_json::from_slice::<serde_json::Value>(&body) {
                Ok(serde_json::Value::Array(_)) => {
                    return bad_request(mode, "Batched requests are not supported", "BAD_REQUEST");
                }
                Ok(v @ serde_json::Value::Object(_)) => {
                    match serde_json::from_value::<GqlHttpRequest>(v) {
                        Ok(r) => r,
                        Err(e) => {
                            return bad_request(
                                mode,
                                &format!("Invalid GraphQL request: {e}"),
                                "BAD_REQUEST",
                            );
                        }
                    }
                }
                Ok(_) => {
                    return bad_request(mode, "Request body must be a JSON object", "BAD_REQUEST")
                }
                Err(e) => {
                    return bad_request(mode, &format!("Malformed JSON body: {e}"), "BAD_REQUEST");
                }
            }
        }
        Some("application/graphql") => GqlHttpRequest {
            query: Some(String::from_utf8_lossy(&body).into_owned()),
            ..GqlHttpRequest::default()
        },
        Some(other) => {
            return gql_json(
                mode,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                error_body(
                    &format!(
                        "Unsupported Content-Type {other:?}: use application/json (or application/graphql)"
                    ),
                    "UNSUPPORTED_MEDIA_TYPE",
                ),
            );
        }
    };
    execute(&state, req, false, mode).await
}

async fn graphql_get(
    Extension(state): Extension<GqlState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let mut req = GqlHttpRequest::default();
    let mut errors = Vec::new();
    for (k, v) in form_urlencoded::parse(uri.query().unwrap_or("").as_bytes()) {
        match k.as_ref() {
            "query" => req.query = Some(v.into_owned()),
            "operationName" => req.operation_name = Some(v.into_owned()),
            "variables" => match serde_json::from_str(&v) {
                Ok(val) => req.variables = Some(val),
                Err(_) => errors.push("variables must be JSON"),
            },
            "extensions" => match serde_json::from_str(&v) {
                Ok(val) => req.extensions = Some(val),
                Err(_) => errors.push("extensions must be JSON"),
            },
            _ => {}
        }
    }
    let wants_html = choose(
        &headers,
        &[
            ("text", "html"),
            ("application", "json"),
            ("application", "graphql-response+json"),
        ],
    ) == Some(0);
    if req.query.is_none() && req.extensions.is_none() && wants_html {
        let mut resp = Html(PLAYGROUND_HTML).into_response();
        add_vary(resp.headers_mut(), "Accept");
        return resp;
    }
    let mode = ResponseMode::from_headers(&headers);
    if let Some(e) = errors.first() {
        return bad_request(mode, e, "BAD_REQUEST");
    }
    execute(&state, req, true, mode).await
}

/// Self-contained GraphiQL page with exact, pinned CDN versions. The fetcher
/// and subscription URLs are relative to the page, so it keeps working behind
/// a gateway path prefix.
const PLAYGROUND_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Rustybin GraphiQL</title>
  <link rel="stylesheet" href="https://unpkg.com/graphiql@3.7.1/graphiql.min.css" />
  <style>html, body, #graphiql { height: 100%; margin: 0; }</style>
</head>
<body>
  <div id="graphiql">Loading GraphiQL...</div>
  <script crossorigin src="https://unpkg.com/react@18.3.1/umd/react.production.min.js"></script>
  <script crossorigin src="https://unpkg.com/react-dom@18.3.1/umd/react-dom.production.min.js"></script>
  <script crossorigin src="https://unpkg.com/graphiql@3.7.1/graphiql.min.js"></script>
  <script>
    var path = window.location.pathname.replace(/\/+$/, '');
    var wsUrl = (window.location.protocol === 'https:' ? 'wss://' : 'ws://') + window.location.host + path + '/ws';
    var fetcher = GraphiQL.createFetcher({ url: path, subscriptionUrl: wsUrl });
    ReactDOM.createRoot(document.getElementById('graphiql')).render(
      React.createElement(GraphiQL, {
        fetcher: fetcher,
        defaultQuery: '{\n  users(limit: 3) {\n    id\n    name\n    orders { id status total }\n  }\n}\n',
      })
    );
  </script>
</body>
</html>
"#;

async fn graphql_sdl(Extension(state): Extension<GqlState>) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        state.schema.sdl(),
    )
        .into_response()
}

// ── WebSocket subscriptions ─────────────────────────────────────────

async fn graphql_ws(
    Extension(state): Extension<GqlState>,
    State(config): State<Arc<Config>>,
    protocol: Result<GraphQLProtocol, StatusCode>,
    lease: Option<Extension<crate::limits::StreamLease>>,
    ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
    let ws = match ws {
        Ok(ws) => ws,
        Err(rejection) => return rejection.into_response(),
    };
    let Ok(protocol) = protocol else {
        return gql_json(
            ResponseMode::Json,
            StatusCode::BAD_REQUEST,
            error_body(
                "Sec-WebSocket-Protocol must offer graphql-transport-ws or graphql-ws",
                "BAD_REQUEST",
            ),
        );
    };
    let limits = crate::websocket::limits(&config);
    let schema = state.schema.clone();
    ws.protocols(async_graphql::http::ALL_WEBSOCKET_PROTOCOLS)
        .max_message_size(limits.max_message_size)
        .max_frame_size(limits.max_message_size)
        .on_upgrade(move |socket| async move {
            let lease = lease.map(|Extension(l)| l);
            let (mut sink, mut stream) = futures_util::StreamExt::split(socket);
            let serve = GraphQLWebSocket::new_with_pair(&mut sink, &mut stream, schema, protocol).serve();
            tokio::select! {
                _ = tokio::time::timeout(limits.max_lifetime, serve) => {}
                _ = crate::limits::lease_expired(&lease) => {
                    // The plan's stream lifetime: close 1008 with a reason.
                    let frame = axum::extract::ws::CloseFrame {
                        code: axum::extract::ws::close_code::POLICY,
                        reason: crate::limits::STREAM_END_REASON.into(),
                    };
                    let _ = futures_util::SinkExt::send(&mut sink, axum::extract::ws::Message::Close(Some(frame))).await;
                }
            }
        })
}

// ── Router ──────────────────────────────────────────────────────────

pub fn router(_state: &AppState) -> Router<AppState> {
    let state = GqlState {
        schema: build_schema(),
        apq: Arc::new(ApqCache::new(APQ_CAPACITY, APQ_TTL)),
    };
    Router::new()
        .route("/graphql", get(graphql_get).post(graphql_post))
        .route("/graphql/schema", get(graphql_sdl))
        .route("/graphql/ws", get(graphql_ws))
        .layer(Extension(state))
}

pub fn catalog() -> Vec<Endpoint> {
    vec![
        Endpoint::new(
            "/graphql",
            &["GET", "POST"],
            category::GRAPHQL,
            "GraphQL over HTTP (POST or GET ?query=; GraphiQL for browsers)",
        )
        .description(
            "Honours operationName, application/graphql-response+json or */* (spec status codes), \
             automatic persisted queries (extensions.persistedQuery), depth limit 10, \
             complexity limit 500 and 30 aliases. GET cannot run mutations (405).",
        )
        .example(
            Example::post("Query users", "/graphql")
                .json(r#"{"query":"{ users { id name email } }"}"#),
        )
        .example(Example::get(
            "Query via GET",
            "/graphql?query=%7B%20products(limit%3A%202)%20%7B%20id%20name%20price%20%7D%20%7D",
        ))
        .example(
            Example::post("Named operation", "/graphql")
                .header("Accept", "application/graphql-response+json")
                .json(r#"{"query":"query A { users(limit: 1) { name } } query B { products(limit: 1) { name } }","operationName":"B"}"#),
        ),
        Endpoint::new(
            "/graphql/schema",
            &["GET"],
            category::GRAPHQL,
            "Schema in SDL",
        )
        .example(Example::get("SDL schema", "/graphql/schema")),
        Endpoint::new(
            "/graphql/ws",
            &["GET"],
            category::GRAPHQL,
            "GraphQL subscriptions over WebSocket (graphql-transport-ws and graphql-ws)",
        )
        .description("Subscriptions: ticker(count, intervalMs), orderUpdates(orderId, intervalMs).")
        .websocket()
        .example(
            Example::get("Subscriptions (WebSocket)", "/graphql/ws")
                .header("Sec-WebSocket-Protocol", "graphql-transport-ws")
                .skip_check("WebSocket upgrade"),
        ),
    ]
}

pub fn openapi_paths() -> serde_json::Value {
    json!({
        "/graphql/ws": {
            "get": {
                "tags": ["GraphQL"],
                "summary": "GraphQL subscriptions over WebSocket",
                "description": "WebSocket upgrade with Sec-WebSocket-Protocol graphql-transport-ws (graphql-ws library) or graphql-ws (legacy subscriptions-transport-ws). Subscriptions: ticker(count, intervalMs), orderUpdates(orderId, intervalMs).",
                "operationId": "graphqlSubscriptions",
                "parameters": [{
                    "name": "Sec-WebSocket-Protocol",
                    "in": "header",
                    "required": true,
                    "schema": { "type": "string", "enum": ["graphql-transport-ws", "graphql-ws"] }
                }],
                "responses": {
                    "101": { "description": "Switching Protocols" },
                    "400": { "description": "Missing or unsupported subprotocol" }
                }
            }
        }
    })
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
                    .header("accept", "text/html,application/xhtml+xml,*/*;q=0.8")
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
        let html = crate::test_support::body_string(resp).await;
        // Exact pinned versions only.
        assert!(html.contains("graphiql@3.7.1/graphiql.min.js"));
        assert!(html.contains("react@18.3.1/umd/react.production.min.js"));
        assert!(!html.contains("unpkg.com/graphiql/"));
    }

    const GRAPHQL_RESPONSE_JSON: &str = "application/graphql-response+json";

    async fn send(app: Router, req: Request<Body>) -> (StatusCode, String, serde_json::Value) {
        let resp = app.oneshot(req).await.expect("response");
        let status = resp.status();
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        (status, ct, json_body(resp).await)
    }

    fn post_json(body: serde_json::Value, accept: Option<&str>) -> Request<Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri("/graphql")
            .header("content-type", "application/json");
        if let Some(a) = accept {
            b = b.header("accept", a);
        }
        b.body(Body::from(body.to_string())).expect("request")
    }

    fn get(uri: &str, accept: Option<&str>) -> Request<Body> {
        let mut b = Request::builder().uri(uri);
        if let Some(a) = accept {
            b = b.header("accept", a);
        }
        b.body(Body::empty()).expect("request")
    }

    #[tokio::test]
    async fn operation_name_selects_the_operation() {
        let q = "query A { users(limit: 1) { name } } query B { products(limit: 1) { name } }";
        let (status, _, json) = send(
            test_app(),
            post_json(
                serde_json::json!({ "query": q, "operationName": "B" }),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(json["data"]["products"].is_array());
        assert!(json["data"].get("users").is_none());
        // Without a name, a multi-operation document is an error.
        let (status, _, json) = send(
            test_app(),
            post_json(
                serde_json::json!({ "query": q }),
                Some(GRAPHQL_RESPONSE_JSON),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["errors"].is_array());
    }

    #[tokio::test]
    async fn get_queries_and_mutations() {
        let (status, ct, json) = send(
            test_app(),
            get(
                "/graphql?query=%7B%20users(limit%3A%201)%20%7B%20name%20%7D%20%7D",
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ct, "application/json");
        assert_eq!(json["data"]["users"][0]["name"], "Alice Chen");

        let (status, _, json) = send(
            test_app(),
            get(
                "/graphql?query=mutation%20%7B%20createUser(input%3A%20%7Bname%3A%20%22x%22%2C%20email%3A%20%22y%22%7D)%20%7B%20id%20%7D%20%7D",
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            json["errors"][0]["extensions"]["code"],
            "METHOD_NOT_ALLOWED"
        );

        // GET without a query and without an HTML Accept: JSON 400.
        let (status, _, json) = send(test_app(), get("/graphql", None)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["errors"][0]["message"].is_string());
        let (status, _, _) = send(
            test_app(),
            get("/graphql?query=%7Ba%7D&variables=nope", None),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn graphql_response_json_status_codes() {
        // Validation error: 400 with the new media type, 200 with legacy JSON.
        let bad = serde_json::json!({ "query": "{ nope }" });
        let (status, ct, json) = send(
            test_app(),
            post_json(bad.clone(), Some(GRAPHQL_RESPONSE_JSON)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(ct.starts_with(GRAPHQL_RESPONSE_JSON));
        assert!(json["errors"].is_array());
        let (status, ct, _) = send(test_app(), post_json(bad, None)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ct, "application/json");
        // Syntax error.
        let (status, _, _) = send(
            test_app(),
            post_json(
                serde_json::json!({ "query": "{ users {" }),
                Some(GRAPHQL_RESPONSE_JSON),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // Success with the new media type.
        let (status, ct, json) = send(
            test_app(),
            post_json(
                serde_json::json!({ "query": "{ users(limit: 1) { id } }" }),
                Some(GRAPHQL_RESPONSE_JSON),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(ct.starts_with(GRAPHQL_RESPONSE_JSON));
        assert!(json["data"]["users"].is_array());
    }

    #[tokio::test]
    async fn bad_bodies_and_content_types_get_json_errors() {
        let req = Request::builder()
            .method("POST")
            .uri("/graphql")
            .header("content-type", "text/plain")
            .body(Body::from("{ users { id } }"))
            .expect("request");
        let (status, ct, json) = send(test_app(), req).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(ct, "application/json");
        assert_eq!(
            json["errors"][0]["extensions"]["code"],
            "UNSUPPORTED_MEDIA_TYPE"
        );

        let req = Request::builder()
            .method("POST")
            .uri("/graphql")
            .header("content-type", "application/json")
            .body(Body::from("{not json"))
            .expect("request");
        let (status, _, json) = send(test_app(), req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["errors"][0]["message"].is_string());

        let (status, _, _) = send(
            test_app(),
            post_json(
                serde_json::json!({ "query": "{ users { id } }", "variables": [1] }),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // application/graphql bodies are the query itself.
        let req = Request::builder()
            .method("POST")
            .uri("/graphql")
            .header("content-type", "application/graphql")
            .body(Body::from("{ users(limit: 2) { id } }"))
            .expect("request");
        let (status, _, json) = send(test_app(), req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["data"]["users"].as_array().map(Vec::len), Some(2));
    }

    #[tokio::test]
    async fn depth_complexity_and_alias_limits() {
        // Depth 12: users > (orders > user) x 5 > name
        let mut q = String::from("name");
        for _ in 0..5 {
            q = format!("orders {{ user {{ {q} }} }}");
        }
        let deep = format!("{{ users {{ {q} }} }}");
        let (status, _, json) = send(
            test_app(),
            post_json(
                serde_json::json!({ "query": deep }),
                Some(GRAPHQL_RESPONSE_JSON),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");

        let aliases: String = (0..31)
            .map(|i| format!("a{i}: users(limit: 1) {{ id }} "))
            .collect();
        let (status, _, json) = send(
            test_app(),
            post_json(
                serde_json::json!({ "query": format!("{{ {aliases} }}") }),
                Some(GRAPHQL_RESPONSE_JSON),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            json["errors"][0]["extensions"]["code"],
            "ALIAS_LIMIT_EXCEEDED"
        );

        // Aliases hidden behind a fragment spread twice are counted twice.
        let frag_aliases: String = (0..16).map(|i| format!("f{i}: name ")).collect();
        let q = format!(
            "{{ a: users {{ ...F }} b: users {{ ...F }} }} fragment F on User {{ {frag_aliases} }}"
        );
        let (_, _, json) = send(
            test_app(),
            post_json(serde_json::json!({ "query": q }), None),
        )
        .await;
        assert_eq!(
            json["errors"][0]["extensions"]["code"],
            "ALIAS_LIMIT_EXCEEDED"
        );

        // Complexity: many fields, each counted.
        let fields: String = (0..30)
            .map(|i| format!("x{i}: products {{ id name description price category inStock reviews {{ id rating comment author {{ id name email role createdAt }} }} }} "))
            .collect();
        let (status, _, _) = send(
            test_app(),
            post_json(
                serde_json::json!({ "query": format!("{{ {fields} }}") }),
                Some(GRAPHQL_RESPONSE_JSON),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn automatic_persisted_queries() {
        let app = test_app();
        let query = "{ users(limit: 1) { name } }";
        let hash = sha256_hex(query);
        let ext = serde_json::json!({ "persistedQuery": { "version": 1, "sha256Hash": hash } });

        let (status, _, json) = send(
            app.clone(),
            post_json(serde_json::json!({ "extensions": ext }), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["errors"][0]["message"], "PersistedQueryNotFound");
        assert_eq!(
            json["errors"][0]["extensions"]["code"],
            "PERSISTED_QUERY_NOT_FOUND"
        );

        let (status, _, json) = send(
            app.clone(),
            post_json(
                serde_json::json!({ "query": query, "extensions": ext }),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["data"]["users"][0]["name"], "Alice Chen");

        // Hash only, now registered; also over GET.
        let (_, _, json) = send(
            app.clone(),
            post_json(serde_json::json!({ "extensions": ext }), None),
        )
        .await;
        assert_eq!(json["data"]["users"][0]["name"], "Alice Chen");
        let ext_q: String = form_urlencoded::byte_serialize(ext.to_string().as_bytes()).collect();
        let (_, _, json) = send(
            app.clone(),
            get(&format!("/graphql?extensions={ext_q}"), None),
        )
        .await;
        assert_eq!(json["data"]["users"][0]["name"], "Alice Chen");

        // Mismatched hash.
        let wrong = serde_json::json!({ "persistedQuery": { "version": 1, "sha256Hash": sha256_hex("{ other }") } });
        let (status, _, json) = send(
            app,
            post_json(
                serde_json::json!({ "query": query, "extensions": wrong }),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            json["errors"][0]["extensions"]["code"],
            "PERSISTED_QUERY_HASH_MISMATCH"
        );
    }

    #[test]
    fn apq_cache_is_bounded() {
        let cache = ApqCache::new(3, Duration::from_secs(60));
        for i in 0..10 {
            cache.insert(format!("h{i}"), "{ a }");
        }
        assert_eq!(cache.len(), 3);
        assert!(cache.get("h9").is_some());
        assert!(cache.get("h0").is_none());
        let expiring = ApqCache::new(3, Duration::from_millis(0));
        expiring.insert("h".into(), "{ a }");
        std::thread::sleep(Duration::from_millis(5));
        assert!(expiring.get("h").is_none());
    }

    async fn spawn_server() -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let app = test_app();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        addr
    }

    async fn ws_connect(
        addr: std::net::SocketAddr,
        protocol: &str,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut req = format!("ws://{addr}/graphql/ws")
            .into_client_request()
            .expect("request");
        req.headers_mut()
            .insert("sec-websocket-protocol", protocol.parse().expect("header"));
        let (socket, resp) = tokio_tungstenite::connect_async(req)
            .await
            .expect("connect");
        assert_eq!(
            resp.headers()
                .get("sec-websocket-protocol")
                .and_then(|v| v.to_str().ok()),
            Some(protocol)
        );
        socket
    }

    async fn next_json(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> serde_json::Value {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message as TMessage;
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .expect("in time")
                .expect("message")
                .expect("ok");
            if let TMessage::Text(t) = msg {
                return serde_json::from_str(&t).expect("json");
            }
        }
    }

    #[tokio::test]
    async fn subscriptions_over_graphql_transport_ws() {
        use futures_util::SinkExt;
        use tokio_tungstenite::tungstenite::Message as TMessage;
        let addr = spawn_server().await;
        let mut socket = ws_connect(addr, "graphql-transport-ws").await;
        socket
            .send(TMessage::Text(r#"{"type":"connection_init"}"#.into()))
            .await
            .expect("send");
        assert_eq!(next_json(&mut socket).await["type"], "connection_ack");
        let sub = serde_json::json!({
            "id": "1",
            "type": "subscribe",
            "payload": { "query": "subscription { orderUpdates(orderId: \"o1\", intervalMs: 50) { sequence status } }" }
        });
        socket
            .send(TMessage::Text(sub.to_string()))
            .await
            .expect("send");
        let mut statuses = Vec::new();
        loop {
            let msg = next_json(&mut socket).await;
            match msg["type"].as_str() {
                Some("next") => {
                    statuses.push(msg["payload"]["data"]["orderUpdates"]["status"].clone())
                }
                Some("complete") => break,
                other => panic!("unexpected {other:?}: {msg}"),
            }
        }
        assert_eq!(
            statuses,
            vec!["PENDING", "PROCESSING", "SHIPPED", "DELIVERED"]
        );
    }

    #[tokio::test]
    async fn subscriptions_over_legacy_graphql_ws() {
        use futures_util::SinkExt;
        use tokio_tungstenite::tungstenite::Message as TMessage;
        let addr = spawn_server().await;
        let mut socket = ws_connect(addr, "graphql-ws").await;
        socket
            .send(TMessage::Text(r#"{"type":"connection_init"}"#.into()))
            .await
            .expect("send");
        assert_eq!(next_json(&mut socket).await["type"], "connection_ack");
        let start = serde_json::json!({
            "id": "t",
            "type": "start",
            "payload": { "query": "subscription { ticker(count: 2, intervalMs: 50) { sequence } }" }
        });
        socket
            .send(TMessage::Text(start.to_string()))
            .await
            .expect("send");
        let mut seqs = Vec::new();
        loop {
            let msg = next_json(&mut socket).await;
            match msg["type"].as_str() {
                Some("data") => seqs.push(msg["payload"]["data"]["ticker"]["sequence"].clone()),
                Some("complete") => break,
                Some("ka") => {}
                other => panic!("unexpected {other:?}: {msg}"),
            }
        }
        assert_eq!(seqs, vec![0, 1]);
    }

    #[tokio::test]
    async fn ws_without_subprotocol_is_rejected() {
        let req = Request::builder()
            .uri("/graphql/ws")
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .body(Body::empty())
            .expect("request");
        let resp = test_app().oneshot(req).await.expect("response");
        assert!(resp.status().is_client_error());
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
