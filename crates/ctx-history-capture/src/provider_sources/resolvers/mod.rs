use std::{
    collections::HashSet,
    io::ErrorKind,
    path::{Path, PathBuf},
};

#[cfg(test)]
use std::fs;

use ctx_history_core::CaptureProvider;

use super::{
    context::DiscoveryContext,
    probes::{default_location_import_probe, BoundedProbe},
    reasons::{
        empty_source_reason, path_presence_unknown_reason, probe_io_error_reason,
        unknown_source_reason,
    },
    selectors::{
        encoded_path_within_limit, source_path_kind, SourcePathError,
        MAX_RENDERED_DIAGNOSTIC_BYTES, MAX_SOURCE_CANDIDATES_PER_PROVIDER,
    },
    types::{
        DiscoveryIssue, DiscoveryIssueKind, DiscoveryReport, ProviderCatalogSupport,
        ProviderDefaultLocation, ProviderImportSupport, ProviderSource, ProviderSourceKind,
        ProviderSourceSpec, ProviderSourceStatus,
    },
};

const UNSUPPORTED_SOURCE_ROOT_REASON: &str =
    "the selected provider path uses an unsupported, non-local, or unsafe source root";

mod config_project;
mod manual_unsupported;
mod platform;
mod profile_project;
mod simple;

pub(crate) use config_project::{
    CrushDiscoveredProjectInventory, CrushProjectInventorySelector,
    CrushProjectInventorySelectorError,
};
pub(super) use platform::{resolve_lingma_with_authority, resolve_warp_with_authority};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResolverGroup {
    Simple,
    Platform,
    ConfigProject,
    ProfileProject,
    ManualUnsupported,
}

/// The result of one no-follow filesystem presence observation.
///
/// Only a proven absent path is [`Missing`](Self::Missing). Unsafe or
/// unqualified roots are [`Unsupported`](Self::Unsupported), while ambiguous
/// inspection failures remain [`Unknown`](Self::Unknown), so a winner selector
/// cannot silently unlock a stale fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PathPresence {
    Missing,
    Present,
    Unsupported,
    Unknown(ErrorKind),
}

impl PathPresence {
    pub(super) fn suppresses_fallback(self) -> bool {
        !matches!(self, Self::Missing)
    }
}

pub(super) fn path_presence(path: &Path) -> PathPresence {
    match source_path_kind(path) {
        Ok(_) => PathPresence::Present,
        Err(SourcePathError::Missing) => PathPresence::Missing,
        Err(SourcePathError::Unsupported) => PathPresence::Unsupported,
        Err(SourcePathError::Unavailable(kind)) => PathPresence::Unknown(kind),
    }
}

pub(super) fn select_current_or_legacy(current: PathBuf, legacy: PathBuf) -> PathBuf {
    match path_presence(&current) {
        PathPresence::Present | PathPresence::Unsupported | PathPresence::Unknown(_) => current,
        PathPresence::Missing => match path_presence(&legacy) {
            PathPresence::Present | PathPresence::Unsupported | PathPresence::Unknown(_) => legacy,
            PathPresence::Missing => current,
        },
    }
}

pub(super) fn resolver_group(provider: CaptureProvider) -> Option<ResolverGroup> {
    match provider {
        CaptureProvider::Codex
        | CaptureProvider::Claude
        | CaptureProvider::OpenCode
        | CaptureProvider::Kilo
        | CaptureProvider::MiMoCode
        | CaptureProvider::Goose
        | CaptureProvider::Continue
        | CaptureProvider::Gemini
        | CaptureProvider::Tabnine
        | CaptureProvider::Cursor
        | CaptureProvider::KimiCodeCli
        | CaptureProvider::Junie
        | CaptureProvider::ForgeCode => Some(ResolverGroup::Simple),
        CaptureProvider::KiroCli
        | CaptureProvider::Warp
        | CaptureProvider::CodeBuddy
        | CaptureProvider::Lingma
        | CaptureProvider::Zed
        | CaptureProvider::CopilotCli
        | CaptureProvider::Trae
        | CaptureProvider::Antigravity
        | CaptureProvider::Windsurf => Some(ResolverGroup::Platform),
        CaptureProvider::Pi
        | CaptureProvider::Crush
        | CaptureProvider::QwenCode
        | CaptureProvider::MistralVibe
        | CaptureProvider::RovoDev
        | CaptureProvider::RooCode => Some(ResolverGroup::ConfigProject),
        CaptureProvider::OpenClaw
        | CaptureProvider::Hermes
        | CaptureProvider::NanoClaw
        | CaptureProvider::AstrBot
        | CaptureProvider::Shelley
        | CaptureProvider::OpenHands => Some(ResolverGroup::ProfileProject),
        CaptureProvider::Qoder
        | CaptureProvider::FactoryAiDroid
        | CaptureProvider::Firebender
        | CaptureProvider::Auggie
        | CaptureProvider::DeepAgents
        | CaptureProvider::Mux
        | CaptureProvider::Cline => Some(ResolverGroup::ManualUnsupported),
        CaptureProvider::Shell
        | CaptureProvider::Git
        | CaptureProvider::Jj
        | CaptureProvider::Gh
        | CaptureProvider::Custom
        | CaptureProvider::Unknown => None,
    }
}

pub(super) fn resolve(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    match resolver_group(spec.provider) {
        Some(ResolverGroup::Simple) => simple::resolve(context, spec),
        Some(ResolverGroup::Platform) => platform::resolve(context, spec),
        Some(ResolverGroup::ConfigProject) => config_project::resolve(context, spec),
        Some(ResolverGroup::ProfileProject) => profile_project::resolve(context, spec),
        Some(ResolverGroup::ManualUnsupported) => manual_unsupported::resolve(context, spec),
        None => DiscoveryReport::default(),
    }
}

pub(super) fn push_source_candidate(
    sources: &mut Vec<ProviderSource>,
    source: ProviderSource,
) -> bool {
    if sources.len() >= MAX_SOURCE_CANDIDATES_PER_PROVIDER
        || !encoded_path_within_limit(&source.path)
    {
        return false;
    }
    sources.push(source);
    true
}

pub(super) fn issue(
    provider: CaptureProvider,
    path: Option<PathBuf>,
    kind: DiscoveryIssueKind,
    reason: &'static str,
) -> DiscoveryIssue {
    let reason = if reason.len() <= MAX_RENDERED_DIAGNOSTIC_BYTES {
        reason
    } else {
        "provider discovery produced an overlong diagnostic; use an exact --path"
    };
    DiscoveryIssue {
        provider,
        path,
        kind,
        reason,
    }
}

pub(super) fn source_from_parts(
    spec: &ProviderSourceSpec,
    path: PathBuf,
    source_format: &'static str,
    source_kind: ProviderSourceKind,
) -> ProviderSource {
    source_from_parts_with_data_root(None, spec, path, source_format, source_kind)
}

pub(super) fn source_from_parts_with_data_root(
    data_root: Option<&Path>,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    source_format: &'static str,
    source_kind: ProviderSourceKind,
) -> ProviderSource {
    let location = ProviderDefaultLocation {
        path_components: &[],
        source_format,
        source_kind,
    };
    source_from_location(data_root, spec, &location, path)
}

pub(super) fn source_from_location(
    data_root: Option<&Path>,
    spec: &ProviderSourceSpec,
    location: &ProviderDefaultLocation,
    path: PathBuf,
) -> ProviderSource {
    let presence = path_presence(&path);
    let exists = !matches!(presence, PathPresence::Missing);
    let (status, unsupported_reason) =
        if matches!(spec.import_support, ProviderImportSupport::Unsupported) {
            (ProviderSourceStatus::Unsupported, spec.unsupported_reason)
        } else {
            match presence {
                PathPresence::Missing => (ProviderSourceStatus::Missing, spec.unsupported_reason),
                PathPresence::Unknown(kind) => (
                    ProviderSourceStatus::Unknown,
                    Some(path_presence_unknown_reason(kind)),
                ),
                PathPresence::Unsupported => (
                    ProviderSourceStatus::Unsupported,
                    Some(UNSUPPORTED_SOURCE_ROOT_REASON),
                ),
                PathPresence::Present => {
                    match default_location_import_probe(data_root, spec.provider, location, &path) {
                        BoundedProbe::Found => {
                            (ProviderSourceStatus::Available, spec.unsupported_reason)
                        }
                        BoundedProbe::NotFound => (
                            ProviderSourceStatus::Empty,
                            empty_source_reason(spec.provider),
                        ),
                        BoundedProbe::BudgetExhausted => (
                            ProviderSourceStatus::Unknown,
                            unknown_source_reason(spec.provider),
                        ),
                        BoundedProbe::IoError => (
                            ProviderSourceStatus::Unknown,
                            probe_io_error_reason(spec.provider),
                        ),
                    }
                }
            }
        };
    ProviderSource {
        provider: spec.provider,
        path,
        exists,
        source_format: location.source_format,
        source_kind: location.source_kind,
        import_support: spec.import_support,
        catalog_support: spec.catalog_support,
        status,
        unsupported_reason,
    }
}

pub(super) fn unsupported_source(
    spec: &ProviderSourceSpec,
    path: PathBuf,
    reason: &'static str,
) -> ProviderSource {
    ProviderSource {
        provider: spec.provider,
        exists: path_presence(&path).suppresses_fallback(),
        path,
        source_format: "unsupported",
        source_kind: ProviderSourceKind::DetectionOnly,
        import_support: ProviderImportSupport::Unsupported,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Unsupported,
        unsupported_reason: Some(if reason.len() <= MAX_RENDERED_DIAGNOSTIC_BYTES {
            reason
        } else {
            "detected provider history uses an unsupported format"
        }),
    }
}

pub(super) fn dedupe_report(mut report: DiscoveryReport) -> DiscoveryReport {
    let mut seen = HashSet::new();
    report.sources.retain(|source| {
        let path = comparison_path(&source.path).unwrap_or_else(|| source.path.clone());
        seen.insert((source.provider, path, source.source_format))
    });
    report
}

fn comparison_path(path: &Path) -> Option<PathBuf> {
    source_path_kind(path).ok()?;
    Some(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_source_specs;

    fn spec(provider: CaptureProvider) -> &'static ProviderSourceSpec {
        provider_source_specs()
            .iter()
            .find(|spec| spec.provider == provider)
            .expect("registered provider")
    }

    #[test]
    fn path_presence_distinguishes_missing_present_and_malformed_parents() {
        let temp = tempfile::tempdir().unwrap();
        let present = temp.path().join("present");
        fs::write(&present, b"present").unwrap();
        assert_eq!(path_presence(&present), PathPresence::Present);
        assert_eq!(
            path_presence(&temp.path().join("missing")),
            PathPresence::Missing
        );
        assert_eq!(
            path_presence(&present.join("child")),
            PathPresence::Unknown(ErrorKind::NotADirectory)
        );
    }

    #[cfg(unix)]
    #[test]
    fn inaccessible_parent_is_unknown_and_does_not_expose_os_diagnostics() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("locked");
        fs::create_dir(&parent).unwrap();
        let original = fs::metadata(&parent).unwrap().permissions();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o000)).unwrap();
        let presence = path_presence(&parent.join("winner"));
        let source = source_from_parts(
            spec(CaptureProvider::Kilo),
            parent.join("winner"),
            "kilo_sqlite",
            ProviderSourceKind::NativeHistory,
        );
        fs::set_permissions(&parent, original).unwrap();

        assert_eq!(presence, PathPresence::Unknown(ErrorKind::PermissionDenied));
        assert_eq!(source.status, ProviderSourceStatus::Unknown);
        let diagnostic = source.unsupported_reason.expect("bounded diagnostic");
        assert!(diagnostic.len() <= MAX_RENDERED_DIAGNOSTIC_BYTES);
        assert!(!diagnostic.contains(parent.to_string_lossy().as_ref()));
    }

    #[cfg(unix)]
    #[test]
    fn dangling_links_and_symlink_loops_never_unlock_fallbacks() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let dangling = temp.path().join("dangling");
        symlink(temp.path().join("absent-target"), &dangling).unwrap();
        assert_eq!(path_presence(&dangling), PathPresence::Unsupported);
        assert_eq!(
            path_presence(&dangling.join("child")),
            PathPresence::Unsupported
        );

        let loop_link = temp.path().join("loop");
        symlink(&loop_link, &loop_link).unwrap();
        let legacy = temp.path().join("legacy");
        fs::write(&legacy, b"legacy").unwrap();
        assert_eq!(
            select_current_or_legacy(loop_link.join("child"), legacy),
            loop_link.join("child")
        );
    }

    #[cfg(windows)]
    #[test]
    fn reparse_parent_uncertainty_suppresses_legacy_fallback() {
        use std::os::windows::fs::symlink_dir;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let junction = temp.path().join("junction");
        fs::create_dir(&target).unwrap();
        if let Err(error) = symlink_dir(&target, &junction) {
            if matches!(
                error.kind(),
                ErrorKind::PermissionDenied | ErrorKind::Unsupported
            ) {
                return;
            }
            panic!("failed to create Windows directory reparse point: {error}");
        }
        let current = junction.join("missing.db");
        let legacy = temp.path().join("legacy.db");
        fs::write(&legacy, b"legacy").unwrap();

        assert_eq!(path_presence(&current), PathPresence::Unsupported);
        assert_eq!(select_current_or_legacy(current.clone(), legacy), current);
    }

    #[test]
    fn current_legacy_selection_matrix_is_deterministic_and_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let current = temp.path().join("current");
        let legacy = temp.path().join("legacy");

        for _ in 0..32 {
            assert_eq!(
                select_current_or_legacy(current.clone(), legacy.clone()),
                current
            );
        }
        fs::write(&legacy, b"legacy").unwrap();
        assert_eq!(
            select_current_or_legacy(current.clone(), legacy.clone()),
            legacy
        );
        fs::write(&current, b"current").unwrap();
        assert_eq!(
            select_current_or_legacy(current.clone(), legacy.clone()),
            current
        );

        fs::remove_file(&current).unwrap();
        fs::create_dir(&current).unwrap();
        let uncertain_current = current.join("missing").join("child");
        fs::write(current.join("missing"), b"not a directory").unwrap();
        assert_eq!(
            select_current_or_legacy(uncertain_current.clone(), legacy),
            uncertain_current
        );

        let missing_current = temp.path().join("missing-current");
        let malformed_legacy_parent = temp.path().join("malformed-legacy-parent");
        fs::write(&malformed_legacy_parent, b"not a directory").unwrap();
        let uncertain_legacy = malformed_legacy_parent.join("legacy");
        assert_eq!(
            select_current_or_legacy(missing_current, uncertain_legacy.clone()),
            uncertain_legacy
        );
    }

    #[test]
    fn disappearance_after_selection_does_not_reselect_legacy() {
        let temp = tempfile::tempdir().unwrap();
        let current = temp.path().join("kilo.db");
        let legacy = temp.path().join("opencode.db");
        fs::write(&current, b"current").unwrap();
        fs::write(&legacy, b"legacy").unwrap();

        let selected = select_current_or_legacy(current.clone(), legacy);
        fs::remove_file(&current).unwrap();
        let source = source_from_parts(
            spec(CaptureProvider::Kilo),
            selected,
            "kilo_sqlite",
            ProviderSourceKind::NativeHistory,
        );
        assert_eq!(source.path, current);
        assert_eq!(source.status, ProviderSourceStatus::Missing);
    }

    #[test]
    fn every_registered_provider_has_exactly_one_grouped_dispatch_lane() {
        let specs = provider_source_specs();
        assert_eq!(specs.len(), 41);
        assert!(specs
            .iter()
            .all(|spec| resolver_group(spec.provider).is_some()));
        for (group, expected) in [
            (ResolverGroup::Simple, 13),
            (ResolverGroup::Platform, 9),
            (ResolverGroup::ConfigProject, 6),
            (ResolverGroup::ProfileProject, 6),
            (ResolverGroup::ManualUnsupported, 7),
        ] {
            assert_eq!(
                specs
                    .iter()
                    .filter(|spec| resolver_group(spec.provider) == Some(group))
                    .count(),
                expected
            );
        }
    }
}
