use chrono::{Duration as ChronoDuration, Utc};
use ctx_core::ids::{MessageId, SessionId, TaskId, TurnId, WorkspaceId};
use ctx_core::models::{
    Message, MessageDelivery, MessageRole, SessionEvent, SessionEventType, SessionTurn,
    SessionTurnStatus, SessionTurnTool,
};
use serde_json::{json, Value};

use super::TestDaemon;

pub struct ReplayProjectionActiveCaseSeed {
    pub workspace_id: WorkspaceId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub turn_id: TurnId,
    pub user_message_id: MessageId,
    pub assistant_message_id: MessageId,
    pub user_content: String,
    pub assistant_content: String,
    pub tool_call_id: String,
    pub tool_title: String,
    pub tool_kind: String,
    pub tool_input: Value,
    pub tool_output: String,
    pub stream_assistant_chunk: String,
}

pub struct ReplayProjectionGapCaseSeed {
    pub workspace_id: WorkspaceId,
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub turn_id: TurnId,
    pub user_message_id: MessageId,
    pub assistant_message_id: MessageId,
    pub user_content: String,
    pub assistant_content: String,
    pub notice_count: usize,
}

pub struct ReplayProjectionTailSeed {
    pub workspace_id: WorkspaceId,
    pub session_id: SessionId,
    pub event_count: usize,
}

impl TestDaemon {
    pub async fn seed_replay_active_projection_case_for_test(
        &self,
        seed: ReplayProjectionActiveCaseSeed,
    ) -> anyhow::Result<Vec<i64>> {
        let store = self.state.store_for_session(seed.session_id).await?;
        let started_at = Utc::now();
        let tool_at = started_at + ChronoDuration::seconds(1);
        let assistant_at = started_at + ChronoDuration::seconds(2);
        let updated_at = started_at + ChronoDuration::seconds(3);

        store
            .insert_session_turn(SessionTurn {
                turn_id: seed.turn_id,
                session_id: seed.session_id,
                run_id: None,
                user_message_id: Some(seed.user_message_id),
                status: SessionTurnStatus::Running,
                start_seq: Some(1),
                end_seq: None,
                started_at,
                updated_at,
                assistant_partial: None,
                thought_partial: None,
                metrics_json: None,
                failure: None,
                tool_total: 1,
                tool_pending: 0,
                tool_running: 0,
                tool_completed: 1,
                tool_failed: 0,
            })
            .await?;

        store
            .insert_message(Message {
                id: seed.user_message_id,
                session_id: seed.session_id,
                task_id: seed.task_id,
                run_id: None,
                turn_id: Some(seed.turn_id),
                turn_sequence: Some(1),
                order_seq: Some(1),
                role: MessageRole::User,
                content: seed.user_content.clone(),
                attachments: Vec::new(),
                delivery: MessageDelivery::Immediate,
                delivered_at: Some(started_at),
                created_at: started_at,
            })
            .await?;
        store
            .insert_message(Message {
                id: seed.assistant_message_id,
                session_id: seed.session_id,
                task_id: seed.task_id,
                run_id: None,
                turn_id: Some(seed.turn_id),
                turn_sequence: Some(3),
                order_seq: Some(3),
                role: MessageRole::Assistant,
                content: seed.assistant_content.clone(),
                attachments: Vec::new(),
                delivery: MessageDelivery::Immediate,
                delivered_at: Some(assistant_at),
                created_at: assistant_at,
            })
            .await?;

        let durable_specs = [
            (
                SessionEventType::UserMessage,
                json!({
                    "message_id": seed.user_message_id.0,
                    "content": seed.user_content.clone(),
                    "attachments": [],
                    "order_seq": 1,
                }),
            ),
            (
                SessionEventType::ToolCall,
                json!({
                    "tool_call_id": seed.tool_call_id.clone(),
                    "title": seed.tool_title.clone(),
                    "kind": seed.tool_kind.clone(),
                    "input": seed.tool_input.clone(),
                    "order_seq": 2,
                }),
            ),
            (
                SessionEventType::ToolResult,
                json!({
                    "tool_call_id": seed.tool_call_id.clone(),
                    "title": seed.tool_title.clone(),
                    "kind": seed.tool_kind.clone(),
                    "outputText": seed.tool_output.clone(),
                    "order_seq": 2,
                }),
            ),
            (
                SessionEventType::AssistantComplete,
                json!({
                    "message_id": seed.assistant_message_id.0,
                    "content": seed.assistant_content.clone(),
                    "full_content": seed.assistant_content.clone(),
                    "order_seq": 3,
                }),
            ),
        ];

        let mut seqs = Vec::new();
        for (event_type, payload) in durable_specs {
            let event = store
                .append_session_event(
                    seed.session_id,
                    None,
                    Some(seed.turn_id),
                    event_type,
                    payload,
                )
                .await?;
            seqs.push(event.seq);
            self.publish_replay_projection_event_for_test(event).await;
        }

        store
            .update_session_turn_status(
                seed.session_id,
                seed.turn_id,
                SessionTurnStatus::Completed,
                seqs.last().copied(),
                None,
                updated_at,
            )
            .await?;

        let partial = store
            .append_session_event(
                seed.session_id,
                None,
                Some(seed.turn_id),
                SessionEventType::AssistantChunk,
                json!({
                    "content_fragment": seed.stream_assistant_chunk.clone(),
                    "order_seq": 3,
                }),
            )
            .await?;
        self.publish_replay_projection_event_for_test(partial).await;

        store
            .upsert_session_turn_tool(SessionTurnTool {
                session_id: seed.session_id,
                tool_call_id: seed.tool_call_id.clone(),
                turn_id: seed.turn_id,
                tool_kind: Some(seed.tool_kind.clone()),
                provider_tool_name: Some(seed.tool_kind.clone()),
                title: Some(seed.tool_title.clone()),
                subtitle: Some("fixture subtitle".to_string()),
                status: Some("completed".to_string()),
                input_json: Some(seed.tool_input.clone()),
                output_text: Some(seed.tool_output.clone()),
                order_seq: 3,
                first_event_seq: Some(2),
                input_truncated: None,
                input_original_bytes: None,
                output_truncated: None,
                output_original_bytes: None,
                created_at: tool_at,
                updated_at,
            })
            .await?;

        self.refresh_replay_projection_for_test(seed.workspace_id, seed.session_id)
            .await?;
        Ok(seqs)
    }

    pub async fn seed_replay_gap_case_for_test(
        &self,
        seed: ReplayProjectionGapCaseSeed,
    ) -> anyhow::Result<()> {
        let store = self.state.store_for_session(seed.session_id).await?;
        let started_at = Utc::now();
        let assistant_at = started_at + ChronoDuration::seconds(2);
        let updated_at = started_at + ChronoDuration::seconds(3);

        store
            .insert_session_turn(SessionTurn {
                turn_id: seed.turn_id,
                session_id: seed.session_id,
                run_id: None,
                user_message_id: Some(seed.user_message_id),
                status: SessionTurnStatus::Running,
                start_seq: Some(1),
                end_seq: None,
                started_at,
                updated_at,
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

        store
            .insert_message(Message {
                id: seed.user_message_id,
                session_id: seed.session_id,
                task_id: seed.task_id,
                run_id: None,
                turn_id: Some(seed.turn_id),
                turn_sequence: Some(1),
                order_seq: Some(1),
                role: MessageRole::User,
                content: seed.user_content.clone(),
                attachments: Vec::new(),
                delivery: MessageDelivery::Immediate,
                delivered_at: Some(started_at),
                created_at: started_at,
            })
            .await?;
        store
            .insert_message(Message {
                id: seed.assistant_message_id,
                session_id: seed.session_id,
                task_id: seed.task_id,
                run_id: None,
                turn_id: Some(seed.turn_id),
                turn_sequence: Some(3),
                order_seq: Some(3),
                role: MessageRole::Assistant,
                content: seed.assistant_content.clone(),
                attachments: Vec::new(),
                delivery: MessageDelivery::Immediate,
                delivered_at: Some(assistant_at),
                created_at: assistant_at,
            })
            .await?;

        let user_message = store
            .append_session_event(
                seed.session_id,
                None,
                Some(seed.turn_id),
                SessionEventType::UserMessage,
                json!({
                    "message_id": seed.user_message_id.0,
                    "content": seed.user_content.clone(),
                    "attachments": [],
                    "order_seq": 1,
                }),
            )
            .await?;
        self.publish_replay_projection_event_for_test(user_message)
            .await;

        for idx in 0..seed.notice_count {
            let event = store
                .append_session_event(
                    seed.session_id,
                    None,
                    Some(seed.turn_id),
                    SessionEventType::Notice,
                    json!({
                        "message_id": format!("gap-note-{idx}"),
                        "content": format!("note-{idx}"),
                        "order_seq": 2,
                    }),
                )
                .await?;
            self.publish_replay_projection_event_for_test(event).await;
        }

        let assistant_complete = store
            .append_session_event(
                seed.session_id,
                None,
                Some(seed.turn_id),
                SessionEventType::AssistantComplete,
                json!({
                    "message_id": seed.assistant_message_id.0,
                    "content": seed.assistant_content.clone(),
                    "full_content": seed.assistant_content.clone(),
                    "order_seq": 3,
                }),
            )
            .await?;
        let assistant_complete_seq = assistant_complete.seq;
        self.publish_replay_projection_event_for_test(assistant_complete)
            .await;
        store
            .update_session_turn_status(
                seed.session_id,
                seed.turn_id,
                SessionTurnStatus::Completed,
                Some(assistant_complete_seq),
                None,
                updated_at,
            )
            .await?;

        self.refresh_replay_projection_for_test(seed.workspace_id, seed.session_id)
            .await
    }

    pub async fn seed_replay_tail_events_for_test(
        &self,
        seed: ReplayProjectionTailSeed,
    ) -> anyhow::Result<Vec<i64>> {
        let store = self.state.store_for_session(seed.session_id).await?;
        let mut seqs = Vec::with_capacity(seed.event_count);
        for i in 0..seed.event_count {
            let event = store
                .append_session_event(
                    seed.session_id,
                    None,
                    None,
                    SessionEventType::Notice,
                    json!({ "i": i }),
                )
                .await?;
            self.publish_replay_projection_event_for_test(event.clone())
                .await;
            seqs.push(event.seq);
        }

        self.refresh_replay_projection_for_test(seed.workspace_id, seed.session_id)
            .await?;
        Ok(seqs)
    }

    pub async fn clear_replay_projection_head_for_test(&self, session_id: SessionId) {
        self.state
            .workspaces
            .workspace_active_snapshot
            .remove_session_head(session_id)
            .await;
    }

    async fn publish_replay_projection_event_for_test(&self, event: SessionEvent) {
        self.state.session_publication.publish_event(event).await;
    }

    async fn refresh_replay_projection_for_test(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
    ) -> anyhow::Result<()> {
        self.state
            .task_session_cleanup
            .refresh_session_head_cache(session_id)
            .await;
        self.ensure_workspace_active_snapshot_hydrated(workspace_id)
            .await
            .map_err(|err| anyhow::anyhow!("hydrate replay projection fixture: {err:?}"))
    }
}
