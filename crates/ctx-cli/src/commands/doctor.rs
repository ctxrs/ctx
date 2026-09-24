use std::path::PathBuf;

use anyhow::Result;
use serde_json::{json, Value};

use crate::analytics::{count_bucket, DoctorTelemetry};
use crate::output::print_json;
use crate::semantic::source_epoch_status_report;
use crate::ui::Ui;
use crate::unified_health::UnifiedHealth;
use crate::DoctorArgs;
use ctx_app_config::AppConfig;

const SOURCE_DISCOVERY_FINDING: &str =
    "provider source discovery is incomplete; run `ctx sources --all` for details";

pub(crate) fn run_doctor_with_components(
    args: DoctorArgs,
    data_root: PathBuf,
    telemetry: &mut DoctorTelemetry,
    components: Option<&UnifiedHealth>,
    ui: &mut Ui,
) -> Result<()> {
    let json_output = args.format.is_json();
    let mut model = match doctor_read_model(&data_root) {
        Ok(model) => model,
        Err(error) => {
            return match components {
                Some(components) => {
                    telemetry.healthy = Some(false);
                    telemetry.finding_count = Some(count_bucket(1));
                    crate::unified_health::history_health_failure(1, json_output, components, ui)
                }
                None => Err(error),
            };
        }
    };
    if let Some(components) = components {
        components.append_json(&mut model.facts)?;
    }
    let findings = model.facts["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if json_output {
        telemetry.finding_count = Some(count_bucket(findings.len() as u64));
        telemetry.healthy = Some(findings.is_empty());
        print_json(model.facts)?;
    } else {
        // Human presentation already renders the inventory's coverage finding.
        let findings = findings
            .into_iter()
            .filter(|finding| finding != SOURCE_DISCOVERY_FINDING)
            .collect::<Vec<_>>();
        let coverage_issue = model
            .health
            .as_ref()
            .is_some_and(ctx_history_read_application::HistoryHealthReport::is_partial);
        telemetry.finding_count = Some(count_bucket(
            (findings.len() + usize::from(coverage_issue)) as u64,
        ));
        telemetry.healthy = Some(findings.is_empty() && !coverage_issue);
        let source_report = &model.facts["source_epoch"];
        let mut document = ctx_cli_presentation::commands::render_doctor_human(
            ui.stdout_context(),
            &findings,
            model.health.as_ref(),
            human_refresh_failure(source_report),
        );
        ctx_cli_presentation::commands::append_attribution(
            ui.stdout_context(),
            &mut document,
            source_report,
        );
        if let Some(components) = components {
            components.append_human(ui.stdout_context(), &mut document);
        }
        ui.write_stdout(&document)?;
    }
    Ok(())
}

/// Used when configuration failed before the ordinary doctor command could run.
pub(crate) fn malformed_config_failure_with_components(
    json_output: bool,
    components: Option<&UnifiedHealth>,
    ui: &mut Ui,
) -> Result<()> {
    let message = "History configuration could not be read.";
    let report = json!({
        "schema_version": 1,
        "ok": false,
        "findings": [message],
        "error": {"code": "history_config_unavailable", "message": message},
        "read_only": true,
    });
    let document = ctx_cli_presentation::commands::render_doctor_human(
        ui.stderr_context(),
        &[message.to_owned()],
        None,
        None,
    );
    crate::unified_health::emit_history_failure(json_output, report, document, components, ui)
}

fn human_refresh_failure(
    report: &Value,
) -> Option<ctx_cli_presentation::commands::DoctorRefreshFailure> {
    let refresh = report.get("refresh")?;
    let partial = matches!(
        refresh.get("status").and_then(Value::as_str),
        Some("partial" | "paused")
    );
    let detail = if refresh.get("reason").and_then(Value::as_str) == Some("core_refresh_failed") {
        refresh
            .get("last_error")
            .and_then(Value::as_str)
            .filter(|detail| !detail.is_empty())?
            .to_owned()
    } else if partial {
        let failures = refresh
            .pointer("/diagnostics/source_failures")?
            .as_array()?;
        let details = failures
            .iter()
            .filter_map(|failure| {
                let detail = failure
                    .get("detail")
                    .and_then(Value::as_str)
                    .filter(|detail| !detail.is_empty())?;
                Some(match failure.get("provider").and_then(Value::as_str) {
                    Some(provider) => format!("{provider}: {detail}"),
                    None => detail.to_owned(),
                })
            })
            .take(3)
            .collect::<Vec<_>>();
        if details.is_empty() {
            return None;
        }
        details.join("; ")
    } else {
        return None;
    };
    let search = match report
        .get("lexical")
        .and_then(|lexical| lexical.get("status"))
        .and_then(Value::as_str)
    {
        Some("ready" | "stale") => {
            ctx_cli_presentation::commands::DoctorSearchAvailability::Available
        }
        _ => ctx_cli_presentation::commands::DoctorSearchAvailability::Unavailable,
    };
    Some(ctx_cli_presentation::commands::DoctorRefreshFailure {
        detail,
        search,
        partial,
    })
}

struct DoctorReadModel {
    facts: Value,
    health: Option<ctx_history_read_application::HistoryHealthReport>,
}

fn doctor_read_model(data_root: &std::path::Path) -> Result<DoctorReadModel> {
    let mut findings = Vec::new();
    if !data_root.exists() {
        findings.push(format!("data root does not exist: {}", data_root.display()));
    }
    let config = AppConfig::load(data_root)?;
    let mut source = source_epoch_status_report(data_root, &config)?;
    super::history_health::reconcile_history_inventory(&mut source.health, data_root, &config)?;
    findings.extend(ctx_cli_presentation::commands::source_epoch_findings(
        &source.report,
        config.semantic_search_enabled(),
    ));
    if source.health.as_ref().is_some_and(|health| {
        health
            .provider_roots
            .is_some_and(|roots| roots.partial > 0 || roots.excluded > 0 || roots.unknown > 0)
    }) {
        findings.push(SOURCE_DISCOVERY_FINDING.to_owned());
    }
    let daemon = source.report["daemon"].clone();
    let upgrade_diagnostics = crate::upgrade::upgrade_diagnostics(&config);
    findings.extend(upgrade_diagnostics.findings);
    let upgrade = upgrade_diagnostics.report;
    let facts = json!({
        "schema_version": 1,
        "ok": findings.is_empty(),
        "findings": findings,
        "source_epoch": source.report,
        "daemon": daemon,
        "upgrade": upgrade,
    });
    Ok(DoctorReadModel {
        facts,
        health: source.health,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::JsonOutputFormat;
    use crate::ui::{RenderContext, StreamKind, TestContext};
    use ctx_cli_presentation::commands::DoctorSearchAvailability;

    #[test]
    fn malformed_history_reports_independent_components_and_never_healthy() {
        let root = tempfile::tempdir().unwrap();
        let config_path = root.path().join(ctx_app_config::CONFIG_FILE);
        let invalid = "invalid history config fixture 85e3c";
        std::fs::write(&config_path, invalid).unwrap();
        for before_dispatch in [false, true] {
            for format in [JsonOutputFormat::Text, JsonOutputFormat::Json] {
                let stdout = tempfile::NamedTempFile::new().unwrap();
                let stderr = tempfile::NamedTempFile::new().unwrap();
                let mut ui = Ui::with_writers(
                    stdout.reopen().unwrap(),
                    RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
                    stderr.reopen().unwrap(),
                    RenderContext::for_test(TestContext::pipe(StreamKind::Stderr)),
                );
                let health = UnifiedHealth::inspect(None);
                let mut telemetry = DoctorTelemetry::default();
                let result = if before_dispatch {
                    malformed_config_failure_with_components(
                        format.is_json(),
                        Some(&health),
                        &mut ui,
                    )
                } else {
                    run_doctor_with_components(
                        DoctorArgs { format },
                        root.path().to_path_buf(),
                        &mut telemetry,
                        Some(&health),
                        &mut ui,
                    )
                };
                assert!(result
                    .unwrap_err()
                    .is::<crate::dispatch::RenderedCliError>());
                if !before_dispatch {
                    assert_eq!(telemetry.healthy, Some(false));
                }
                ui.flush().unwrap();
                assert!(std::fs::read(stdout.path()).unwrap().is_empty());
                let rendered = std::fs::read_to_string(stderr.path()).unwrap();
                assert!(!rendered.contains(invalid));
                if format.is_json() {
                    let report: Value = serde_json::from_str(&rendered).unwrap();
                    assert_eq!(report["schema_version"], 1);
                    assert_eq!(report["ok"], false);
                    assert_eq!(report["history"]["status"], "unavailable");
                    assert_eq!(report["graph"]["status"], "not_indexed");
                    assert_eq!(report["output"]["status"], "available");
                    assert!(!report["findings"].as_array().unwrap().is_empty());
                } else {
                    assert!(rendered.contains("History"));
                    assert!(rendered.contains("unavailable"));
                    assert!(rendered.contains("Graph"));
                    assert!(rendered.contains("Command output"));
                    assert!(!rendered.contains("No problems found"));
                }
                assert_eq!(std::fs::read_to_string(&config_path).unwrap(), invalid);
                assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
            }
        }
    }

    #[test]
    fn human_refresh_failure_distinguishes_retained_and_cold_search() {
        for (lexical, expected) in [
            ("stale", DoctorSearchAvailability::Available),
            ("unavailable", DoctorSearchAvailability::Unavailable),
        ] {
            let report = json!({
                "lexical": {"status": lexical},
                "refresh": {
                    "status": "unavailable",
                    "reason": "core_refresh_failed",
                    "last_error": "root cause",
                },
            });
            let failure = human_refresh_failure(&report).unwrap();
            assert_eq!(failure.detail, "root cause");
            assert_eq!(failure.search, expected);
        }
    }

    #[test]
    fn human_refresh_failure_requires_the_root_failure_detail() {
        for report in [
            json!({"lexical": {"status": "ready"}, "refresh": {"reason": "core_refresh_failed"}}),
            json!({"lexical": {"status": "ready"}, "refresh": {"reason": "daemon_unavailable", "last_error": "noise"}}),
        ] {
            assert!(human_refresh_failure(&report).is_none());
        }
    }

    #[test]
    fn human_partial_refresh_preserves_actionable_provider_failure() {
        for detail in [
            "provider SQLite scratch has insufficient free-space headroom: required 16019288989, available 7329218560",
            "open provider snapshot: Too many open files (os error 24)",
        ] {
            let report = json!({
                "history_epoch": {"status": "ready"},
                "lexical": {"status": "ready"},
                "catalog": {"status": "ready"},
                "refresh": {
                    "status": "partial",
                    "reason": "completed_with_source_failures",
                    "diagnostics": {
                        "source_failures": [{"provider": "cursor", "detail": detail}]
                    }
                }
            });
            let failure = human_refresh_failure(&report).expect("partial failure detail");
            assert!(failure.detail.contains("cursor"));
            assert!(failure.detail.contains(detail));
            assert_eq!(failure.search, DoctorSearchAvailability::Available);
            let findings = ctx_cli_presentation::commands::source_epoch_findings(&report, false);
            let context = crate::ui::RenderContext::for_test(crate::ui::TestContext::tty(
                crate::ui::StreamKind::Stdout, 120,
            ));
            let rendered = ctx_cli_presentation::commands::render_doctor_human(
                &context, &findings, None, Some(failure),
            ).render_plain();
            let text = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(text.contains(detail), "{rendered}");
            assert!(text.contains("History refresh is partial"), "{rendered}");
            assert!(!text.contains("The component is not ready"), "{rendered}");
            assert!(text.contains("ctx import --all"), "{rendered}");
        }
    }
}
