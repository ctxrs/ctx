use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};

use ctx_history_capture::{
    discover_provider_sources_for_provider_report,
    discover_provider_sources_for_provider_with_context, discover_provider_sources_report,
    discover_provider_sources_with_context, provider_source_status_reason, DiscoveryContext,
    DiscoveryIssue, DiscoveryIssueKind, DiscoveryReport, ProviderImportSupport,
    ProviderRootDefinition, ProviderSource, ProviderSourceStatus,
};
use ctx_history_core::CaptureProvider;
pub use ctx_history_ingest_application::history_source_plugin_report;
use ctx_history_ingest_application::SourceDiscoveryPort;

use crate::{
    cli_supported_provider, discover_history_source_plugins_with_diagnostics, provider_cli_name,
    HistorySourcePluginManifestFailure, HistorySourcePluginRefresh, HistorySourcePluginSource,
};

pub const MAX_DISCOVERY_ISSUES: usize = 64;
pub const MAX_DISCOVERY_ISSUE_MESSAGE_BYTES: usize = 512;
pub const DEFAULT_VISIBLE_SOURCE_PROVIDERS: &[CaptureProvider] = &[
    CaptureProvider::Claude,
    CaptureProvider::Codex,
    CaptureProvider::Cursor,
    CaptureProvider::Pi,
    CaptureProvider::CopilotCli,
    CaptureProvider::OpenCode,
];
pub type SourceInfo = ProviderSource;

/// Request-scoped native-source discovery. The final transport resolves home
/// once and supplies it as data, keeping identity/config concerns out here.
pub struct CliSourceDiscoveryPort {
    home: Option<PathBuf>,
    data_root: PathBuf,
    automatic_provider_discovery: bool,
    provider_roots: Vec<ProviderRootDefinition>,
}

impl CliSourceDiscoveryPort {
    pub fn new(home: Option<PathBuf>, data_root: PathBuf) -> Self {
        Self {
            home,
            data_root,
            automatic_provider_discovery: true,
            provider_roots: Vec::new(),
        }
    }

    pub fn with_provider_roots(mut self, roots: Vec<ProviderRootDefinition>) -> Self {
        self.provider_roots = roots;
        self
    }

    pub fn with_automatic_provider_discovery(mut self, enabled: bool) -> Self {
        self.automatic_provider_discovery = enabled;
        self
    }
}

impl SourceDiscoveryPort for CliSourceDiscoveryPort {
    fn discover_all(&self) -> Result<DiscoveryReport> {
        Ok(discovered_sources_report_with_data_root_and_provider_roots(
            self.home.as_deref(),
            &self.data_root,
            self.automatic_provider_discovery,
            &self.provider_roots,
        ))
    }

    fn discover_provider(&self, provider: CaptureProvider) -> Result<DiscoveryReport> {
        Ok(
            discovered_sources_for_provider_report_with_data_root_and_provider_roots(
                self.home.as_deref(),
                &self.data_root,
                provider,
                self.automatic_provider_discovery,
                &self.provider_roots,
            ),
        )
    }

    fn provider_selection_guidance(
        &self,
        provider: CaptureProvider,
    ) -> ctx_history_ingest_application::ProviderSelectionGuidance {
        provider_selection_guidance(provider)
    }
}

pub fn provider_selection_guidance(
    provider: CaptureProvider,
) -> ctx_history_ingest_application::ProviderSelectionGuidance {
    ctx_history_ingest_application::ProviderSelectionGuidance {
        display_name: provider_cli_name(provider).to_owned(),
        manual_path_command: manual_path_guidance(provider),
    }
}

pub fn discovered_plugin_sources_json(data_root: &Path) -> Result<Vec<Value>> {
    let plugin_discovery = discover_history_source_plugins_with_diagnostics(data_root, &[])?;
    let mut values = plugin_sources_json(&plugin_discovery.sources);
    values.extend(plugin_manifest_failures_json(&plugin_discovery.failures));
    Ok(values)
}

pub fn discovered_sources_report(home: Option<&Path>) -> DiscoveryReport {
    home.map(discover_provider_sources_report)
        .map(filter_cli_supported_report)
        .unwrap_or_default()
}

pub fn discovered_sources_report_with_data_root(
    home: Option<&Path>,
    data_root: &Path,
) -> DiscoveryReport {
    discovered_sources_report_with_data_root_and_provider_roots(home, data_root, true, &[])
}

pub fn discovered_sources_report_with_data_root_and_provider_roots(
    home: Option<&Path>,
    data_root: &Path,
    automatic_provider_discovery: bool,
    provider_roots: &[ProviderRootDefinition],
) -> DiscoveryReport {
    if home.is_none() && provider_roots.is_empty() {
        return DiscoveryReport::default();
    }
    let context = discovery_context_with_optional_home(
        home,
        data_root,
        automatic_provider_discovery,
        provider_roots,
    );
    filter_cli_supported_report(discover_provider_sources_with_context(&context))
}

pub fn discovered_sources_for_provider_report(
    home: Option<&Path>,
    provider: CaptureProvider,
) -> DiscoveryReport {
    if !cli_supported_provider(provider) {
        return DiscoveryReport::default();
    }
    home.map(|home| discover_provider_sources_for_provider_report(home, provider))
        .unwrap_or_default()
}

pub fn discovered_sources_for_provider_report_with_data_root(
    home: Option<&Path>,
    data_root: &Path,
    provider: CaptureProvider,
) -> DiscoveryReport {
    discovered_sources_for_provider_report_with_data_root_and_provider_roots(
        home,
        data_root,
        provider,
        true,
        &[],
    )
}

pub fn discovered_sources_for_provider_report_with_data_root_and_provider_roots(
    home: Option<&Path>,
    data_root: &Path,
    provider: CaptureProvider,
    automatic_provider_discovery: bool,
    provider_roots: &[ProviderRootDefinition],
) -> DiscoveryReport {
    if !cli_supported_provider(provider) {
        return DiscoveryReport::default();
    }
    if home.is_none() && provider_roots.is_empty() {
        return DiscoveryReport::default();
    }
    let context = discovery_context_with_optional_home(
        home,
        data_root,
        automatic_provider_discovery,
        provider_roots,
    );
    discover_provider_sources_for_provider_with_context(&context, provider)
}

fn discovery_context_with_optional_home(
    home: Option<&Path>,
    data_root: &Path,
    automatic_provider_discovery: bool,
    provider_roots: &[ProviderRootDefinition],
) -> DiscoveryContext {
    let home_available = home.is_some();
    DiscoveryContext::from_process(home.unwrap_or(data_root))
        .with_home_directory_available(home_available)
        .with_data_root(data_root)
        .with_automatic_provider_discovery(automatic_provider_discovery)
        .with_configured_provider_roots(provider_roots.to_vec())
}

pub fn filter_cli_supported_sources(sources: Vec<SourceInfo>) -> Vec<SourceInfo> {
    sources
        .into_iter()
        .filter(|source| cli_supported_provider(source.provider))
        .collect()
}

pub fn filter_cli_supported_report(mut report: DiscoveryReport) -> DiscoveryReport {
    report.sources = filter_cli_supported_sources(report.sources);
    report
        .issues
        .retain(|issue| cli_supported_provider(issue.provider));
    report
}

pub fn manual_path_guidance(provider: CaptureProvider) -> String {
    format!(
        "ctx import --provider {} --path <path>",
        provider_cli_name(provider)
    )
}

pub fn sources_json(sources: &[SourceInfo]) -> Vec<Value> {
    sources
        .iter()
        .map(|source| {
            json!({
                "provider": source.provider.as_str(),
                "path": source.path,
                "exists": source.exists,
                "source_format": source.source_format,
                "status": source.status.as_str(),
                "status_reason": provider_source_status_reason(source).map(|reason| reason.as_str()),
                "import_support": import_support_json(source.import_support),
                "native_import": source.import_support.is_auto_importable(),
                "importable": source.status == ProviderSourceStatus::Available
                    && source.import_support.is_importable(),
                "unsupported_reason": source.unsupported_reason,
            })
        })
        .collect()
}

pub(crate) fn configured_root_for_source<'a>(
    roots: &'a [ProviderRootDefinition],
    source: &SourceInfo,
) -> Option<&'a ProviderRootDefinition> {
    roots
        .iter()
        .find(|root| ctx_history_capture::provider_source_belongs_to_configured_root(root, source))
}

pub(crate) fn sources_json_with_selection(
    sources: &[SourceInfo],
    roots: &[ProviderRootDefinition],
) -> Vec<Value> {
    let mut entries = sources_json(sources);
    enrich_sources_json_with_selection(&mut entries, sources, roots);
    entries
}

pub fn enrich_sources_json_with_selection(
    entries: &mut [Value],
    sources: &[SourceInfo],
    roots: &[ProviderRootDefinition],
) {
    for (entry, source) in entries.iter_mut().zip(sources) {
        entry["selection"] = match configured_root_for_source(roots, source) {
            Some(root) => json!({
                "kind": "configured",
                "root": root.id,
                "group": root.group,
            }),
            None => json!({
                "kind": "automatic",
                "root": null,
                "group": null,
            }),
        };
    }
}

pub fn discovery_report_issues_json(report: &DiscoveryReport) -> (Vec<Value>, bool) {
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

pub fn plugin_sources_json(sources: &[HistorySourcePluginSource]) -> Vec<Value> {
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

pub fn plugin_manifest_failures_json(
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

pub fn history_source_plugin_refresh_json(refresh: HistorySourcePluginRefresh) -> &'static str {
    match refresh {
        HistorySourcePluginRefresh::Manual => "manual",
        HistorySourcePluginRefresh::Auto => "auto",
    }
}

pub fn import_support_json(support: ProviderImportSupport) -> &'static str {
    match support {
        ProviderImportSupport::Native => "native",
        ProviderImportSupport::Explicit => "explicit",
        ProviderImportSupport::Unsupported => "unsupported",
    }
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
            assert!(message.is_char_boundary(message.len()));
            assert_eq!(issue["message_truncated"], true);
        }
    }

    #[test]
    fn native_discovery_does_not_need_plugin_state() {
        let temp = tempfile::tempdir().unwrap();
        let port =
            CliSourceDiscoveryPort::new(Some(temp.path().to_owned()), temp.path().join("ctx-data"));
        let report = port.discover_provider(CaptureProvider::Codex).unwrap();
        assert!(report
            .sources
            .iter()
            .all(|source| source.provider == CaptureProvider::Codex));
    }

    #[test]
    fn absolute_configured_roots_work_without_a_process_home() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("ctx-data");
        let claude_home = temp.path().join("claude-work");
        std::fs::create_dir_all(claude_home.join("projects")).unwrap();
        let roots = vec![ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Claude,
            path: claude_home.clone(),
            group: Some("work".to_owned()),
        }];

        for automatic_provider_discovery in [true, false] {
            let report = discovered_sources_for_provider_report_with_data_root_and_provider_roots(
                None,
                &data_root,
                CaptureProvider::Claude,
                automatic_provider_discovery,
                &roots,
            );

            assert_eq!(report.sources.len(), 1);
            assert_eq!(report.sources[0].path, claude_home.join("projects"));
            assert!(report.issues.is_empty());
        }
    }

    #[test]
    fn hermes_has_importable_application_guidance() {
        let guidance = provider_selection_guidance(CaptureProvider::Hermes);
        assert_eq!(guidance.display_name, "hermes");
        assert_eq!(
            guidance.manual_path_command,
            "ctx import --provider hermes --path <path>"
        );
    }
}
