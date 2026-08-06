use serde_json::json;

use super::compact_json;

#[test]
fn compact_json_preserves_non_null_bytes_and_prunes_only_object_nulls() {
    let value = compact_json(json!({
        "a": null,
        "b": {"c": null, "d": 1},
        "e": [null, {"f": null, "g": "x"}],
    }));
    assert_eq!(
        serde_json::to_string(&value).unwrap(),
        r#"{"b":{"d":1},"e":[null,{"g":"x"}]}"#
    );
}
