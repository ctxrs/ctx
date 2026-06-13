#[cfg(test)]
use super::*;
#[cfg(test)]
use ctx_bundled_assets as bundled_assets;
#[cfg(test)]
use ctx_harness_setup::{
    observe_log, HarnessSetupLogLevel, HarnessSetupObserver, HarnessSetupPhase,
    ManagedArtifactDownloadReporter, ManagedDownloadAggregate,
};
#[cfg(test)]
use ctx_runtime_assets::{
    acquire_managed_artifact_file_lock, download_managed_artifact, extract_archive_to_dir,
    finalize_managed_artifact_download, managed_artifact_lock_path, managed_artifact_partial_path,
    resolve_single_extracted_root,
};
#[cfg(test)]
use ctx_update_service as updates;
#[cfg(test)]
use sha2::Digest;
#[cfg(test)]
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::OnceLock;
#[cfg(test)]
use tokio::fs;
#[cfg(test)]
use tokio::sync::Mutex;

#[cfg(test)]
const SANDBOX_MACHINE_CACHE_ID: &str = "sandbox-machine";

pub(super) mod archive;
mod cache;
mod cli_runtime;

pub(in crate::daemon::workspace_runtime) use cache::{
    managed_sandbox_machine_cache_path, persist_sandbox_machine_cache_to_shared,
    persist_sandbox_machine_cache_to_shared_best_effort, sandbox_machine_cache_root,
    sandbox_machine_home_root, sandbox_machine_runtime_root, sandbox_machine_temp_root,
    seed_shared_sandbox_machine_cache, seed_shared_sandbox_machine_cache_best_effort,
};
pub(in crate::daemon::workspace_runtime) use cli_runtime::{
    ensure_managed_sandbox_cli_runtime, ensure_managed_sandbox_machine_cache,
};
