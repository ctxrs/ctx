use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use ctx_history_core::platform_security::{restrict_private_file, verify_private_file};
use ctx_pro_host_protocol::ProFilesystemLayout;
use serde_json::json;

use super::super::lifecycle_lock::LifecycleLock;
use super::super::{
    default_helper_path, install_marker_path, previous_helper_path, previous_marker_path,
    publish_helper_path, publish_marker_path, replace_file, rollback_helper_stage_path,
    rollback_marker_stage_path, sync_parent_directory, transaction_helper_path,
    transaction_journal_next_path, transaction_journal_path, transaction_marker_path,
};
use super::render::uninstall as render_uninstall_human;
use super::{LocalProDataOutcome, ProDeletionService, UninstallDataDisposition};
use crate::pro::{
    local_deletion::{
        clear_local_pro_initialization_indicator, local_pro_graph_data_exists,
        local_pro_graph_key_cleanup_phase_exists, local_pro_initialization_indicator_exists,
    },
    pending_materialization,
};
use crate::ui::Ui;

pub(super) fn run_uninstall(
    data_root: &Path,
    service: Option<&mut dyn ProDeletionService>,
    disposition: UninstallDataDisposition,
    json_output: bool,
    ui: &mut Ui,
) -> Result<serde_json::Value> {
    let delete_data = disposition == UninstallDataDisposition::Delete;
    let target = default_helper_path(data_root);
    let initial_state = inspect_local_pro_uninstall_state(data_root)?;
    if !initial_state.data_artifact() && !initial_state.lifecycle_lock {
        return emit_uninstall_result(false, LocalProDataOutcome::Absent, json_output, ui);
    }
    let Some(_lifecycle_lock) = LifecycleLock::acquire(&target, false)? else {
        return emit_uninstall_result(false, LocalProDataOutcome::Absent, json_output, ui);
    };
    let state = inspect_local_pro_uninstall_state(data_root)?;
    crate::semantic::cancel_core_finalization_generation_lease(data_root, "Pro was uninstalled")?;
    pending_materialization::clear(data_root)?;
    if !delete_data && state.cleanup_phase {
        bail!(
            "key_store_unavailable: interrupted Pro deletion must be completed with `ctx pro uninstall --delete-data`"
        );
    }
    let helper_removed = if delete_data {
        let helper_removed =
            if state.initialized || state.graph_data || state.helper_files || state.cleanup_phase {
                let service = service.ok_or_else(|| {
                    anyhow::anyhow!("key_store_unavailable: local deletion service is unavailable")
                })?;
                // The public delete-only adapter verifies and removes the exact current
                // graph inventory before destroying its native key record. It does not
                // launch or retain the private helper and remains available after an
                // ordinary uninstall.
                service.delete_graph_data(data_root)?;
                service.delete_commercial_credentials(data_root)?;
                let helper_removed = delete_helper_files(data_root)?;
                service.finish_deletion(data_root)?;
                helper_removed
            } else {
                delete_helper_files(data_root)?
            };
        clear_local_pro_initialization_indicator(data_root)?;
        clear_preserved_data_marker(data_root)?;
        if inspect_local_pro_uninstall_state(data_root)?.data_artifact() {
            bail!("key_store_unavailable: local Pro data deletion could not be verified");
        }
        helper_removed
    } else if state.graph_data {
        write_preserved_data_marker(data_root)?;
        delete_helper_files(data_root)?
    } else {
        clear_preserved_data_marker(data_root)?;
        delete_helper_files(data_root)?
    };
    let data_outcome = if state.graph_data {
        if delete_data {
            LocalProDataOutcome::Deleted
        } else {
            LocalProDataOutcome::Preserved
        }
    } else {
        LocalProDataOutcome::Absent
    };
    emit_uninstall_result(helper_removed, data_outcome, json_output, ui)
}

fn emit_uninstall_result(
    helper_removed: bool,
    data_outcome: LocalProDataOutcome,
    json_output: bool,
    ui: &mut Ui,
) -> Result<serde_json::Value> {
    let value = uninstall_payload(helper_removed, data_outcome);
    if json_output {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        let document = render_uninstall_human(ui.stdout_context(), helper_removed, data_outcome);
        ui.write_stdout(&document)?;
    }
    Ok(value)
}

pub(super) fn uninstall_payload(
    helper_removed: bool,
    data_outcome: LocalProDataOutcome,
) -> serde_json::Value {
    let next_action = match data_outcome {
        LocalProDataOutcome::Deleted => Some(json!({
            "command": "ctx pro",
            "reason": "rebuild_pro_data",
        })),
        LocalProDataOutcome::Preserved => Some(json!({
            "command": "ctx pro",
            "reason": "restore_preserved_pro_data",
        })),
        LocalProDataOutcome::Absent => None,
    };
    json!({
        "schema_version": 1,
        "payload_type": "pro_uninstall",
        "uninstalled": true,
        "helper_removed": helper_removed,
        "local_pro_data": match data_outcome {
            LocalProDataOutcome::Absent => "absent",
            LocalProDataOutcome::Deleted => "deleted",
            LocalProDataOutcome::Preserved => "preserved",
        },
        "canonical_history_preserved": true,
        "next_action": next_action,
    })
}

pub(super) const PRESERVED_DATA_MARKER_CONTENT: &[u8] = b"ctx-local-pro-data-preserved-v1\n";

pub(super) fn preserved_data_marker_is_set(data_root: &Path) -> bool {
    let path = ProFilesystemLayout::new(data_root).preserved_data_marker_path();
    path.symlink_metadata()
        .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        && fs::read(path).is_ok_and(|content| content == PRESERVED_DATA_MARKER_CONTENT)
        && local_pro_graph_data_exists(data_root).unwrap_or(false)
}

fn write_preserved_data_marker(data_root: &Path) -> Result<()> {
    if !local_pro_graph_data_exists(data_root)? {
        bail!("invalid_request: cannot mark absent local Pro data as preserved");
    }
    let path = ProFilesystemLayout::new(data_root).preserved_data_marker_path();
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            verify_private_file(&path).context("verify local Pro data marker")?;
            if fs::read(&path).context("read local Pro data marker")?
                != PRESERVED_DATA_MARKER_CONTENT
            {
                bail!("invalid_request: local Pro data marker has invalid content");
            }
            return Ok(());
        }
        Ok(_) => bail!("invalid_request: local Pro data marker is not a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect local Pro data marker"),
    }
    let staged = path.with_extension("data-preserved.next");
    delete_one_file(&staged)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.mode(0o600);
    }
    let mut file = options
        .open(&staged)
        .context("create local Pro data marker")?;
    file.write_all(PRESERVED_DATA_MARKER_CONTENT)
        .context("write local Pro data marker")?;
    file.sync_all().context("sync local Pro data marker")?;
    restrict_private_file(&staged).context("protect local Pro data marker")?;
    verify_private_file(&staged).context("verify local Pro data marker")?;
    replace_file(&staged, &path).context("publish local Pro data marker")?;
    sync_parent_directory(&path)?;
    Ok(())
}

fn clear_preserved_data_marker(data_root: &Path) -> Result<()> {
    let path = ProFilesystemLayout::new(data_root).preserved_data_marker_path();
    let removed = delete_one_file(&path)?;
    let staged = path.with_extension("data-preserved.next");
    let removed_staged = delete_one_file(&staged)?;
    if removed || removed_staged {
        sync_parent_directory(&path)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct LocalProUninstallState {
    initialized: bool,
    cleanup_phase: bool,
    graph_data: bool,
    helper_files: bool,
    preserved_marker: bool,
    pending_materialization: bool,
    lifecycle_lock: bool,
}

impl LocalProUninstallState {
    const fn data_artifact(self) -> bool {
        self.initialized
            || self.cleanup_phase
            || self.graph_data
            || self.helper_files
            || self.preserved_marker
            || self.pending_materialization
    }
}

fn inspect_local_pro_uninstall_state(data_root: &Path) -> Result<LocalProUninstallState> {
    let layout = ProFilesystemLayout::new(data_root);
    let helper_files =
        helper_file_candidates(data_root)?
            .iter()
            .try_fold(false, |present, path| {
                let exists = regular_file_exists(path, "local Pro helper file")?;
                Ok::<_, anyhow::Error>(present || exists)
            })?;
    Ok(LocalProUninstallState {
        initialized: local_pro_initialization_indicator_exists(data_root)?,
        cleanup_phase: local_pro_graph_key_cleanup_phase_exists(data_root)?,
        graph_data: local_pro_graph_data_exists(data_root)?,
        helper_files,
        preserved_marker: regular_file_exists(
            &layout.preserved_data_marker_path(),
            "local Pro data marker",
        )?,
        pending_materialization: pending_materialization::pending(data_root)?,
        lifecycle_lock: regular_file_exists(&layout.lifecycle_lock_path(), "Pro lifecycle lock")?,
    })
}

fn regular_file_exists(path: &Path, label: &str) -> Result<bool> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => bail!("invalid_request: {label} is not a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspect {label}")),
    }
}

fn delete_helper_files(data_root: &Path) -> Result<bool> {
    let target = default_helper_path(data_root);
    let candidates = helper_file_candidates(data_root)?;
    let mut removed = false;
    for candidate in &candidates {
        removed |= delete_one_file(candidate)?;
    }
    for candidate in &candidates {
        match candidate.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => bail!("invalid_request: failed to verify local Pro helper removal"),
            Err(error) => return Err(error).context("verify local Pro helper removal"),
        }
    }
    if let Some(bin) = target.parent() {
        let _ = fs::remove_dir(bin);
        if let Some(pro) = bin.parent() {
            let _ = fs::remove_dir(pro);
        }
    }
    Ok(removed)
}

fn helper_file_candidates(data_root: &Path) -> Result<[PathBuf; 12]> {
    let target = default_helper_path(data_root);
    Ok([
        target.clone(),
        install_marker_path(&target)?,
        previous_helper_path(&target)?,
        previous_marker_path(&target)?,
        transaction_journal_path(&target)?,
        transaction_journal_next_path(&target)?,
        transaction_helper_path(&target)?,
        transaction_marker_path(&target)?,
        publish_helper_path(&target)?,
        publish_marker_path(&target)?,
        rollback_helper_stage_path(&target)?,
        rollback_marker_stage_path(&target)?,
    ])
}

fn delete_one_file(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(path).context("remove local Pro file")?;
            Ok(true)
        }
        Ok(_) => bail!("invalid_request: a local Pro file path is not a file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("inspect local Pro file"),
    }
}
