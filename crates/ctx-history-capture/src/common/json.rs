use serde_json::{json, Value};

pub(crate) fn default_metadata() -> Value {
    json!({})
}
