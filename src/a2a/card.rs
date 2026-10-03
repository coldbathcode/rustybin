//! Agent Cards: v1.0 (`supportedInterfaces`), legacy v0.3 (`url` +
//! `preferredTransport`), the v0.3 ProtoJSON flavour used by the v0.3 REST
//! `/v1/card` endpoint, and the "hybrid" card served at
//! `.well-known/agent-card.json` (v1.0 plus the v0.3 fields, which is what
//! the reference SDK serves so both client generations can read it).

use serde_json::{json, Map, Value};

use super::agents::{AgentDef, SkillDef, AGENTS, SECURE_EXTENDED_SKILLS};

/// URI of the extension listing every demo agent on the default card.
pub const DIRECTORY_EXTENSION: &str = "urn:rustybin:a2a:agent-directory:v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardKind {
    /// Public card.
    Public,
    /// Authenticated extended card (more skills).
    Extended,
}

pub fn agent_url(base: &str, agent: &AgentDef) -> String {
    format!("{base}/a2a/{}", agent.id)
}

fn skills(agent: &AgentDef, kind: CardKind) -> Vec<&'static SkillDef> {
    let mut out: Vec<&'static SkillDef> = agent.skills.iter().collect();
    if kind == CardKind::Extended && agent.secured {
        out.extend(SECURE_EXTENDED_SKILLS.iter());
    }
    out
}

fn skill_json(s: &SkillDef) -> Value {
    json!({
        "id": s.id,
        "name": s.name,
        "description": s.description,
        "tags": s.tags,
        "examples": s.examples,
        "inputModes": s.input_modes,
        "outputModes": s.output_modes,
    })
}

fn description(agent: &AgentDef, kind: CardKind) -> String {
    match kind {
        CardKind::Public => agent.description.to_string(),
        CardKind::Extended => format!(
            "{} (authenticated extended card: includes the audit-log skill)",
            agent.description
        ),
    }
}

fn provider(base: &str) -> Value {
    json!({ "organization": "Rustybin", "url": format!("{base}/") })
}

fn scopes() -> Value {
    json!({ "openid": "OpenID Connect", "profile": "Basic profile", "email": "Email address" })
}

/// v1.0 / v0.3-proto security schemes (oneof wrappers).
fn security_schemes_proto(base: &str) -> Value {
    let discovery = format!("{base}/.well-known/openid-configuration");
    json!({
        "bearer": { "httpAuthSecurityScheme": {
            "description": "RS256 JWT access token issued by this server's built-in OIDC provider",
            "scheme": "Bearer",
            "bearerFormat": "JWT",
        }},
        "oauth2": { "oauth2SecurityScheme": {
            "description": "Client credentials flow against the built-in IdP (client_id rustybin, any secret)",
            "flows": { "clientCredentials": {
                "tokenUrl": format!("{base}/oauth/token"),
                "scopes": scopes(),
            }},
            "oauth2MetadataUrl": format!("{base}/.well-known/oauth-authorization-server"),
        }},
        "oidc": { "openIdConnectSecurityScheme": {
            "description": "OpenID Connect discovery of the built-in IdP",
            "openIdConnectUrl": discovery,
        }},
    })
}

/// v0.3 JSON security schemes (OpenAPI style `type` discriminator).
fn security_schemes_v03(base: &str) -> Value {
    let discovery = format!("{base}/.well-known/openid-configuration");
    json!({
        "bearer": {
            "type": "http",
            "scheme": "bearer",
            "bearerFormat": "JWT",
            "description": "RS256 JWT access token issued by this server's built-in OIDC provider",
        },
        "oauth2": {
            "type": "oauth2",
            "description": "Client credentials flow against the built-in IdP (client_id rustybin, any secret)",
            "flows": { "clientCredentials": {
                "tokenUrl": format!("{base}/oauth/token"),
                "scopes": scopes(),
            }},
        },
        "oidc": {
            "type": "openIdConnect",
            "description": "OpenID Connect discovery of the built-in IdP",
            "openIdConnectUrl": discovery,
        },
    })
}

fn security_requirements_proto() -> Value {
    json!([
        { "schemes": { "bearer": { "list": [] } } },
        { "schemes": { "oauth2": { "list": [] } } },
    ])
}

fn directory(base: &str) -> Value {
    let agents: Vec<Value> = AGENTS
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "name": a.name,
                "url": agent_url(base, a),
                "cardUrl": format!("{}/.well-known/agent-card.json", agent_url(base, a)),
            })
        })
        .collect();
    json!({
        "uri": DIRECTORY_EXTENSION,
        "description": "Directory of the demo agents served by this host (each has its own card and endpoint)",
        "params": { "agents": agents },
    })
}

/// Pure v1.0 Agent Card (`lf.a2a.v1.AgentCard` ProtoJSON).
pub fn v1(base: &str, agent: &AgentDef, kind: CardKind, with_directory: bool) -> Value {
    let url = agent_url(base, agent);
    let mut caps = json!({
        "streaming": true,
        "pushNotifications": true,
        "extendedAgentCard": agent.secured,
    });
    if with_directory {
        caps["extensions"] = json!([directory(base)]);
    }
    let mut card = json!({
        "name": agent.name,
        "description": description(agent, kind),
        "supportedInterfaces": [
            { "url": url, "protocolBinding": "JSONRPC", "protocolVersion": "1.0" },
            { "url": format!("{url}/v1"), "protocolBinding": "HTTP+JSON", "protocolVersion": "1.0" },
            { "url": url, "protocolBinding": "JSONRPC", "protocolVersion": "0.3" },
            { "url": url, "protocolBinding": "HTTP+JSON", "protocolVersion": "0.3" },
        ],
        "provider": provider(base),
        "version": env!("CARGO_PKG_VERSION"),
        "documentationUrl": format!("{base}/docs"),
        "capabilities": caps,
        "defaultInputModes": agent.input_modes,
        "defaultOutputModes": agent.output_modes,
        "skills": skills(agent, kind).into_iter().map(skill_json).collect::<Vec<_>>(),
    });
    if agent.secured {
        card["securitySchemes"] = security_schemes_proto(base);
        card["securityRequirements"] = security_requirements_proto();
    }
    card
}

/// Legacy v0.3 Agent Card (JSON-RPC era JSON schema).
pub fn v03(base: &str, agent: &AgentDef, kind: CardKind) -> Value {
    let url = agent_url(base, agent);
    let mut card = json!({
        "protocolVersion": "0.3.0",
        "name": agent.name,
        "description": description(agent, kind),
        "url": url,
        "preferredTransport": "JSONRPC",
        "additionalInterfaces": [
            { "url": url, "transport": "JSONRPC" },
            { "url": url, "transport": "HTTP+JSON" },
        ],
        "provider": provider(base),
        "version": env!("CARGO_PKG_VERSION"),
        "documentationUrl": format!("{base}/docs"),
        "capabilities": {
            "streaming": true,
            "pushNotifications": true,
            "stateTransitionHistory": false,
        },
        "defaultInputModes": agent.input_modes,
        "defaultOutputModes": agent.output_modes,
        "skills": skills(agent, kind).into_iter().map(skill_json).collect::<Vec<_>>(),
    });
    if agent.secured {
        card["securitySchemes"] = security_schemes_v03(base);
        card["security"] = json!([{ "bearer": [] }, { "oauth2": [] }]);
        card["supportsAuthenticatedExtendedCard"] = json!(true);
    }
    card
}

/// v0.3 card as ProtoJSON of the v0.3 proto (v0.3 HTTP+JSON `/v1/card`).
pub fn v03_proto(base: &str, agent: &AgentDef, kind: CardKind) -> Value {
    let mut card = v03(base, agent, kind);
    if agent.secured {
        card["securitySchemes"] = security_schemes_proto(base);
        card["security"] = security_requirements_proto();
    }
    card
}

/// Recursive merge used for the hybrid card (adds missing keys only).
fn merge(target: &mut Value, extra: &Value) {
    match (target, extra) {
        (Value::Object(t), Value::Object(e)) => {
            for (k, v) in e {
                match t.get_mut(k) {
                    None => {
                        t.insert(k.clone(), v.clone());
                    }
                    Some(existing) => merge(existing, v),
                }
            }
        }
        (Value::Array(t), Value::Array(e)) => {
            for (a, b) in t.iter_mut().zip(e.iter()) {
                if a.is_object() && b.is_object() {
                    merge(a, b);
                }
            }
        }
        _ => {}
    }
}

/// The card served at `.well-known/agent-card.json`: v1.0 plus the v0.3
/// top-level fields so v0.3 clients can read it too.
pub fn hybrid(base: &str, agent: &AgentDef, with_directory: bool) -> Value {
    let mut card = v1(base, agent, CardKind::Public, with_directory);
    let legacy = v03(base, agent, CardKind::Public);
    merge(&mut card, &legacy);
    card
}

/// `GET /a2a`: a human friendly index of the agents.
pub fn directory_index(base: &str) -> Value {
    let agents: Vec<Value> = AGENTS
        .iter()
        .map(|a| {
            let url = agent_url(base, a);
            let mut o = Map::new();
            o.insert("id".into(), json!(a.id));
            o.insert("name".into(), json!(a.name));
            o.insert("description".into(), json!(a.description));
            o.insert("jsonrpc".into(), json!(url));
            o.insert("rest".into(), json!(format!("{url}/v1")));
            o.insert(
                "card".into(),
                json!(format!("{url}/.well-known/agent-card.json")),
            );
            o.insert(
                "legacyCard".into(),
                json!(format!("{url}/.well-known/agent.json")),
            );
            o.insert("requiresAuth".into(), json!(a.secured));
            Value::Object(o)
        })
        .collect();
    json!({
        "protocolVersions": ["1.0", "0.3"],
        "versionHeader": "A2A-Version (empty means 0.3)",
        "defaultAgent": format!("{base}/a2a"),
        "agents": agents,
        "webhookSink": format!("{base}/a2a/webhook-sink/{{id}}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a::agents::find;

    #[test]
    fn v1_card_is_accurate() {
        let a = find("secure").expect("secure");
        let c = v1("http://h:1", a, CardKind::Public, false);
        assert_eq!(c["supportedInterfaces"][0]["url"], "http://h:1/a2a/secure");
        assert_eq!(
            c["supportedInterfaces"][1]["url"],
            "http://h:1/a2a/secure/v1"
        );
        assert_eq!(c["supportedInterfaces"][1]["protocolBinding"], "HTTP+JSON");
        assert_eq!(c["capabilities"]["extendedAgentCard"], true);
        assert_eq!(
            c["securitySchemes"]["oauth2"]["oauth2SecurityScheme"]["oauth2MetadataUrl"],
            "http://h:1/.well-known/oauth-authorization-server"
        );
        assert!(c.get("url").is_none(), "pure v1 card has no legacy fields");
        let ext = v1("http://h:1", a, CardKind::Extended, false);
        assert!(
            ext["skills"].as_array().map(|s| s.len()).unwrap_or(0)
                > c["skills"].as_array().map(|s| s.len()).unwrap_or(0)
        );
    }

    #[test]
    fn hybrid_card_has_both_generations() {
        let a = find("echo").expect("echo");
        let c = hybrid("http://h", a, true);
        assert_eq!(c["url"], "http://h/a2a/echo");
        assert_eq!(c["preferredTransport"], "JSONRPC");
        assert_eq!(c["protocolVersion"], "0.3.0");
        assert!(c["supportedInterfaces"].is_array());
        assert_eq!(
            c["capabilities"]["extensions"][0]["uri"],
            DIRECTORY_EXTENSION
        );
        let listed = c["capabilities"]["extensions"][0]["params"]["agents"]
            .as_array()
            .map(|a| a.len());
        assert_eq!(listed, Some(AGENTS.len()));
    }

    #[test]
    fn legacy_card_security() {
        let a = find("secure").expect("secure");
        let c = v03("http://h", a, CardKind::Public);
        assert_eq!(c["securitySchemes"]["bearer"]["type"], "http");
        assert_eq!(c["supportsAuthenticatedExtendedCard"], true);
        assert_eq!(c["security"][0]["bearer"], json!([]));
        let p = v03_proto("http://h", a, CardKind::Public);
        assert!(p["securitySchemes"]["bearer"]["httpAuthSecurityScheme"].is_object());
    }
}
