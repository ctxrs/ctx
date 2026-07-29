use std::{fmt::Write as _, io::Write as _, path::PathBuf};

use anyhow::Result;
use serde_json::json;

use ctx_history_capture::{
    DiscoveryIssue, DiscoveryIssueKind, DiscoveryReport, ProviderSourceStatus,
};
use ctx_history_core::CaptureProvider;

use crate::analytics::{count_bucket, SourcesTelemetry};
use crate::history_source_plugins::discover_history_source_plugins_with_diagnostics;
use crate::local_usage::{CliUsage, ResultObservationAction};
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
    local_usage: &mut CliUsage,
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
    let mut canonical_entries = sources_json(&visible_sources);
    canonical_entries.extend(plugin_sources_json(&plugin_sources));
    canonical_entries.extend(plugin_manifest_failures_json(&plugin_failures));
    let result_count = canonical_entries.len();
    let content_bytes = serde_json::to_vec(&canonical_entries)?.len();
    let output_bytes = if args.format.is_json() {
        let (issues, issues_truncated) = discovery_report_issues_json(&discovery_report);
        let value = json!({
            "schema_version": 1,
            "scope": if show_all_sources { "all" } else { "default" },
            "hidden_missing_sources": hidden_missing_sources,
            "sources": canonical_entries,
            "issues": issues,
            "issues_truncated": issues_truncated,
        });
        let output_bytes = serde_json::to_string_pretty(&value)?
            .len()
            .saturating_add(1);
        print_json(value)?;
        output_bytes
    } else {
        let mut body = String::new();
        for source in &visible_sources {
            let _ = writeln!(
                body,
                "{} {} {} ({})",
                source_provider_cli_name(source.provider),
                source.path.display(),
                source.status.as_str(),
                source.source_format
            );
            if source.status == ProviderSourceStatus::Unsupported {
                if let Some(reason) = &source.unsupported_reason {
                    let _ = writeln!(body, "  {reason}");
                }
                let _ = writeln!(
                    body,
                    "  current ctx cannot import this path; for supported history, use `{}`",
                    manual_path_guidance(source.provider)
                );
            }
        }
        for issue in &discovery_report.issues {
            render_discovery_issue(&mut body, issue);
        }
        for failure in &plugin_failures {
            let _ = writeln!(
                body,
                "custom history-source-plugin invalid: {}: {}",
                failure.manifest_path.display(),
                failure.error
            );
        }
        for source in &plugin_sources {
            let _ = writeln!(
                body,
                "custom {} unsupported (history-source-plugin:{}): no v0.26 source-backed adapter",
                source.label(),
                source.source_format
            );
        }
        if hidden_missing_sources > 0 {
            let _ = writeln!(
                body,
                "{hidden_missing_sources} missing provider locations hidden. Run `ctx sources --all` to show every known provider location."
            );
        }
        std::io::stdout().lock().write_all(body.as_bytes())?;
        body.len()
    };
    local_usage.set_result_observation(
        ResultObservationAction::Sources,
        result_count,
        0,
        content_bytes,
    );
    local_usage.set_measured_output_bytes(output_bytes);
    Ok(())
}

fn render_discovery_issue(body: &mut String, issue: &DiscoveryIssue) {
    let provider = source_provider_cli_name(issue.provider);
    match issue.kind {
        DiscoveryIssueKind::NoDiskHistory => {
            let _ = writeln!(
                body,
                "{provider}: no disk history is selected: {}",
                issue.reason
            );
            let _ = writeln!(
                body,
                "  select a disk-backed history location, then use `{}`",
                manual_path_guidance(issue.provider)
            );
        }
        DiscoveryIssueKind::SelectorUnreconstructible => {
            let _ = writeln!(
                body,
                "{provider}: the automatic history location cannot be safely reconstructed: {}",
                issue.reason
            );
            let _ = writeln!(body, "  use `{}`", manual_path_guidance(issue.provider));
        }
        DiscoveryIssueKind::InsufficientOfficialEvidence => {
            let _ = writeln!(
                body,
                "{provider}: no official automatic history location is established"
            );
            let _ = writeln!(body, "  use `{}`", manual_path_guidance(issue.provider));
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
