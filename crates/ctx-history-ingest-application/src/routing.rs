use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use ctx_history_core::CaptureProvider;
use ctx_history_refresh::{ExplicitSourceCatalogUpsert, RefreshSelection};
use ctx_history_source_discovery::{
    validate_provider_source_roots_outside_data_root, DiscoveryIssueKind, DiscoveryReport,
    ProviderSource, ProviderSourceStatus,
};

use crate::{HistorySourcePluginSource, IngestPublication, SourceStats};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSelectionGuidance {
    pub display_name: String,
    pub manual_path_command: String,
}

/// Coarse discovery boundary. Implementations return one fully assembled
/// snapshot and must not expose per-record callbacks.
pub trait SourceDiscoveryPort {
    fn discover_all(&self) -> Result<DiscoveryReport>;
    fn discover_provider(&self, provider: CaptureProvider) -> Result<DiscoveryReport>;
    fn provider_selection_guidance(&self, provider: CaptureProvider) -> ProviderSelectionGuidance;
}

/// Provider-owned exact-path and plugin admission boundary. Admission is
/// request-scoped: it never registers a durable automatic root.
pub trait CaptureAdmissionPort {
    fn protect_data_root(&mut self, data_root: &Path) -> Result<()>;
    fn explicit_source(
        &self,
        data_root: &Path,
        path: &Path,
        provider: Option<CaptureProvider>,
        custom_jsonl: bool,
    ) -> Result<ProviderSource>;
    fn prepare_plugin(
        &mut self,
        source: &HistorySourcePluginSource,
        reset_cursor: bool,
    ) -> Result<ProviderSource>;
    fn admit_exact(
        &mut self,
        data_root: &Path,
        source: &ProviderSource,
        relocate_from: Option<&Path>,
    ) -> Result<ExplicitSourceCatalogUpsert>;
    fn source_failure_identity(&self, source: &ProviderSource) -> Result<String>;
}

/// Exactly one logical publication request boundary. Implementations own
/// daemon coordination and receipt pin verification; application code supplies
/// no parser, process, or serialization callback.
pub trait IngestRefreshPort {
    fn refresh(
        &mut self,
        data_root: &Path,
        selection: RefreshSelection,
        no_daemon: bool,
    ) -> Result<IngestPublication>;
}

/// Bounded operation-level progress boundary; no source record is permitted to
/// trigger a call through this port.
pub trait IngestProgressPort {
    fn begin(&mut self, total_bytes: u64) -> Result<()>;
    fn catalog_exact(&mut self, source: &ProviderSource, stats: SourceStats) -> Result<()>;
    fn catalog_plugin(&mut self, source: &HistorySourcePluginSource) -> Result<()>;
}

/// Result of the one bounded automatic safety discovery. It is intentionally
/// separate from data-root initialization so callers can reject unsafe roots
/// without creating ctx state.
#[derive(Debug, Clone)]
pub struct AutomaticSourcePreflight {
    pub sources: Vec<ProviderSource>,
}

pub fn automatic_source_preflight(
    discovery: &dyn SourceDiscoveryPort,
    data_root: &Path,
) -> Result<AutomaticSourcePreflight> {
    let snapshot = discovery.discover_all()?;
    validate_provider_source_roots_outside_data_root(data_root, snapshot.sources.iter())?;
    Ok(AutomaticSourcePreflight {
        sources: snapshot.sources,
    })
}

#[derive(Debug, Clone, Default)]
pub struct IngestRequest {
    pub path: Option<PathBuf>,
    pub provider: Option<CaptureProvider>,
    pub custom_jsonl: bool,
    pub history_source: Option<String>,
    pub history_source_manifests: Vec<PathBuf>,
    pub all: bool,
    pub resume: bool,
    pub relocate_from: Option<PathBuf>,
    pub reset_cursor: bool,
    pub no_daemon: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestRoute {
    Automatic,
    ExplicitPath,
    HistorySourcePlugin,
}

pub fn validate_ingest_request(request: &IngestRequest) -> Result<IngestRoute> {
    if request.custom_jsonl && request.path.is_none() {
        return Err(anyhow!(
            "ctx import --input-format requires --path for a source-backed catalog entry"
        ));
    }
    if request.path.is_some() && !request.custom_jsonl && request.provider.is_none() {
        return Err(anyhow!("ctx import --path requires --provider for native provider history; use `ctx import --provider codex --path <path>` or `ctx import --input-format ctx-history-jsonl-v2 --path <file>"));
    }
    if request.history_source.is_some() || !request.history_source_manifests.is_empty() {
        if request.all {
            return Err(anyhow!(
                "the source-backed history plugin route imports one explicitly selected source; use --history-source or a manifest containing exactly one source"
            ));
        }
        Ok(IngestRoute::HistorySourcePlugin)
    } else if request.path.is_some() {
        Ok(IngestRoute::ExplicitPath)
    } else {
        Ok(IngestRoute::Automatic)
    }
}

pub(crate) fn validate_selected_provider(
    discovery: &dyn SourceDiscoveryPort,
    provider: CaptureProvider,
    report: &DiscoveryReport,
) -> Result<()> {
    if report.sources.iter().any(|source| {
        source.status == ProviderSourceStatus::Available && source.import_support.is_importable()
    }) {
        return Ok(());
    }
    let guidance = discovery.provider_selection_guidance(provider);
    if let Some(source) = report
        .sources
        .iter()
        .find(|source| source.status == ProviderSourceStatus::Unsupported)
    {
        return Err(anyhow!(
            "detected unsupported history at {}; current ctx cannot import that path for {}; use `{}`",
            source.path.display(),
            guidance.display_name,
            guidance.manual_path_command
        ));
    }
    if let Some(issue) = report.issues.first() {
        if issue.kind == DiscoveryIssueKind::ConfiguredRootConflict {
            let location = issue
                .path
                .as_deref()
                .map(|path| format!(" at {}", path.display()))
                .unwrap_or_default();
            return Err(anyhow!(
                "{} configured history roots conflict{location}: {}; repair the persisted configuration with `ctx sources remove <name>` or `ctx sources add <name> --provider {} --root <different-path> --replace`; use `[sources] automatic=false` when named roots should replace automatic discovery",
                guidance.display_name,
                issue.reason,
                provider.as_str(),
            ));
        }
        let summary = match issue.kind {
            DiscoveryIssueKind::NoDiskHistory => {
                format!("{} has no disk history selected", guidance.display_name)
            }
            DiscoveryIssueKind::SelectorUnreconstructible => format!(
                "{} automatic history location cannot be safely reconstructed",
                guidance.display_name
            ),
            DiscoveryIssueKind::InsufficientOfficialEvidence => format!(
                "{} has no official automatic history location established",
                guidance.display_name
            ),
            DiscoveryIssueKind::ConfiguredRootMissing => format!(
                "{} configured history root is missing",
                guidance.display_name
            ),
            DiscoveryIssueKind::ConfiguredRootConflict => unreachable!(),
        };
        return Err(anyhow!(
            "{summary}: {}; use `{}`",
            issue.reason,
            guidance.manual_path_command
        ));
    }
    Err(anyhow!(
        "no importable {} history source was discovered; use `{}` to select one",
        guidance.display_name,
        guidance.manual_path_command
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routing_rejects_format_without_path_before_any_port() {
        let request = IngestRequest {
            custom_jsonl: true,
            ..IngestRequest::default()
        };
        assert!(validate_ingest_request(&request).is_err());
    }
    #[test]
    fn plugin_route_wins_over_automatic() {
        let request = IngestRequest {
            history_source: Some("x/y".into()),
            ..IngestRequest::default()
        };
        assert_eq!(
            validate_ingest_request(&request).unwrap(),
            IngestRoute::HistorySourcePlugin
        );
    }
    #[test]
    fn exact_path_remains_one_shot_route() {
        let request = IngestRequest {
            path: Some("history.jsonl".into()),
            provider: Some(CaptureProvider::Codex),
            ..IngestRequest::default()
        };
        assert_eq!(
            validate_ingest_request(&request).unwrap(),
            IngestRoute::ExplicitPath
        );
    }

    #[test]
    fn configured_root_conflicts_recommend_persistent_repairs() {
        struct Conflict;
        impl SourceDiscoveryPort for Conflict {
            fn discover_all(&self) -> Result<DiscoveryReport> {
                unreachable!()
            }
            fn discover_provider(&self, _: CaptureProvider) -> Result<DiscoveryReport> {
                unreachable!()
            }
            fn provider_selection_guidance(&self, _: CaptureProvider) -> ProviderSelectionGuidance {
                ProviderSelectionGuidance {
                    display_name: "claude".to_owned(),
                    manual_path_command: "ctx import --provider claude --path <path>".to_owned(),
                }
            }
        }
        let report = DiscoveryReport {
            sources: Vec::new(),
            issues: vec![ctx_history_capture_model::DiscoveryIssue {
                provider: CaptureProvider::Claude,
                path: Some("/provider/claude".into()),
                kind: DiscoveryIssueKind::ConfiguredRootConflict,
                reason: "distinct configured roots resolve to the same physical provider root",
            }],
        };

        let error = validate_selected_provider(&Conflict, CaptureProvider::Claude, &report)
            .unwrap_err()
            .to_string();

        assert!(error.contains("/provider/claude"), "{error}");
        assert!(error.contains("ctx sources remove <name>"), "{error}");
        assert!(error.contains("--replace"), "{error}");
        assert!(error.contains("[sources] automatic=false"), "{error}");
        assert!(!error.contains("ctx import"), "{error}");
    }

    #[test]
    fn unsafe_roots_fail_before_any_admission_or_refresh_port_exists() {
        struct Unsafe;
        impl SourceDiscoveryPort for Unsafe {
            fn discover_all(&self) -> Result<DiscoveryReport> {
                Ok(DiscoveryReport::default())
            }
            fn discover_provider(&self, _: CaptureProvider) -> Result<DiscoveryReport> {
                unreachable!()
            }
            fn provider_selection_guidance(&self, _: CaptureProvider) -> ProviderSelectionGuidance {
                unreachable!()
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let preflight = automatic_source_preflight(&Unsafe, temp.path()).unwrap();
        assert!(preflight.sources.is_empty());
    }
}
