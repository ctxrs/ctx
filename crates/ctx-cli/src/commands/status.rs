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
use crate::StatusArgs;

mod health;
mod usage;

use health::*;
pub(crate) use usage::{malformed_config_failure, removed_cloud_config_failure, run_usage_action};

pub(super) fn upgrade_report(config: &config::AppConfig) -> serde_json::Value {
    crate::upgrade::upgrade_diagnostics(config).report
}

pub(crate) struct StatusReadModel {
    pub(crate) report: Value,
    local_usage: local_usage::UsageReport,
    initialized: bool,
    indexed_items: Option<u64>,
    indexed_sessions: Option<u64>,
    indexed_events: Option<u64>,
    indexed_sources: Option<u64>,
}

pub(crate) fn status_read_model(
    data_root: &Path,
    config: &config::AppConfig,
) -> Result<StatusReadModel> {
    let source = source_epoch_status_report(data_root, config)?;
    let mut pro = crate::pro::lifecycle_status_json(data_root);
    if let Some(object) = pro.as_object_mut() {
        object.insert(
            "conversion_action".to_owned(),
            local_usage::pro_conversion_action(object.get("access_state").and_then(Value::as_str))
                .unwrap_or(Value::Null),
        );
    }
    let upgrade = upgrade_report(config);
    let local_usage = local_usage::read_report(data_root, config.local_usage.enabled, false);
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
    Ok(StatusReadModel {
        report,
        local_usage,
        initialized: source.initialized,
        indexed_items: source.indexed_items,
        indexed_sessions: source.indexed_sessions,
        indexed_events: source.indexed_events,
        indexed_sources: source.indexed_sources,
    })
}

pub(crate) fn run_status(
    args: StatusArgs,
    data_root: PathBuf,
    quiet: bool,
    telemetry: &mut StatusTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    if let Some(mode) = args.usage {
        return run_usage_action(mode, &data_root, args.format.is_json(), quiet, ui);
    }
    let config_path = data_root.join(CONFIG_FILE);
    let Some(config) = load_status_config(&data_root) else {
        return malformed_config_failure(args.format.is_json(), ui);
    };
    let status = status_read_model(&data_root, &config)?;
    telemetry.initialized = Some(status.initialized);
    telemetry.indexed_items = status.indexed_items.map(count_bucket);
    telemetry.indexed_sessions = status.indexed_sessions.map(count_bucket);
    telemetry.indexed_events = status.indexed_events.map(count_bucket);
    telemetry.indexed_sources = status.indexed_sources.map(count_bucket);
    if args.format.is_json() {
        print_json(status.report)?;
    } else if !quiet {
        let document = render_status_human(
            ui.stdout_context(),
            &status.report,
            &data_root,
            &config_path,
            &status.report["upgrade"],
            &status.report["pro"],
            &status.local_usage,
        );
        ui.write_stdout(&document)?;
    }
    Ok(())
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
    let history_health = history_health(report);
    let service_issues = actionable_service_issue_count(report, upgrade, local_usage);
    let health = if history_health == StatusHealth::Healthy && service_issues > 0 {
        StatusHealth::Partial
    } else {
        history_health
    };
    let unhealthy = unhealthy_history_components(report);
    let pending = unhealthy
        .iter()
        .filter(|(_, component)| component_status(component) == "pending")
        .count();
    let lexical_status = component_status(&report["lexical"]);
    let (state, title, detail) = match health {
        StatusHealth::Healthy => (OutcomeState::Success, "ctx is healthy", None),
        StatusHealth::Partial if history_health == StatusHealth::Healthy => (
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
    if history_health == StatusHealth::Healthy {
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
        .map(|(label, value)| Field::new(label, value.as_str()))
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
    service_values.push(("Local usage", local_usage_display(local_usage)));
    let service_fields = service_values
        .iter()
        .map(|(label, value)| Field::new(label, value.as_str()))
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
            .map(|(label, value)| Field::new(label, value.as_str()))
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

fn humanize_code(value: &str) -> String {
    match value {
        "projection_missing" => "still being prepared".to_owned(),
        "generation_not_published" => "history has not been indexed yet".to_owned(),
        "catalog_publication_pending" => "catalog is still being prepared".to_owned(),
        "lexical_generation_unavailable" => "search index unavailable".to_owned(),
        "path_shadowed" => "another ctx binary appears earlier in PATH".to_owned(),
        _ => value.replace('_', " "),
    }
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

pub(super) fn malformed_config_json() -> Value {
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
            assert!(normalized.contains("Daemon running"));
            assert!(normalized.contains("Semantic disabled"));
            assert!(!rendered.contains("Automatic upgrades"));
            assert!(rendered.contains("Local usage"));
            assert!(normalized.contains("enabled; store readable"));
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
        assert!(rendered.starts_with("! ctx needs attention\n"));
        assert!(rendered.contains("Automatic upgrades  blocked (path shadowed)\n"));
        assert!(rendered.contains("Local usage"));
        assert!(rendered.contains("local_usage_config_unavailable"));
        assert!(normalized.contains("local usage configuration could not be read"));
        assert_fits(&document, &context);
    }

    #[test]
    fn actionable_daemon_or_semantic_state_prevents_a_healthy_headline() {
        let mut daemon_failed = status_report(true, "ready", "ready", "ready", "ready", "ready");
        daemon_failed["daemon"] = json!({
            "status": "failed",
            "enabled": true,
            "running": false,
            "reason": "daemon_process_failed",
        });
        let context = context(80, ColorMode::Never);
        let rendered = render_report(&context, &daemon_failed).render_plain();
        assert!(
            rendered.starts_with("! ctx needs attention\n"),
            "{rendered}"
        );
        assert!(rendered.contains("Daemon"), "{rendered}");
        assert!(rendered.contains("failed"), "{rendered}");
        assert!(rendered.contains("Next\n  ctx doctor\n"), "{rendered}");

        let mut semantic_pending = status_report(true, "ready", "ready", "ready", "ready", "ready");
        semantic_pending["semantic"] = json!({
            "status": "pending",
            "enabled": true,
            "reason": "projection_missing",
        });
        let rendered = render_report(&context, &semantic_pending).render_plain();
        assert!(
            rendered.starts_with("! ctx needs attention\n"),
            "{rendered}"
        );
        assert!(
            rendered.contains("pending (still being prepared)"),
            "{rendered}"
        );
        assert!(!rendered.contains("projection missing"), "{rendered}");
    }

    #[test]
    fn normal_status_always_reports_local_usage_enablement_and_store_health() {
        let report = status_report(true, "ready", "ready", "ready", "ready", "ready");
        let context = context(80, ColorMode::Never);
        for (usage, expected) in [
            (
                local_usage::UsageReport {
                    enabled: false,
                    state: "disabled",
                    ..usage_report()
                },
                "Local usage  disabled",
            ),
            (
                local_usage::UsageReport {
                    state: "empty",
                    ..usage_report()
                },
                "Local usage  enabled; store missing or empty",
            ),
        ] {
            let rendered = render_status_human(
                &context,
                &report,
                std::path::Path::new("/tmp/ctx"),
                std::path::Path::new("/tmp/ctx/config.toml"),
                &healthy_upgrade(),
                &no_pro(),
                &usage,
            )
            .render_plain();
            assert!(rendered.contains(expected), "{rendered}");
        }
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
