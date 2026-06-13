use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use ctx_storage_admission::StorageGuardObservedPath;

use crate::daemon::DaemonState;

pub(super) async fn sample_storage_disks(
    state: &Arc<DaemonState>,
) -> Vec<ctx_resource_utilization::DiskSnapshot> {
    let mut sampler = state.telemetry.resource_sampler.lock().await;
    let (_system, disks, _cache_age_ms) = sampler.system_snapshot();
    disks
}

pub(super) async fn collect_observed_paths(
    state: &Arc<DaemonState>,
    extra_paths: &[PathBuf],
) -> Vec<StorageGuardObservedPath> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    push_observed_path(
        &mut paths,
        &mut seen,
        "CTX data root",
        state.core.data_root.clone(),
    );
    push_observed_path(&mut paths, &mut seen, "temp storage", std::env::temp_dir());

    for workdir in running_session_workdirs(state).await {
        push_observed_path(&mut paths, &mut seen, "active worktree", workdir);
    }
    for workdir in extra_paths {
        push_observed_path(&mut paths, &mut seen, "active worktree", workdir.clone());
    }
    paths
}

fn push_observed_path(
    paths: &mut Vec<StorageGuardObservedPath>,
    seen: &mut HashSet<PathBuf>,
    label: &'static str,
    path: PathBuf,
) {
    if !seen.insert(path.clone()) {
        return;
    }
    paths.push(StorageGuardObservedPath::new(label, path));
}

async fn running_session_workdirs(state: &Arc<DaemonState>) -> Vec<PathBuf> {
    let mut workdirs = Vec::new();
    for session_id in state.sessions.list_running_sessions().await {
        let Ok(store) = state.store_for_session(session_id).await else {
            continue;
        };
        let Ok(Some(session)) = store.get_session(session_id).await else {
            continue;
        };
        let Ok(Some(worktree)) = store.get_worktree(session.worktree_id).await else {
            continue;
        };
        workdirs.push(PathBuf::from(worktree.root_path));
    }
    workdirs
}
