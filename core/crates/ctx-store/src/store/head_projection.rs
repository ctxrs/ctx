use serde::Serialize;

use super::*;

#[derive(Clone, Copy, Debug)]
pub(crate) struct SessionHeadLimits {
    pub turn_limit: usize,
    pub message_limit: usize,
    pub tool_summary_limit: usize,
    pub event_limit: usize,
    pub byte_limit: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionHeadMaterialization {
    pub head_rev: i64,
    pub last_event_seq: i64,
    pub turns: Vec<SessionTurn>,
    pub tool_summaries: Vec<SessionTurnToolSummary>,
    pub events: Vec<SessionEvent>,
    pub messages: Vec<Message>,
    pub has_more_turns: bool,
    pub head_window: SessionHeadWindow,
}

impl SessionHeadMaterialization {
    pub(crate) fn from_head(head: &SessionHead) -> Self {
        Self {
            head_rev: head.projection_rev,
            last_event_seq: head.last_event_seq,
            turns: head.turns.clone(),
            tool_summaries: head.tool_summaries.clone(),
            events: head.events.clone(),
            messages: head.messages.clone(),
            has_more_turns: head.has_more_turns,
            head_window: head.head_window.clone(),
        }
    }

    pub(crate) fn into_session_head(
        self,
        session: Session,
        projection_rev: i64,
        summary_checkpoint: Option<SessionSummaryCheckpoint>,
    ) -> SessionHead {
        let last_status = self.turns.last().map(|t| t.status.clone());
        let has_running_turn = self.turns.iter().any(|turn| {
            matches!(
                turn.status,
                SessionTurnStatus::Starting | SessionTurnStatus::Running
            )
        });
        let activity = derive_activity_from_status(last_status, has_running_turn);
        SessionHead {
            session,
            turns: self.turns,
            tool_summaries: self.tool_summaries,
            events: self.events,
            messages: self.messages,
            last_event_seq: self.last_event_seq,
            projection_rev,
            activity,
            has_more_turns: self.has_more_turns,
            summary_checkpoint,
            head_window: self.head_window,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ActiveSnapshotHeadProjection {
    pub head_rev: i64,
    pub last_event_seq: i64,
    pub turns: Vec<SessionTurn>,
    pub tool_summaries: Vec<SessionTurnToolSummary>,
    pub messages: Vec<Message>,
    pub has_more_turns: bool,
    pub head_window: SessionHeadWindow,
    pub summary_checkpoint: Option<SessionSummaryCheckpoint>,
}

impl ActiveSnapshotHeadProjection {
    pub(crate) fn from_head(head: &SessionHead) -> Self {
        let mut tool_summaries = head.tool_summaries.clone();
        let mut head_window = head.head_window.clone();
        if tool_summaries.len() > ACTIVE_SNAPSHOT_TOOL_SUMMARY_LIMIT {
            tool_summaries.sort_by(compare_tool_summary_order);
            tool_summaries =
                tool_summaries.split_off(tool_summaries.len() - ACTIVE_SNAPSHOT_TOOL_SUMMARY_LIMIT);
            head_window.truncated = true;
        }
        head_window.event_limit = 0;
        head_window.event_count = 0;
        head_window.bytes =
            head_window_bytes(&head.turns, &tool_summaries, &[], &head.messages) as i64;
        Self {
            head_rev: head.projection_rev,
            last_event_seq: head.last_event_seq,
            turns: head.turns.clone(),
            tool_summaries,
            messages: head.messages.clone(),
            has_more_turns: head.has_more_turns,
            head_window,
            summary_checkpoint: head.summary_checkpoint.clone(),
        }
    }

    pub(crate) fn into_session_head(self, session: Session, projection_rev: i64) -> SessionHead {
        let last_status = self.turns.last().map(|t| t.status.clone());
        let has_running_turn = self.turns.iter().any(|turn| {
            matches!(
                turn.status,
                SessionTurnStatus::Starting | SessionTurnStatus::Running
            )
        });
        let activity = derive_activity_from_status(last_status, has_running_turn);
        SessionHead {
            session,
            turns: self.turns,
            tool_summaries: self.tool_summaries,
            events: Vec::new(),
            messages: self.messages,
            last_event_seq: self.last_event_seq,
            projection_rev,
            activity,
            has_more_turns: self.has_more_turns,
            summary_checkpoint: self.summary_checkpoint,
            head_window: self.head_window,
        }
    }
}

#[derive(Serialize)]
struct SessionHeadWindowPayload<'a> {
    turns: &'a [SessionTurn],
    tool_summaries: &'a [SessionTurnToolSummary],
    events: &'a [SessionEvent],
    messages: &'a [Message],
}

pub(crate) fn head_window_bytes(
    turns: &[SessionTurn],
    tool_summaries: &[SessionTurnToolSummary],
    events: &[SessionEvent],
    messages: &[Message],
) -> usize {
    let payload = SessionHeadWindowPayload {
        turns,
        tool_summaries,
        events,
        messages,
    };
    serde_json::to_vec(&payload)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn test_head_with_tool_summaries(count: usize) -> SessionHead {
        let now = Utc::now();
        let session = Session {
            id: SessionId::new(),
            task_id: TaskId::new(),
            workspace_id: WorkspaceId::new(),
            worktree_id: WorktreeId::new(),
            execution_environment: ExecutionEnvironment::Host,
            parent_session_id: None,
            relationship: None,
            provider_id: "codex".to_string(),
            model_id: "gpt-5.4".to_string(),
            reasoning_effort: Some("high".to_string()),
            title: "head".to_string(),
            agent_role: "implementer".to_string(),
            status: SessionStatus::Active,
            provider_session_ref: None,
            created_at: now,
            updated_at: now,
        };
        let turn_id = TurnId::new();
        let tool_summaries = (0..count)
            .map(|idx| SessionTurnToolSummary {
                session_id: session.id,
                tool_call_id: format!("tool-{idx:03}"),
                turn_id,
                tool_kind: Some("exec".to_string()),
                provider_tool_name: None,
                title: Some(format!("Tool {idx}")),
                subtitle: None,
                status: Some("completed".to_string()),
                input_preview: None,
                output_preview: None,
                order_seq: idx as i64,
                first_event_seq: Some(idx as i64),
                input_truncated: None,
                input_original_bytes: None,
                output_truncated: None,
                output_original_bytes: None,
                created_at: now,
                updated_at: now,
            })
            .collect();
        SessionHead {
            session,
            turns: Vec::new(),
            tool_summaries,
            events: Vec::new(),
            messages: Vec::new(),
            last_event_seq: 17,
            projection_rev: 19,
            activity: SessionActivityState::default(),
            has_more_turns: false,
            summary_checkpoint: None,
            head_window: SessionHeadWindow {
                turn_limit: 5,
                message_limit: 200,
                event_limit: 0,
                byte_limit: 1_500_000,
                turn_count: 0,
                message_count: 0,
                event_count: 0,
                bytes: 0,
                truncated: false,
            },
        }
    }

    #[test]
    fn active_snapshot_projection_caps_tool_summaries() {
        let head = test_head_with_tool_summaries(ACTIVE_SNAPSHOT_TOOL_SUMMARY_LIMIT + 5);
        let projection = ActiveSnapshotHeadProjection::from_head(&head);

        assert_eq!(
            projection.tool_summaries.len(),
            ACTIVE_SNAPSHOT_TOOL_SUMMARY_LIMIT
        );
        assert!(projection.head_window.truncated);
        assert_eq!(projection.head_window.event_limit, 0);
        assert_eq!(projection.head_window.event_count, 0);
    }
}
