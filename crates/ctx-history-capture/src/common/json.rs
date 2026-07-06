use serde_json::{json, Value};

pub(crate) fn sanitize_value(value: Value) -> (Value, bool) {
    (value, false)
}

pub(crate) fn default_metadata() -> Value {
    json!({})
}

pub(crate) fn payload_has_record_fields(value: &Value) -> bool {
    [
        "title",
        "body",
        "summary",
        "tags",
        "record_kind",
        "history_record_kind",
        "workspace",
    ]
    .iter()
    .any(|field| value.get(*field).is_some())
}
