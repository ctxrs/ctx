use std::path::PathBuf;

use anyhow::Result;
use serde_json::{json, Value};

use crate::analytics::{count_bucket, DoctorTelemetry};
use crate::config::AppConfig;
use crate::output::print_json;
use crate::semantic::{current_rejected_record_count, source_epoch_status_report};
use crate::ui::Ui;
use crate::DoctorArgs;

pub(crate) fn run_doctor(
    args: DoctorArgs,
    data_root: PathBuf,
    telemetry: &mut DoctorTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let json_output = args.format.is_json();
    let facts = doctor_facts(&data_root)?;
    let findings = facts["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let source_report = facts["source_epoch"].clone();
    let rejected_records = current_rejected_record_count(&source_report);
    let config = AppConfig::load(&data_root)?;
    telemetry.finding_count = Some(count_bucket(findings.len() as u64));
    telemetry.healthy = Some(findings.is_empty());
    if json_output {
        print_json(facts)?;
    } else {
        let document = ctx_cli_presentation::commands::render_doctor_human(
            ui.stdout_context(),
            config.auto_upgrade_mode().as_str(),
            &findings,
            rejected_records,
            human_refresh_failure(&source_report),
        );
        ui.write_stdout(&document)?;
    }
    Ok(())
}

fn human_refresh_failure(
    report: &Value,
) -> Option<ctx_cli_presentation::commands::DoctorRefreshFailure<'_>> {
    let refresh = report.get("refresh")?;
    if refresh.get("reason").and_then(Value::as_str) != Some("core_refresh_failed") {
        return None;
    }
    let detail = refresh
        .get("last_error")
        .and_then(Value::as_str)
        .filter(|detail| !detail.is_empty())?;
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
    Some(ctx_cli_presentation::commands::DoctorRefreshFailure { detail, search })
}

pub(crate) fn doctor_facts(data_root: &std::path::Path) -> Result<Value> {
    let mut findings = Vec::new();
    if !data_root.exists() {
        findings.push(format!("data root does not exist: {}", data_root.display()));
    }
    let config = AppConfig::load(data_root)?;
    let source = source_epoch_status_report(data_root, &config)?;
    findings.extend(ctx_cli_presentation::commands::source_epoch_findings(
        &source.report,
        config.semantic_search_enabled(),
    ));
    let daemon = source.report["daemon"].clone();
    let upgrade_diagnostics = crate::upgrade::upgrade_diagnostics(&config);
    findings.extend(upgrade_diagnostics.findings);
    let upgrade = upgrade_diagnostics.report;
    Ok(json!({
        "schema_version": 1,
        "ok": findings.is_empty(),
        "findings": findings,
        "source_epoch": source.report,
        "daemon": daemon,
        "upgrade": upgrade,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_cli_presentation::commands::DoctorSearchAvailability;

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
}
