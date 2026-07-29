use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_core::database_path;
use ctx_history_store::Store;
use ctx_pro_host_protocol::{
    BlameRequest, BlameTarget, JournalCheckpoint, JournalPosition, MaterializationAuthority,
    ProFilesystemLayout, QuerySnapshotExpectation, SourceManifestReceiptIdentity, StatusResult,
    PROTOCOL_FINGERPRINT,
};

use crate::pro::verified_executable::VerifiedHelperExecutable;

const MAX_GIT_EXECUTABLE_PATH_BYTES: usize = 4 * 1024;

pub(super) fn error_code(error: &anyhow::Error) -> String {
    error
        .to_string()
        .split(':')
        .next()
        .unwrap_or("helper_crashed")
        .to_owned()
}

pub(super) fn current_blame_request(
    data_root: &Path,
    target: BlameTarget,
    limit: u32,
    cursor: Option<String>,
    status: &StatusResult,
) -> Result<BlameRequest> {
    status
        .validate()
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
    if status.authority == MaterializationAuthority::Source {
        let receipt = status.source_receipt.as_ref().ok_or_else(|| {
            anyhow!(
                "source_unavailable: source-backed Pro graph is not ready ({})",
                graph_state_name(status.state)
            )
        })?;
        return Ok(BlameRequest {
            target,
            limit,
            cursor,
            expected_snapshot: QuerySnapshotExpectation::Source {
                receipt: SourceManifestReceiptIdentity::from_receipt(receipt)
                    .map_err(|error| anyhow!("invalid_response: {}", error.message))?,
            },
        });
    }

    let db_path = database_path(data_root.to_path_buf());
    if !db_path.exists() {
        bail!(
            "source_unavailable: ctx store is not initialized at {}; run `ctx setup` or `ctx import` first",
            db_path.display()
        );
    }
    let store = Store::open_read_only(&db_path).with_context(|| {
        format!(
            "source_unavailable: open canonical ctx store {}",
            db_path.display()
        )
    })?;
    let snapshot = store
        .projection_journal_snapshot(None)
        .context("not_materialized: canonical projection journal is not active")?;
    if snapshot.frozen_through.contract_fingerprint != PROTOCOL_FINGERPRINT {
        bail!("protocol_mismatch: canonical projection journal uses a different contract");
    }
    Ok(BlameRequest {
        target,
        limit,
        cursor,
        expected_snapshot: QuerySnapshotExpectation::Journal {
            checkpoint: JournalCheckpoint {
                position: JournalPosition {
                    generation: snapshot.frozen_through.position.generation,
                    sequence: snapshot.frozen_through.position.sequence,
                },
                contract_fingerprint: snapshot.frozen_through.contract_fingerprint,
                cumulative_digest: snapshot.frozen_through.cumulative_digest,
            },
            projection_pending: false,
        },
    })
}

fn graph_state_name(state: ctx_pro_host_protocol::GraphState) -> &'static str {
    use ctx_pro_host_protocol::GraphState;

    match state {
        GraphState::NotMaterialized => "not_materialized",
        GraphState::NeedsRebuild => "needs_rebuild",
        GraphState::Partial => "partial",
        GraphState::NeedsResume => "needs_resume",
        GraphState::Ready => "ready",
    }
}

pub(crate) fn default_helper_path(data_root: &Path) -> PathBuf {
    ProFilesystemLayout::new(data_root).helper_path()
}

pub(super) fn helper_path(data_root: &Path) -> Result<PathBuf> {
    #[cfg(ctx_pro_qualification)]
    if let Some(bundle) =
        crate::pro::qualification_helper::QualificationHelperBundle::from_process_environment(
            crate::pro::commercial_config::selected_channel()?,
        )?
    {
        return Ok(bundle.source_path().to_path_buf());
    }

    #[cfg(any(test, ctx_pro_test_helper))]
    if let Some(value) = env::var_os("CTX_PRO_HELPER") {
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            bail!("pro_not_installed: developer Pro helper must be an absolute path");
        }
        return regular_helper_path(path);
    }

    crate::pro::lifecycle::validated_installed_helper_path(data_root)
}

pub(super) fn helper_executable(data_root: &Path) -> Result<VerifiedHelperExecutable> {
    #[cfg(ctx_pro_qualification)]
    if let Some(bundle) =
        crate::pro::qualification_helper::QualificationHelperBundle::from_process_environment(
            crate::pro::commercial_config::selected_channel()?,
        )?
    {
        return VerifiedHelperExecutable::open_qualification(bundle);
    }

    #[cfg(any(test, ctx_pro_test_helper))]
    if let Some(value) = env::var_os("CTX_PRO_HELPER") {
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            bail!("pro_not_installed: developer Pro helper must be an absolute path");
        }
        return VerifiedHelperExecutable::open_developer(&path);
    }

    crate::pro::lifecycle::validated_installed_helper(data_root)
}

pub(super) fn git_executable() -> Result<PathBuf> {
    let search_path = env::var_os("PATH")
        .ok_or_else(|| anyhow!("repository_unavailable: Git is not available on PATH"))?;
    let current_directory = env::current_dir().context("repository_unavailable: resolve cwd")?;
    for directory in env::split_paths(&search_path) {
        let directory = if directory.is_absolute() {
            directory
        } else {
            current_directory.join(directory)
        };
        for executable_name in git_executable_names() {
            let candidate = directory.join(executable_name);
            let Ok(canonical) = candidate.canonicalize() else {
                continue;
            };
            if canonical.as_os_str().as_encoded_bytes().len() > MAX_GIT_EXECUTABLE_PATH_BYTES {
                continue;
            }
            let Ok(metadata) = fs::symlink_metadata(&canonical) else {
                continue;
            };
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                if metadata.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            return Ok(canonical);
        }
    }
    bail!("repository_unavailable: Git is not available on PATH")
}

#[cfg(windows)]
fn git_executable_names() -> &'static [&'static str] {
    &["git.exe"]
}

#[cfg(not(windows))]
fn git_executable_names() -> &'static [&'static str] {
    &["git"]
}

#[cfg(any(test, ctx_pro_test_helper))]
fn regular_helper_path(path: PathBuf) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("pro_not_installed: no Pro helper at {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("pro_not_installed: Pro helper must be a regular non-symlink file");
    }
    Ok(path)
}
