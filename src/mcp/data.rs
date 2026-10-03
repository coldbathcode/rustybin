//! Deterministic fake data shared by MCP tools, resources and prompts.
//!
//! Customers and orders mirror the GraphQL module's users (`u1`..`u5`) and
//! orders (`o1`..`o8`) so cross-protocol demos line up.

use serde_json::{json, Value};

pub struct Customer {
    pub id: &'static str,
    pub name: &'static str,
    pub email: &'static str,
    pub role: &'static str,
    pub created_at: &'static str,
    pub tier: &'static str,
}

pub struct Order {
    pub id: &'static str,
    pub customer_id: &'static str,
    pub status: &'static str,
    pub total: f64,
    pub items: &'static [(&'static str, i64)],
    pub created_at: &'static str,
}

pub static CUSTOMERS: &[Customer] = &[
    Customer {
        id: "u1",
        name: "Alice Chen",
        email: "alice@example.com",
        role: "admin",
        created_at: "2024-01-15T09:30:00Z",
        tier: "gold",
    },
    Customer {
        id: "u2",
        name: "Bob Martinez",
        email: "bob@example.com",
        role: "user",
        created_at: "2024-02-20T14:15:00Z",
        tier: "silver",
    },
    Customer {
        id: "u3",
        name: "Carol Nakamura",
        email: "carol@example.com",
        role: "user",
        created_at: "2024-03-10T11:00:00Z",
        tier: "gold",
    },
    Customer {
        id: "u4",
        name: "David Okafor",
        email: "david@example.com",
        role: "guest",
        created_at: "2024-04-05T16:45:00Z",
        tier: "bronze",
    },
    Customer {
        id: "u5",
        name: "Eva Johansson",
        email: "eva@example.com",
        role: "user",
        created_at: "2024-05-12T08:20:00Z",
        tier: "silver",
    },
];

pub static ORDERS: &[Order] = &[
    Order {
        id: "o1",
        customer_id: "u1",
        status: "delivered",
        total: 229.98,
        items: &[("Mechanical Keyboard", 1), ("Wireless Mouse", 1)],
        created_at: "2024-06-01T10:00:00Z",
    },
    Order {
        id: "o2",
        customer_id: "u2",
        status: "shipped",
        total: 599.99,
        items: &[("Espresso Machine", 1)],
        created_at: "2024-06-05T14:30:00Z",
    },
    Order {
        id: "o3",
        customer_id: "u3",
        status: "processing",
        total: 389.97,
        items: &[("Running Shoes", 2), ("Yoga Mat", 1)],
        created_at: "2024-06-10T09:15:00Z",
    },
    Order {
        id: "o4",
        customer_id: "u1",
        status: "delivered",
        total: 124.98,
        items: &[("Cast Iron Skillet", 1), ("Chef's Knife", 1)],
        created_at: "2024-06-15T16:00:00Z",
    },
    Order {
        id: "o5",
        customer_id: "u4",
        status: "pending",
        total: 151.97,
        items: &[("USB-C Hub", 2), ("Monitor Stand", 1)],
        created_at: "2024-06-20T11:45:00Z",
    },
    Order {
        id: "o6",
        customer_id: "u5",
        status: "cancelled",
        total: 149.99,
        items: &[("Mechanical Keyboard", 1)],
        created_at: "2024-06-25T13:20:00Z",
    },
    Order {
        id: "o7",
        customer_id: "u2",
        status: "shipped",
        total: 254.96,
        items: &[("Resistance Bands Set", 3), ("Running Shoes", 1)],
        created_at: "2024-07-01T08:00:00Z",
    },
    Order {
        id: "o8",
        customer_id: "u3",
        status: "pending",
        total: 669.97,
        items: &[("Espresso Machine", 1), ("Cast Iron Skillet", 2)],
        created_at: "2024-07-05T15:30:00Z",
    },
];

pub const ORDER_STATUSES: &[&str] = &["pending", "processing", "shipped", "delivered", "cancelled"];

pub fn customer_json(c: &Customer) -> Value {
    json!({
        "id": c.id,
        "name": c.name,
        "email": c.email,
        "role": c.role,
        "tier": c.tier,
        "createdAt": c.created_at,
    })
}

pub fn order_json(o: &Order) -> Value {
    let items: Vec<Value> = o
        .items
        .iter()
        .map(|(name, qty)| json!({ "product": name, "quantity": qty }))
        .collect();
    json!({
        "id": o.id,
        "customerId": o.customer_id,
        "status": o.status,
        "total": o.total,
        "items": items,
        "createdAt": o.created_at,
    })
}

/// Find a customer by id (`u1`), email or case-insensitive name fragment.
pub fn find_customer(query: &str) -> Option<&'static Customer> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return None;
    }
    CUSTOMERS
        .iter()
        .find(|c| c.id == q || c.email == q)
        .or_else(|| {
            CUSTOMERS
                .iter()
                .find(|c| c.name.to_ascii_lowercase().contains(&q))
        })
}

pub fn customers_json() -> Value {
    Value::Array(CUSTOMERS.iter().map(customer_json).collect())
}

/// Cities known to the weather tool (others still get a deterministic report).
pub const CITIES: &[&str] = &[
    "Amsterdam",
    "Berlin",
    "Cape Town",
    "Lisbon",
    "London",
    "New York",
    "Paris",
    "San Francisco",
    "Singapore",
    "Sydney",
    "Tokyo",
    "Toronto",
];

const CONDITIONS: &[&str] = &[
    "sunny",
    "partly cloudy",
    "cloudy",
    "light rain",
    "showers",
    "windy",
    "foggy",
    "snow",
];

/// FNV-1a: stable across runs and platforms (unlike `DefaultHasher`).
pub fn stable_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Deterministic fake weather for a city (same city, same answer).
pub fn weather_for(city: &str, units: &str) -> Value {
    let key = city.trim().to_ascii_lowercase();
    let h = stable_hash(&key);
    let celsius = (h % 35) as i64 - 5;
    let condition = CONDITIONS[(h / 35 % CONDITIONS.len() as u64) as usize];
    let humidity = 30 + (h / 7 % 60) as i64;
    let wind_kph = 2 + (h / 11 % 40) as i64;
    let (temperature, unit) = if units == "imperial" {
        (celsius * 9 / 5 + 32, "F")
    } else {
        (celsius, "C")
    };
    json!({
        "city": city.trim(),
        "temperature": temperature,
        "unit": unit,
        "conditions": condition,
        "humidity": humidity,
        "windKph": wind_kph,
        "source": "rustybin deterministic fake weather",
    })
}

pub const README: &str = "# Rustybin MCP demo server\n\n\
This is a mock Model Context Protocol server for API and AI gateway demos.\n\
Everything it returns is fake and deterministic.\n\n\
## Endpoints\n\n\
- `/mcp`: open Streamable HTTP endpoint (2026-07-28, 2025-11-25, 2025-06-18, 2025-03-26)\n\
- `/mcp/protected`: OAuth 2.1 bearer protected (RFC 9728 metadata)\n\
- `/mcp/apikey`: requires an `X-API-Key` header\n\
- `/mcp/servers/{weather,crm,devtools}`: tool subsets for aggregation demos\n\
- `/mcp/sse` + `/mcp/messages`: legacy HTTP+SSE transport (2024-11-05)\n\n\
## Tools\n\n\
Weather, CRM lookups, a calculator, a slow task with progress, failure modes,\n\
large output, images, resource links, elicitation, sampling, request\n\
inspection and a labelled prompt-injection sample for guardrail demos.\n";

/// Text of the runbook embedded in the `incident_report` prompt.
pub const RUNBOOK: &str = "Incident runbook (demo data):\n\
1. Acknowledge the page and open an incident channel.\n\
2. Check the dashboards for error rate, latency and saturation.\n\
3. Roll back the most recent deploy if it correlates with the start time.\n\
4. Post a status update every 30 minutes until resolved.\n\
5. Schedule a blameless review within five business days.\n";

/// Services offered by `incident_report` argument completion.
pub const SERVICES: &[&str] = &[
    "api-gateway",
    "auth-service",
    "billing",
    "checkout",
    "inventory",
    "notifications",
    "orders",
    "payments",
    "search",
];

// ── Minimal PNG encoder (stored deflate blocks, no compression crate) ──

fn crc32(chunks: &[&[u8]]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;
    for chunk in chunks {
        for &b in *chunk {
            crc ^= u32::from(b);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &d in data {
        a = (a + u32::from(d)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
}

/// Encode an RGB image (`size` x `size`, max 64) with a diagonal two-colour
/// gradient. Deterministic, valid PNG, a few KB at most.
pub fn png(size: u32, a: [u8; 3], b: [u8; 3]) -> Vec<u8> {
    let size = size.clamp(1, 64);
    let mut raw = Vec::with_capacity((size * (size * 3 + 1)) as usize);
    for y in 0..size {
        raw.push(0); // filter: none
        for x in 0..size {
            let t = (x + y) as f32 / ((2 * size).max(2) - 2).max(1) as f32;
            for c in 0..3 {
                let v = f32::from(a[c]) * (1.0 - t) + f32::from(b[c]) * t;
                raw.push(v.round().clamp(0.0, 255.0) as u8);
            }
        }
    }
    // zlib stream with stored (uncompressed) deflate blocks of <= 65535 bytes.
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, block) in blocks.iter().enumerate() {
        let last = i + 1 == blocks.len();
        z.push(u8::from(last));
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&size.to_be_bytes());
    ihdr.extend_from_slice(&size.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, RGB, deflate, no filter, no interlace
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", &z);
    png_chunk(&mut out, b"IEND", &[]);
    out
}

/// The Rustybin "logo" (rust orange to dark) used by the blob resource.
pub fn logo_png() -> Vec<u8> {
    png(16, [0xce, 0x42, 0x2b], [0x2b, 0x1d, 0x16])
}

/// Two colours derived from a prompt string.
pub fn colors_for(prompt: &str) -> ([u8; 3], [u8; 3]) {
    let h = stable_hash(prompt).to_be_bytes();
    ([h[0], h[1], h[2]], [h[3], h[4], h[5]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_is_well_formed() {
        let p = png(8, [255, 0, 0], [0, 0, 255]);
        assert_eq!(&p[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        assert_eq!(&p[12..16], b"IHDR");
        assert_eq!(&p[p.len() - 8..p.len() - 4], b"IEND");
        // CRC of an empty IEND chunk is a well known constant.
        assert_eq!(&p[p.len() - 4..], &[0xae, 0x42, 0x60, 0x82]);
    }

    #[test]
    fn weather_is_deterministic() {
        let a = weather_for("Paris", "metric");
        let b = weather_for(" paris ", "metric");
        assert_eq!(a["temperature"], b["temperature"]);
        assert_eq!(a["conditions"], b["conditions"]);
        assert_eq!(weather_for("Paris", "imperial")["unit"], "F");
    }

    #[test]
    fn customer_lookup() {
        assert_eq!(find_customer("u2").map(|c| c.name), Some("Bob Martinez"));
        assert_eq!(find_customer("carol").map(|c| c.id), Some("u3"));
        assert!(find_customer("nobody").is_none());
        assert!(find_customer("").is_none());
    }
}
