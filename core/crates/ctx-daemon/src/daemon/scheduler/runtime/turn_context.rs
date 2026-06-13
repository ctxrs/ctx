use std::path::{Path, PathBuf};

use anyhow::Result;
use ctx_core::models::{ExecutionEnvironment, Session};
use ctx_session_tools::model_resolution::compose_model_id;

use crate::daemon::scheduler::host::TurnRuntimeHost;

pub(super) struct TurnRuntimeContext {
    pub(super) store: ctx_store::Store,
    pub(super) workdir_root: PathBuf,
    pub(super) workdir_canonical: Option<PathBuf>,
    pub(super) workdir_str: String,
    pub(super) execution_environment: ExecutionEnvironment,
    pub(super) full_model_id: String,
}

pub(super) async fn prepare_turn_runtime_context(
    host: &TurnRuntimeHost,
    session: &Session,
    workdir: &Path,
) -> Result<TurnRuntimeContext> {
    host.wait_for_worktree_bootstrap(session.worktree_id).await;
    host.reject_if_update_draining().await?;
    host.preflight_turn_start(workdir).await?;

    let store = host.store_for_session(session.id).await?;
    let workdir_root = workdir.to_path_buf();
    let workdir_canonical = tokio::fs::canonicalize(&workdir_root).await.ok();
    let workdir_str = workdir_root.to_string_lossy().to_string();
    let execution_environment = session.execution_environment;
    let full_model_id = compose_model_id(&session.model_id, session.reasoning_effort.as_deref());

    Ok(TurnRuntimeContext {
        store,
        workdir_root,
        workdir_canonical,
        workdir_str,
        execution_environment,
        full_model_id,
    })
}
