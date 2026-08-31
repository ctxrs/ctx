use super::*;

fn unthrottled_builtin_config() -> DaemonConfigSnapshot {
    DaemonConfigSnapshot {
        enabled: true,
        mode: DaemonMode::Full,
        semantic_enabled: true,
        semantic_executor: "builtin".to_owned(),
        semantic_contract_fingerprint: "sha256:builtin-space".to_owned(),
        semantic_builtin_throttling_configured: false,
        semantic_builtin_throttling_effective: Some(false),
    }
}

fn unthrottled_builtin_value() -> Value {
    json!({
        "daemon_enabled": true,
        "daemon_mode": "full",
        "semantic_enabled": true,
        "semantic_executor": "builtin",
        "semantic_contract_fingerprint": "sha256:builtin-space",
        "semantic_builtin_throttling_configured": false,
        "semantic_builtin_throttling_effective": false,
    })
}

#[test]
fn readiness_identity_requires_exact_explicitly_unthrottled_builtin_state() {
    let expected = unthrottled_builtin_config();
    let exact = unthrottled_builtin_value();
    assert!(daemon_config_value_matches(&exact, &expected));

    for replacement in [Some(json!(true)), Some(Value::Null), None] {
        let mut stale = exact.clone();
        match replacement {
            Some(value) => stale["semantic_builtin_throttling_effective"] = value,
            None => {
                stale
                    .as_object_mut()
                    .unwrap()
                    .remove("semantic_builtin_throttling_effective");
            }
        }
        assert!(!daemon_config_value_matches(&stale, &expected));
    }

    for field in [
        "semantic_builtin_throttling_configured",
        "semantic_builtin_throttling_effective",
    ] {
        let mut stale = exact.clone();
        stale.as_object_mut().unwrap().remove(field);
        assert!(!daemon_config_value_matches(&stale, &expected));
    }
}
