use std::sync::Arc;

use ctx_core::ids::{SessionId, TurnId, WorktreeId};

use super::super::{SpawnedChild, SubagentSpawnHost};

pub(super) fn spawn_subagent_completion_tasks(
    host: &Arc<SubagentSpawnHost>,
    spawned_children: &[SpawnedChild],
    invocation_id: String,
    tool_call_id: String,
    parent_id: SessionId,
    parent_turn_id: Option<TurnId>,
    parent_worktree_id: WorktreeId,
) {
    for spawned in spawned_children.iter().cloned() {
        host.spawn_subagent_completion_task(
            spawned.child,
            invocation_id.clone(),
            tool_call_id.clone(),
            parent_id,
            parent_turn_id,
            parent_worktree_id,
        );
    }
}
