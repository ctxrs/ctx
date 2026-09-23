use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use crate::analytics::{count_bucket, StatusTelemetry};
use crate::local_usage;
use crate::output::print_json;
use crate::semantic::source_epoch_status_report;
use crate::ui::Ui;
use crate::unified_health::UnifiedHealth;
use crate::StatusArgs;
use ctx_app_config::{self as config, CONFIG_FILE};
use ctx_cli_presentation::commands::compact_usage_health_json;

mod usage;

pub(crate) use usage::{
    malformed_config_failure_with_components, removed_cloud_config_failure_with_components,
    run_usage_action,
};

pub(super) fn upgrade_report(config: &config::AppConfig) -> serde_json::Value {
    crate::upgrade::upgrade_diagnostics(config).report
}

pub(crate) struct StatusReadModel {
    pub(crate) report: Value,
    health: Option<ctx_history_read_application::HistoryHealthReport>,
    local_usage: local_usage::UsageReport,
    initialized: bool,
    indexed_items: Option<u64>,
    indexed_sessions: Option<u64>,
    indexed_events: Option<u64>,
    indexed_sources: Option<u64>,
}

#[cfg(test)]
pub(crate) fn status_read_model_authorized(
    data_root: &Path,
    config: &config::AppConfig,
    storage: &local_usage::LocalUsageStorageAuthority,
    control: &local_usage::UsageControlSnapshot,
) -> Result<StatusReadModel> {
    status_read_model_authorized_with_components(data_root, config, storage, control, None)
}

pub(crate) fn status_read_model_authorized_with_components(
    data_root: &Path,
    config: &config::AppConfig,
    storage: &local_usage::LocalUsageStorageAuthority,
    control: &local_usage::UsageControlSnapshot,
    components: Option<&UnifiedHealth>,
) -> Result<StatusReadModel> {
    let source = source_epoch_status_report(data_root, config)?;
    let health = source.health;
    let upgrade = upgrade_report(config);
    let local_usage = local_usage::read_report_authorized(storage, control, false);
    let mut report = source.report;
    if let Some(object) = report.as_object_mut() {
        object.remove("catalog");
        object.insert(
            "indexing".to_owned(),
            json!({"mode": config.indexing.mode.as_str()}),
        );
        object.insert("upgrade".to_owned(), upgrade);
        object.insert(
            "local_usage".to_owned(),
            compact_usage_health_json(&local_usage),
        );
        object.insert("read_only".to_owned(), json!(true));
    }
    if let Some(components) = components {
        components.append_json(&mut report)?;
    }
    Ok(StatusReadModel {
        report,
        health,
        local_usage,
        initialized: source.initialized,
        indexed_items: source.indexed_items,
        indexed_sessions: source.indexed_sessions,
        indexed_events: source.indexed_events,
        indexed_sources: source.indexed_sources,
    })
}

// Dispatch assembles these independently borrowed authorities and output sinks.
// Bundling them would only move this call boundary into `dispatch.rs` without
// simplifying status orchestration or ownership.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_status_authorized_with_components(
    args: StatusArgs,
    data_root: &Path,
    config: &config::AppConfig,
    quiet: bool,
    telemetry: &mut StatusTelemetry,
    storage: &local_usage::LocalUsageStorageAuthority,
    control: &local_usage::UsageControlSnapshot,
    components: Option<&UnifiedHealth>,
    ui: &mut Ui,
) -> Result<()> {
    if let Some(mode) = args.usage {
        return run_usage_action(mode, data_root, storage, args.format.is_json(), quiet, ui);
    }
    let config_path = data_root.join(CONFIG_FILE);
    let mut status = match status_read_model_authorized_with_components(
        data_root, config, storage, control, components,
    ) {
        Ok(status) => status,
        Err(error) => {
            return match components {
                Some(components) => crate::unified_health::history_health_failure(
                    2,
                    args.format.is_json(),
                    components,
                    ui,
                ),
                None => Err(error),
            };
        }
    };
    telemetry.initialized = Some(status.initialized);
    telemetry.indexed_items = status.indexed_items.map(count_bucket);
    telemetry.indexed_sessions = status.indexed_sessions.map(count_bucket);
    telemetry.indexed_events = status.indexed_events.map(count_bucket);
    telemetry.indexed_sources = status.indexed_sources.map(count_bucket);
    if args.format.is_json() {
        print_json(status.report)?;
    } else if !quiet {
        if let Err(error) = super::history_health::reconcile_history_inventory(
            &mut status.health,
            data_root,
            config,
        ) {
            return match components {
                Some(components) => {
                    crate::unified_health::history_health_failure(2, false, components, ui)
                }
                None => Err(error),
            };
        }
        let mut document = ctx_cli_presentation::commands::render_status_human(
            ui.stdout_context(),
            &status.report,
            status.health.as_ref(),
            data_root,
            &config_path,
            &status.report["upgrade"],
            &status.local_usage,
        );
        if let Some(components) = components {
            components.append_human(ui.stdout_context(), &mut document);
        }
        ui.write_stdout(&document)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn status_read_model(
    data_root: &Path,
    config: &config::AppConfig,
) -> Result<StatusReadModel> {
    let storage = crate::observability_composition::local_usage_storage_authority(data_root);
    let control =
        crate::observability_composition::usage_control_snapshot(config.local_usage.enabled);
    status_read_model_authorized(data_root, config, &storage, &control)
}

#[cfg(test)]
fn load_status_config(data_root: &Path) -> Option<config::AppConfig> {
    // Dispatch already loaded this file, but a concurrent replacement can make
    // the status-specific reread fail. Discard that raw cause here so neither
    // its path nor its content can reach the generic CLI error renderer.
    config::AppConfig::load(data_root).ok()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ctx_history_index::{GenerationWriter, WriterOptions};

    use super::*;

    #[test]
    fn published_core_generation_flows_through_final_status_composition() {
        crate::semantic::initialize().unwrap();
        let root = tempfile::tempdir().unwrap();
        let data_root = root.path().join("data");
        let publication =
            GenerationWriter::open(data_root.join("search/lexical"), WriterOptions::default())
                .unwrap()
                .into_writer()
                .unwrap()
                .commit(|_| true)
                .unwrap();
        let generation_id = publication.generation_id.clone();

        let config = config::AppConfig::default();
        let status = status_read_model(&data_root, &config).unwrap();
        assert_eq!(status.report["lexical"]["status"], "ready");
        assert_eq!(status.report["indexing"]["mode"], "auto");
        assert_eq!(status.report["lexical"]["generation_id"], generation_id);
        assert!(status.report.get("catalog").is_none());

        let components = UnifiedHealth::inspect(None);
        let storage = crate::observability_composition::local_usage_storage_authority(&data_root);
        let control =
            crate::observability_composition::usage_control_snapshot(config.local_usage.enabled);
        let unified = status_read_model_authorized_with_components(
            &data_root,
            &config,
            &storage,
            &control,
            Some(&components),
        )
        .unwrap();
        assert_eq!(unified.report["lexical"], status.report["lexical"]);
        assert_eq!(unified.report["local_usage"], status.report["local_usage"]);
        assert_eq!(unified.report["graph"]["status"], "not_indexed");
        assert_eq!(unified.report["output"]["status"], "available");
        assert!(!data_root.join(".graf").exists());
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
        let rendered =
            serde_json::to_string(&ctx_cli_presentation::commands::malformed_status_config_json())
                .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&rendered).unwrap()["local_usage"]["error"]["code"],
            "local_usage_config_unavailable"
        );
        assert!(!rendered.contains(marker));
        assert!(!rendered.contains("credential"));
        assert!(!rendered.contains(temp.path().to_string_lossy().as_ref()));
    }
}
