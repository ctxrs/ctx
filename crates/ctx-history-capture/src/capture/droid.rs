#[allow(unused_imports)]
use super::*;

pub(crate) fn droid_content_has(value: &Value, expected: &str) -> bool {
    value
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
        .unwrap_or(false)
}
