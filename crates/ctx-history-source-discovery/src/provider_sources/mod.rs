mod configured_roots;
mod context;
mod discovery;
mod lingma;
mod probes;
mod reasons;
mod resolvers;
mod selectors;
mod specs;
mod types;
mod warp;

use std::path::Path;

/// Discovery-only route format for independently owned current CLI stores.
pub const OPENHANDS_CURRENT_CLI_SOURCE_FORMAT: &str = "openhands_cli_file_events";

/// The Cursor-specific result shape discovery needs without taking ownership of
/// Cursor's inventory implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorTranscriptProbeOutcome {
    Found,
    NotFound,
    BudgetExhausted,
    IoError,
}

pub type CursorTranscriptProbe = for<'a> fn(&'a Path) -> CursorTranscriptProbeOutcome;

#[derive(Debug, Clone, Copy)]
pub struct CursorProbeFragment {
    probe: CursorTranscriptProbe,
}

impl CursorProbeFragment {
    pub const fn new(probe: CursorTranscriptProbe) -> Self {
        Self { probe }
    }
}

/// Closed composition seam for the provider implementation that cannot move
/// into generic discovery. This type intentionally has no default, optional
/// fragments, registration, or dynamic lookup.
#[derive(Debug, Clone, Copy)]
pub struct StaticProviderProbeCatalog {
    cursor: CursorProbeFragment,
}

impl StaticProviderProbeCatalog {
    pub const fn new(cursor: CursorProbeFragment) -> Self {
        Self { cursor }
    }
}

pub use configured_roots::{
    configured_root_capabilities, configured_root_capability, ConfiguredRootCapability,
    ConfiguredRootCapabilityState, ConfiguredRootExpander, ConfiguredRootPathKind,
};
pub use context::{
    DiscoveryContext, DiscoveryPlatform, DiscoveryPlatformDirs, DISCOVERY_ENV_ALLOWLIST,
};
pub use ctx_history_capture_model::{
    ProviderRootDefinition, ProviderRootSet, ProviderRootSetError,
};
pub(crate) use ctx_history_source_io::open_ordinary_file_without_following;
pub use ctx_history_source_io::OrdinaryFileObservation;
#[cfg(test)]
pub(crate) use ctx_history_source_sqlite::{
    fail_next_opened_snapshot_cleanup_for_test, SqliteSourceDirectoryAuthority,
};
pub(crate) use ctx_history_source_sqlite::{
    open_root_handle_sqlite_source_snapshot_with_limits, retain_sqlite_source_directory_authority,
    SqliteSourceAccessError, SqliteSourceReadSnapshot, SqliteSourceSnapshotLimits,
};
pub use discovery::{
    discover_canonical_automatic_provider_sources_with_context, discover_provider_sources,
    discover_provider_sources_for_provider, discover_provider_sources_for_provider_report,
    discover_provider_sources_for_provider_with_context,
    discover_provider_sources_for_provider_with_projects, discover_provider_sources_report,
    discover_provider_sources_with_context, discover_provider_sources_with_context_and_work_budget,
    discover_provider_sources_with_projects, provider_source_for_path,
    provider_source_for_path_with_data_root, validate_provider_source_roots_outside_data_root,
    ProviderSourceRootBoundaryError,
};
pub use lingma::{
    discover_lingma_inventory_with_authority, resolve_lingma_discovery_authority,
    resolve_lingma_released_identity_authority, DiscoveredLingmaDatabase,
    LingmaDatabaseCatalogLineage, LingmaDiscoveredInventory, LingmaDiscoveryUnavailable,
    LingmaInventorySelector, LingmaVscodeClient, LingmaVscodeProfile,
};

pub use resolvers::PathPresence;
pub use resolvers::{
    path_presence, provider_paths_equivalent, provider_source_belongs_to_configured_root,
    released_provider_home, resolve_crush_released_project_inventories,
    resolve_crush_released_project_inventory, resolve_openhands_conversations_root,
    CrushDiscoveredProjectInventory, CrushProjectInventorySelector,
    CrushProjectInventorySelectorError, CrushReleasedProjectInventory,
};
pub use specs::{provider_source_spec, provider_source_specs};
pub use types::{
    provider_source_status_reason, DiscoveryIssue, DiscoveryIssueKind, DiscoveryReport,
    ProviderCatalogSupport, ProviderDefaultLocation, ProviderImportSupport, ProviderSource,
    ProviderSourceKind, ProviderSourceRouteProvenance, ProviderSourceSpec, ProviderSourceStatus,
    ProviderSourceStatusReason,
};
pub use warp::{
    discover_warp_sources_with_authority, resolve_warp_discovery_authority,
    resolve_warp_released_identity_authority, DiscoveredWarpSource, WarpDiscoveryUnavailable,
    WarpInstalledPlatform, WarpInstalledSurfaceKey, WarpReleaseChannel, WarpTerminalSurface,
};

#[cfg(test)]
pub(crate) const TEST_PROVIDER_PROBES: StaticProviderProbeCatalog =
    StaticProviderProbeCatalog::new(CursorProbeFragment::new(test_cursor_transcript_probe));

#[cfg(test)]
fn test_cursor_transcript_probe(path: &Path) -> CursorTranscriptProbeOutcome {
    const MAX_DIRECTORY_ENTRIES: usize = 1_024;
    const MAX_TRAVERSAL_ENTRIES: usize = 4_096;

    fn is_valid_transcript(projects: &Path, candidate: &Path) -> bool {
        let Ok(relative) = candidate.strip_prefix(projects) else {
            return false;
        };
        let components = relative.components().collect::<Vec<_>>();
        if components.len() != 4
            || components[1].as_os_str() != "agent-transcripts"
            || candidate
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("jsonl")
        {
            return false;
        }
        let Some(session) = components[2].as_os_str().to_str() else {
            return false;
        };
        !session.trim().is_empty()
            && candidate.file_stem().and_then(|name| name.to_str()) == Some(session)
    }

    fn selected_projects_root(path: &Path) -> std::path::PathBuf {
        if path.file_name().and_then(|name| name.to_str()) == Some(".cursor") {
            return path.join("projects");
        }
        path.ancestors()
            .find(|candidate| {
                candidate.file_name().and_then(|name| name.to_str()) == Some("projects")
            })
            .unwrap_or(path)
            .to_path_buf()
    }

    fn scan(
        path: &Path,
        projects: &Path,
        entries: &mut usize,
    ) -> Result<bool, CursorTranscriptProbeOutcome> {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                CursorTranscriptProbeOutcome::NotFound
            } else {
                CursorTranscriptProbeOutcome::IoError
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(CursorTranscriptProbeOutcome::IoError);
        }
        if metadata.is_file() {
            return Ok(is_valid_transcript(projects, path));
        }
        if !metadata.is_dir() {
            return Ok(false);
        }
        let entries_in_directory = std::fs::read_dir(path)
            .map_err(|_| CursorTranscriptProbeOutcome::IoError)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| CursorTranscriptProbeOutcome::IoError)?;
        if entries_in_directory.len() > MAX_DIRECTORY_ENTRIES {
            return Err(CursorTranscriptProbeOutcome::BudgetExhausted);
        }
        for entry in entries_in_directory {
            *entries = entries.saturating_add(1);
            if *entries > MAX_TRAVERSAL_ENTRIES {
                return Err(CursorTranscriptProbeOutcome::BudgetExhausted);
            }
            if scan(&entry.path(), projects, entries)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    let projects = selected_projects_root(path);
    let mut entries = 0;
    match scan(&projects, &projects, &mut entries) {
        Ok(true) => CursorTranscriptProbeOutcome::Found,
        Ok(false) => CursorTranscriptProbeOutcome::NotFound,
        Err(outcome) => outcome,
    }
}

#[cfg(test)]
mod tests;
