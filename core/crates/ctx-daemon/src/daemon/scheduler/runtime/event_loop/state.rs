use std::collections::HashMap;

use ctx_core::models::{SessionEventType, SessionTurnStatus, SessionTurnTool};

use crate::daemon::scheduler::TurnStartProgress;

use super::TurnEventLoop;

pub(super) struct EventLoopRuntimeState {
    pub(super) assistant_partial: String,
    pub(super) assistant_partial_message_id: Option<String>,
    pub(super) assistant_sequence: i64,
    pub(super) assistant_emitted: String,
    pub(super) thought_partial: String,
    pub(super) tool_cache: HashMap<String, SessionTurnTool>,
    pub(super) terminal_status: Option<SessionTurnStatus>,
    start_progress: TurnStartProgress,
    first_event_seen: bool,
    pub(super) telemetry_emitted: bool,
}

impl Default for EventLoopRuntimeState {
    fn default() -> Self {
        Self {
            assistant_partial: String::new(),
            assistant_partial_message_id: None,
            assistant_sequence: 0,
            assistant_emitted: String::new(),
            thought_partial: String::new(),
            tool_cache: HashMap::new(),
            terminal_status: None,
            start_progress: TurnStartProgress::Pending,
            first_event_seen: false,
            telemetry_emitted: false,
        }
    }
}

impl EventLoopRuntimeState {
    pub(super) fn mark_first_event_seen(&mut self) -> bool {
        if self.first_event_seen {
            return false;
        }
        self.first_event_seen = true;
        true
    }

    pub(super) fn promote_started_if_pending(
        &mut self,
        start_progress_tx: &tokio::sync::watch::Sender<TurnStartProgress>,
    ) -> bool {
        if self.start_progress != TurnStartProgress::Pending {
            return false;
        }
        self.start_progress = TurnStartProgress::Started;
        let _ = start_progress_tx.send(TurnStartProgress::Started);
        true
    }

    pub(super) fn promote_terminal(
        &mut self,
        start_progress_tx: &tokio::sync::watch::Sender<TurnStartProgress>,
    ) {
        if self.start_progress == TurnStartProgress::Terminal {
            return;
        }
        self.start_progress = TurnStartProgress::Terminal;
        let _ = start_progress_tx.send(TurnStartProgress::Terminal);
    }
}

fn is_terminal_turn_status(status: &SessionTurnStatus) -> bool {
    matches!(
        status,
        SessionTurnStatus::Completed | SessionTurnStatus::Failed | SessionTurnStatus::Interrupted
    )
}

pub(super) fn should_check_store_terminal_status(event_type: &SessionEventType) -> bool {
    !matches!(
        event_type,
        SessionEventType::AssistantChunk
            | SessionEventType::ThoughtChunk
            | SessionEventType::ContextWindowUpdate
            | SessionEventType::ToolCallUpdate
    )
}

pub(super) async fn should_drop_post_terminal_event(
    ctx: &TurnEventLoop,
    runtime: &mut EventLoopRuntimeState,
) -> bool {
    if runtime.terminal_status.is_some() {
        return true;
    }

    match ctx
        .store
        .get_session_turn(ctx.session_id, ctx.turn_id)
        .await
    {
        Ok(Some(turn)) if is_terminal_turn_status(&turn.status) => {
            runtime.terminal_status = Some(turn.status);
            runtime.promote_terminal(&ctx.start_progress_tx);
            true
        }
        Ok(_) => false,
        Err(err) => {
            tracing::warn!(
                session_id = %ctx.session_id.0,
                run_id = %ctx.run_id.0,
                turn_id = %ctx.turn_id.0,
                "failed to check turn terminal status before appending provider event: {err:#}"
            );
            false
        }
    }
}

pub(super) fn should_process_post_terminal_assistant_complete(
    event_type: &SessionEventType,
    terminal_status: Option<&SessionTurnStatus>,
) -> bool {
    matches!(event_type, SessionEventType::AssistantComplete)
        && matches!(terminal_status, Some(SessionTurnStatus::Completed))
}
