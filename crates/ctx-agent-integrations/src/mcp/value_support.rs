use serde_json::Value;

use super::{
    invalid_tool_request, optional_strings, ToolBackendError, MAX_PROVIDER_ROOT_SELECTORS,
};

pub(super) fn provider_root_selectors(
    arguments: &Value,
    key: &str,
) -> Result<Vec<String>, ToolBackendError> {
    let values = optional_strings(arguments, key)?;
    if values.len() > MAX_PROVIDER_ROOT_SELECTORS {
        return Err(invalid_tool_request(format!(
            "{key} exceeds the maximum of {MAX_PROVIDER_ROOT_SELECTORS} entries"
        )));
    }
    if values
        .iter()
        .any(|value| !provider_root_selector_is_valid(value))
    {
        return Err(invalid_tool_request(format!(
            "{key} entries must each be 1..=64 ASCII letters, digits, hyphens, or underscores"
        )));
    }
    Ok(values)
}

fn provider_root_selector_is_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(super) fn compact_json(mut value: Value) -> Value {
    prune_null_json(&mut value);
    value
}

fn prune_null_json(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, nested| {
                prune_null_json(nested);
                !nested.is_null()
            });
        }
        Value::Array(items) => {
            for item in items {
                prune_null_json(item);
            }
        }
        _ => {}
    }
}
