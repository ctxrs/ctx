use std::path::Path;

use ctx_session_tools::{NormalizedToolEvent, ToolOutputArtifactRef};

pub(in crate::daemon::scheduler::runtime) use ctx_run_scheduler::tool_runtime::cwd_outside_worktree;

pub(super) struct ToolOutputArtifactScope {
    pub(super) session_id: ctx_core::ids::SessionId,
    pub(super) task_id: ctx_core::ids::TaskId,
    pub(super) workspace_id: ctx_core::ids::WorkspaceId,
    pub(super) worktree_id: ctx_core::ids::WorktreeId,
    pub(super) turn_id: ctx_core::ids::TurnId,
}

pub(super) async fn maybe_spool_tool_output(
    spool_enabled: bool,
    spool_dir: &Path,
    store: &ctx_store::Store,
    tool_event: &NormalizedToolEvent,
    scope: ToolOutputArtifactScope,
) -> Option<ToolOutputArtifactRef> {
    if !spool_enabled {
        return None;
    }
    let tool_call_id = tool_event.tool_call_id.as_deref()?;
    let output = tool_event.raw_output_text.as_deref()?;
    if output.trim().is_empty() {
        return None;
    }
    if !tool_event
        .output_preview
        .as_ref()
        .is_some_and(|preview| preview.truncated)
    {
        return None;
    }

    let artifact = match ctx_session_artifacts::spool_tool_output_artifact(
        store,
        spool_dir,
        ctx_session_artifacts::ToolOutputArtifactScope {
            session_id: scope.session_id,
            task_id: scope.task_id,
            workspace_id: scope.workspace_id,
            worktree_id: scope.worktree_id,
            turn_id: scope.turn_id,
        },
        tool_call_id,
        output,
    )
    .await
    {
        Ok(artifact) => artifact,
        Err(err) => {
            tracing::warn!("failed to spool tool output artifact: {err}");
            return None;
        }
    };

    Some(ToolOutputArtifactRef {
        artifact_id: artifact.artifact_id,
        name: artifact.name,
        mime_type: artifact.mime_type,
        bytes: artifact.bytes,
    })
}
