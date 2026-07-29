use std::path::PathBuf;

use anyhow::Result;
use serde_json::json;

use ctx_history_capture::{
    DiscoveryIssue, DiscoveryIssueKind, DiscoveryReport, ProviderSourceStatus,
};
use ctx_history_core::CaptureProvider;

use crate::analytics::{count_bucket, SourcesTelemetry};
use crate::history_source_plugins::discover_history_source_plugins_with_diagnostics;
use crate::output::print_json;
use crate::provider_args::ProviderArg;
use crate::provider_sources::{
    discovered_sources_for_provider_report, discovered_sources_report,
    discovery_report_issues_json, manual_path_guidance, plugin_manifest_failures_json,
    plugin_sources_json, provider_cli_name, sources_json, SourceInfo,
};
use crate::{SourcesArgs, DEFAULT_VISIBLE_SOURCE_PROVIDERS};

pub(crate) fn run_sources(
    args: SourcesArgs,
    data_root: PathBuf,
    telemetry: &mut SourcesTelemetry,
) -> Result<()> {
    let provider_filter = args.provider.map(ProviderArg::capture_provider);
    let discovery_report = match provider_filter {
        Some(CaptureProvider::Custom) => DiscoveryReport::default(),
        Some(provider) => discovered_sources_for_provider_report(provider),
        None => discovered_sources_report(),
    };
    let sources = &discovery_report.sources;
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
    telemetry.providers_detected = Some(count_bucket(
        sources
            .len()
            .saturating_add(plugin_sources.len())
            .saturating_add(plugin_failures.len()) as u64,
    ));
    telemetry.providers_existing = Some(count_bucket(existing as u64));
    telemetry.providers_importable = Some(count_bucket(importable as u64));
    let show_all_sources = args.all || args.show_missing || provider_filter.is_some();
    let visible_sources = sources
        .iter()
        .filter(|source| show_all_sources || source_visible_by_default(source))
        .cloned()
        .collect::<Vec<_>>();
    let hidden_missing_sources = sources.len().saturating_sub(visible_sources.len());
    if args.format.is_json() {
        let mut source_values = sources_json(&visible_sources);
        source_values.extend(plugin_sources_json(&plugin_sources));
        source_values.extend(plugin_manifest_failures_json(&plugin_failures));
        let (issues, issues_truncated) = discovery_report_issues_json(&discovery_report);
        print_json(json!({
            "schema_version": 1,
            "scope": if show_all_sources { "all" } else { "default" },
            "hidden_missing_sources": hidden_missing_sources,
            "sources": source_values,
            "issues": issues,
            "issues_truncated": issues_truncated,
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
            if source.status == ProviderSourceStatus::Unsupported {
                if let Some(reason) = source.unsupported_reason {
                    println!("  {reason}");
                }
                println!(
                    "  current ctx cannot import this path; for supported history, use `{}`",
                    manual_path_guidance(source.provider)
                );
            }
        }
        for issue in &discovery_report.issues {
            print_discovery_issue(issue);
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
                "custom {} unsupported (history-source-plugin:{}): no v0.26 source-backed adapter",
                source.label(),
                source.source_format
            );
        }
        if hidden_missing_sources > 0 {
            println!(
                "{hidden_missing_sources} missing provider locations hidden. Run `ctx sources --all` to show every known provider location."
            );
        }
    }
    Ok(())
}

fn print_discovery_issue(issue: &DiscoveryIssue) {
    let provider = source_provider_cli_name(issue.provider);
    match issue.kind {
        DiscoveryIssueKind::NoDiskHistory => {
            println!("{provider}: no disk history is selected: {}", issue.reason);
            println!(
                "  select a disk-backed history location, then use `{}`",
                manual_path_guidance(issue.provider)
            );
        }
        DiscoveryIssueKind::SelectorUnreconstructible => {
            println!(
                "{provider}: the automatic history location cannot be safely reconstructed: {}",
                issue.reason
            );
            println!("  use `{}`", manual_path_guidance(issue.provider));
        }
        DiscoveryIssueKind::InsufficientOfficialEvidence => {
            println!("{provider}: no official automatic history location is established");
            println!("  use `{}`", manual_path_guidance(issue.provider));
        }
    }
}

pub(crate) fn source_visible_by_default(source: &SourceInfo) -> bool {
    source.exists
        || source.status != ProviderSourceStatus::Missing
        || DEFAULT_VISIBLE_SOURCE_PROVIDERS.contains(&source.provider)
}

pub(crate) fn source_provider_cli_name(provider: CaptureProvider) -> &'static str {
    provider_cli_name(provider)
}
