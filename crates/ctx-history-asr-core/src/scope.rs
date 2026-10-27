//! Installation namespace custody and composition with native container identities.
use crate::{Provider, Result};
use serde_json::{json, Value};

pub fn qualify(value: &mut Value, provider: Provider, installation: &str) -> Result<()> {
    match value {
        Value::String(reference) if reference.starts_with(&format!("{}:", provider.agent())) => {
            let (session, fragment) = reference
                .split_once('#')
                .map(|(a, b)| (a, format!("#{b}")))
                .unwrap_or((reference.as_str(), String::new()));
            let parts: Vec<_> = session.split(':').collect();
            let namespace = if parts.len() == 3 {
                let native = decode(parts[1])?;
                json!([installation, native]).to_string()
            } else {
                installation.to_owned()
            };
            *reference = format!(
                "{}:{}:{}{}",
                provider.agent(),
                encode(&namespace),
                parts.last().unwrap(),
                fragment
            );
        }
        Value::Array(items) => {
            for item in items {
                qualify(item, provider, installation)?;
            }
        }
        Value::Object(fields) => {
            for (key, child) in fields {
                if matches!(key.as_str(), "session" | "from")
                    || (!child.is_string()
                        && !matches!(
                            key.as_str(),
                            "_meta" | "native" | "raw" | "input" | "content" | "displayContent"
                        ))
                {
                    qualify(child, provider, installation)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn decode(value: &str) -> Result<String> {
    let mut bytes = Vec::new();
    let mut at = 0;
    while at < value.len() {
        if value.as_bytes()[at] == b'%' {
            let pair = value
                .get(at + 1..at + 3)
                .ok_or_else(|| crate::Error::Invalid("invalid session reference escape".into()))?;
            bytes.push(
                u8::from_str_radix(pair, 16).map_err(|_| {
                    crate::Error::Invalid("invalid session reference escape".into())
                })?,
            );
            at += 3;
        } else {
            bytes.push(value.as_bytes()[at]);
            at += 1;
        }
    }
    String::from_utf8(bytes).map_err(|_| crate::Error::Invalid("non-UTF8 session reference".into()))
}
