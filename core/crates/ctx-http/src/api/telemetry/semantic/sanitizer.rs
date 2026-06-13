use ctx_observability::telemetry::TelemetryProperties;
use serde_json::{Map, Value};

pub(super) const MAX_TELEMETRY_STRING_LENGTH: usize = 512;

const MAX_TELEMETRY_PROPERTY_COUNT: usize = 64;
const MAX_TELEMETRY_KEY_LENGTH: usize = 80;

pub(super) fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

pub(super) fn sanitize_semantic_properties(raw: Map<String, Value>) -> TelemetryProperties {
    let mut out = TelemetryProperties::new();
    for (key, value) in raw {
        if out.len() >= MAX_TELEMETRY_PROPERTY_COUNT {
            break;
        }
        if key.trim().is_empty() || key.len() > MAX_TELEMETRY_KEY_LENGTH {
            continue;
        }
        if is_forbidden_semantic_property_key(&key) {
            continue;
        }
        let Some(value) = sanitize_semantic_value(value) else {
            continue;
        };
        out.insert(key, value);
    }
    out
}

fn sanitize_semantic_value(value: Value) -> Option<Value> {
    match value {
        Value::Null => Some(Value::Null),
        Value::Bool(value) => Some(Value::Bool(value)),
        Value::Number(value) => {
            if value.is_f64() {
                value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .map(Value::from)
            } else {
                Some(Value::Number(value))
            }
        }
        Value::String(value) => Some(Value::String(
            value.chars().take(MAX_TELEMETRY_STRING_LENGTH).collect(),
        )),
        _ => None,
    }
}

fn normalized_property_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_forbidden_semantic_property_key(key: &str) -> bool {
    let normalized = normalized_property_key(key);
    matches!(
        normalized.as_str(),
        "workspaceid"
            | "taskid"
            | "sessionid"
            | "worktreeid"
            | "runid"
            | "turnid"
            | "accountid"
            | "orgid"
            | "organizationid"
            | "userid"
            | "email"
    ) || [
        "prompt",
        "code",
        "filepath",
        "reponame",
        "branch",
        "command",
        "token",
        "secret",
        "apikey",
        "password",
        "authorization",
        "cookie",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_properties_drop_nested_values_and_bound_strings() {
        let mut properties = Map::new();
        properties.insert("ok".to_string(), Value::Bool(true));
        properties.insert("nested".to_string(), serde_json::json!({ "bad": "value" }));
        properties.insert("workspace_id".to_string(), Value::String("raw".to_string()));
        properties.insert("sessionId".to_string(), Value::String("raw".to_string()));
        properties.insert("account_id".to_string(), Value::String("raw".to_string()));
        properties.insert("org_id".to_string(), Value::String("raw".to_string()));
        properties.insert("email".to_string(), Value::String("raw".to_string()));
        properties.insert("prompt_body".to_string(), Value::String("raw".to_string()));
        properties.insert("command".to_string(), Value::String("raw".to_string()));
        properties.insert(
            "long".to_string(),
            Value::String("x".repeat(MAX_TELEMETRY_STRING_LENGTH + 20)),
        );

        let sanitized = sanitize_semantic_properties(properties);
        assert_eq!(sanitized.get("ok"), Some(&Value::Bool(true)));
        assert!(!sanitized.contains_key("nested"));
        assert!(!sanitized.contains_key("workspace_id"));
        assert!(!sanitized.contains_key("sessionId"));
        assert!(!sanitized.contains_key("account_id"));
        assert!(!sanitized.contains_key("org_id"));
        assert!(!sanitized.contains_key("email"));
        assert!(!sanitized.contains_key("prompt_body"));
        assert!(!sanitized.contains_key("command"));
        assert_eq!(
            sanitized.get("long"),
            Some(&Value::String("x".repeat(MAX_TELEMETRY_STRING_LENGTH)))
        );
    }
}
