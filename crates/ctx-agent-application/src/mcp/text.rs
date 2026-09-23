use serde_json::Value;

/// Renders a bounded, schema-agnostic text projection when a product has no
/// richer MCP text representation for a tool result.
pub fn render_generic_tool_text(value: &Value) -> String {
    let mut out = String::from("ctx tool result\n");
    match value {
        Value::Object(object) => {
            let mut fields: Vec<_> = object.iter().collect();
            fields.sort_unstable_by_key(|(left, _)| *left);
            for (key, value) in fields.into_iter().take(12) {
                match value {
                    Value::Array(values) => {
                        out.push_str(&format!("{key}: [{} items]\n", values.len()));
                    }
                    Value::Object(fields) => {
                        out.push_str(&format!("{key}: [{} fields]\n", fields.len()));
                    }
                    _ => push_key_value(&mut out, key, value),
                }
            }
            push_omitted_line(&mut out, object.len(), 12, "fields");
        }
        Value::Array(values) => {
            out.push_str(&format!("items: {}\n", values.len()));
            for (index, value) in values.iter().take(12).enumerate() {
                out.push_str(&format!("{}. {}\n", index + 1, scalar_text(value)));
            }
            push_omitted_line(&mut out, values.len(), 12, "items");
        }
        _ => push_key_value(&mut out, "value", value),
    }
    out
}

fn push_key_value(out: &mut String, key: &str, value: &Value) {
    if let Some(value) = value_to_text(value) {
        out.push_str(&format!("{key}: {value}\n"));
    }
}

fn value_to_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::Array(_) | Value::Object(_) => None,
    }
}

fn scalar_text(value: &Value) -> String {
    value_to_text(value).unwrap_or_else(|| match value {
        Value::Array(values) => format!("[{} values]", values.len()),
        Value::Object(object) => format!("[{} fields]", object.len()),
        Value::Null => "null".to_owned(),
        Value::String(_) | Value::Bool(_) | Value::Number(_) => unreachable!(),
    })
}

fn push_omitted_line(out: &mut String, total: usize, shown: usize, noun: &str) {
    if total > shown {
        out.push_str(&format!(
            "... {} more {noun} omitted from text\n",
            total - shown
        ));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn generic_text_is_bounded_without_a_json_round_trip() {
        assert_eq!(
            render_generic_tool_text(&json!({
                "payload_type": "status",
                "generation": 7,
                "ready": true
            })),
            "ctx tool result\ngeneration: 7\npayload_type: status\nready: true\n"
        );
    }

    #[test]
    fn generic_text_selects_lexical_fields_before_applying_the_bound() {
        let mut fields = serde_json::Map::new();
        for index in (0..14).rev() {
            fields.insert(format!("field_{index:02}"), json!(index));
        }
        assert_eq!(
            render_generic_tool_text(&Value::Object(fields)),
            concat!(
                "ctx tool result\n",
                "field_00: 0\n",
                "field_01: 1\n",
                "field_02: 2\n",
                "field_03: 3\n",
                "field_04: 4\n",
                "field_05: 5\n",
                "field_06: 6\n",
                "field_07: 7\n",
                "field_08: 8\n",
                "field_09: 9\n",
                "field_10: 10\n",
                "field_11: 11\n",
                "... 2 more fields omitted from text\n",
            )
        );
    }
}
