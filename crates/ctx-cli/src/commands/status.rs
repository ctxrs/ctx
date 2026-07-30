use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};

use crate::analytics::{count_bucket, StatusTelemetry};
use crate::config::{self, CONFIG_FILE};
use crate::local_usage;
use crate::output::print_json;
use crate::pro::PRO_MONTHLY_PRICE_DISPLAY;
use crate::semantic::source_epoch_status_report;
use crate::ui::{
    fields, outcome, section, Document, Field, Line, Outcome, OutcomeState, RenderContext, Span,
    Token, Ui,
};
use crate::{StatusArgs, UsageStatusMode};

pub(super) fn upgrade_report(config: &config::AppConfig) -> serde_json::Value {
    crate::upgrade::upgrade_diagnostics(config).report
}

pub(crate) fn run_status(
    args: StatusArgs,
    data_root: PathBuf,
    quiet: bool,
    telemetry: &mut StatusTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    if let Some(mode) = args.usage {
        return run_usage_action(mode, &data_root, args.format.is_json(), quiet);
    }
    let config_path = data_root.join(CONFIG_FILE);
    let Some(config) = load_status_config(&data_root) else {
        return malformed_config_failure(args.format.is_json());
    };
    let source = source_epoch_status_report(&data_root, &config)?;
    telemetry.initialized = Some(source.initialized);
    telemetry.indexed_items = source.indexed_items.map(count_bucket);
    telemetry.indexed_sessions = source.indexed_sessions.map(count_bucket);
    telemetry.indexed_events = source.indexed_events.map(count_bucket);
    telemetry.indexed_sources = source.indexed_sources.map(count_bucket);
    let mut pro = crate::pro::lifecycle_status_json(&data_root);
    if let Some(object) = pro.as_object_mut() {
        object.insert(
            "conversion_action".to_owned(),
            local_usage::pro_conversion_action(object.get("access_state").and_then(Value::as_str))
                .unwrap_or(Value::Null),
        );
    }
    let upgrade = upgrade_report(&config);
    let local_usage = local_usage::read_report(&data_root, config.local_usage.enabled, false);
    if args.format.is_json() {
        let mut report = source.report;
        if let Some(object) = report.as_object_mut() {
            object.insert("upgrade".to_owned(), upgrade);
            object.insert("pro".to_owned(), pro);
            object.insert(
                "local_usage".to_owned(),
                compact_usage_health_json(&local_usage),
            );
            object.insert("read_only".to_owned(), json!(true));
        }
        print_json(report)?;
    } else if !quiet {
        let document = render_status_human(
            ui.stdout_context(),
            &source.report,
            &data_root,
            &config_path,
            &upgrade,
            &pro,
            &local_usage,
        );
        ui.write_stdout(&document)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusHealth {
    Healthy,
    Partial,
    Failed,
}

fn render_status_human(
    context: &RenderContext,
    report: &Value,
    data_root: &Path,
    config_path: &Path,
    upgrade: &Value,
    pro: &Value,
    local_usage: &local_usage::UsageReport,
) -> Document {
    let health = status_health(report);
    let unhealthy = unhealthy_history_components(report);
    let pending = unhealthy
        .iter()
        .filter(|(_, component)| component_status(component) == "pending")
        .count();
    let lexical_status = component_status(&report["lexical"]);
    let (state, title, detail) = match health {
        StatusHealth::Healthy => (OutcomeState::Success, "ctx is healthy", None),
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
    for (label, field, singular, plural) in [
        (
            "Sources",
            "indexed_sources",
            "indexed source",
            "indexed sources",
        ),
        (
            "Sessions",
            "indexed_sessions",
            "indexed session",
            "indexed sessions",
        ),
        (
            "Events",
            "indexed_events",
            "searchable event",
            "searchable events",
        ),
    ] {
        if let Some(count) = report[field].as_u64() {
            history_values.push((label, counted(count, singular, plural)));
        }
    }
    if health == StatusHealth::Healthy {
        history_values.push(("Refresh", component_display(&report["refresh"]).to_owned()));
    } else {
        for (label, component) in supporting_history_components(report) {
            if !component_is_healthy(component) {
                history_values.push((label, component_display(component)));
            }
        }
    }
    let history_fields = history_values
        .iter()
        .map(|(label, value)| Field::new(*label, value.as_str()))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("History", fields(context, &history_fields)));

    let daemon = &report["daemon"];
    let daemon_status = if daemon.get("running").and_then(Value::as_bool) == Some(true) {
        "running".to_owned()
    } else {
        component_display(daemon)
    };
    let mut service_values = vec![
        ("Daemon", daemon_status),
        (
            "Semantic",
            component_display(&report["semantic"]).to_owned(),
        ),
    ];
    if let Some(issue) = upgrade_service_issue(upgrade) {
        service_values.push(("Automatic upgrades", issue));
    }
    if let Some(error) = &local_usage.error {
        service_values.push((
            "Local usage",
            format!("{} ({}: {})", local_usage.state, error.code, error.message),
        ));
    }
    let service_fields = service_values
        .iter()
        .map(|(label, value)| Field::new(*label, value.as_str()))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("Services", fields(context, &service_fields)));

    if pro["installed"].as_bool() == Some(true) {
        let mut pro_values = vec![(
            "Status",
            pro["state"].as_str().unwrap_or("unavailable").to_owned(),
        )];
        if let Some(access) = pro["access_state"].as_str() {
            pro_values.push(("Access", humanize_code(access)));
        }
        if let Some(action) = pro["conversion_action"].as_object() {
            if action.get("kind").and_then(Value::as_str) == Some("pro_restore_access") {
                pro_values.push(("Data", "graph preserved".to_owned()));
            } else {
                pro_values.push((
                    "Upgrade",
                    action
                        .get("price")
                        .and_then(Value::as_str)
                        .unwrap_or(PRO_MONTHLY_PRICE_DISPLAY)
                        .to_owned(),
                ));
            }
            pro_values.push((
                "Next",
                action
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("ctx pro manage")
                    .to_owned(),
            ));
        } else if pro["state"].as_str() != Some("ready") {
            if let Some(command) = pro["next_action"]["command"].as_str() {
                pro_values.push(("Next", command.to_owned()));
            }
        }
        let pro_fields = pro_values
            .iter()
            .map(|(label, value)| Field::new(*label, value.as_str()))
            .collect::<Vec<_>>();
        document.push_blank();
        document.append(section("Pro", fields(context, &pro_fields)));
    }

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
        document.push_blank();
        document.append(section(
            "Next",
            Document::from_line(
                Line::new()
                    .with(Span::text("  "))
                    .with(Span::new(command, Token::Command)),
            ),
        ));
    }
    document
}

fn status_health(report: &Value) -> StatusHealth {
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

fn status_next_command(
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

fn history_components(report: &Value) -> [(&'static str, &Value); 6] {
    [
        ("Epoch", &report["history_epoch"]),
        ("Search", &report["lexical"]),
        ("Catalog", &report["catalog"]),
        ("Refresh service", &report["resolver"]),
        ("Refresh", &report["refresh"]),
        ("Session view", &report["relational"]),
    ]
}

fn supporting_history_components(report: &Value) -> [(&'static str, &Value); 4] {
    [
        ("Catalog", &report["catalog"]),
        ("Refresh service", &report["resolver"]),
        ("Refresh", &report["refresh"]),
        ("Session view", &report["relational"]),
    ]
}

fn unhealthy_history_components(report: &Value) -> Vec<(&'static str, &Value)> {
    history_components(report)
        .into_iter()
        .filter(|(_, component)| !component_is_healthy(component))
        .collect()
}

fn component_is_healthy(component: &Value) -> bool {
    matches!(component_status(component), "ready" | "disabled")
}

fn component_status(component: &Value) -> &str {
    component
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unavailable")
}

fn component_display(component: &Value) -> String {
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

fn upgrade_service_issue(upgrade: &Value) -> Option<String> {
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

fn service_progress_message(count: usize) -> String {
    if count == 1 {
        "1 history service is catching up".to_owned()
    } else {
        format!("{count} history services are catching up")
    }
}

fn service_attention_message(count: usize) -> String {
    if count == 1 {
        "1 history service needs attention".to_owned()
    } else {
        format!("{count} history services need attention")
    }
}

fn humanize_code(value: &str) -> String {
    value.replace('_', " ")
}

fn counted(count: u64, singular: &str, plural: &str) -> String {
    let noun = if count == 1 { singular } else { plural };
    format!("{} {noun}", grouped_count(count))
}

fn grouped_count(count: u64) -> String {
    let digits = count.to_string();
    let mut reversed = String::with_capacity(digits.len().saturating_add(digits.len() / 3));
    for (index, character) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            reversed.push(',');
        }
        reversed.push(character);
    }
    reversed.chars().rev().collect()
}

fn load_status_config(data_root: &Path) -> Option<config::AppConfig> {
    // Dispatch already loaded this file, but a concurrent replacement can make
    // the status-specific reread fail. Discard that raw cause here so neither
    // its path nor its content can reach the generic CLI error renderer.
    config::AppConfig::load(data_root).ok()
}

pub(crate) fn run_usage_action(
    mode: UsageStatusMode,
    data_root: &std::path::Path,
    json_output: bool,
    quiet: bool,
) -> Result<()> {
    match mode {
        UsageStatusMode::Enable => {
            if config::set_local_usage_enabled(data_root, true).is_err() {
                return usage_action_failure(
                    mode,
                    json_output,
                    "usage_control_failed",
                    "local usage enablement could not be changed",
                );
            }
            let Ok(control) = config::read_local_usage_control(data_root) else {
                return usage_action_failure(
                    mode,
                    json_output,
                    "usage_control_failed",
                    "local usage enablement could not be confirmed",
                );
            };
            emit_usage_action(
                mode,
                json_output,
                quiet,
                json!({
                    "persisted_enabled": control.persisted_enabled,
                    "effective_enabled": control.effective_enabled,
                    "environment_override": control.environment_override.as_str(),
                }),
            )
        }
        UsageStatusMode::Disable => {
            if config::set_local_usage_enabled(data_root, false).is_err() {
                return usage_action_failure(
                    mode,
                    json_output,
                    "usage_control_failed",
                    "local usage enablement could not be changed",
                );
            }
            let Ok(control) = config::read_local_usage_control(data_root) else {
                return usage_action_failure(
                    mode,
                    json_output,
                    "usage_control_failed",
                    "local usage enablement could not be confirmed",
                );
            };
            emit_usage_action(
                mode,
                json_output,
                quiet,
                json!({
                    "persisted_enabled": control.persisted_enabled,
                    "effective_enabled": control.effective_enabled,
                    "environment_override": control.environment_override.as_str(),
                }),
            )
        }
        UsageStatusMode::Reset => {
            let store_state = match local_usage::reset(data_root) {
                Ok(true) => "cleared",
                Ok(false) => "missing",
                Err(_) => {
                    return usage_action_failure(
                        mode,
                        json_output,
                        "usage_reset_failed",
                        "local usage could not be reset",
                    );
                }
            };
            emit_usage_action(
                mode,
                json_output,
                quiet,
                json!({"store_state": store_state}),
            )
        }
    }
}

pub(crate) fn malformed_config_failure(json_output: bool) -> Result<()> {
    if json_output {
        eprintln!(
            "{}",
            serde_json::to_string(&malformed_config_json())
                .expect("malformed-config status errors contain only static JSON")
        );
    } else {
        eprintln!("local_usage_config_unavailable: local usage configuration could not be read");
    }
    Err(crate::dispatch::rendered_cli_error())
}

pub(crate) fn removed_cloud_config_failure(json_output: bool) -> Result<()> {
    if json_output {
        eprintln!(
            "{}",
            serde_json::to_string(&json!({
                "schema_version": 1,
                "error": {
                    "code": "removed_config_key",
                    "config_key": "cloud.mode",
                    "message": "cloud history configuration is no longer supported",
                },
                "local_only": true,
                "read_only": true,
            }))
            .expect("removed-cloud status errors contain only static JSON")
        );
    } else {
        eprintln!(
            "removed_config_key: cloud.mode is no longer supported; remove it from config.toml"
        );
    }
    Err(crate::dispatch::rendered_cli_error())
}

fn malformed_config_json() -> Value {
    json!({
        "schema_version": 1,
        "local_usage": compact_usage_health_json(&local_usage::UsageReport::config_error()),
        "local_only": true,
        "read_only": true,
    })
}

fn compact_usage_health_json(report: &local_usage::UsageReport) -> Value {
    json!({
        "schema_version": report.schema_version,
        "enabled": report.enabled,
        "state": report.state,
        "definition_version": report.definition_version,
        "retention_days": report.retention_days,
        "error": report.error,
    })
}

fn emit_usage_action(
    mode: UsageStatusMode,
    json_output: bool,
    quiet: bool,
    fields: Value,
) -> Result<()> {
    let mut action = fields.as_object().cloned().unwrap_or_default();
    action.insert("action".to_owned(), json!(mode.as_str()));
    action.insert("ok".to_owned(), json!(true));
    if json_output {
        print_json(json!({
            "schema_version": 1,
            "local_usage_action": action,
            "local_only": true,
            "read_only": false,
        }))?;
    } else if !quiet {
        println!("local_usage_action: {}", mode.as_str());
        match mode {
            UsageStatusMode::Enable | UsageStatusMode::Disable => {
                println!(
                    "local_usage_persisted_enabled: {}",
                    action["persisted_enabled"].as_bool().unwrap_or(false)
                );
                println!(
                    "local_usage_effective_enabled: {}",
                    action["effective_enabled"].as_bool().unwrap_or(false)
                );
                println!(
                    "local_usage_environment_override: {}",
                    action["environment_override"].as_str().unwrap_or("invalid")
                );
            }
            UsageStatusMode::Reset => println!(
                "local_usage_store: {}",
                action["store_state"].as_str().unwrap_or("missing")
            ),
        }
    }
    Ok(())
}

fn usage_action_failure(
    mode: UsageStatusMode,
    json_output: bool,
    code: &'static str,
    message: &'static str,
) -> Result<()> {
    if json_output {
        eprintln!(
            "{}",
            serde_json::to_string(&json!({
                "schema_version": 1,
                "local_usage_action": {
                    "action": mode.as_str(),
                    "ok": false,
                    "error": {
                        "code": code,
                        "message": message,
                    },
                },
                "local_only": true,
                "read_only": false,
            }))
            .expect("usage action errors contain only static JSON")
        );
    } else {
        eprintln!("{code}: {message}");
    }
    Err(crate::dispatch::rendered_cli_error())
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write as _};

    use unicode_width::UnicodeWidthStr as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize, color: ColorMode) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
    }

    fn assert_fits(document: &Document, context: &RenderContext) {
        let width = context.content_width().unwrap_or(1);
        for line in document.render_plain().lines() {
            assert!(line.width() <= width, "{line:?} exceeded {width} columns");
        }
    }

    fn strip_ansi(rendered: &str) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        stream.write_all(rendered.as_bytes()).unwrap();
        String::from_utf8(stream.into_inner()).unwrap()
    }

    fn status_report(
        initialized: bool,
        lexical: &str,
        catalog: &str,
        resolver: &str,
        refresh: &str,
        relational: &str,
    ) -> Value {
        json!({
            "initialized": initialized,
            "history_epoch": {"status": lexical},
            "lexical": {"status": lexical},
            "catalog": {"status": catalog},
            "resolver": {"status": resolver},
            "refresh": {"status": refresh},
            "relational": {"status": relational},
            "semantic": {"status": "disabled"},
            "daemon": {"status": "running", "running": true},
            "indexed_sources": 1,
            "indexed_sessions": 2,
            "indexed_events": 1000,
        })
    }

    fn usage_report() -> local_usage::UsageReport {
        local_usage::UsageReport {
            schema_version: 2,
            local_only: true,
            read_only: true,
            enabled: true,
            state: "ready",
            retention_days: 400,
            definition_version: 2,
            definitions: None,
            estimates: None,
            error: None,
        }
    }

    fn no_pro() -> Value {
        json!({"installed": false})
    }

    fn healthy_upgrade() -> Value {
        json!({
            "auto": "apply",
            "install": {
                "managed": false,
                "marker": "absent",
            },
        })
    }

    fn render_report(context: &RenderContext, report: &Value) -> Document {
        render_status_human(
            context,
            report,
            std::path::Path::new("/tmp/ctx"),
            std::path::Path::new("/tmp/ctx/config.toml"),
            &healthy_upgrade(),
            &no_pro(),
            &usage_report(),
        )
    }

    #[test]
    fn healthy_status_is_concise_and_hides_routine_internal_paths() {
        let report = status_report(true, "ready", "ready", "ready", "ready", "ready");
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_report(&context, &report);
            let rendered = document.render_plain();
            let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(rendered.starts_with("✓ ctx is healthy\n\nHistory\n"));
            assert!(rendered.contains("1 indexed source"));
            assert!(rendered.contains("2 indexed sessions"));
            assert!(normalized.contains("1,000 searchable events"));
            assert!(rendered.contains("\nServices\n"));
            assert!(rendered.contains("Daemon    running\n"));
            assert!(rendered.contains("Semantic  disabled\n"));
            assert!(!rendered.contains("Automatic upgrades"));
            assert!(!rendered.contains("Local usage"));
            assert!(!rendered.contains("\nData\n"));
            assert!(!rendered.contains("/tmp/ctx"));
            assert!(!rendered.contains("local-only"));
            assert!(!rendered.contains("read-only"));
            assert!(!rendered.contains("Generation"));
            assert!(!rendered.contains("PID"));
            assert!(!rendered.contains("\nNext\n"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn status_surfaces_only_actionable_auxiliary_service_errors() {
        let report = status_report(true, "ready", "ready", "ready", "ready", "ready");
        let upgrade = json!({
            "auto": "apply",
            "install": {
                "managed": true,
                "marker": "valid",
                "path": {
                    "background_apply": {
                        "allowed": false,
                        "reason": "path_shadowed",
                    },
                },
            },
        });
        let usage = local_usage::UsageReport::config_error();
        let context = context(80, ColorMode::Never);
        let document = render_status_human(
            &context,
            &report,
            std::path::Path::new("/tmp/ctx"),
            std::path::Path::new("/tmp/ctx/config.toml"),
            &upgrade,
            &no_pro(),
            &usage,
        );
        let rendered = document.render_plain();
        let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(rendered.contains("Automatic upgrades  blocked (path shadowed)\n"));
        assert!(rendered.contains("Local usage"));
        assert!(rendered.contains("local_usage_config_unavailable"));
        assert!(normalized.contains("local usage configuration could not be read"));
        assert_fits(&document, &context);
    }

    #[test]
    fn partial_status_keeps_searchable_counts_and_points_to_index_watch() {
        let report = status_report(true, "ready", "pending", "pending", "pending", "ready");
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_report(&context, &report);
            let rendered = document.render_plain();
            let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(rendered.starts_with("! ctx is partially ready\n"));
            assert!(rendered.contains("Catalog"));
            assert!(rendered.contains("pending"));
            assert!(normalized.contains("1,000 searchable events"));
            assert!(rendered.contains("Next\n  ctx index watch\n"));
            assert!(!rendered.contains("\nData\n"));
            assert_fits(&document, &context);
        }

        let context = context(80, ColorMode::Never);
        let rendered = render_report(&context, &report).render_plain();
        assert!(rendered.contains("3 history services are catching up; search remains available."));
    }

    #[test]
    fn failed_status_exposes_actionable_paths_and_doctor_recovery() {
        let mut report = status_report(
            false,
            "unavailable",
            "unavailable",
            "unavailable",
            "unavailable",
            "unavailable",
        );
        report["lexical"]["reason"] = json!("generation_verification_failed");
        report["daemon"] = json!({
            "status": "failed",
            "reason": "daemon_process_failed",
            "running": false,
        });

        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_report(&context, &report);
            let rendered = document.render_plain();
            let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(rendered.starts_with("✗ History status: failed\n"));
            assert!(normalized.contains("generation verification failed"));
            assert!(rendered.contains("\nData\n"));
            assert!(rendered.contains("/tmp/ctx"));
            assert!(rendered.contains("Next\n  ctx doctor\n"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn status_without_a_published_generation_points_to_setup() {
        let mut report = status_report(
            false,
            "unavailable",
            "pending",
            "unavailable",
            "unavailable",
            "unavailable",
        );
        report["lexical"]["reason"] = json!("generation_not_published");
        let context = context(80, ColorMode::Never);
        let rendered = render_report(&context, &report).render_plain();
        assert!(rendered.contains("Next\n  ctx setup\n"));
    }

    #[test]
    fn installed_pro_status_is_grouped_without_internal_deadlines() {
        let report = status_report(true, "ready", "ready", "ready", "ready", "ready");
        let pro = json!({
            "installed": true,
            "state": "ready",
            "access_state": "trial",
            "refresh_after_unix": 100,
            "access_deadline_unix": 200,
            "conversion_action": {
                "kind": "pro_monthly_conversion",
                "price": "$20/month",
                "command": "ctx pro manage",
            },
        });

        for width in [32, 80] {
            let context = context(width, ColorMode::Never);
            let document = render_status_human(
                &context,
                &report,
                std::path::Path::new("/tmp/ctx"),
                std::path::Path::new("/tmp/ctx/config.toml"),
                &healthy_upgrade(),
                &pro,
                &usage_report(),
            );
            let rendered = document.render_plain();
            assert!(rendered.contains("\nPro\n"));
            assert!(rendered.contains("$20/month"));
            assert!(rendered.contains("ctx pro manage"));
            assert!(!rendered.contains("100"));
            assert!(!rendered.contains("200"));
            assert!(!rendered.contains("deadline"));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn status_plain_output_equals_ansi_stripped_styled_output() {
        let report = status_report(true, "ready", "ready", "ready", "ready", "ready");
        let context = context(80, ColorMode::Always);
        let document = render_report(&context, &report);
        assert_eq!(
            strip_ansi(&document.render(&context)),
            document.render_plain()
        );
    }

    #[test]
    fn status_config_replacement_discards_raw_second_load_failure() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(CONFIG_FILE);
        fs::write(&path, "[local_usage]\nenabled = true\n").unwrap();
        config::AppConfig::load(temp.path()).unwrap();

        let marker = "SECRET_REPLACEMENT_CONFIG_15d2";
        fs::write(
            &path,
            format!("malformed status replacement /private/{marker}/credential\n"),
        )
        .unwrap();

        assert!(load_status_config(temp.path()).is_none());
        let rendered = serde_json::to_string(&malformed_config_json()).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&rendered).unwrap()["local_usage"]["error"]["code"],
            "local_usage_config_unavailable"
        );
        assert!(!rendered.contains(marker));
        assert!(!rendered.contains("credential"));
        assert!(!rendered.contains(temp.path().to_string_lossy().as_ref()));
    }
}
