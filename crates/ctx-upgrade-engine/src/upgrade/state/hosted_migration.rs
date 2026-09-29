//! Completion and recovery of the hosted migration scheduler record.
use super::*;

/// A hosted journal permits retry only of its own interrupted migration.
pub(in crate::upgrade) fn ensure_hosted_install_scheduler_available(
    install_path: &Path,
    hosted_retry: bool,
) -> Result<()> {
    let Some(bytes) = super::super::install::read_stable_file(
        &state_path(install_path),
        "ctx upgrade scheduler state",
        DUE_HINT_STATE_MAX_BYTES,
        super::super::install::StableFileKind::Data,
    )?
    else {
        return Ok(());
    };
    let state: UpgradeState = serde_json::from_slice(&bytes)?;
    let own_retry = hosted_retry
        && state.status == "quiescing"
        && state.attempt_source.as_deref() == Some("hosted_migration")
        && state
            .attempt_id
            .as_deref()
            .is_some_and(is_valid_upgrade_attempt_id);
    if state.schema_version != STATE_SCHEMA_VERSION
        || (is_active_upgrade_status(&state.status) && !own_retry)
    {
        return Err(anyhow!(
            "finish the pending upgrade before changing the installation"
        ));
    }
    // Terminal publication no longer consumes legacy files. A stale daemon
    // restart record is not a second installation fence.
    Ok(())
}

pub(in crate::upgrade) fn finish_hosted_migration_locked(
    lock: &UpgradeLock,
    attempt: &UpgradeAttempt,
) -> Result<()> {
    let mut state = read_state_object(&lock.install_path);
    if !state.is_current(attempt) {
        return Err(anyhow!(
            "hosted migration lost its upgrade attempt identity"
        ));
    }
    state.terminal(attempt, "applied", Duration::ZERO, now_unix_s());
    write_state_object_locked(lock, state)
}

/// Called only by the explicit installer while it owns the installation lock.
/// Released installers could strand quiescing state by attempting migration
/// during uninstall. Recover only after every installation witness is gone.
pub(in crate::upgrade) fn recover_removed_hosted_migration_under_installation_lock(
    install_path: &Path,
) -> Result<()> {
    use super::super::install::{
        install_marker_path, installation_transaction_exists, read_stable_file, StableFileKind,
    };
    use ctx_managed_pair_engine::MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH;

    for path in [
        install_path.to_path_buf(),
        install_marker_path(install_path),
    ] {
        match fs::symlink_metadata(path) {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    super::super::install::ensure_hosted_transaction_inactive_under_installation_lock(
        install_path,
    )?;
    if installation_transaction_exists(install_path)? {
        return Ok(());
    }
    if let Some(root) = super::super::managed_pair::install_root_for_executable(install_path) {
        match fs::symlink_metadata(root.join(MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH)) {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let path = state_path(install_path);
    let Some(bytes) = read_stable_file(
        &path,
        "ctx upgrade scheduler state",
        DUE_HINT_STATE_MAX_BYTES,
        StableFileKind::Data,
    )?
    else {
        return Ok(());
    };
    let mut state: UpgradeState = serde_json::from_slice(&bytes)?;
    if state.schema_version != STATE_SCHEMA_VERSION
        || state.status != "quiescing"
        || state.attempt_source.as_deref() != Some("hosted_migration")
    {
        return Ok(());
    }
    let Some(id) = state
        .attempt_id
        .as_deref()
        .filter(|id| is_valid_upgrade_attempt_id(id))
    else {
        return Ok(());
    };
    let attempt = UpgradeAttempt { id: id.to_owned() };
    state.fail(
        &attempt,
        "hosted migration ended when the installation was removed",
        now_unix_s(),
    );
    atomic_write_json(&path, &serde_json::to_value(state)?)
}
