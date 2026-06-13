use anyhow::Result;
use serde_json::json;

use crate::app_server::{ItemLifecycleNotification, ThreadItem};
use crate::protocol::{CrpChannel, CrpEvent, CrpToolStatus};

use super::super::AppServerSessionState;

pub(super) fn translate_item_lifecycle(
    session_state: &mut AppServerSessionState,
    completed: bool,
    payload: ItemLifecycleNotification,
) -> Result<Vec<(CrpChannel, CrpEvent)>> {
    let session_id = session_state.tracker.session_id.clone();
    let crp_turn_id = session_state
        .turn_aliases
        .ensure_crp_turn_id(payload.turn_id.as_str());
    let turn = session_state.tracker.ensure_turn(payload.turn_id.as_str());

    let events = match payload.item {
        ThreadItem::AgentMessage { id, text, .. } if completed => {
            turn.message_id = Some(id.clone());
            turn.emitted_final = true;
            vec![(
                CrpChannel::Control,
                CrpEvent::MessageFinal {
                    session_id,
                    turn_id: crp_turn_id,
                    message_id: id,
                    content: text,
                },
            )]
        }
        ThreadItem::Reasoning {
            id,
            summary,
            content,
        } if completed => {
            let mut out = Vec::new();
            for (summary_index, text) in summary.into_iter().enumerate() {
                let summary_index = summary_index as i64;
                let state = turn
                    .reasoning_summaries
                    .entry((id.clone(), summary_index))
                    .or_default();
                if state.text.is_empty() {
                    state.text = text.clone();
                    out.push((
                        CrpChannel::Control,
                        CrpEvent::ReasoningSummary {
                            session_id: session_id.clone(),
                            turn_id: crp_turn_id.clone(),
                            summary_index,
                            text,
                            item_id: Some(id.clone()),
                        },
                    ));
                }
            }
            if !turn.reasoning_text_seen.contains(&id) {
                for block in content {
                    if block.is_empty() {
                        continue;
                    }
                    out.push((
                        CrpChannel::Data,
                        CrpEvent::ReasoningTraceFinal {
                            session_id: session_id.clone(),
                            turn_id: crp_turn_id.clone(),
                            content: block,
                            encoding: None,
                            summary_index: 0,
                            item_id: Some(id.clone()),
                        },
                    ));
                }
            }
            out
        }
        ThreadItem::CommandExecution {
            id,
            command,
            cwd,
            status,
            command_actions,
            aggregated_output,
            exit_code,
            duration_ms,
            ..
        } => {
            session_state.command_execution_seen = true;
            let input_preview = json!({
                "command": command,
                "cwd": cwd,
                "command_actions": command_actions,
            });
            if completed {
                let (status, error) = match status.as_str() {
                    "completed" => (CrpToolStatus::Success, None),
                    "declined" => (CrpToolStatus::Error, Some("command_declined".to_string())),
                    _ => (
                        CrpToolStatus::Error,
                        exit_code.map(|code| format!("exit_code: {code}")),
                    ),
                };
                vec![(
                    CrpChannel::Control,
                    CrpEvent::ToolCompleted {
                        session_id,
                        turn_id: crp_turn_id,
                        tool_call_id: id,
                        tool_name: "exec".to_string(),
                        tool_label: Some("Ran".to_string()),
                        status,
                        output: Some(json!({
                            "aggregated_output": aggregated_output,
                            "exit_code": exit_code,
                            "duration_ms": duration_ms,
                        })),
                        error,
                        input_preview: Some(input_preview),
                    },
                )]
            } else {
                vec![(
                    CrpChannel::Control,
                    CrpEvent::ToolStarted {
                        session_id,
                        turn_id: crp_turn_id,
                        tool_call_id: id,
                        tool_name: "exec".to_string(),
                        tool_label: Some("Running".to_string()),
                        input: Some(input_preview.clone()),
                        input_preview: Some(input_preview),
                    },
                )]
            }
        }
        ThreadItem::FileChange {
            id,
            changes,
            status,
        } => {
            let preview = super::support::patch_input_preview(&changes);
            if completed {
                let (status, error) = match status.as_str() {
                    "completed" => (CrpToolStatus::Success, None),
                    "declined" => (
                        CrpToolStatus::Error,
                        Some("apply_patch_declined".to_string()),
                    ),
                    _ => (CrpToolStatus::Error, Some("apply_patch_failed".to_string())),
                };
                vec![(
                    CrpChannel::Control,
                    CrpEvent::ToolCompleted {
                        session_id,
                        turn_id: crp_turn_id,
                        tool_call_id: id,
                        tool_name: "apply_patch".to_string(),
                        tool_label: Some("Edited".to_string()),
                        status,
                        output: Some(json!({ "changes": changes })),
                        error,
                        input_preview: Some(preview),
                    },
                )]
            } else {
                vec![(
                    CrpChannel::Control,
                    CrpEvent::ToolStarted {
                        session_id,
                        turn_id: crp_turn_id,
                        tool_call_id: id,
                        tool_name: "apply_patch".to_string(),
                        tool_label: Some("Edited".to_string()),
                        input: Some(json!({ "preview": preview.clone() })),
                        input_preview: Some(preview),
                    },
                )]
            }
        }
        ThreadItem::McpToolCall {
            id,
            server,
            tool,
            status,
            arguments,
            result,
            error,
            duration_ms,
        } => {
            let tool_name = format!("mcp.{server}.{tool}");
            let input_preview = json!({ "server": server, "tool": tool });
            if completed {
                let (status, error) = match status.as_str() {
                    "completed" => (CrpToolStatus::Success, None),
                    _ => (CrpToolStatus::Error, error.map(|error| error.message)),
                };
                vec![(
                    CrpChannel::Control,
                    CrpEvent::ToolCompleted {
                        session_id,
                        turn_id: crp_turn_id,
                        tool_call_id: id,
                        tool_name,
                        tool_label: Some("MCP".to_string()),
                        status,
                        output: Some(json!({
                            "duration_ms": duration_ms,
                            "result": result,
                        })),
                        error,
                        input_preview: Some(input_preview),
                    },
                )]
            } else {
                vec![(
                    CrpChannel::Control,
                    CrpEvent::ToolStarted {
                        session_id,
                        turn_id: crp_turn_id,
                        tool_call_id: id,
                        tool_name,
                        tool_label: Some("MCP".to_string()),
                        input: Some(json!({
                            "server": input_preview["server"],
                            "tool": input_preview["tool"],
                            "arguments": arguments,
                        })),
                        input_preview: Some(input_preview),
                    },
                )]
            }
        }
        ThreadItem::WebSearch { id, query, .. } => {
            if completed {
                vec![(
                    CrpChannel::Control,
                    CrpEvent::ToolCompleted {
                        session_id,
                        turn_id: crp_turn_id,
                        tool_call_id: id,
                        tool_name: "web_search".to_string(),
                        tool_label: Some("Searched".to_string()),
                        status: CrpToolStatus::Success,
                        output: Some(json!({ "query": query })),
                        error: None,
                        input_preview: Some(json!({ "query": query })),
                    },
                )]
            } else {
                vec![(
                    CrpChannel::Control,
                    CrpEvent::ToolStarted {
                        session_id,
                        turn_id: crp_turn_id,
                        tool_call_id: id,
                        tool_name: "web_search".to_string(),
                        tool_label: Some("Search".to_string()),
                        input: None,
                        input_preview: None,
                    },
                )]
            }
        }
        ThreadItem::ImageView { id, path } => {
            let payload = json!({ "path": path });
            if completed {
                vec![(
                    CrpChannel::Control,
                    CrpEvent::ToolCompleted {
                        session_id,
                        turn_id: crp_turn_id,
                        tool_call_id: id,
                        tool_name: "view_image".to_string(),
                        tool_label: Some("Viewed".to_string()),
                        status: CrpToolStatus::Success,
                        output: Some(payload),
                        error: None,
                        input_preview: None,
                    },
                )]
            } else {
                vec![(
                    CrpChannel::Control,
                    CrpEvent::ToolStarted {
                        session_id,
                        turn_id: crp_turn_id,
                        tool_call_id: id,
                        tool_name: "view_image".to_string(),
                        tool_label: Some("View".to_string()),
                        input: Some(payload.clone()),
                        input_preview: Some(payload),
                    },
                )]
            }
        }
        ThreadItem::ContextCompaction { .. } if completed => vec![
            (
                CrpChannel::Control,
                CrpEvent::SessionNotice {
                    session_id: session_id.clone(),
                    turn_id: Some(crp_turn_id.clone()),
                    code: "context.compacted".to_string(),
                    severity: Some("info".to_string()),
                    message: Some("Context compacted. Earlier turns were summarized.".to_string()),
                    details: None,
                    transient: None,
                },
            ),
            (
                CrpChannel::Control,
                CrpEvent::SessionGap {
                    session_id: session_id.clone(),
                    turn_id: Some(crp_turn_id.clone()),
                    reason: Some("context_compacted".to_string()),
                },
            ),
        ],
        ThreadItem::EnteredReviewMode { .. }
        | ThreadItem::ExitedReviewMode { .. }
        | ThreadItem::ContextCompaction { .. }
        | ThreadItem::Unknown
        | ThreadItem::AgentMessage { .. }
        | ThreadItem::Reasoning { .. } => Vec::new(),
    };
    Ok(events)
}
