use std::path::PathBuf;

use anyhow::Result;
use serde_json::json;

use ctx_history_capture::ProviderSourceStatus;
use ctx_history_core::CaptureProvider;

use crate::analytics::AnalyticsProperties;
use crate::history_source_plugins::discover_history_source_plugins_with_diagnostics;
use crate::output::print_json;
use crate::provider_args::ProviderArg;
use crate::provider_sources::{
    discovered_sources, discovered_sources_for_provider, plugin_manifest_failures_json,
    plugin_sources_json, sources_json, SourceInfo,
};
use crate::{analytics, SourcesArgs, DEFAULT_VISIBLE_SOURCE_PROVIDERS};

pub(crate) fn run_sources(
    args: SourcesArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let provider_filter = args.provider.map(ProviderArg::capture_provider);
    let sources = match provider_filter {
        Some(CaptureProvider::Custom) => Vec::new(),
        Some(provider) => discovered_sources_for_provider(provider),
        None => discovered_sources(),
    };
    let plugin_discovery = discover_history_source_plugins_with_diagnostics(&data_root, &[])?;
    let (plugin_sources, plugin_failures) = if matches!(provider_filter, Some(provider) if provider != CaptureProvider::Custom)
    {
        (Vec::new(), Vec::new())
    } else {
        (plugin_discovery.sources, plugin_discovery.failures)
    };
    let existing = sources.iter().filter(|source| source.exists).count();
    let importable = sources
        .iter()
        .filter(|source| {
            source.exists
                && source.import_support.is_importable()
                && source.status == ProviderSourceStatus::Available
        })
        .count();
    analytics::insert_count_bucket(
        analytics_properties,
        "providers_detected_bucket",
        sources
            .len()
            .saturating_add(plugin_sources.len())
            .saturating_add(plugin_failures.len()) as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "providers_existing_bucket",
        existing as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "providers_importable_bucket",
        importable as u64,
    );
    let show_all_sources = args.all || args.show_missing || provider_filter.is_some();
    let visible_sources = sources
        .iter()
        .filter(|source| show_all_sources || source_visible_by_default(source))
        .cloned()
        .collect::<Vec<_>>();
    let hidden_missing_sources = sources.len().saturating_sub(visible_sources.len());
    if args.json {
        let mut source_values = sources_json(&visible_sources);
        source_values.extend(plugin_sources_json(&plugin_sources));
        source_values.extend(plugin_manifest_failures_json(&plugin_failures));
        print_json(json!({
            "schema_version": 1,
            "scope": if show_all_sources { "all" } else { "default" },
            "hidden_missing_sources": hidden_missing_sources,
            "sources": source_values,
        }))?;
    } else {
        for source in visible_sources {
            println!(
                "{} {} {} ({})",
                source_provider_cli_name(source.provider),
                source.path.display(),
                source.status.as_str(),
                source.source_format
            );
        }
        for failure in plugin_failures {
            println!(
                "custom history-source-plugin invalid: {}: {}",
                failure.manifest_path.display(),
                failure.error
            );
        }
        for source in plugin_sources {
            println!(
                "custom {} available (history-source-plugin:{})",
                source.label(),
                source.source_format
            );
        }
        if hidden_missing_sources > 0 {
            println!(
                "{} missing provider locations hidden. Run `ctx sources --all` to show every known provider location.",
                hidden_missing_sources
            );
        }
    }
    Ok(())
}

pub(crate) fn source_visible_by_default(source: &SourceInfo) -> bool {
    source.exists
        || source.status != ProviderSourceStatus::Missing
        || DEFAULT_VISIBLE_SOURCE_PROVIDERS.contains(&source.provider)
}

pub(crate) fn source_provider_cli_name(provider: CaptureProvider) -> &'static str {
    ProviderArg::parse_name(provider.as_str())
        .map(ProviderArg::cli_name)
        .unwrap_or_else(|| provider.as_str())
}
