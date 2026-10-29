//! The privacy pass: nothing that holds a stored secret or the admin token
//! leaves the instance in a prompt. The check runs on the rendered prompts
//! (system and user) before a provider is called; a match refuses the call
//! and names the setting, never the value.

use zeroize::Zeroizing;

/// Secrets shorter than this are not matched (too easy to hit by accident).
pub const MIN_SECRET_CHARS: usize = 8;

/// One secret to look for, by the name the refusal mentions.
pub struct Guarded {
    pub name: &'static str,
    pub value: Zeroizing<String>,
}

/// `Err(name)` when `text` contains the value of a guarded secret.
pub fn check(text: &str, guarded: &[Guarded]) -> Result<(), &'static str> {
    for g in guarded {
        if g.value.chars().count() < MIN_SECRET_CHARS {
            continue;
        }
        if text.contains(g.value.as_str()) {
            return Err(g.name);
        }
    }
    Ok(())
}

/// The guarded secrets of an instance: every stored or seeded secret
/// setting and the admin token.
pub fn guarded(settings: &crate::settings::Settings, admin_token: Option<&str>) -> Vec<Guarded> {
    let mut out: Vec<Guarded> = settings
        .specs()
        .iter()
        .filter(|s| s.secret)
        .filter_map(|s| {
            settings
                .secret(s.key)
                .map(|value| Guarded { name: s.key, value })
        })
        .collect();
    if let Some(t) = admin_token {
        out.push(Guarded {
            name: "the admin token",
            value: Zeroizing::new(t.to_string()),
        });
    }
    out
}
