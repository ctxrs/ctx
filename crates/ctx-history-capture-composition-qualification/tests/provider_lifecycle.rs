//! Generation-level provider lifecycle qualification.

use std::{fs, path::PathBuf};

use ctx_history_capture_composition::*;
use ctx_history_capture_model::{
    ProviderRootDefinition, ProviderRouteRole, ProviderSourceRouteProvenance,
    RetainedProviderRootAuthority, SourceRouteIdentity,
};
use ctx_history_core::{CaptureProvider, CoreRecord, LiteralFactKind, SourceAnchor, SourceKey};
use ctx_history_index::{AppliedProviderRoot, VerifiedIndex};
use tempfile::tempdir;

#[path = "support/lexical.rs"]
mod lexical_test_support;
use lexical_test_support::{search_event_candidates, search_event_candidates_with_filters};

#[path = "provider_lifecycle/codex_child_independence.rs"]
mod codex_child_independence;
#[path = "provider_lifecycle/compound_root_ownership.rs"]
mod compound_root_ownership;
#[path = "provider_lifecycle/publication_registry.rs"]
mod publication_registry;
#[path = "provider_lifecycle/registry_roots.rs"]
mod registry_roots;
#[path = "provider_lifecycle/released_root_equivalence.rs"]
mod released_root_equivalence;
#[path = "provider_lifecycle/sqlite_selected.rs"]
mod sqlite_selected;

fn has_literal_fact(record: &CoreRecord, kind: LiteralFactKind, value: &str) -> bool {
    record
        .content
        .activity
        .iter()
        .flat_map(|activity| activity.facts.iter())
        .any(|fact| fact.kind == kind && fact.value == value)
}

fn fixture_provider_source_at(
    provider: CaptureProvider,
    source_format: &'static str,
    import_support: ProviderImportSupport,
    path: impl Into<PathBuf>,
) -> ProviderSource {
    ProviderSource {
        provider,
        path: path.into(),
        exists: true,
        source_format,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
        route_provenance: Default::default(),
    }
}

fn fixture_provider_source(
    provider: CaptureProvider,
    source_format: &'static str,
    import_support: ProviderImportSupport,
) -> ProviderSource {
    fixture_provider_source_at(
        provider,
        source_format,
        import_support,
        PathBuf::from(format!("/fixture/{}", provider.as_str())),
    )
}

fn fixture_session_id(source: &SourceKey) -> ctx_history_core::StableEntityId {
    use ctx_history_core::{derive_session_id, NativeSessionKey, SessionIdentityInput, TypedKey};

    let session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("session").unwrap()).unwrap();
    derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "session",
        native_session_key: &session_key,
    })
    .unwrap()
}

fn fixture_executable_route(
    provider: CaptureProvider,
    source_format: &'static str,
    driver: SourceBackedRouteDriver,
) -> SourceBackedRoute {
    SourceBackedRoute::automatic(
        fixture_provider_source(provider, source_format, ProviderImportSupport::Native),
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )
    .unwrap()
}

fn route_coordinator_error(error: SourceBackedCoordinatorError) -> SourceBackedRouteError {
    match error {
        SourceBackedCoordinatorError::CoreEmission(source) => source,
        error => {
            SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, error.to_string())
        }
    }
}

fn test_provider_probes() -> StaticProviderProbeCatalog {
    use ctx_history_source_discovery::{CursorProbeFragment, CursorTranscriptProbeOutcome};

    fn cursor(_: &std::path::Path) -> CursorTranscriptProbeOutcome {
        CursorTranscriptProbeOutcome::NotFound
    }

    StaticProviderProbeCatalog::new(CursorProbeFragment::new(cursor))
}

mod provider_sources {
    use std::path::PathBuf;

    use ctx_history_core::CaptureProvider;

    pub(crate) fn provider_source_for_path(
        provider: CaptureProvider,
        path: PathBuf,
    ) -> ctx_history_capture_composition::ProviderSource {
        ctx_history_source_discovery::provider_source_for_path(
            &super::test_provider_probes(),
            provider,
            path,
        )
    }
}

mod test_support_paths {
    use std::{
        fs, io,
        path::{Path, PathBuf},
    };

    pub(crate) fn tempdir() -> io::Result<tempfile::TempDir> {
        let temp_root = fs::canonicalize(std::env::temp_dir())?;
        tempfile::Builder::new()
            .prefix("ctx-history-capture-provider-lifecycle-")
            .tempdir_in(temp_root)
    }

    pub(crate) fn capture_repo_root() -> PathBuf {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        if manifest.is_absolute() {
            return repo_root_from_manifest(manifest);
        }

        if let Ok(current_dir) = std::env::current_dir() {
            if let Some(path) = manifest_dir_from(&current_dir, &manifest) {
                return repo_root_from_manifest(path);
            }
        }

        if let Ok(current_exe) = std::env::current_exe() {
            for ancestor in current_exe.ancestors() {
                if let Some(path) = manifest_dir_from(ancestor, &manifest) {
                    return repo_root_from_manifest(path);
                }
            }
        }

        repo_root_from_manifest(manifest)
    }

    fn repo_root_from_manifest(manifest: PathBuf) -> PathBuf {
        manifest
            .ancestors()
            .find(|candidate| {
                candidate.join("Cargo.toml").is_file()
                    && candidate.join("tests/fixtures/provider-history").is_dir()
            })
            .unwrap_or_else(|| panic!("locate ctx repository above {}", manifest.display()))
            .to_path_buf()
    }

    fn manifest_dir_from(base: &Path, manifest: &Path) -> Option<PathBuf> {
        let candidate = base.join(manifest);
        if candidate.join("Cargo.toml").is_file() {
            return fs::canonicalize(&candidate).ok().or(Some(candidate));
        }
        None
    }
}

mod provider {
    pub(crate) mod codex {
        pub(crate) use ctx_history_provider_codex::codex::*;
    }

    pub(crate) mod source_backed {
        pub(crate) mod family {
            pub(crate) mod jsonl {
                pub(crate) use ctx_history_provider_runtime::{
                    set_after_jsonl_append_observation_route_binding_hook,
                    set_after_jsonl_semantic_preflight_hook, set_after_standard_zstd_snapshot_hook,
                    set_before_jsonl_terminal_physical_revalidation_hook,
                };
            }
        }
    }
}
