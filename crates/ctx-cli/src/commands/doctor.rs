use std::path::PathBuf;

use anyhow::Result;
use serde_json::json;

use crate::analytics::{count_bucket, DoctorTelemetry};
use crate::config::AppConfig;
use crate::output::print_json;
use crate::semantic::source_epoch_status_report;
use crate::DoctorArgs;

pub(crate) fn run_doctor(
    args: DoctorArgs,
    data_root: PathBuf,
    telemetry: &mut DoctorTelemetry,
) -> Result<()> {
    let json_output = args.format.is_json();
    let mut findings = Vec::new();
    if !data_root.exists() {
        findings.push(format!("data root does not exist: {}", data_root.display()));
    }
    let config = AppConfig::load(&data_root)?;
    let source = source_epoch_status_report(&data_root, &config)?;
    let pro = crate::pro::lifecycle_status_json(&data_root);
    for (name, required) in [
        ("history_epoch", true),
        ("lexical", true),
        ("catalog", true),
        ("resolver", true),
        ("relational", true),
        ("semantic", config.semantic_search_enabled()),
        (
            "pro_projection",
            pro.get("installed").and_then(serde_json::Value::as_bool) == Some(true),
        ),
    ] {
        if !required {
            continue;
        }
        let component = &source.report[name];
        let status = component
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unavailable");
        if !matches!(status, "ready" | "disabled") {
            let reason = component
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            findings.push(format!("{name} is {status} ({reason})"));
        }
    }
    let daemon = source.report["daemon"].clone();
    let upgrade_diagnostics = crate::upgrade::upgrade_diagnostics(&config);
    findings.extend(upgrade_diagnostics.findings);
    let upgrade = upgrade_diagnostics.report;
    if pro["installed"].as_bool() == Some(true) {
        if let Some(code @ ("helper_upgrade_required" | "protocol_mismatch")) =
            pro["error_code"].as_str()
        {
            findings.push(format!(
                "ctx Pro helper is incompatible ({code}); run `ctx pro`"
            ));
        } else if let Some(code @ ("key_store_unavailable" | "key_store_locked")) =
            pro["error_code"].as_str()
        {
            findings.push(format!(
                "ctx Pro key store is unavailable ({code}); unlock or repair the already selected secure key store, then run `ctx pro`; a fresh installation can select the owner-private local vault only when the native store is genuinely unavailable, and ctx never downgrades existing state"
            ));
        } else if pro["error_code"].as_str() == Some("corrupt_graph") {
            findings.push(
                "ctx Pro graph needs repair; run `ctx pro` or reinstall with `ctx pro uninstall --delete-data`"
                    .to_owned(),
            );
        }
    }
    telemetry.finding_count = Some(count_bucket(findings.len() as u64));
    telemetry.healthy = Some(findings.is_empty());
    if json_output {
        print_json(json!({
            "schema_version": 1,
            "ok": findings.is_empty(),
            "findings": findings,
            "source_epoch": source.report,
            "daemon": daemon,
            "upgrade": upgrade,
            "pro": pro,
        }))?;
    } else {
        println!("upgrade_auto: {}", config.auto_upgrade_mode().as_str());
        if findings.is_empty() {
            println!("ok");
        } else {
            for finding in findings {
                println!("{finding}");
            }
        }
    }
    Ok(())
}
