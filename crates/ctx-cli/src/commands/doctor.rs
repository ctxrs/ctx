use std::path::PathBuf;

use anyhow::Result;
use serde_json::json;

use ctx_history_core::database_path;

use crate::analytics::{count_bucket, DoctorTelemetry};
use crate::config::AppConfig;
use crate::output::print_json;
use crate::semantic::{daemon_report, semantic_health_findings, semantic_worker_report};
use crate::store_util::open_existing_store_read_only;
use crate::DoctorArgs;

pub(crate) fn run_doctor(
    args: DoctorArgs,
    data_root: PathBuf,
    telemetry: &mut DoctorTelemetry,
) -> Result<()> {
    let json_output = args.format.is_json();
    let db_path = database_path(data_root.clone());
    let mut findings = Vec::new();
    if !data_root.exists() {
        findings.push(format!("data root does not exist: {}", data_root.display()));
    }
    if !db_path.exists() {
        findings.push(format!(
            "ctx store is not initialized at {}; run `ctx setup` or `ctx import` first",
            db_path.display()
        ));
    } else {
        let store = open_existing_store_read_only(&db_path, "ctx doctor")?;
        findings.extend(store.validate()?);
    }
    findings.extend(semantic_health_findings(&data_root));
    let semantic_report = if db_path.exists() {
        let store = open_existing_store_read_only(&db_path, "ctx doctor semantic status")?;
        semantic_worker_report(&data_root, Some(&store))?
    } else {
        semantic_worker_report(&data_root, None)?
    };
    let daemon = daemon_report(&data_root, &semantic_report);
    let pro = crate::pro::lifecycle_status_json(&data_root);
    let config = AppConfig::load(&data_root)?;
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
                "ctx Pro key store is unavailable ({code}); configure and unlock a persistent platform key store (not an ephemeral session collection), then run `ctx pro`; plaintext key fallback is not supported"
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
