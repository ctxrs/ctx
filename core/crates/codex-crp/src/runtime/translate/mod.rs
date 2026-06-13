mod item_lifecycle;
mod support;

use anyhow::Result;
use serde_json::Value;

use crate::app_server::{
    AgentMessageDeltaNotification, CommandExecutionOutputDeltaNotification,
    ItemLifecycleNotification, ReasoningSummaryPartAddedNotification,
    ReasoningSummaryTextDeltaNotification, ReasoningTextDeltaNotification,
    ThreadTokenUsageUpdatedNotification, TurnCompletedNotification, TurnStartedNotification,
};
use crate::protocol::{CrpChannel, CrpEvent, CrpTurnError, CrpTurnStatus};

pub(super) use self::support::{
    canonical_context_window_from_thread_usage, emit_turn_request_error,
    emit_unsupported_server_request_notice,
};
use super::AppServerSessionState;

pub(super) fn translate_notification(
    session_state: &mut AppServerSessionState,
    method: &str,
    params: Value,
) -> Result<Vec<(CrpChannel, CrpEvent)>> {
    match method {
        "thread/tokenUsage/updated" => {
            let payload: ThreadTokenUsageUpdatedNotification = serde_json::from_value(params)?;
            if payload.thread_id != session_state.thread_id {
                return Ok(Vec::new());
            }
            let turn_id = session_state
                .turn_aliases
                .ensure_crp_turn_id(payload.turn_id.as_str());
            let context_window = canonical_context_window_from_thread_usage(&payload.token_usage);
            session_state
                .turn_aliases
                .note_token_usage(payload.turn_id.as_str(), payload.token_usage);
            if let Some(context_window) = context_window {
                Ok(vec![(
                    CrpChannel::Control,
                    CrpEvent::TurnContextWindowUpdated {
                        session_id: session_state.tracker.session_id.clone(),
                        turn_id,
                        context_window,
                    },
                )])
            } else {
                Ok(Vec::new())
            }
        }
        "turn/started" => {
            let payload: TurnStartedNotification = serde_json::from_value(params)?;
            if payload.thread_id != session_state.thread_id {
                return Ok(Vec::new());
            }
            let turn_id = session_state
                .turn_aliases
                .ensure_crp_turn_id(payload.turn.id.as_str());
            session_state.tracker.ensure_turn(payload.turn.id.as_str());
            Ok(vec![(
                CrpChannel::Control,
                CrpEvent::TurnStarted {
                    session_id: session_state.tracker.session_id.clone(),
                    turn_id,
                },
            )])
        }
        "turn/completed" => {
            let payload: TurnCompletedNotification = serde_json::from_value(params)?;
            if payload.thread_id != session_state.thread_id {
                return Ok(Vec::new());
            }
            session_state
                .turn_aliases
                .note_terminal_turn(payload.turn.id.as_str());
            let turn_id = session_state
                .turn_aliases
                .ensure_crp_turn_id(payload.turn.id.as_str());
            let (status, error) = match payload.turn.status.as_str() {
                "completed" => (CrpTurnStatus::Success, None),
                "interrupted" => (CrpTurnStatus::Interrupted, None),
                "failed" => {
                    let error = payload.turn.error.map(|err| CrpTurnError {
                        message: err.message,
                        kind: Some("app_server_error".to_string()),
                        details: support::merge_error_details(
                            err.codex_error_info,
                            err.additional_details,
                        ),
                    });
                    (CrpTurnStatus::Error, error)
                }
                _ => (CrpTurnStatus::Canceled, None),
            };
            let context_window = if matches!(status, CrpTurnStatus::Success) {
                session_state
                    .turn_aliases
                    .take_context_window_for_app_turn(payload.turn.id.as_str())
            } else {
                None
            };
            Ok(vec![(
                CrpChannel::Control,
                CrpEvent::TurnCompleted {
                    session_id: session_state.tracker.session_id.clone(),
                    turn_id,
                    status,
                    context_window,
                    error,
                },
            )])
        }
        "item/agentMessage/delta" => {
            let payload: AgentMessageDeltaNotification = serde_json::from_value(params)?;
            if payload.thread_id != session_state.thread_id {
                return Ok(Vec::new());
            }
            let turn = session_state.tracker.ensure_turn(payload.turn_id.as_str());
            turn.message_id = Some(payload.item_id.clone());
            Ok(vec![(
                CrpChannel::Data,
                CrpEvent::MessageDelta {
                    session_id: session_state.tracker.session_id.clone(),
                    turn_id: session_state
                        .turn_aliases
                        .ensure_crp_turn_id(payload.turn_id.as_str()),
                    message_id: payload.item_id,
                    delta: payload.delta,
                },
            )])
        }
        "item/reasoning/summaryPartAdded" => {
            let payload: ReasoningSummaryPartAddedNotification = serde_json::from_value(params)?;
            if payload.thread_id != session_state.thread_id {
                return Ok(Vec::new());
            }
            let turn = session_state.tracker.ensure_turn(payload.turn_id.as_str());
            turn.reasoning_summaries
                .entry((payload.item_id, payload.summary_index))
                .or_default();
            Ok(Vec::new())
        }
        "item/reasoning/summaryTextDelta" => {
            let payload: ReasoningSummaryTextDeltaNotification = serde_json::from_value(params)?;
            if payload.thread_id != session_state.thread_id {
                return Ok(Vec::new());
            }
            let summary_text = {
                let turn = session_state.tracker.ensure_turn(payload.turn_id.as_str());
                let state = turn
                    .reasoning_summaries
                    .entry((payload.item_id.clone(), payload.summary_index))
                    .or_default();
                state.text.push_str(&payload.delta);
                state.text.clone()
            };
            Ok(vec![(
                CrpChannel::Control,
                CrpEvent::ReasoningSummary {
                    session_id: session_state.tracker.session_id.clone(),
                    turn_id: session_state
                        .turn_aliases
                        .ensure_crp_turn_id(payload.turn_id.as_str()),
                    summary_index: payload.summary_index,
                    text: summary_text,
                    item_id: Some(payload.item_id),
                },
            )])
        }
        "item/reasoning/textDelta" => {
            let payload: ReasoningTextDeltaNotification = serde_json::from_value(params)?;
            if payload.thread_id != session_state.thread_id {
                return Ok(Vec::new());
            }
            let turn = session_state.tracker.ensure_turn(payload.turn_id.as_str());
            turn.reasoning_text_seen.insert(payload.item_id.clone());
            let _ = payload.content_index;
            Ok(vec![(
                CrpChannel::Data,
                CrpEvent::ReasoningTrace {
                    session_id: session_state.tracker.session_id.clone(),
                    turn_id: session_state
                        .turn_aliases
                        .ensure_crp_turn_id(payload.turn_id.as_str()),
                    chunk: payload.delta,
                    encoding: None,
                    summary_index: 0,
                    item_id: Some(payload.item_id),
                },
            )])
        }
        "item/commandExecution/outputDelta" => {
            let payload: CommandExecutionOutputDeltaNotification = serde_json::from_value(params)?;
            if payload.thread_id != session_state.thread_id {
                return Ok(Vec::new());
            }
            session_state.command_execution_seen = true;
            Ok(vec![(
                CrpChannel::Data,
                CrpEvent::ToolOutputDelta {
                    session_id: session_state.tracker.session_id.clone(),
                    turn_id: session_state
                        .turn_aliases
                        .ensure_crp_turn_id(payload.turn_id.as_str()),
                    tool_call_id: payload.item_id,
                    stream: None,
                    chunk: payload.delta,
                },
            )])
        }
        "item/started" | "item/completed" => {
            let payload: ItemLifecycleNotification = serde_json::from_value(params)?;
            if payload.thread_id != session_state.thread_id {
                return Ok(Vec::new());
            }
            item_lifecycle::translate_item_lifecycle(
                session_state,
                method == "item/completed",
                payload,
            )
        }
        "error" => Ok(Vec::new()),
        _ => Ok(Vec::new()),
    }
}
