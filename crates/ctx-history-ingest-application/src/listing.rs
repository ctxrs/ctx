use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;
use ctx_history_core::CaptureProvider;
use ctx_history_source_discovery::{
    provider_source_belongs_to_configured_root, DiscoveryReport, ProviderRootDefinition,
    ProviderSource, ProviderSourceStatus,
};

use crate::{
    discover_history_source_plugins_with_diagnostics, HistorySourcePluginDiscovery,
    HistorySourcePluginSource,
};

#[derive(Debug, Clone)]
pub struct SourceListingRequest {
    pub provider_filter: Option<CaptureProvider>,
    pub show_all: bool,
    /// Persisted roots for this request. They are authoritative for normal
    /// missing-source visibility; the default provider set remains only a
    /// compatibility policy for automatic missing locations.
    pub configured_provider_roots: Vec<ProviderRootDefinition>,
    pub default_visible_missing_providers: Vec<CaptureProvider>,
}

/// One immutable listing snapshot. The caller renders it; this crate neither
/// writes output nor performs a second discovery/manifest walk.
#[derive(Debug, Clone)]
pub struct SourceListing {
    pub discovery: DiscoveryReport,
    pub visible_sources: Vec<ProviderSource>,
    pub plugins: HistorySourcePluginDiscovery,
    pub hidden_missing_sources: usize,
}

pub fn assemble_source_listing(
    discovery: &dyn crate::SourceDiscoveryPort,
    data_root: &Path,
    request: SourceListingRequest,
) -> Result<SourceListing> {
    let report = match request.provider_filter {
        Some(CaptureProvider::Custom) => DiscoveryReport::default(),
        Some(provider) => discovery.discover_provider(provider)?,
        None => discovery.discover_all()?,
    };
    let plugins = if matches!(request.provider_filter, Some(provider) if provider != CaptureProvider::Custom)
    {
        HistorySourcePluginDiscovery::default()
    } else {
        // This is deliberately the sole manifest walk for a listing request.
        discover_history_source_plugins_with_diagnostics(data_root, &[])?
    };
    let show_all = request.show_all || request.provider_filter.is_some();
    // Exact imports are request overlays and never durable automatic roots.
    let visible_sources = report
        .sources
        .iter()
        .filter(|source| {
            source_is_visible(
                source,
                show_all,
                &request.configured_provider_roots,
                &request.default_visible_missing_providers,
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let hidden_missing_sources = report.sources.len().saturating_sub(visible_sources.len());
    Ok(SourceListing {
        discovery: report,
        visible_sources,
        plugins,
        hidden_missing_sources,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistorySourcePluginReportingStatus {
    Available,
    Missing,
    Unsupported,
}
impl HistorySourcePluginReportingStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Missing => "missing",
            Self::Unsupported => "unsupported",
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct HistorySourcePluginReport<'a> {
    pub durable_path: Option<&'a Path>,
    pub status: HistorySourcePluginReportingStatus,
    pub unsupported_reason: Option<&'static str>,
}
impl HistorySourcePluginReport<'_> {
    pub const fn is_importable(self) -> bool {
        matches!(self.status, HistorySourcePluginReportingStatus::Available)
    }
}

pub fn history_source_plugin_report(
    source: &HistorySourcePluginSource,
) -> HistorySourcePluginReport<'_> {
    let Some(path) = source.source_path.as_deref() else {
        return HistorySourcePluginReport {
            durable_path: None,
            status: HistorySourcePluginReportingStatus::Unsupported,
            unsupported_reason: Some(crate::COMMAND_ONLY_UNSUPPORTED_REASON),
        };
    };
    let regular = fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file());
    HistorySourcePluginReport {
        durable_path: Some(path),
        status: if regular {
            HistorySourcePluginReportingStatus::Available
        } else {
            HistorySourcePluginReportingStatus::Missing
        },
        unsupported_reason: (!regular).then_some(
            "the declared provider-owned durable source path is not a regular non-symlink file",
        ),
    }
}

pub fn source_identity(source: &ProviderSource) -> (String, PathBuf, String) {
    (
        source.provider.as_str().to_owned(),
        source.path.clone(),
        source.source_format.to_owned(),
    )
}
pub fn merge_sources(discovered: &mut Vec<ProviderSource>, configured: Vec<ProviderSource>) {
    let mut seen = BTreeSet::new();
    discovered.retain(|source| seen.insert(source_identity(source)));
    discovered.extend(
        configured
            .into_iter()
            .filter(|source| seen.insert(source_identity(source))),
    );
}
pub fn source_is_visible(
    source: &ProviderSource,
    show_all_sources: bool,
    configured_provider_roots: &[ProviderRootDefinition],
    default_visible_missing_providers: &[CaptureProvider],
) -> bool {
    show_all_sources
        || source.route_provenance.configured_root().is_some()
        || configured_provider_roots
            .iter()
            .any(|root| provider_source_belongs_to_configured_root(root, source))
        || source_visible_by_default(source, default_visible_missing_providers)
}
fn source_visible_by_default(
    source: &ProviderSource,
    default_visible_missing_providers: &[CaptureProvider],
) -> bool {
    source.exists
        || source.status == ProviderSourceStatus::Unsupported
        || source.status != ProviderSourceStatus::Missing
        || default_visible_missing_providers.contains(&source.provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_history_capture_model::ProviderRouteRole;
    use ctx_history_source_discovery::{
        ProviderCatalogSupport, ProviderImportSupport, ProviderSourceKind,
        ProviderSourceRouteProvenance,
    };
    use std::cell::Cell;
    struct Port {
        all: DiscoveryReport,
        calls: Cell<usize>,
    }
    impl crate::SourceDiscoveryPort for Port {
        fn discover_all(&self) -> Result<DiscoveryReport> {
            self.calls.set(self.calls.get() + 1);
            Ok(self.all.clone())
        }
        fn discover_provider(&self, _: CaptureProvider) -> Result<DiscoveryReport> {
            self.discover_all()
        }
        fn provider_selection_guidance(
            &self,
            provider: CaptureProvider,
        ) -> crate::ProviderSelectionGuidance {
            crate::ProviderSelectionGuidance {
                display_name: provider.as_str().to_owned(),
                manual_path_command: String::new(),
            }
        }
    }
    fn source(path: &str, status: ProviderSourceStatus) -> ProviderSource {
        ProviderSource {
            provider: CaptureProvider::Codex,
            path: PathBuf::from(path),
            exists: status != ProviderSourceStatus::Missing,
            source_format: "codex",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::Native,
            status,
            unsupported_reason: None,
            route_provenance: Default::default(),
        }
    }
    #[test]
    fn listing_uses_one_discovery_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let port = Port {
            all: DiscoveryReport {
                sources: vec![source("history", ProviderSourceStatus::Available)],
                issues: vec![],
            },
            calls: Cell::new(0),
        };
        let listing = assemble_source_listing(
            &port,
            temp.path(),
            SourceListingRequest {
                provider_filter: None,
                show_all: false,
                configured_provider_roots: vec![],
                default_visible_missing_providers: vec![CaptureProvider::Codex],
            },
        )
        .unwrap();
        assert_eq!(port.calls.get(), 1);
        assert_eq!(listing.visible_sources.len(), 1);
    }
    #[test]
    fn merge_keeps_missing_configured_identity_visible_once() {
        let mut sources = vec![source("same", ProviderSourceStatus::Available)];
        let missing = source("gone", ProviderSourceStatus::Missing);
        merge_sources(
            &mut sources,
            vec![source("same", ProviderSourceStatus::Available), missing],
        );
        assert_eq!(sources.len(), 2);
    }

    #[test]
    fn configured_missing_source_is_visible_without_a_provider_allowlist() {
        let mut missing = source("gone", ProviderSourceStatus::Missing);
        missing.provider = CaptureProvider::Goose;
        let root = ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Goose,
            path: missing.path.clone(),
            group: Some("team".to_owned()),
            kind: None,
        };
        missing.route_provenance = ProviderSourceRouteProvenance::ConfiguredRoot {
            root_id: root.id.clone(),
            root_path: root.path.clone(),
            route_role: ProviderRouteRole::from_static("goose-sessions-database"),
            automatic_route_role: None,
        };

        assert!(source_is_visible(&missing, false, &[root], &[]));
    }

    #[test]
    fn automatic_missing_source_stays_hidden_without_compatibility_policy() {
        let mut missing = source("gone", ProviderSourceStatus::Missing);
        missing.provider = CaptureProvider::Goose;

        assert!(!source_is_visible(&missing, false, &[], &[]));
    }
}
