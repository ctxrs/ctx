use std::{fs, path::PathBuf};

use anyhow::{anyhow, Result};
use serde_json::Value;

use ctx_history_capture::CaptureError;

use crate::analytics::{ImportTelemetry, ProviderRefreshTrigger};
use crate::progress::ProgressArg;
use crate::ImportArgs;

mod automatic_source_refresh;
mod catalog;
mod entry;
mod explicit;
mod explicit_source_catalog;
mod pro_output;
mod provider_refresh;
mod report;
mod totals;

use automatic_source_refresh::{
    run_automatic_source_refresh_import, AutomaticSourceRefreshImportContext,
};
pub(crate) use entry::{import_report_analytics_outcome, import_report_failure_type, run_import};
use explicit::{
    run_explicit_source_catalog_import, ExplicitSourceCatalogImportContext,
};
pub(crate) use explicit_source_catalog::{
    explicit_source_for_import, load_explicit_source_catalog_authority,
    register_explicit_source_catalog_routes, upsert_explicit_source,
    ExplicitSourceCatalogAuthority,
};
pub(crate) use pro_output::{
    catch_up_pro_outputs, prepare_core_for_pro_materialization,
};
use pro_output::ProOutputSelection;
pub(crate) use provider_refresh::{ProviderRefreshCollector, ProviderRefreshRuntimeFacts};
pub(crate) use report::{ImportFailureScope, ImportFailureType};
pub(crate) use totals::ImportTotals;

#[derive(Debug)]
pub(crate) struct ImportReport {
    pub(crate) resume: bool,
    pub(crate) totals: ImportTotals,
    pub(crate) sources: Vec<Value>,
}

impl ImportReport {
    pub(crate) fn resume_mode(&self) -> &'static str {
        resume_mode_name(self.resume)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ImportRunOptions {
    pub(crate) progress: ProgressArg,
    pub(crate) json: bool,
    pub(crate) print_human: bool,
    pub(crate) operation: &'static str,
}

pub(crate) fn resume_mode_name(resume: bool) -> &'static str {
    if resume {
        "idempotent_rescan"
    } else {
        "normal_scan"
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SourceStats {
    pub(crate) files: usize,
    pub(crate) bytes: u64,
    pub(crate) change_token: Option<[u8; 32]>,
}

pub(crate) fn run_import_internal(
    args: &ImportArgs,
    data_root: PathBuf,
    telemetry: &mut ImportTelemetry,
    provider_refreshes: &mut ProviderRefreshCollector,
    refresh_trigger: ProviderRefreshTrigger,
    config: &crate::config::AppConfig,
    options: ImportRunOptions,
) -> Result<ImportReport> {
    run_import_internal_with_pro_output(
        args,
        data_root,
        telemetry,
        provider_refreshes,
        refresh_trigger,
        config,
        options,
        ProOutputSelection::Automatic,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_import_internal_with_pro_output(
    args: &ImportArgs,
    data_root: PathBuf,
    telemetry: &mut ImportTelemetry,
    provider_refreshes: &mut ProviderRefreshCollector,
    refresh_trigger: ProviderRefreshTrigger,
    config: &crate::config::AppConfig,
    options: ImportRunOptions,
    _pro_output_selection: ProOutputSelection,
) -> Result<ImportReport> {
    validate_import_args(args)?;
    fs::create_dir_all(&data_root).map_err(|source| CaptureError::SystemIo {
        operation: "initialize ctx data root",
        source,
    })?;
    if args.path.is_some() {
        return run_explicit_source_catalog_import(ExplicitSourceCatalogImportContext {
            args,
            data_root,
            telemetry,
            provider_refreshes,
            refresh_trigger,
            config,
            options,
        });
    }
    run_automatic_source_refresh_import(AutomaticSourceRefreshImportContext {
        args,
        data_root,
        telemetry,
        provider_refreshes,
        config,
        options,
    })
}

fn validate_import_args(args: &ImportArgs) -> Result<()> {
    if args.input_format.is_some() && args.path.is_none() {
        return Err(anyhow!(
            "ctx import --input-format requires --path for a source-backed catalog entry"
        ));
    }
    if args.path.is_some() && args.input_format.is_none() && args.provider.is_none() {
        return Err(anyhow!(
            "ctx import --path requires --provider for native provider history; use `ctx import --provider codex --path <path>` or `ctx import --input-format ctx-history-jsonl-v1 --path <file>`"
        ));
    }
    if args.history_source.is_some() || !args.history_source_manifest.is_empty() {
        return Err(anyhow!(
            "history source plugin imports have no source-backed adapter; no legacy import fallback was used"
        ));
    }
    Ok(())
}
