use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;
use serde_json::{json, Value};

use ctx_history_capture::{
    discover_provider_sources_for_provider_report, discover_provider_sources_report,
    DiscoveryIssue, DiscoveryIssueKind, DiscoveryReport, ProviderImportSupport, ProviderSource,
    ProviderSourceStatus,
};
use ctx_history_core::CaptureProvider;

use crate::history_source_plugins::{
    discover_history_source_plugins_with_diagnostics, HistorySourcePluginManifestFailure,
    HistorySourcePluginRefresh, HistorySourcePluginSource, COMMAND_ONLY_UNSUPPORTED_REASON,
};
use crate::identity;
use crate::provider_args::{cli_supported_provider, ProviderArg};

pub(crate) const MAX_DISCOVERY_ISSUES: usize = 64;
pub(crate) const MAX_DISCOVERY_ISSUE_MESSAGE_BYTES: usize = 512;
const MISSING_DURABLE_PLUGIN_SOURCE_REASON: &str =
    "the declared provider-owned durable source path is not a regular non-symlink file";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HistorySourcePluginReportingStatus {
    Available,
    Missing,
    Unsupported,
}

impl HistorySourcePluginReportingStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Missing => "missing",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct HistorySourcePluginReport<'a> {
    pub(crate) durable_path: Option<&'a Path>,
    pub(crate) status: HistorySourcePluginReportingStatus,
    pub(crate) unsupported_reason: Option<&'static str>,
}

impl HistorySourcePluginReport<'_> {
    pub(crate) const fn is_importable(self) -> bool {
        matches!(self.status, HistorySourcePluginReportingStatus::Available)
    }
}

pub(crate) type SourceInfo = ProviderSource;
pub(crate) fn discovered_plugin_sources_json(data_root: &Path) -> Result<Vec<Value>> {
    let plugin_discovery = discover_history_source_plugins_with_diagnostics(data_root, &[])?;
    let mut values = plugin_sources_json(&plugin_discovery.sources);
    values.extend(plugin_manifest_failures_json(&plugin_discovery.failures));
    Ok(values)
}
pub(crate) fn discovered_sources_report() -> DiscoveryReport {
    home_dir()
        .as_deref()
        .map(discover_provider_sources_report)
        .map(filter_cli_supported_report)
        .unwrap_or_default()
}

pub(crate) fn discovered_sources_for_provider_report(provider: CaptureProvider) -> DiscoveryReport {
    if !cli_supported_provider(provider) {
        return DiscoveryReport::default();
    }
    home_dir()
        .as_deref()
        .map(|home| discover_provider_sources_for_provider_report(home, provider))
        .unwrap_or_default()
}

pub(crate) fn filter_cli_supported_sources(sources: Vec<SourceInfo>) -> Vec<SourceInfo> {
    sources
        .into_iter()
        .filter(|source| cli_supported_provider(source.provider))
        .collect()
}

pub(crate) fn filter_cli_supported_report(mut report: DiscoveryReport) -> DiscoveryReport {
    report.sources = filter_cli_supported_sources(report.sources);
    report
        .issues
        .retain(|issue| cli_supported_provider(issue.provider));
    report
}

pub(crate) fn provider_cli_name(provider: CaptureProvider) -> &'static str {
    ProviderArg::parse_name(provider.as_str())
        .map(ProviderArg::cli_name)
        .unwrap_or_else(|| provider.as_str())
}

pub(crate) fn manual_path_guidance(provider: CaptureProvider) -> String {
    format!(
        "ctx import --provider {} --path <path>",
        provider_cli_name(provider)
    )
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
                "unsupported_reason": source.unsupported_reason,
            })
        })
        .collect()
}

pub(crate) fn discovery_report_issues_json(report: &DiscoveryReport) -> (Vec<Value>, bool) {
    let issues = report
        .issues
        .iter()
        .take(MAX_DISCOVERY_ISSUES)
        .map(discovery_issue_json)
        .collect();
    (issues, report.issues.len() > MAX_DISCOVERY_ISSUES)
}

fn discovery_issue_json(issue: &DiscoveryIssue) -> Value {
    let (message, message_truncated) =
        bounded_utf8(issue.reason, MAX_DISCOVERY_ISSUE_MESSAGE_BYTES);
    json!({
        "provider": issue.provider.as_str(),
        "path": issue.path,
        "code": discovery_issue_code(issue.kind),
        "message": message,
        "message_truncated": message_truncated,
    })
}

fn discovery_issue_code(kind: DiscoveryIssueKind) -> &'static str {
    match kind {
        DiscoveryIssueKind::NoDiskHistory => "no_disk_history",
        DiscoveryIssueKind::SelectorUnreconstructible => "selector_unreconstructible",
        DiscoveryIssueKind::InsufficientOfficialEvidence => "insufficient_official_evidence",
    }
}

fn bounded_utf8(value: &str, maximum_bytes: usize) -> (&str, bool) {
    if value.len() <= maximum_bytes {
        return (value, false);
    }
    let mut end = maximum_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    (&value[..end], true)
}

pub(crate) fn plugin_sources_json(sources: &[HistorySourcePluginSource]) -> Vec<Value> {
    sources
        .iter()
        .map(|source| {
            let report = history_source_plugin_report(source);
            let importable = report.is_importable();
            json!({
                "provider": CaptureProvider::Custom.as_str(),
                "kind": "history_source_plugin",
                "plugin": source.plugin_name,
                "plugin_display_name": source.plugin_display_name,
                "plugin_version": source.plugin_version,
                "history_source": source.history_source(),
                "plugin_source": source.label(),
                "history_source_id": source.id,
                "display_name": source.display_name,
                "provider_key": source.provider_key,
                "source_id": source.source_id,
                "source_format": source.source_format,
                "path": report.durable_path,
                "manifest_path": source.manifest_path,
                "enabled": source.enabled,
                "refresh": history_source_plugin_refresh_json(source.refresh),
                "status": report.status.as_str(),
                "import_support": "history_source_plugin",
                "native_import": false,
                "importable": importable,
                "import_mode": importable.then_some("explicit_source_backed"),
                "provider_source_authority": importable,
                "unsupported_reason": report.unsupported_reason,
            })
        })
        .collect()
}

pub(crate) fn history_source_plugin_report(
    source: &HistorySourcePluginSource,
) -> HistorySourcePluginReport<'_> {
    let Some(durable_path) = source.source_path.as_deref() else {
        return HistorySourcePluginReport {
            durable_path: None,
            status: HistorySourcePluginReportingStatus::Unsupported,
            unsupported_reason: Some(COMMAND_ONLY_UNSUPPORTED_REASON),
        };
    };
    let is_regular_file =
        fs::symlink_metadata(durable_path).is_ok_and(|metadata| metadata.file_type().is_file());
    HistorySourcePluginReport {
        durable_path: Some(durable_path),
        status: if is_regular_file {
            HistorySourcePluginReportingStatus::Available
        } else {
            HistorySourcePluginReportingStatus::Missing
        },
        unsupported_reason: (!is_regular_file).then_some(MISSING_DURABLE_PLUGIN_SOURCE_REASON),
    }
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
                "plugin_source": null,
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

pub(crate) fn home_dir() -> Option<PathBuf> {
    identity::home_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(kind: DiscoveryIssueKind, reason: &'static str) -> DiscoveryIssue {
        DiscoveryIssue {
            provider: CaptureProvider::Claude,
            path: Some(PathBuf::from("relative-provider-root")),
            kind,
            reason,
        }
    }

    #[test]
    fn discovery_issue_codes_are_stable_and_typed() {
        let report = DiscoveryReport {
            sources: Vec::new(),
            issues: vec![
                issue(DiscoveryIssueKind::NoDiskHistory, "memory-only"),
                issue(
                    DiscoveryIssueKind::SelectorUnreconstructible,
                    "unsafe selector",
                ),
                issue(
                    DiscoveryIssueKind::InsufficientOfficialEvidence,
                    "no official location",
                ),
            ],
        };

        let (issues, truncated) = discovery_report_issues_json(&report);

        assert!(!truncated);
        assert_eq!(issues[0]["code"], "no_disk_history");
        assert_eq!(issues[1]["code"], "selector_unreconstructible");
        assert_eq!(issues[2]["code"], "insufficient_official_evidence");
    }

    #[test]
    fn discovery_issue_serialization_bounds_count_and_utf8_message_bytes() {
        let long_reason: &'static str = Box::leak("€".repeat(200).into_boxed_str());
        let report = DiscoveryReport {
            sources: Vec::new(),
            issues: (0..=MAX_DISCOVERY_ISSUES)
                .map(|_| issue(DiscoveryIssueKind::SelectorUnreconstructible, long_reason))
                .collect(),
        };

        let (issues, truncated) = discovery_report_issues_json(&report);

        assert!(truncated);
        assert_eq!(issues.len(), MAX_DISCOVERY_ISSUES);
        for issue in issues {
            let message = issue["message"].as_str().unwrap();
            assert!(message.len() <= MAX_DISCOVERY_ISSUE_MESSAGE_BYTES);
            assert_eq!(issue["message_truncated"], true);
        }
    }
}
