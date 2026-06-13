use async_trait::async_trait;

use super::*;
use crate::head_projection::{
    activity_from_turn, build_session_summary_delta, derive_message_preview,
    derive_summary_activity, event_context_window, is_session_gap_notice, message_from_event,
    patch_turn_from_event, recompute_turn_tool_counts, resolve_projection_rev_for_stream_delta,
    session_metadata_from_session, should_include_session_metadata_in_head_delta,
    should_refresh_turn_from_store, turn_from_event,
};

impl<SchedulerCommand> SessionRuntime<SchedulerCommand> {
    pub async fn refresh_session_head_cache_with_host<H>(&self, host: &H, session_id: SessionId)
    where
        H: SessionHeadRefreshHost,
    {
        match host.load_active_snapshot_head(session_id).await {
            SessionHeadRefreshLoad::Found(head) => {
                host.update_compact_session_head(*head).await;
            }
            SessionHeadRefreshLoad::Missing => {
                host.remove_session_from_active_head_cache(session_id).await;
            }
            SessionHeadRefreshLoad::Failed { error } => {
                tracing::warn!(
                    session_id = %session_id.0,
                    "active session head cache refresh failed: {error}"
                );
            }
        }
    }

    pub async fn publish_event_with_host<H>(&self, host: &H, event: SessionEvent)
    where
        H: SessionEventPublicationHost,
    {
        let tx = self.get_broadcaster(event.session_id).await;
        let _ = tx.send(event.clone());
        self.publish_session_event_head(event.session_id, event.seq)
            .await;

        if is_session_gap_notice(&event) {
            return;
        }

        let session = match self.cached_session_meta(event.session_id).await {
            Some(session) => session,
            None => {
                let Some(session) = host.load_session(event.session_id).await else {
                    return;
                };
                self.remember_session_meta(&session).await;
                session
            }
        };

        if should_refresh_task_delta_for_event(&event.event_type) {
            self.queue_task_delta_refresh_with_host(
                host.task_delta_refresh_host(),
                session.task_id,
            )
            .await;
        }

        let message = if should_materialize_message(&event.event_type) {
            message_from_event(&event, &session)
        } else {
            None
        };

        let tool_event = is_tool_event(&event.event_type);
        let mut tool_summaries = Vec::new();
        if tool_event {
            if let Some(turn_id) = event.turn_id {
                tool_summaries = host
                    .list_turn_tool_summaries_for_turn(event.session_id, turn_id)
                    .await;
            }
        }

        let mut turn = turn_from_event(&event, message.as_ref());
        let prefers_cached_turn = tool_event
            || event_context_window(&event).is_some()
            || should_refresh_turn_from_store(&event.event_type);
        if turn.is_none() && prefers_cached_turn {
            if let Some(turn_id) = event.turn_id {
                turn = host.cached_turn_for_read(event.session_id, turn_id).await;
            }
        }
        if turn.is_none() && (should_refresh_turn_from_store(&event.event_type) || tool_event) {
            if let Some(turn_id) = event.turn_id {
                turn = host.load_turn(event.session_id, turn_id).await;
            }
        }
        if let Some(turn) = turn.as_mut() {
            patch_turn_from_event(turn, &event);
            if !tool_summaries.is_empty() {
                recompute_turn_tool_counts(turn, &tool_summaries);
            }
        }

        let stream_only = is_stream_only_event(&event.event_type);
        let cached_replay_cursor = if stream_only {
            host.session_replay_cursor(session.workspace_id, event.session_id)
                .await
        } else {
            SessionReplayCursor::default()
        };
        let last_event_seq = if stream_only {
            cached_replay_cursor.last_event_seq
        } else {
            event.seq
        };
        let projection_rev = resolve_projection_rev_for_stream_delta(
            stream_only,
            last_event_seq,
            cached_replay_cursor.projection_rev,
            || async { host.load_projection_rev(event.session_id).await },
        )
        .await;
        let state_rev = last_event_seq;

        let activity =
            derive_summary_activity(&event).or_else(|| turn.as_ref().map(activity_from_turn));

        let mut last_message_at = None;
        let mut last_message_preview = None;
        if let Some(message) = message.as_ref() {
            last_message_at = Some(message.created_at);
            last_message_preview = Some(derive_message_preview(&message.content));
        }

        let summary_delta = build_session_summary_delta(
            &session,
            activity.clone(),
            last_message_at,
            last_message_preview,
            last_event_seq,
            projection_rev,
            state_rev,
        );

        let delta = SessionHeadDelta {
            session_id: event.session_id,
            last_event_seq,
            projection_rev,
            state_rev,
            emitted_at_ms: Some(chrono::Utc::now().timestamp_millis()),
            session: should_include_session_metadata_in_head_delta(&event.event_type)
                .then(|| session_metadata_from_session(&session)),
            activity,
            event: Some(event),
            turn,
            message,
            tool_summaries,
        };
        host.publish_session_head_delta(session.workspace_id, &session, delta, !stream_only)
            .await;

        if let Some(summary_delta) = summary_delta {
            host.publish_session_summary_delta(session.workspace_id, summary_delta)
                .await;
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionReplayCursor {
    pub last_event_seq: i64,
    pub projection_rev: i64,
}

#[async_trait]
pub trait SessionEventPublicationHost: Send + Sync {
    type TaskDeltaRefreshHost: SessionTaskDeltaRefreshHost;

    fn task_delta_refresh_host(&self) -> Arc<Self::TaskDeltaRefreshHost>;

    async fn load_session(&self, session_id: SessionId) -> Option<Session>;

    async fn list_turn_tool_summaries_for_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Vec<SessionTurnToolSummary>;

    async fn cached_turn_for_read(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Option<SessionTurn>;

    async fn load_turn(&self, session_id: SessionId, turn_id: TurnId) -> Option<SessionTurn>;

    async fn session_replay_cursor(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
    ) -> SessionReplayCursor;

    async fn load_projection_rev(&self, session_id: SessionId) -> Option<i64>;

    async fn publish_session_head_delta(
        &self,
        workspace_id: WorkspaceId,
        session: &Session,
        delta: SessionHeadDelta,
        durable: bool,
    );

    async fn publish_session_summary_delta(
        &self,
        workspace_id: WorkspaceId,
        delta: SessionSummaryDelta,
    );
}

#[derive(Debug)]
pub enum SessionHeadRefreshLoad {
    Found(Box<SessionHeadSnapshot>),
    Missing,
    Failed { error: String },
}

#[async_trait]
pub trait SessionHeadRefreshHost: Send + Sync {
    async fn load_active_snapshot_head(&self, session_id: SessionId) -> SessionHeadRefreshLoad;

    async fn update_compact_session_head(&self, head: SessionHeadSnapshot);

    async fn remove_session_from_active_head_cache(&self, session_id: SessionId);
}

fn is_stream_only_event(event_type: &SessionEventType) -> bool {
    matches!(
        event_type,
        SessionEventType::AssistantChunk
            | SessionEventType::ThoughtChunk
            | SessionEventType::ContextWindowUpdate
    )
}

fn should_refresh_task_delta_for_event(event_type: &SessionEventType) -> bool {
    matches!(
        event_type,
        SessionEventType::UserMessage
            | SessionEventType::AssistantMessageInserted
            | SessionEventType::AssistantComplete
            | SessionEventType::Done
            | SessionEventType::TurnQueued
            | SessionEventType::TurnStarted
            | SessionEventType::TurnFinished
            | SessionEventType::TurnInterrupted
            | SessionEventType::MessageQueueAdded
            | SessionEventType::MessageQueueUpdated
            | SessionEventType::MessageQueueRemoved
            | SessionEventType::MessageQueuePromoted
    )
}

fn should_materialize_message(event_type: &SessionEventType) -> bool {
    matches!(
        event_type,
        SessionEventType::UserMessage
            | SessionEventType::AssistantMessageInserted
            | SessionEventType::Notice
    )
}

fn is_tool_event(event_type: &SessionEventType) -> bool {
    matches!(
        event_type,
        SessionEventType::ToolCall
            | SessionEventType::ToolCallUpdate
            | SessionEventType::ToolResult
    )
}
