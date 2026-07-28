//! Upgrade decision path for provider-derived canonical rows.
//!
//! A store that ctx 0.25 wrote projected its provider rows under a
//! capture-source identity this binary no longer derives. Re-importing into
//! such a store appends a second copy of every event instead of reconciling,
//! so a superseded store is fenced out of every provider write until its
//! provider projection is re-derived from the provider sources.
//!
//! The ctx store is a reconstructable representation of local provider data,
//! not an archive, so re-derivation is a rebuild, not a row migration.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use ctx_history_core::database_path;
use ctx_history_store::{ProviderProjectionGeneration, Store};

pub(crate) mod disk;
pub(crate) mod rederive;
pub(crate) mod sources;
#[cfg(test)]
mod tests;

pub(crate) use rederive::run_rebuild;

/// Command a user runs to re-derive a superseded provider projection.
pub(crate) const REBUILD_COMMAND: &str = "ctx index rebuild";

/// Working directory for a rebuild in progress. Its name is fixed so that a
/// restart after process death finds and clears the previous attempt.
pub(crate) const STAGING_DIR: &str = ".ctx-provider-rebuild";
/// Lock that admits one rebuild per store across processes.
pub(crate) const REBUILD_LOCK_FILE: &str = ".ctx-provider-rebuild.lock";
/// Where the superseded generation is retired during publication.
pub(crate) const RETIRED_SUFFIX: &str = ".ctx-superseded";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectionState {
    /// No store on disk yet. A fresh install has nothing to decide.
    Uninitialized,
    /// No writable open has reached this store yet, so its generation is not
    /// recorded. Never treated as native.
    Unknown,
    /// Provider rows carry the current NativePath identity.
    Native,
    /// Provider rows predate it and must be re-derived before any import.
    RebuildRequired,
    /// A rebuild is running in another process right now.
    RebuildRunning,
}

impl ProjectionState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Uninitialized => "uninitialized",
            Self::Unknown => "unknown",
            Self::Native => "native",
            Self::RebuildRequired => "rebuild_required",
            Self::RebuildRunning => "rebuild_running",
        }
    }

    pub(crate) const fn blocks_provider_import(self) -> bool {
        matches!(self, Self::RebuildRequired | Self::RebuildRunning)
    }
}

pub(crate) fn staging_root(data_root: &Path) -> PathBuf {
    data_root.join(STAGING_DIR)
}

pub(crate) fn rebuild_lock_path(data_root: &Path) -> PathBuf {
    data_root.join(REBUILD_LOCK_FILE)
}

pub(crate) fn retired_store_path(db_path: &Path) -> PathBuf {
    let mut value = db_path.as_os_str().to_owned();
    value.push(RETIRED_SUFFIX);
    PathBuf::from(value)
}

/// Reads the recorded generation without migrating or writing anything.
///
/// `ctx status` must not mutate the store, and must not stall behind a rebuild
/// that is already running, so this is a read-only observation only.
pub(crate) fn observe(data_root: &Path) -> ProjectionState {
    let db_path = database_path(data_root.to_path_buf());
    if !db_path.exists() {
        return ProjectionState::Uninitialized;
    }
    let Ok(store) = Store::open_read_only(&db_path) else {
        return ProjectionState::Unknown;
    };
    let state = match store.provider_projection_state() {
        Ok(Some(state)) => state,
        _ => return ProjectionState::Unknown,
    };
    if !state.generation.requires_rederivation() {
        return ProjectionState::Native;
    }
    if rederive::rebuild_is_running(data_root) {
        ProjectionState::RebuildRunning
    } else {
        ProjectionState::RebuildRequired
    }
}

pub(crate) fn status_json(data_root: &Path) -> Value {
    let state = observe(data_root);
    let mut report = json!({
        "state": state.as_str(),
        "blocks_import": state.blocks_provider_import(),
    });
    if state.blocks_provider_import() {
        let db_path = database_path(data_root.to_path_buf());
        report["rebuild_command"] = json!(REBUILD_COMMAND);
        report["reason"] = json!(
            "provider history was indexed by an older ctx and must be re-derived before importing"
        );
        if let Ok(headroom) = disk::headroom(&db_path) {
            report["required_bytes"] = json!(headroom.required_bytes);
            report["available_bytes"] = json!(headroom.available_bytes);
            report["sufficient_disk"] = json!(headroom.is_sufficient());
        }
    }
    report
}

/// One line for humans, shown wherever a stall would otherwise be unexplained.
pub(crate) fn pending_notice(state: ProjectionState) -> Option<String> {
    match state {
        ProjectionState::RebuildRequired => Some(format!(
            "ctx: provider history was indexed by an older ctx and must be re-derived \
             before new history can be imported. Run `{REBUILD_COMMAND}`. \
             Searching the existing index keeps working until then."
        )),
        ProjectionState::RebuildRunning => Some(
            "ctx: a provider history rebuild is in progress; imports resume when it completes."
                .to_owned(),
        ),
        _ => None,
    }
}

/// Fence for every provider write into an existing store.
///
/// Fails closed: an unreadable or unrecorded generation is not proof that the
/// rows are addressable, but an existing store always has a recorded
/// generation because a writable open records one before any migration runs.
pub(crate) fn ensure_native_provider_projection(store: &Store) -> Result<()> {
    match store.provider_projection_state()? {
        Some(state) if state.generation == ProviderProjectionGeneration::Superseded => {
            Err(superseded_projection_error())
        }
        _ => Ok(()),
    }
}

pub(crate) fn superseded_projection_error() -> anyhow::Error {
    anyhow!(
        "this ctx index was built by an older ctx whose provider identities this version \
         cannot address; importing into it would index every session a second time. \
         Run `{REBUILD_COMMAND}` to re-derive the provider index from your provider history \
         (your settings, usage data and Pro state are kept). Searching the existing index \
         keeps working until then."
    )
}

/// Refuses provider imports for a store on disk, before any source is read.
pub(crate) fn ensure_native_provider_projection_at(db_path: &Path) -> Result<()> {
    if !db_path.exists() {
        return Ok(());
    }
    // A writable open is what records the generation for a store this binary
    // has never migrated, so the check is deliberately not read-only.
    let store = Store::open(db_path)?;
    ensure_native_provider_projection(&store)
}
