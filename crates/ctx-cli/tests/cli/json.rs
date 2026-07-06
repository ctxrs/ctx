#[allow(unused_imports)]
use super::*;

pub(crate) fn json_output(command: &mut Command) -> Value {
    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).unwrap()
}

pub(crate) fn assert_omits_keys(value: &Value, forbidden_keys: &[&str]) {
    match value {
        Value::Object(map) => {
            for key in forbidden_keys {
                assert!(
                    !map.contains_key(*key),
                    "forbidden JSON key {key} appeared in {value:#}"
                );
            }
            for nested in map.values() {
                assert_omits_keys(nested, forbidden_keys);
            }
        }
        Value::Array(items) => {
            for item in items {
                assert_omits_keys(item, forbidden_keys);
            }
        }
        _ => {}
    }
}

#[test]
pub(crate) fn sources_default_hides_unsupported_missing_locations() {
    let temp = tempdir();

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    assert_eq!(sources["scope"], "default");
    assert!(sources["hidden_missing_sources"].as_u64().unwrap() > 0);
    let visible = sources["sources"].as_array().unwrap();
    assert!(visible.iter().any(|source| source["provider"] == "codex"));
    assert!(visible.iter().any(|source| source["provider"] == "claude"));
    assert!(visible.iter().any(|source| source["provider"] == "cursor"));
    assert!(visible.iter().any(|source| source["provider"] == "pi"));
    assert!(visible
        .iter()
        .any(|source| source["provider"] == "opencode"));
    assert!(visible
        .iter()
        .any(|source| source["provider"] == "copilot_cli"));

    let text = ctx(&temp)
        .arg("sources")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(text).unwrap();
    assert!(text.contains("missing provider locations hidden"));
    assert!(text.contains("ctx sources --all"));

    let all_sources = json_output(ctx(&temp).args(["sources", "--json", "--all"]));
    assert_eq!(all_sources["scope"], "all");
    assert_eq!(all_sources["hidden_missing_sources"], 0);
    let all = all_sources["sources"].as_array().unwrap();
    assert!(all.len() > visible.len());
}

#[test]
pub(crate) fn sources_provider_filter_rejects_unsupported_providers() {
    let temp = tempdir();

    ctx(&temp)
        .args(["sources", "--provider", "not-a-real-provider", "--json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown provider"));
}
