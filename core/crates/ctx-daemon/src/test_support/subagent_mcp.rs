use std::path::PathBuf;

use anyhow::{anyhow, Context};
use ctx_core::ids::{RunId, SessionId, TaskId, TurnId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    ExecutionEnvironment, MessageDelivery, SandboxBinding, SandboxGuestIdentity, SandboxProfile,
    SandboxSubstrate, Session, SessionEventType, SessionHeadDelta, SessionTurn, SessionTurnStatus,
    SessionTurnTool,
};
use serde_json::{json, Value};

use super::TestDaemon;

pub struct SubagentMcpChildFixture {
    pub session_id: SessionId,
    pub workspace_id: WorkspaceId,
    pub worktree_id: WorktreeId,
    pub task_id: TaskId,
    pub turn_id: Option<TurnId>,
}

pub struct SubagentMcpArchivedHistoryFixture {
    pub child_session_id: SessionId,
    pub turn_id: TurnId,
}

pub struct SubagentMcpChildWorktreeSnapshot {
    pub session_id: SessionId,
    pub workspace_id: WorkspaceId,
    pub worktree_id: WorktreeId,
    pub root_path: PathBuf,
    pub git_branch: Option<String>,
}

pub struct SubagentMcpCleanupSnapshot {
    pub archived: bool,
    pub sandbox_binding_present: bool,
    pub worktree_metadata_present: bool,
    pub workspace_index_present: bool,
}

pub struct SubagentMcpLatestTurnSnapshot {
    pub status: SessionTurnStatus,
    pub message_delivery: Option<MessageDelivery>,
    pub delivered_at_present: bool,
    pub turn_interrupted: bool,
}

impl TestDaemon {
    async fn subagent_mcp_parent_session_for_test(
        &self,
        parent_session_id: SessionId,
    ) -> anyhow::Result<Session> {
        self.state
            .store_for_session(parent_session_id)
            .await?
            .get_session(parent_session_id)
            .await?
            .ok_or_else(|| {
                anyhow!(
                    "missing subagent MCP parent session {}",
                    parent_session_id.0
                )
            })
    }

    async fn seed_subagent_mcp_child_internal_for_test(
        &self,
        parent_session_id: SessionId,
        label: &str,
        turn_status: Option<SessionTurnStatus>,
        metrics_json: Option<Value>,
        seed_tool: bool,
    ) -> anyhow::Result<SubagentMcpChildFixture> {
        let parent = self
            .subagent_mcp_parent_session_for_test(parent_session_id)
            .await?;
        let store = self.state.store_for_session(parent.id).await?;
        let child = store
            .create_session(
                parent.task_id,
                parent.workspace_id,
                parent.worktree_id,
                parent.execution_environment,
                "fake".into(),
                "fake-model".into(),
                "subagent".into(),
                Some(parent.id),
                Some("sub_agent".into()),
                None,
            )
            .await?;
        store
            .update_session_title(child.id, label.to_string())
            .await?;
        self.state
            .global_store()
            .upsert_workspace_session_index(child.id, parent.workspace_id)
            .await?;

        let mut turn_id = None;
        if let Some(status) = turn_status {
            let id = TurnId::new();
            let now = chrono::Utc::now();
            store
                .insert_session_turn(SessionTurn {
                    turn_id: id,
                    session_id: child.id,
                    run_id: Some(RunId::new()),
                    user_message_id: None,
                    status: status.clone(),
                    start_seq: Some(1),
                    end_seq: match status {
                        SessionTurnStatus::Queued
                        | SessionTurnStatus::Starting
                        | SessionTurnStatus::Running => None,
                        _ => Some(2),
                    },
                    started_at: now,
                    updated_at: now,
                    assistant_partial: None,
                    thought_partial: None,
                    metrics_json,
                    failure: None,
                    tool_total: if seed_tool { 1 } else { 0 },
                    tool_pending: 0,
                    tool_running: 0,
                    tool_completed: if seed_tool { 1 } else { 0 },
                    tool_failed: 0,
                })
                .await?;
            if seed_tool {
                store
                    .upsert_session_turn_tool(SessionTurnTool {
                        session_id: child.id,
                        tool_call_id: "archived-tool".to_string(),
                        turn_id: id,
                        tool_kind: Some("execute".to_string()),
                        provider_tool_name: Some("Bash".to_string()),
                        title: Some("Bash".to_string()),
                        subtitle: Some("archived tool".to_string()),
                        status: Some("completed".to_string()),
                        input_json: Some(json!({ "cmd": "echo archived" })),
                        output_text: Some("archived output".to_string()),
                        order_seq: 1,
                        first_event_seq: None,
                        input_truncated: Some(false),
                        input_original_bytes: None,
                        output_truncated: Some(false),
                        output_original_bytes: None,
                        created_at: now,
                        updated_at: now,
                    })
                    .await?;
            }
            turn_id = Some(id);
        }

        Ok(SubagentMcpChildFixture {
            session_id: child.id,
            workspace_id: child.workspace_id,
            worktree_id: child.worktree_id,
            task_id: child.task_id,
            turn_id,
        })
    }

    pub async fn seed_subagent_mcp_existing_label_child_for_test(
        &self,
        parent_session_id: SessionId,
        label: &str,
    ) -> anyhow::Result<SubagentMcpChildFixture> {
        self.seed_subagent_mcp_child_internal_for_test(parent_session_id, label, None, None, false)
            .await
    }

    pub async fn seed_subagent_mcp_archived_history_children_for_test(
        &self,
        parent_session_id: SessionId,
        total_children: usize,
        archived_label: &str,
    ) -> anyhow::Result<SubagentMcpArchivedHistoryFixture> {
        let mut archived = None;
        for idx in 0..total_children {
            let label = if idx == 0 {
                archived_label.to_string()
            } else {
                format!("Child {idx}")
            };
            let child = self
                .seed_subagent_mcp_child_internal_for_test(
                    parent_session_id,
                    &label,
                    if idx == 0 {
                        Some(SessionTurnStatus::Completed)
                    } else {
                        None
                    },
                    None,
                    idx == 0,
                )
                .await?;
            if idx == 0 {
                archived = Some(SubagentMcpArchivedHistoryFixture {
                    child_session_id: child.session_id,
                    turn_id: child
                        .turn_id
                        .ok_or_else(|| anyhow!("archived child missing seeded turn"))?,
                });
            }
        }
        archived.ok_or_else(|| anyhow!("no archived child was seeded"))
    }

    pub async fn seed_subagent_mcp_busy_archive_child_for_test(
        &self,
        parent_session_id: SessionId,
        label: &str,
    ) -> anyhow::Result<SubagentMcpChildFixture> {
        self.seed_subagent_mcp_child_internal_for_test(
            parent_session_id,
            label,
            Some(SessionTurnStatus::Running),
            None,
            false,
        )
        .await
    }

    pub async fn seed_subagent_mcp_context_window_child_for_test(
        &self,
        parent_session_id: SessionId,
        label: &str,
    ) -> anyhow::Result<SubagentMcpChildFixture> {
        self.seed_subagent_mcp_child_internal_for_test(
            parent_session_id,
            label,
            Some(SessionTurnStatus::Completed),
            Some(json!({
                "context_window_tokens": 100,
                "context_tokens_estimate": 40,
                "remaining_tokens_estimate": 60,
                "remaining_fraction": 0.6
            })),
            false,
        )
        .await
    }

    pub async fn seed_subagent_mcp_queued_history_child_for_test(
        &self,
        parent_session_id: SessionId,
        label: &str,
    ) -> anyhow::Result<SubagentMcpChildFixture> {
        let parent = self
            .subagent_mcp_parent_session_for_test(parent_session_id)
            .await?;
        let store = self.state.store_for_session(parent.id).await?;
        let child = self
            .seed_subagent_mcp_child_internal_for_test(parent_session_id, label, None, None, false)
            .await?;
        let now = chrono::Utc::now();
        for (idx, status) in [
            SessionTurnStatus::Completed,
            SessionTurnStatus::Failed,
            SessionTurnStatus::Queued,
        ]
        .into_iter()
        .enumerate()
        {
            store
                .insert_session_turn(SessionTurn {
                    turn_id: TurnId::new(),
                    session_id: child.session_id,
                    run_id: Some(RunId::new()),
                    user_message_id: None,
                    status: status.clone(),
                    start_seq: Some((idx as i64 * 2) + 1),
                    end_seq: if matches!(status, SessionTurnStatus::Queued) {
                        None
                    } else {
                        Some((idx as i64 * 2) + 2)
                    },
                    started_at: now,
                    updated_at: now,
                    assistant_partial: None,
                    thought_partial: None,
                    metrics_json: None,
                    failure: None,
                    tool_total: 0,
                    tool_pending: 0,
                    tool_running: 0,
                    tool_completed: 0,
                    tool_failed: 0,
                })
                .await?;
        }
        Ok(child)
    }

    pub async fn subagent_mcp_child_by_label_for_test(
        &self,
        parent_session_id: SessionId,
        label: &str,
    ) -> anyhow::Result<SubagentMcpChildFixture> {
        let parent = self
            .subagent_mcp_parent_session_for_test(parent_session_id)
            .await?;
        let store = self.state.store_for_session(parent.id).await?;
        let child = store
            .get_subagent_session_by_label(parent.id, label)
            .await?
            .ok_or_else(|| anyhow!("missing subagent child {label}"))?;
        Ok(SubagentMcpChildFixture {
            session_id: child.id,
            workspace_id: child.workspace_id,
            worktree_id: child.worktree_id,
            task_id: child.task_id,
            turn_id: None,
        })
    }

    pub async fn subagent_mcp_child_worktree_snapshot_for_test(
        &self,
        parent_session_id: SessionId,
        label: &str,
    ) -> anyhow::Result<SubagentMcpChildWorktreeSnapshot> {
        let child = self
            .subagent_mcp_child_by_label_for_test(parent_session_id, label)
            .await?;
        let store = self.state.store_for_session(child.session_id).await?;
        let worktree = store
            .get_worktree(child.worktree_id)
            .await?
            .ok_or_else(|| anyhow!("missing child worktree {}", child.worktree_id.0))?;
        Ok(SubagentMcpChildWorktreeSnapshot {
            session_id: child.session_id,
            workspace_id: child.workspace_id,
            worktree_id: child.worktree_id,
            root_path: PathBuf::from(worktree.root_path),
            git_branch: worktree.git_branch,
        })
    }

    pub async fn seed_subagent_mcp_sandbox_binding_for_test(
        &self,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
    ) -> anyhow::Result<()> {
        let store = self.state.store_for_workspace(workspace_id).await?;
        store
            .upsert_sandbox_binding(SandboxBinding {
                worktree_id,
                workspace_id,
                sandbox_instance_id: ctx_core::models::sandbox_instance_id_for_workspace(
                    workspace_id,
                ),
                substrate: SandboxSubstrate::SharedVmContainer,
                guest_identity: SandboxGuestIdentity::linux_container_ubuntu(),
                profile: SandboxProfile::Standard,
                live_workspace_root: "/workspace".to_string(),
                live_worktree_root: format!("/workspace/worktrees/{}", worktree_id.0),
                execution_settings_json: None,
                container_name: None,
                host_materialization_root: None,
                created_at: chrono::Utc::now(),
            })
            .await?;
        Ok(())
    }

    pub async fn subagent_mcp_cleanup_snapshot_for_test(
        &self,
        child_session_id: SessionId,
    ) -> anyhow::Result<SubagentMcpCleanupSnapshot> {
        let store = self.state.store_for_session(child_session_id).await?;
        let child = store
            .get_session(child_session_id)
            .await?
            .ok_or_else(|| anyhow!("missing child session {}", child_session_id.0))?;
        let archived = store.is_archived_subagent_session(child.id).await?;
        let sandbox_binding_present = store
            .get_sandbox_binding(child.worktree_id)
            .await?
            .is_some();
        let worktree_metadata_present = store.get_worktree(child.worktree_id).await?.is_some();
        let workspace_index_present = self
            .state
            .global_store()
            .get_workspace_id_for_worktree(child.worktree_id)
            .await?
            .is_some();
        Ok(SubagentMcpCleanupSnapshot {
            archived,
            sandbox_binding_present,
            worktree_metadata_present,
            workspace_index_present,
        })
    }

    pub async fn subagent_mcp_active_child_count_for_test(
        &self,
        parent_session_id: SessionId,
    ) -> anyhow::Result<usize> {
        let store = self.state.store_for_session(parent_session_id).await?;
        store
            .count_active_subagent_sessions(parent_session_id)
            .await
            .context("count active subagent sessions")
    }

    pub async fn subagent_mcp_latest_turn_snapshot_for_test(
        &self,
        parent_session_id: SessionId,
        label: &str,
    ) -> anyhow::Result<SubagentMcpLatestTurnSnapshot> {
        let child = self
            .subagent_mcp_child_by_label_for_test(parent_session_id, label)
            .await?;
        let store = self.state.store_for_session(child.session_id).await?;
        let turn = store
            .list_session_turns_page_by_seq(child.session_id, None, Some(1))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("missing latest turn for {label}"))?;
        let (message_delivery, delivered_at_present) =
            if let Some(message_id) = turn.user_message_id {
                let message = store
                    .get_message(message_id)
                    .await?
                    .ok_or_else(|| anyhow!("missing latest turn message for {label}"))?;
                (Some(message.delivery), message.delivered_at.is_some())
            } else {
                (None, false)
            };
        let turn_interrupted = store
            .list_session_events_for_turn(child.session_id, turn.turn_id, false)
            .await?
            .iter()
            .any(|event| matches!(event.event_type, SessionEventType::TurnInterrupted));
        Ok(SubagentMcpLatestTurnSnapshot {
            status: turn.status,
            message_delivery,
            delivered_at_present,
            turn_interrupted,
        })
    }

    pub async fn seed_subagent_mcp_worktree_bootstrap_config_for_test(
        &self,
        parent_session_id: SessionId,
        setup_command: String,
    ) -> anyhow::Result<()> {
        let parent = self
            .subagent_mcp_parent_session_for_test(parent_session_id)
            .await?;
        let store = self.state.store_for_session(parent.id).await?;
        ctx_workspace_config::update_worktree_bootstrap_config(
            &store,
            ctx_workspace_config::WorktreeBootstrapConfigUpdate {
                setup_command: Some(setup_command),
                timeout_sec: None,
                wait_for_completion: Some(true),
                cleanup_command: None,
                cleanup_timeout_sec: None,
            },
        )
        .await
        .context("seed subagent MCP worktree bootstrap config")
    }

    pub async fn publish_subagent_mcp_head_delta_for_test(
        &self,
        child_session_id: SessionId,
    ) -> anyhow::Result<()> {
        let store = self.state.store_for_session(child_session_id).await?;
        let child = store
            .get_session(child_session_id)
            .await?
            .ok_or_else(|| anyhow!("missing child session {}", child_session_id.0))?;
        self.publish_session_head_delta(
            &child,
            SessionHeadDelta {
                session_id: child.id,
                last_event_seq: 1,
                projection_rev: 1,
                state_rev: 0,
                emitted_at_ms: None,
                session: None,
                activity: None,
                event: None,
                turn: None,
                message: None,
                tool_summaries: Vec::new(),
            },
            true,
        )
        .await;
        Ok(())
    }

    pub async fn seed_subagent_mcp_child_with_execution_for_test(
        &self,
        parent_session_id: SessionId,
        label: &str,
        execution_environment: ExecutionEnvironment,
    ) -> anyhow::Result<SubagentMcpChildFixture> {
        let parent = self
            .subagent_mcp_parent_session_for_test(parent_session_id)
            .await?;
        let store = self.state.store_for_session(parent.id).await?;
        let child = store
            .create_session(
                parent.task_id,
                parent.workspace_id,
                parent.worktree_id,
                execution_environment,
                "fake".into(),
                "fake-model".into(),
                "subagent".into(),
                Some(parent.id),
                Some("sub_agent".into()),
                None,
            )
            .await?;
        store
            .update_session_title(child.id, label.to_string())
            .await?;
        self.state
            .global_store()
            .upsert_workspace_session_index(child.id, parent.workspace_id)
            .await?;
        Ok(SubagentMcpChildFixture {
            session_id: child.id,
            workspace_id: child.workspace_id,
            worktree_id: child.worktree_id,
            task_id: child.task_id,
            turn_id: None,
        })
    }
}
