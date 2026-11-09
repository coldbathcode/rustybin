//! Profiles: `profiles.json` under `RUSTYBIN_CONFIG_HOME` (else
//! `$XDG_CONFIG_HOME/rustybin`, else `~/.config/rustybin`), mode 0600, one
//! entry per instance with its URL and the token `rustybin login` got; the
//! default profile is the last login. Precedence for a call: `--url` and
//! `--token`, then `RUSTYBIN_URL` and `RUSTYBIN_TOKEN`, then `--profile`
//! (or `RUSTYBIN_PROFILE`, or the default profile), then
//! `http://localhost:<RUSTYBIN_HTTP_PORT or 80>` without a token.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Args, Env, Fail, Result};

pub const FILE: &str = "profiles.json";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// An agent sign-in (`rustybin login --agent`): tokens of the control
    /// plane's own authorization server, accepted on `/_rustybin/mcp` only.
    /// `rustybin mcp-stdio` uses it; no other command sends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentLogin>,
}

/// What `rustybin login --agent` keeps: the OAuth client it registered and
/// the token pair the authorization server issued (an access token of an
/// hour, a refresh token of thirty days).
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLogin {
    pub client_id: String,
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// When the access token expires (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl std::fmt::Debug for AgentLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLogin")
            .field("scope", &self.scope)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profiles {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

/// Where the profiles live (`None` without a home).
pub fn path(env: &Env) -> Option<PathBuf> {
    if let Some(dir) = &env.config_home {
        return Some(dir.join(FILE));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("rustybin").join(FILE));
    }
    env.home
        .as_ref()
        .map(|h| h.join(".config").join("rustybin").join(FILE))
}

pub fn load(path: &Path) -> Result<Profiles> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| Fail::Usage(format!("{}: not a profiles file: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Profiles::default()),
        Err(e) => Err(Fail::Usage(format!("{}: {e}", path.display()))),
    }
}

pub fn save(path: &Path, profiles: &Profiles) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(profiles).unwrap_or_default();
    crate::settings::write_private(path, &bytes).map_err(Fail::Usage)
}

/// A profile name from a URL (`host-port`).
pub fn name_for(url: &str) -> String {
    let trimmed = url
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    let name: String = trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = name.trim_matches('-').to_string();
    if name.is_empty() {
        "default".into()
    } else {
        name
    }
}

/// What a call uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub url: String,
    pub token: Option<String>,
    /// The profile the URL came from, when one did.
    pub profile: Option<String>,
    /// The profile holds an agent sign-in and nothing a command may send
    /// (`rustybin login --agent`: for `rustybin mcp-stdio` only).
    pub agent_only: bool,
}

/// The instance and credential for a call (see the module doc).
pub fn resolve(env: &Env, args: &Args) -> Result<Resolved> {
    let flag_url = args.value("--url").map(normalise);
    let flag_token = args.value("--token").map(str::to_string);
    let profile_name = args.value("--profile").map(str::to_string).or_else(|| {
        std::env::var("RUSTYBIN_PROFILE")
            .ok()
            .filter(|p| !p.is_empty())
    });
    let mut profile: Option<(String, Profile)> = None;
    if let Some(p) = path(env) {
        let all = load(&p)?;
        let name = profile_name.clone().or_else(|| all.default.clone());
        if let Some(name) = name {
            match all.profiles.get(&name) {
                Some(found) => profile = Some((name, found.clone())),
                None if profile_name.is_some() => {
                    return Err(Fail::Usage(format!(
                        "no profile {name:?} in {} (rustybin login --url ... --profile {name})",
                        p.display()
                    )))
                }
                None => {}
            }
        }
    } else if profile_name.is_some() {
        return Err(Fail::Usage(
            "no home directory: profiles are unavailable, use --url and --token".into(),
        ));
    }
    // --url without --profile: the environment, then the profile only when
    // it is for that URL.
    let from_flag = flag_url.is_some();
    let url = flag_url
        .or_else(|| env.url.as_deref().map(normalise))
        .or_else(|| profile.as_ref().map(|(_, p)| p.url.clone()))
        .unwrap_or_else(|| format!("http://localhost:{}", env.http_port.unwrap_or(80)));
    let profile_for_url = profile.filter(|(_, p)| p.url == url);
    // RUSTYBIN_TOKEN (or RUSTYBIN_ADMIN_TOKEN) never goes to another
    // instance a `--url` names (a shell exporting a local admin token would
    // hand it out): only to RUSTYBIN_URL's instance or one on this machine.
    let env_token = env.token.clone().filter(|_| {
        !from_flag
            || env.url.as_deref().map(normalise).as_deref() == Some(url.as_str())
            || is_loopback_url(&url)
    });
    let token = flag_token
        .or(env_token)
        .or_else(|| profile_for_url.as_ref().and_then(|(_, p)| p.token.clone()));
    if token.is_some() && url.starts_with("http://") && !is_loopback_url(&url) {
        eprintln!("warning: sending a token over plain http to {url}: use https");
    }
    let agent_only = token.is_none()
        && profile_for_url
            .as_ref()
            .is_some_and(|(_, p)| p.agent.is_some());
    Ok(Resolved {
        url,
        token,
        profile: profile_for_url.map(|(n, _)| n),
        agent_only,
    })
}

/// A URL as the profiles keep it: a scheme, no trailing slash.
pub fn normalise(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    if u.starts_with("http://") || u.starts_with("https://") {
        u.to_string()
    } else {
        format!("http://{u}")
    }
}

/// Is `url`'s host this machine (localhost, a loopback address)?
pub fn is_loopback_url(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| h.trim_matches(['[', ']']).to_ascii_lowercase())
        })
        .is_some_and(|h| {
            h == "localhost"
                || h.ends_with(".localhost")
                || h.parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
}
