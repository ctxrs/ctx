#[test]
fn explicit_crate_dependency_is_available_without_shared_support() {
    let value: serde_yaml::Value = serde_yaml::from_str("mode: minimal\n").unwrap();
    assert_eq!(value["mode"].as_str(), Some("minimal"));
}
