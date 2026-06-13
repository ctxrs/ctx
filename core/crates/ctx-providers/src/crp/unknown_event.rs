use serde_json::{json, Value};

const UNKNOWN_EVENT_MAX_DEPTH: usize = 5;
const UNKNOWN_EVENT_MAX_KEYS: usize = 24;
const UNKNOWN_EVENT_MAX_ITEMS: usize = 24;
const UNKNOWN_EVENT_MAX_STRING_CHARS: usize = 400;

fn truncate_unknown_string(value: &str) -> (String, bool) {
    let mut chars = value.chars();
    let truncated: String = chars
        .by_ref()
        .take(UNKNOWN_EVENT_MAX_STRING_CHARS)
        .collect();
    if chars.next().is_some() {
        (format!("{truncated}..."), true)
    } else {
        (truncated, false)
    }
}

pub(super) fn bound_unknown_crp_payload(value: Value) -> (Value, bool) {
    bound_unknown_crp_payload_inner(value, 0)
}

fn bound_unknown_crp_payload_inner(value: Value, depth: usize) -> (Value, bool) {
    if depth >= UNKNOWN_EVENT_MAX_DEPTH {
        return (json!("[truncated]"), true);
    }
    match value {
        Value::String(text) => {
            let (text, truncated) = truncate_unknown_string(&text);
            (Value::String(text), truncated)
        }
        Value::Array(items) => {
            let mut truncated = false;
            let total = items.len();
            let mut out = Vec::with_capacity(total.min(UNKNOWN_EVENT_MAX_ITEMS) + 1);
            for item in items.into_iter().take(UNKNOWN_EVENT_MAX_ITEMS) {
                let (bounded, item_truncated) = bound_unknown_crp_payload_inner(item, depth + 1);
                truncated |= item_truncated;
                out.push(bounded);
            }
            if total > UNKNOWN_EVENT_MAX_ITEMS {
                truncated = true;
                out.push(json!({
                    "_truncated_items": total - UNKNOWN_EVENT_MAX_ITEMS,
                }));
            }
            (Value::Array(out), truncated)
        }
        Value::Object(map) => {
            let mut truncated = false;
            let total = map.len();
            let mut out = serde_json::Map::with_capacity(total.min(UNKNOWN_EVENT_MAX_KEYS) + 1);
            for (idx, (key, value)) in map.into_iter().enumerate() {
                if idx >= UNKNOWN_EVENT_MAX_KEYS {
                    truncated = true;
                    break;
                }
                let (bounded, value_truncated) = bound_unknown_crp_payload_inner(value, depth + 1);
                truncated |= value_truncated;
                out.insert(key, bounded);
            }
            if total > UNKNOWN_EVENT_MAX_KEYS {
                out.insert(
                    "_truncated_keys".to_string(),
                    json!(total - UNKNOWN_EVENT_MAX_KEYS),
                );
            }
            (Value::Object(out), truncated)
        }
        other => (other, false),
    }
}
