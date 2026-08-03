use std::path::PathBuf;

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::analytics::{ImportTelemetry, ProviderRefreshTrigger};
use crate::progress::ProgressArg;
use crate::ImportArgs;

mod automatic_source_refresh;
mod catalog;
mod core_refresh;
mod entry;
mod explicit;
mod explicit_source_catalog;
mod history_source_plugin;
mod provider_refresh;
mod report;
mod totals;

use automatic_source_refresh::{
    run_automatic_source_refresh_import, AutomaticSourceRefreshImportContext,
};
pub(crate) use entry::{import_report_analytics_outcome, import_report_failure_type, run_import};
use explicit::{run_explicit_source_catalog_import, ExplicitSourceCatalogImportContext};
#[cfg(test)]
pub(crate) use explicit_source_catalog::{
    explicit_source_catalog_authority_for_test, load_explicit_source_catalog_authority,
};
pub(crate) use explicit_source_catalog::{
    explicit_source_for_import, relocate_explicit_source, relocation_authority_for_import,
    upsert_explicit_source, ExplicitSourceCatalogAuthority, ExplicitSourceCatalogRouteBinding,
    ExplicitSourceRelocationAuthority,
};
use history_source_plugin::{run_history_source_plugin_import, HistorySourcePluginImportContext};
pub(crate) use provider_refresh::{ProviderRefreshCollector, ProviderRefreshRuntimeFacts};
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
    validate_import_args(args)?;
    if args.history_source.is_some() || !args.history_source_manifest.is_empty() {
        return run_history_source_plugin_import(HistorySourcePluginImportContext {
            args,
            data_root,
            telemetry,
            provider_refreshes,
            refresh_trigger,
            config,
            options,
        });
    }
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
    Ok(())
}
