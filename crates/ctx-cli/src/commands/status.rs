use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use crate::analytics::{count_bucket, StatusTelemetry};
use crate::local_usage;
use crate::output::print_json;
use crate::semantic::source_epoch_status_report;
use crate::ui::Ui;
use crate::StatusArgs;
use ctx_app_config::{self as config, CONFIG_FILE};
use ctx_cli_presentation::commands::compact_usage_health_json;

mod usage;

pub(crate) use usage::{malformed_config_failure, removed_cloud_config_failure, run_usage_action};

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

pub(crate) fn status_read_model_authorized(
    data_root: &Path,
    config: &config::AppConfig,
    storage: &local_usage::LocalUsageStorageAuthority,
    control: &local_usage::UsageControlSnapshot,
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
pub(crate) fn run_status_authorized(
    args: StatusArgs,
    data_root: &Path,
    config: &config::AppConfig,
    quiet: bool,
    telemetry: &mut StatusTelemetry,
    storage: &local_usage::LocalUsageStorageAuthority,
    control: &local_usage::UsageControlSnapshot,
    ui: &mut Ui,
) -> Result<()> {
    if let Some(mode) = args.usage {
        return run_usage_action(mode, data_root, storage, args.format.is_json(), quiet, ui);
    }
    let config_path = data_root.join(CONFIG_FILE);
    let mut status = status_read_model_authorized(data_root, config, storage, control)?;
    telemetry.initialized = Some(status.initialized);
    telemetry.indexed_items = status.indexed_items.map(count_bucket);
    telemetry.indexed_sessions = status.indexed_sessions.map(count_bucket);
    telemetry.indexed_events = status.indexed_events.map(count_bucket);
    telemetry.indexed_sources = status.indexed_sources.map(count_bucket);
    if args.format.is_json() {
        print_json(status.report)?;
    } else if !quiet {
        super::history_health::reconcile_history_inventory(&mut status.health, data_root, config)?;
        let document = ctx_cli_presentation::commands::render_status_human(
            ui.stdout_context(),
            &status.report,
            status.health.as_ref(),
            data_root,
            &config_path,
            &status.report["upgrade"],
            &status.local_usage,
        );
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
        let route_identity = "ab".repeat(32);
        let publication = GenerationWriter::open(
            data_root.join("search/lexical"),
            WriterOptions::default(),
        )
        .unwrap()
        .into_writer()
        .unwrap()
        .commit_with_publication_metadata(
            |_| true,
            |context| {
                let generation_id = context.generation_id().to_owned();
                let route = ctx_history_index::SourceRouteIdentity::from_sha256(
                    route_identity.clone(),
                )
                .map_err(|error| {
                    ctx_history_index::IndexError::PublicationMetadata(error.to_string())
                })?;
                let receipt = ctx_history_refresh::SourceBackedRefreshReceipt {
                    previous_generation: None,
                    published_generation: generation_id.clone(),
                    generation_changed: true,
                    published_explicit_source_catalog: None,
                    current: ctx_history_refresh::SourceBackedRefreshCurrent::default(),
                    route_results: vec![
                        ctx_history_refresh::SourceBackedRefreshRouteResult::succeeded(
                            route_identity.clone(),
                            true,
                        ),
                    ],
                    zero_source_authority: vec![
                        ctx_history_refresh::SourceBackedZeroSourceAuthority {
                            generation_id,
                            route_identity: route,
                            kind: ctx_history_refresh::SourceBackedZeroSourceAuthorityKind::CompleteEmptyInventory,
                        },
                    ],
                    catalog_route_bindings: Vec::new(),
                };
                serde_json::to_vec(&json!({
                    "version": ctx_history_refresh::SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
                    "request_id": "final-status-composition",
                    "operation": "refresh",
                    "refresh_scope": {"kind": "all"},
                    "receipt": receipt.to_json(),
                    "route_observations": [null],
                    "route_controls": {},
                    "committed_rejection_diagnostics": {},
                }))
                .map_err(|error| {
                    ctx_history_index::IndexError::PublicationMetadata(error.to_string())
                })
            },
        )
        .unwrap();
        let generation_id = publication.receipt().generation_id.clone();

        let config = config::AppConfig::default();
        let status = status_read_model(&data_root, &config).unwrap();
        assert_eq!(status.report["lexical"]["status"], "ready");
        assert_eq!(status.report["indexing"]["mode"], "auto");
        assert_eq!(status.report["lexical"]["generation_id"], generation_id);
        assert!(status.report.get("catalog").is_none());
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
