use super::*;

#[derive(Debug, Clone, Copy)]
pub struct DaemonConfigReloadContext<'a> {
    pub status: &'a str,
    pub out_of_sync: bool,
    pub requested_daemon_enabled: Option<bool>,
    pub requested_semantic_enabled: Option<bool>,
    pub requested_semantic_executor: Option<&'a str>,
    pub requested_semantic_contract_fingerprint: Option<&'a str>,
    pub requested_semantic_builtin_throttling_configured: Option<bool>,
    pub requested_semantic_builtin_throttling_effective: Option<bool>,
    pub applied_daemon_enabled: Option<bool>,
    pub applied_semantic_enabled: Option<bool>,
    pub applied_semantic_executor: Option<&'a str>,
    pub applied_semantic_contract_fingerprint: Option<&'a str>,
    pub applied_semantic_builtin_throttling_configured: Option<bool>,
    pub applied_semantic_builtin_throttling_effective: Option<bool>,
    pub last_error: Option<&'a str>,
}

pub(super) fn daemon_config_reload_report(
    daemon_status: Option<&Value>,
    running: bool,
    current_config: Option<&DaemonConfigSnapshot>,
) -> Value {
    let persisted = daemon_status
        .and_then(|value| value.get("config_reload"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let applied_daemon_enabled = persisted
        .get("applied")
        .and_then(|value| value.get("daemon_enabled"))
        .and_then(Value::as_bool);
    let applied_daemon_mode = persisted
        .get("applied")
        .and_then(|value| value.get("daemon_mode"))
        .and_then(Value::as_str);
    let applied_semantic_enabled = persisted
        .get("applied")
        .and_then(|value| value.get("semantic_enabled"))
        .and_then(Value::as_bool);
    let applied_semantic_executor = persisted
        .get("applied")
        .and_then(|value| value.get("semantic_executor"))
        .and_then(Value::as_str);
    let applied_semantic_contract_fingerprint = persisted
        .get("applied")
        .and_then(|value| value.get("semantic_contract_fingerprint"))
        .and_then(Value::as_str);
    let applied_semantic_builtin_throttling_configured = persisted
        .get("applied")
        .and_then(|value| value.get("semantic_builtin_throttling_configured"))
        .and_then(Value::as_bool);
    let applied_semantic_builtin_throttling_effective = persisted
        .get("applied")
        .and_then(|value| value.get("semantic_builtin_throttling_effective"))
        .and_then(Value::as_bool);
    let applied_semantic_builtin_throttling_effective_present = persisted
        .get("applied")
        .and_then(|value| value.get("semantic_builtin_throttling_effective"))
        .is_some();
    let requested_daemon_enabled = current_config.map(|config| config.enabled);
    let requested_daemon_mode = current_config.map(|config| config.mode.as_str());
    let requested_semantic_enabled = current_config.map(|config| config.semantic_enabled);
    let requested_semantic_executor =
        current_config.map(|config| config.semantic_executor.as_str());
    let requested_semantic_contract_fingerprint =
        current_config.map(|config| config.semantic_contract_fingerprint.as_str());
    let requested_semantic_builtin_throttling_configured =
        current_config.map(|config| config.semantic_builtin_throttling_configured);
    let requested_semantic_builtin_throttling_effective =
        current_config.and_then(|config| config.semantic_builtin_throttling_effective);
    let out_of_sync = running
        && (requested_daemon_enabled != applied_daemon_enabled
            || requested_daemon_mode != applied_daemon_mode
            || requested_semantic_enabled != applied_semantic_enabled
            || requested_semantic_executor != applied_semantic_executor
            || requested_semantic_contract_fingerprint != applied_semantic_contract_fingerprint
            || requested_semantic_builtin_throttling_configured
                != applied_semantic_builtin_throttling_configured
            || requested_semantic_builtin_throttling_effective
                != applied_semantic_builtin_throttling_effective
            || (current_config.is_some()
                && !applied_semantic_builtin_throttling_effective_present));
    let persisted_status = persisted
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let status = if out_of_sync && persisted_status == "applied" {
        "pending"
    } else {
        persisted_status
    };
    let reason = if out_of_sync && persisted_status == "applied" {
        Some("config_changed")
    } else {
        None
    };

    let mut report = compact_json(json!({
        "status": status,
        "reason": reason,
        "out_of_sync": out_of_sync,
        "last_attempt_at_ms": persisted.get("last_attempt_at_ms").cloned(),
        "last_applied_at_ms": persisted.get("last_applied_at_ms").cloned(),
        "requested": {
            "daemon_enabled": requested_daemon_enabled,
            "daemon_mode": requested_daemon_mode,
            "semantic_enabled": requested_semantic_enabled,
            "semantic_executor": requested_semantic_executor,
            "semantic_contract_fingerprint": requested_semantic_contract_fingerprint,
            "semantic_builtin_throttling_configured": requested_semantic_builtin_throttling_configured,
            "semantic_builtin_throttling_effective": requested_semantic_builtin_throttling_effective,
        },
        "applied": {
            "daemon_enabled": applied_daemon_enabled,
            "daemon_mode": applied_daemon_mode,
            "semantic_enabled": applied_semantic_enabled,
            "semantic_executor": applied_semantic_executor,
            "semantic_contract_fingerprint": applied_semantic_contract_fingerprint,
            "semantic_builtin_throttling_configured": applied_semantic_builtin_throttling_configured,
            "semantic_builtin_throttling_effective": applied_semantic_builtin_throttling_effective,
        },
        "last_error": persisted.get("last_error").cloned(),
    }));
    if current_config.is_some() && requested_semantic_builtin_throttling_effective.is_none() {
        report["requested"]["semantic_builtin_throttling_effective"] = Value::Null;
    }
    if applied_semantic_builtin_throttling_effective_present
        && applied_semantic_builtin_throttling_effective.is_none()
    {
        report["applied"]["semantic_builtin_throttling_effective"] = Value::Null;
    }
    report
}
