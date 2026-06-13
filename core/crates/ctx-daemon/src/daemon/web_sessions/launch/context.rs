use std::path::PathBuf;

use anyhow::Context;
use ctx_core::ids::{SessionId, WorktreeId};
use ctx_core::models::ExecutionEnvironment;
use ctx_settings_service::HostExecutionPolicy;
use ctx_transport_runtime::web_sessions::{
    validate_web_session_host_session, validate_web_session_launch_scope,
};

use super::WebSessionLaunchHost;

#[derive(Debug)]
pub(super) struct WebSessionLaunchContext {
    pub(super) work_dir: Option<PathBuf>,
}

pub(super) async fn resolve_web_session_launch_context(
    host: &WebSessionLaunchHost,
    session_id: Option<SessionId>,
    worktree_id: Option<WorktreeId>,
) -> anyhow::Result<WebSessionLaunchContext> {
    HostExecutionPolicy::current()?
        .validate_execution_environment(ExecutionEnvironment::Host)
        .context("web sessions currently run on the host")?;

    validate_web_session_launch_scope(session_id.is_some(), worktree_id.is_some())?;

    let mut session_worktree_id = None;
    if let Some(session_id) = session_id {
        let (execution_environment, worktree) = host.load_session_launch_target(session_id).await?;
        validate_web_session_host_session(execution_environment)?;
        host.validate_worktree_host_launch(&worktree).await?;
        session_worktree_id = Some(worktree.id);
    }

    if let Some(worktree_id) = worktree_id {
        let worktree = host.load_worktree_launch_target(worktree_id).await?;
        host.validate_worktree_host_launch(&worktree).await?;
        return Ok(WebSessionLaunchContext {
            work_dir: Some(PathBuf::from(worktree.root_path)),
        });
    }

    if let Some(worktree_id) = session_worktree_id {
        let worktree = host.load_worktree_launch_target(worktree_id).await?;
        return Ok(WebSessionLaunchContext {
            work_dir: Some(PathBuf::from(worktree.root_path)),
        });
    }
    Ok(WebSessionLaunchContext { work_dir: None })
}
