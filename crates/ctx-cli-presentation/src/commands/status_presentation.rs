use std::path::Path;

use ctx_client_observability::local_usage;
use ctx_history_read_application::HistoryHealthReport;
use serde_json::Value;

#[cfg(test)]
use serde_json::json;

use crate::ui::{
    fields, hint, outcome, section, Action, Document, Field, Hint, Line, Outcome, OutcomeState,
    RenderContext, Span, Token,
};

use super::history_health::{counted, history_health_fields, history_partial_cause};
use super::status_health::*;

pub fn render_status_human(
    context: &RenderContext,
    report: &Value,
    coverage: Option<&HistoryHealthReport>,
    data_root: &Path,
    config_path: &Path,
    upgrade: &Value,
    local_usage: &local_usage::UsageReport,
) -> Document {
    let component_health = history_health(report);
    let coverage_cause = history_partial_cause(coverage);
    let service_issues = actionable_service_issue_count(report, upgrade, local_usage);
    let health = if component_health == StatusHealth::Healthy
        && (service_issues > 0 || coverage_cause.is_some())
    {
        StatusHealth::Partial
    } else {
        component_health
    };
    let unhealthy = unhealthy_history_components(report);
    let pending = unhealthy
        .iter()
        .filter(|(_, component)| component_status(component) == "pending")
        .count();
    let lexical_status = component_status(&report["lexical"]);
    let automatic_retry_state = automatic_retry_state(report);
    let automatic_retry_pause = automatic_retry_pause(report);
    let last_verified_searchable = matches!(
        lexical_status,
        "ready" | "stale" | "rebuilding" | "awaiting_upgrade"
    );
    let (state, title, detail) = match health {
        StatusHealth::Healthy => (OutcomeState::Success, "ctx is healthy", None),
        StatusHealth::Partial if lexical_status == "rebuilding" => (
            OutcomeState::Warning,
            "History status: rebuilding",
            Some("Search remains available while the index is upgraded.".to_owned()),
        ),
        StatusHealth::Partial if lexical_status == "awaiting_upgrade" => (
            OutcomeState::Warning,
            "History status: awaiting upgrade",
            Some("Search remains available. Run ctx import --all to upgrade the index.".to_owned()),
        ),
        _ if matches!(
            report["refresh"]["reason"].as_str(),
            Some("refresh_requires_explicit_request" | "automatic_retry_daemon_unavailable")
        ) =>
        {
            (
                if last_verified_searchable {
                    OutcomeState::Warning
                } else {
                    OutcomeState::Error
                },
                "ctx needs attention",
                Some(format!(
                    "{} {}",
                    if report["refresh"]["reason"] == "refresh_requires_explicit_request" {
                        "History refresh needs an explicit request."
                    } else {
                        "Automatic history refresh is waiting for a running daemon."
                    },
                    if last_verified_searchable {
                        "The last verified history remains searchable."
                    } else {
                        "No verified history is searchable yet."
                    },
                )),
            )
        }
        _ if automatic_retry_pause == Some("paused") => (
            if last_verified_searchable {
                OutcomeState::Warning
            } else {
                OutcomeState::Error
            },
            "ctx needs attention",
            Some(if last_verified_searchable {
                "Automatic history refresh is paused. The last verified history remains searchable."
                    .to_owned()
            } else {
                "Automatic history refresh is paused. No verified history is searchable yet."
                    .to_owned()
            }),
        ),
        _ if automatic_retry_pause == Some("mixed") => (
            OutcomeState::Warning,
            "ctx needs attention",
            Some(if last_verified_searchable {
                "Automatic history refresh is paused for some sources. The last verified history remains searchable."
                    .to_owned()
            } else {
                "Automatic history refresh is paused for some sources. No verified history is searchable yet."
                    .to_owned()
            }),
        ),
        StatusHealth::Partial if automatic_retry_state == Some("confirming") => (
            OutcomeState::Warning,
            "ctx is partially ready",
            Some(if last_verified_searchable {
                "Automatic history refresh will retry to confirm an internal failure. The last verified history remains searchable."
                    .to_owned()
            } else {
                "Automatic history refresh will retry to confirm an internal failure. No verified history is searchable yet."
                    .to_owned()
            }),
        ),
        StatusHealth::Partial if coverage_cause.is_some() => (
            OutcomeState::Warning,
            "ctx needs attention",
            Some(format!(
                "{}. Healthy prior history remains searchable.",
                coverage_cause.as_deref().unwrap_or_default()
            )),
        ),
        StatusHealth::Partial if component_health == StatusHealth::Healthy => (
            OutcomeState::Warning,
            "ctx needs attention",
            Some(format!(
                "{}; search remains available.",
                auxiliary_attention_message(service_issues)
            )),
        ),
        StatusHealth::Partial if lexical_status == "pending" => (
            OutcomeState::Warning,
            "ctx is partially ready",
            Some("History indexing is in progress.".to_owned()),
        ),
        StatusHealth::Partial if pending == unhealthy.len() => (
            OutcomeState::Warning,
            "ctx is partially ready",
            Some(format!(
                "{}; search remains available.",
                service_progress_message(unhealthy.len())
            )),
        ),
        StatusHealth::Partial => (
            OutcomeState::Warning,
            "ctx is partially ready",
            Some(format!(
                "{}; search remains available.",
                service_attention_message(unhealthy.len())
            )),
        ),
        StatusHealth::Failed => (
            OutcomeState::Error,
            "History status: failed",
            Some("A verified search index is not available.".to_owned()),
        ),
    };
    let mut document = outcome(
        context,
        Outcome {
            state,
            title,
            detail: detail.as_deref(),
        },
    );

    let mut history_values = vec![("Search", component_display(&report["lexical"]).to_owned())];
    if matches!(lexical_status, "rebuilding" | "awaiting_upgrade") {
        history_values.push(("Search availability", "available".to_owned()));
        if let Some(generation) = report["lexical"]["generation_id"].as_str() {
            history_values.push(("Serving generation", generation.to_owned()));
        }
        append_lexical_upgrade_fields(lexical_status, &report["lexical"], &mut history_values);
    } else if report["lexical"].get("upgrade").is_some() {
        append_lexical_upgrade_fields(lexical_status, &report["lexical"], &mut history_values);
    }
    history_values.extend(history_health_fields(coverage));
    if report["daemon"]["running"].as_bool() == Some(true) {
        let job = &report["daemon"]["jobs"]["core_refresh"];
        let activity = super::status_diagnostics::RefreshActivity::from_job(job);
        if activity.active() {
            // Restart recovery can leave the previous daemon's active job on
            // disk while the new daemon is already reported as running.
            let current = report["daemon"]["started_at_ms"]
                .as_u64()
                .filter(|start| *start > 0)
                .zip(job["last_run_at_ms"].as_u64())
                .is_some_and(|(start, run)| run >= start);
            history_values.push((
                if current {
                    "History work"
                } else {
                    "Last recorded work"
                },
                activity.work_description(),
            ));
            if current {
                if let Some(providers) = activity.provider_names().filter(|value| !value.is_empty())
                {
                    history_values.push(("Providers", providers));
                }
            }
        }
    }
    if component_health == StatusHealth::Healthy {
        history_values.push(("Refresh", component_display(&report["refresh"]).to_owned()));
    } else {
        for (label, component) in supporting_history_components(report) {
            if !component_is_healthy(component) {
                history_values.push((label, component_display(component)));
            }
        }
    }
    if component_status(&report["refresh"]) == "pending" {
        let progress = report["refresh"].get("progress");
        if let Some(records) = progress
            .and_then(|progress| progress.get("completed_records"))
            .and_then(Value::as_u64)
        {
            history_values.push(("Processed", counted(records, "record", "records")));
        }
        if let Some(bytes) = progress
            .and_then(|progress| {
                progress
                    .get("processed_bytes")
                    .or_else(|| progress.get("completed_bytes"))
            })
            .and_then(Value::as_u64)
        {
            history_values.push((
                "Progress data",
                format!("{} processed", ctx_history_cli::format_bytes(bytes)),
            ));
        }
    }
    if let Some(detail) = refresh_resource_detail(report) {
        history_values.push((
            "Issue",
            super::doctor_presentation::bounded_terminal_detail(detail),
        ));
    } else if automatic_retry_pause.is_some() {
        history_values.push((
            "Issue",
            "The same internal refresh failure happened twice.".to_owned(),
        ));
    } else if automatic_retry_state == Some("confirming") {
        history_values.push((
            "Issue",
            "An automatic retry will check whether the internal failure repeats.".to_owned(),
        ));
    }
    let history_fields = history_values
        .iter()
        .map(|(label, value)| Field::new(label, value.as_str()))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("History", fields(context, &history_fields)));
    super::status_storage::append_storage(context, &mut document, report);

    super::append_previous_refresh_failure(context, &mut document, report);
    super::append_attribution(context, &mut document, report);
    let daemon = &report["daemon"];
    let daemon_status = if daemon.get("running").and_then(Value::as_bool) == Some(true) {
        "running".to_owned()
    } else {
        component_display(daemon)
    };
    let indexing_mode = report
        .pointer("/indexing/mode")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let mut service_values = vec![
        ("Indexing mode", indexing_mode),
        ("Daemon", daemon_status),
        (
            "Semantic",
            component_display(&report["semantic"]).to_owned(),
        ),
    ];
    if let Some(issue) = upgrade_service_issue(upgrade) {
        service_values.push(("Automatic upgrades", issue));
    }
    service_values.push(("Local usage", local_usage_display(local_usage)));
    let service_fields = service_values
        .iter()
        .map(|(label, value)| Field::new(label, value.as_str()))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("Services", fields(context, &service_fields)));

    if health == StatusHealth::Failed {
        let data_root = data_root.display().to_string();
        let config_path = config_path.display().to_string();
        document.push_blank();
        document.append(section(
            "Data",
            fields(
                context,
                &[
                    Field::new("Root", &data_root),
                    Field::new("Config", &config_path),
                ],
            ),
        ));
    }

    if let Some(command) = status_next_command(report, health, pending, unhealthy.len()) {
        if automatic_retry_pause.is_some() {
            document.push_blank();
            document.append(hint(
                context,
                Hint {
                    text: "Retry after fixing the source or updating ctx.",
                },
                Some(Action { command }),
            ));
            return document;
        }
        let mut actions = Document::from_line(
            Line::new()
                .with(Span::text("  "))
                .with(Span::new(command, Token::Command)),
        );
        if coverage_cause.is_some() {
            actions.push_line(
                Line::new()
                    .with(Span::text("  "))
                    .with(Span::new("ctx status --format json", Token::Command)),
            );
        }
        document.push_blank();
        document.append(section("Next", actions));
    }
    document
}

fn append_lexical_upgrade_fields(
    lexical_status: &str,
    lexical: &Value,
    history_values: &mut Vec<(&'static str, String)>,
) {
    let Some(upgrade) = lexical.get("upgrade").and_then(Value::as_object) else {
        return;
    };
    let est_sources = upgrade
        .get("estimated_total_sources")
        .and_then(Value::as_u64);
    let est_bytes = upgrade
        .get("estimated_total_source_bytes")
        .and_then(Value::as_u64);
    let processed_bytes = upgrade.get("processed_bytes").and_then(Value::as_u64);
    let stage = upgrade
        .get("whole_run_stage")
        .or_else(|| upgrade.get("phase"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let eta_ms = upgrade
        .get("estimated_remaining_millis")
        .and_then(Value::as_u64);

    let rebuilding_active =
        lexical_status == "rebuilding" || upgrade.contains_key("estimated_remaining_millis");

    if rebuilding_active {
        if let Some(done) = processed_bytes {
            let mut summary = match (est_bytes, est_sources) {
                (Some(total), _) if total > 0 && done <= total => {
                    let pct = ((u128::from(done) * 100) / u128::from(total)).min(100);
                    format!(
                        "{} of ~{} ({pct}%)",
                        ctx_history_cli::format_bytes(done),
                        ctx_history_cli::format_bytes(total)
                    )
                }
                (Some(total), _) if total > 0 => {
                    format!(
                        "{} (est. ~{})",
                        ctx_history_cli::format_bytes(done),
                        ctx_history_cli::format_bytes(total)
                    )
                }
                (_, Some(sources)) if sources > 0 => {
                    format!(
                        "{} processed (~{})",
                        ctx_history_cli::format_bytes(done),
                        counted(sources, "source", "sources")
                    )
                }
                _ => format!("{} processed", ctx_history_cli::format_bytes(done)),
            };
            if let Some(stage) = stage {
                summary.push_str(&format!(" ({})", stage.replace('_', " ")));
            }
            history_values.push(("Upgrade progress", summary));
            if let Some(eta_ms) = eta_ms {
                history_values.push(("Estimated remaining", format_remaining_duration(eta_ms)));
            }
            return;
        }
    }

    let scope = match (est_bytes, est_sources) {
        (Some(bytes), Some(sources)) if bytes > 0 && sources > 0 => Some(format!(
            "~{} across {}",
            ctx_history_cli::format_bytes(bytes),
            counted(sources, "source", "sources")
        )),
        (Some(bytes), _) if bytes > 0 => Some(format!("~{}", ctx_history_cli::format_bytes(bytes))),
        (_, Some(sources)) if sources > 0 => {
            Some(format!("~{}", counted(sources, "source", "sources")))
        }
        _ => None,
    };
    if let Some(mut scope) = scope {
        if rebuilding_active {
            if let Some(stage) = stage {
                scope.push_str(&format!(" ({})", stage.replace('_', " ")));
            }
        }
        history_values.push(("Upgrade scope", scope));
    }
    if rebuilding_active {
        if let Some(eta_ms) = eta_ms {
            history_values.push(("Estimated remaining", format_remaining_duration(eta_ms)));
        }
    }
}

fn format_remaining_duration(millis: u64) -> String {
    let total_seconds = millis / 1_000;
    if total_seconds == 0 {
        return "<1s".to_owned();
    }
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        if minutes > 0 {
            format!("~{hours}h {minutes:02}m")
        } else {
            format!("~{hours}h")
        }
    } else if minutes > 0 {
        if seconds > 0 {
            format!("~{minutes}m {seconds:02}s")
        } else {
            format!("~{minutes}m")
        }
    } else {
        format!("~{seconds}s")
    }
}

fn refresh_resource_detail(report: &Value) -> Option<&str> {
    let outcome = report.pointer("/refresh/structured_outcome")?;
    (outcome.get("code").and_then(Value::as_str) == Some("resource_unavailable"))
        .then(|| outcome.get("detail").and_then(Value::as_str))
        .flatten()
        .filter(|detail| !detail.is_empty())
}

pub(super) fn humanize_code(value: &str) -> String {
    match value {
        "projection_missing" => "still being prepared".to_owned(),
        "generation_not_published" => "history has not been indexed yet".to_owned(),
        "lexical_generation_unavailable" => "search index unavailable".to_owned(),
        _ => value.replace('_', " "),
    }
}

#[cfg(test)]
#[path = "status_presentation_tests.rs"]
mod tests;
