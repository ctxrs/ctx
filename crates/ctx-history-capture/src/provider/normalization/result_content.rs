use serde_json::Value;

/// Deterministically renders one provider-selected result value without
/// searching it for output-shaped field names.
///
/// Provider adapters must select the native result field explicitly before
/// calling this helper. The renderer itself is deliberately unbounded: callers
/// that retain or return the content own their byte limit.
pub(crate) fn provider_normalized_result_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        Value::Array(items) => items
            .iter()
            .map(provider_normalized_result_value)
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
        Value::Number(_) | Value::Bool(_) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn result_value_rendering_is_unbounded_and_does_not_search_objects() {
        let long = "x".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 17);
        assert_eq!(
            provider_normalized_result_value(&json!([long.clone(), 7, false])),
            format!("{long}\n7\nfalse")
        );
        assert_eq!(
            provider_normalized_result_value(&json!({"output": "kept as json"})),
            r#"{"output":"kept as json"}"#
        );
    }
}
