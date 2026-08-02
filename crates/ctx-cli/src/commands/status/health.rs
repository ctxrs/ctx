use serde_json::Value;

use crate::local_usage;

use super::humanize_code;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatusHealth {
    Healthy,
    Partial,
    Failed,
}

pub(super) fn history_health(report: &Value) -> StatusHealth {
    let lexical_status = component_status(&report["lexical"]);
    if lexical_status == "ready"
        && history_components(report)
            .iter()
            .all(|(_, component)| component_is_healthy(component))
    {
        StatusHealth::Healthy
    } else if matches!(lexical_status, "ready" | "pending") {
        StatusHealth::Partial
    } else {
        StatusHealth::Failed
    }
}

pub(super) fn actionable_service_issue_count(
    report: &Value,
    upgrade: &Value,
    local_usage: &local_usage::UsageReport,
) -> usize {
    usize::from(daemon_needs_attention(&report["daemon"]))
        + usize::from(semantic_needs_attention(&report["semantic"]))
        + usize::from(upgrade_service_issue(upgrade).is_some())
        + usize::from(local_usage.error.is_some())
}

fn daemon_needs_attention(daemon: &Value) -> bool {
    let status = component_status(daemon);
    daemon.get("recoverable").and_then(Value::as_bool) == Some(true)
        || matches!(status, "failed" | "stale_lock" | "unavailable")
        || (daemon.get("enabled").and_then(Value::as_bool) == Some(true)
            && daemon.get("running").and_then(Value::as_bool) != Some(true)
            && status != "completed")
}

fn semantic_needs_attention(semantic: &Value) -> bool {
    semantic.get("enabled").and_then(Value::as_bool) == Some(true)
        && !matches!(component_status(semantic), "ready" | "disabled")
}

pub(super) fn local_usage_display(report: &local_usage::UsageReport) -> String {
    if let Some(error) = &report.error {
        return format!("unavailable ({}: {})", error.code, error.message);
    }
    if !report.enabled {
        return "disabled".to_owned();
    }
    match report.state {
        "ready" => "enabled; store readable".to_owned(),
        "empty" => "enabled; store missing or empty".to_owned(),
        state => format!("enabled; {}", humanize_code(state)),
    }
}

pub(super) fn status_next_command(
    report: &Value,
    health: StatusHealth,
    pending: usize,
    unhealthy: usize,
) -> Option<&'static str> {
    match health {
        StatusHealth::Healthy => None,
        StatusHealth::Partial if unhealthy > 0 && pending == unhealthy => Some("ctx index watch"),
        StatusHealth::Partial => Some("ctx doctor"),
        StatusHealth::Failed
            if report.get("initialized").and_then(Value::as_bool) != Some(true)
                && report["lexical"]["reason"].as_str() == Some("generation_not_published") =>
        {
            Some("ctx setup")
        }
        StatusHealth::Failed => Some("ctx doctor"),
    }
}

fn history_components(report: &Value) -> [(&'static str, &Value); 3] {
    [
        ("Epoch", &report["history_epoch"]),
        ("Search", &report["lexical"]),
        ("Refresh", &report["refresh"]),
    ]
}

pub(super) fn supporting_history_components(report: &Value) -> [(&'static str, &Value); 1] {
    [("Refresh", &report["refresh"])]
}

pub(super) fn unhealthy_history_components(report: &Value) -> Vec<(&'static str, &Value)> {
    history_components(report)
        .into_iter()
        .filter(|(_, component)| !component_is_healthy(component))
        .collect()
}

pub(super) fn component_is_healthy(component: &Value) -> bool {
    matches!(component_status(component), "ready" | "disabled")
}

pub(super) fn component_status(component: &Value) -> &str {
    component
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unavailable")
}

pub(super) fn component_display(component: &Value) -> String {
    let status = component_status(component);
    if matches!(status, "ready" | "disabled" | "running") {
        return humanize_code(status);
    }
    component
        .get("reason")
        .and_then(Value::as_str)
        .map(humanize_code)
        .map_or_else(
            || humanize_code(status),
            |reason| format!("{} ({reason})", humanize_code(status)),
        )
}

pub(super) fn upgrade_service_issue(upgrade: &Value) -> Option<String> {
    let install = upgrade.get("install")?;
    if let Some(error) = install.get("error").and_then(Value::as_str) {
        return Some(format!("needs attention ({error})"));
    }
    let background_apply = install.get("path")?.get("background_apply")?;
    if background_apply.get("allowed").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    background_apply
        .get("reason")
        .and_then(Value::as_str)
        .map(humanize_code)
        .map(|reason| format!("blocked ({reason})"))
}

pub(super) fn service_progress_message(count: usize) -> String {
    if count == 1 {
        "1 history service is catching up".to_owned()
    } else {
        format!("{count} history services are catching up")
    }
}

pub(super) fn service_attention_message(count: usize) -> String {
    if count == 1 {
        "1 history service needs attention".to_owned()
    } else {
        format!("{count} history services need attention")
    }
}

pub(super) fn auxiliary_attention_message(count: usize) -> String {
    if count == 1 {
        "1 service needs attention".to_owned()
    } else {
        format!("{count} services need attention")
    }
}
