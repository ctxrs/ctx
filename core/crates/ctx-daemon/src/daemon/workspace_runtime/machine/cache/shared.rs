use super::materialize::{
    collect_sandbox_machine_cache_file_relpaths, materialize_sandbox_machine_cache_file,
};
use super::paths::{sandbox_machine_cache_root, shared_sandbox_machine_cache_root};
use super::*;

#[cfg(test)]
pub(in crate::daemon::workspace_runtime) async fn seed_shared_sandbox_machine_cache(
    data_root: &Path,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    let Some(shared_root) = shared_sandbox_machine_cache_root() else {
        return Ok(());
    };
    let relpaths = collect_sandbox_machine_cache_file_relpaths(&shared_root)?;
    if relpaths.is_empty() {
        return Ok(());
    }
    let local_root = sandbox_machine_cache_root(data_root);
    let mut seeded = 0usize;
    for relpath in relpaths {
        let src = shared_root.join(&relpath);
        let dest = local_root.join(&relpath);
        if dest.exists() {
            continue;
        }
        materialize_sandbox_machine_cache_file(&src, &dest, true).await?;
        seeded += 1;
    }
    if seeded > 0 {
        observe_log(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            HarnessSetupLogLevel::Info,
            &format!(
                "seeded {seeded} sandbox machine cache file(s) from {}",
                shared_root.display()
            ),
        );
    }
    Ok(())
}

#[cfg(test)]
pub(in crate::daemon::workspace_runtime) async fn persist_sandbox_machine_cache_to_shared(
    data_root: &Path,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    let Some(shared_root) = shared_sandbox_machine_cache_root() else {
        return Ok(());
    };
    let local_root = sandbox_machine_cache_root(data_root);
    let relpaths = collect_sandbox_machine_cache_file_relpaths(&local_root)?;
    if relpaths.is_empty() {
        return Ok(());
    }
    let mut persisted = 0usize;
    for relpath in relpaths {
        let src = local_root.join(&relpath);
        let dest = shared_root.join(&relpath);
        if src == dest || dest.exists() {
            continue;
        }
        materialize_sandbox_machine_cache_file(&src, &dest, false).await?;
        persisted += 1;
    }
    if persisted > 0 {
        observe_log(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            HarnessSetupLogLevel::Info,
            &format!(
                "persisted {persisted} sandbox machine cache file(s) into {}",
                shared_root.display()
            ),
        );
    }
    Ok(())
}

#[cfg(test)]
pub(in crate::daemon::workspace_runtime) async fn seed_shared_sandbox_machine_cache_best_effort(
    data_root: &Path,
    observer: Option<&dyn HarnessSetupObserver>,
) {
    if let Err(err) = seed_shared_sandbox_machine_cache(data_root, observer).await {
        observe_log(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            HarnessSetupLogLevel::Warn,
            &format!("failed to seed shared sandbox machine cache: {err:#}"),
        );
        tracing::warn!("failed to seed shared sandbox machine cache: {err:#}");
    }
}

#[cfg(test)]
pub(in crate::daemon::workspace_runtime) async fn persist_sandbox_machine_cache_to_shared_best_effort(
    data_root: &Path,
    observer: Option<&dyn HarnessSetupObserver>,
) {
    if let Err(err) = persist_sandbox_machine_cache_to_shared(data_root, observer).await {
        observe_log(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            HarnessSetupLogLevel::Warn,
            &format!("failed to persist shared sandbox machine cache: {err:#}"),
        );
        tracing::warn!("failed to persist shared sandbox machine cache: {err:#}");
    }
}
