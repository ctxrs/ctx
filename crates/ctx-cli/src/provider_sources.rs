use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};

use ctx_history_capture::{
    discover_provider_sources, discover_provider_sources_for_provider, provider_source_for_path,
    ProviderImportSupport, ProviderSource, ProviderSourceStatus,
};
use ctx_history_core::{CaptureProvider, ProviderRawRetention};

use crate::history_source_plugins::{
    discover_history_source_plugins_with_diagnostics, HistorySourcePluginManifestFailure,
    HistorySourcePluginRefresh, HistorySourcePluginSource,
};
use crate::identity;
use crate::provider_args::cli_supported_provider;

pub(crate) type SourceInfo = ProviderSource;
pub(crate) fn discovered_plugin_sources_json(data_root: &Path) -> Result<Vec<Value>> {
    let plugin_discovery = discover_history_source_plugins_with_diagnostics(data_root, &[])?;
    let mut values = plugin_sources_json(&plugin_discovery.sources);
    values.extend(plugin_manifest_failures_json(&plugin_discovery.failures));
    Ok(values)
}
pub(crate) fn discovered_sources() -> Vec<SourceInfo> {
    home_dir()
        .as_deref()
        .map(discover_provider_sources)
        .map(filter_cli_supported_sources)
        .unwrap_or_default()
}

pub(crate) fn discovered_sources_for_provider(provider: CaptureProvider) -> Vec<SourceInfo> {
    if !cli_supported_provider(provider) {
        return Vec::new();
    }
    home_dir()
        .as_deref()
        .map(|home| discover_provider_sources_for_provider(home, provider))
        .unwrap_or_default()
}

pub(crate) fn filter_cli_supported_sources(sources: Vec<SourceInfo>) -> Vec<SourceInfo> {
    sources
        .into_iter()
        .filter(|source| cli_supported_provider(source.provider))
        .collect()
}

pub(crate) fn explicit_path_source(provider: CaptureProvider, path: PathBuf) -> SourceInfo {
    source_for_path(provider, path)
}

pub(crate) fn source_for_path(provider: CaptureProvider, path: PathBuf) -> SourceInfo {
    provider_source_for_path(provider, path)
}

pub(crate) fn sources_json(sources: &[SourceInfo]) -> Vec<Value> {
    sources
        .iter()
        .map(|source| {
            json!({
                "provider": source.provider.as_str(),
                "path": source.path,
                "exists": source.exists,
                "source_format": source.source_format,
                "status": source.status.as_str(),
                "import_support": import_support_json(source.import_support),
                "native_import": source.import_support.is_auto_importable(),
                "importable": source.status == ProviderSourceStatus::Available
                    && source.import_support.is_importable(),
                "raw_retention": raw_retention_json(source.raw_retention),
                "unsupported_reason": source.unsupported_reason,
            })
        })
        .collect()
}

pub(crate) fn plugin_sources_json(sources: &[HistorySourcePluginSource]) -> Vec<Value> {
    sources
        .iter()
        .map(|source| {
            json!({
                "provider": CaptureProvider::Custom.as_str(),
                "kind": "history_source_plugin",
                "plugin": source.plugin_name,
                "plugin_display_name": source.plugin_display_name,
                "plugin_version": source.plugin_version,
                "history_source": source.label(),
                "history_source_id": source.id,
                "display_name": source.display_name,
                "provider_key": source.provider_key,
                "source_id": source.source_id,
                "source_format": source.source_format,
                "manifest_path": source.manifest_path,
                "enabled": source.enabled,
                "refresh": history_source_plugin_refresh_json(source.refresh),
                "status": "available",
                "import_support": "history_source_plugin",
                "native_import": false,
                "importable": true,
                "raw_retention": "metadata_only",
                "unsupported_reason": null,
            })
        })
        .collect()
}

pub(crate) fn plugin_manifest_failures_json(
    failures: &[HistorySourcePluginManifestFailure],
) -> Vec<Value> {
    failures
        .iter()
        .map(|failure| {
            json!({
                "provider": CaptureProvider::Custom.as_str(),
                "kind": "history_source_plugin",
                "plugin": null,
                "plugin_display_name": null,
                "plugin_version": null,
                "history_source": null,
                "history_source_id": null,
                "display_name": null,
                "provider_key": null,
                "source_id": null,
                "source_format": null,
                "manifest_path": failure.manifest_path,
                "enabled": false,
                "refresh": null,
                "status": "invalid",
                "import_support": "history_source_plugin",
                "native_import": false,
                "importable": false,
                "raw_retention": "metadata_only",
                "unsupported_reason": failure.error,
                "error": failure.error,
            })
        })
        .collect()
}

pub(crate) fn history_source_plugin_refresh_json(
    refresh: HistorySourcePluginRefresh,
) -> &'static str {
    match refresh {
        HistorySourcePluginRefresh::Manual => "manual",
        HistorySourcePluginRefresh::Auto => "auto",
    }
}

pub(crate) fn import_support_json(support: ProviderImportSupport) -> &'static str {
    match support {
        ProviderImportSupport::Native => "native",
        ProviderImportSupport::Explicit => "explicit",
        ProviderImportSupport::Unsupported => "unsupported",
    }
}

pub(crate) fn raw_retention_json(retention: ProviderRawRetention) -> &'static str {
    match retention {
        ProviderRawRetention::None => "none",
        ProviderRawRetention::PathReference => "path_reference",
        ProviderRawRetention::MetadataOnly => "metadata_only",
        ProviderRawRetention::LocalBlob => "local_blob",
        ProviderRawRetention::Withheld => "withheld",
    }
}
pub(crate) fn home_dir() -> Option<PathBuf> {
    identity::home_dir()
}
