use anyhow::Result;
use serde_json::{json, Value};
use tracing::warn;

use crate::app_server::{ModelListResponse, TurnStartResponse};
use crate::builtins::split_model_and_effort;
use crate::protocol::{CrpChannel, CrpCommand, CrpEvent, CrpTurnStatus};
use crate::RuntimeOptions;

use super::emit_turn_request_error;
use super::io::{dispatch_event, CrpEventRouter};
use super::prompt_items::translate_prompt_items_for_app_server;
use super::session::{open_session, probe_models};
use super::status::query_session_status;
use super::{current_model_id, AppServerSessionState, RuntimeCommand};

pub(super) async fn handle_command(
    command: RuntimeCommand,
    session: &mut Option<AppServerSessionState>,
    router: &CrpEventRouter,
    options: &RuntimeOptions,
) -> Result<()> {
    match command {
        RuntimeCommand::ParseError { message } => {
            warn!(%message, "failed to parse CRP command");
        }
        RuntimeCommand::Parsed(command) => match *command {
            CrpCommand::ToolResult {
                session_id,
                turn_id,
                tool_call_id,
                status,
                output,
                error,
            } => {
                let _ = (session_id, turn_id, tool_call_id, status, output, error);
                warn!("tool.result ignored: app-server-backed runtime owns tool execution");
            }
            command => {
                handle_parsed_command(command, session, router, options).await?;
            }
        },
    }
    Ok(())
}

pub(super) async fn handle_parsed_command(
    command: CrpCommand,
    session: &mut Option<AppServerSessionState>,
    router: &CrpEventRouter,
    options: &RuntimeOptions,
) -> Result<()> {
    match command {
        CrpCommand::SessionOpen {
            session_id,
            provider_session_id,
            config,
        } => {
            if session.is_some() {
                warn!("session.open ignored: session already active");
                return Ok(());
            }

            let config = config.unwrap_or_default();

            let mut state = open_session(config, provider_session_id, options).await?;
            let provider_session_id = state.thread_id.clone();
            let session_id = session_id.unwrap_or_else(|| provider_session_id.clone());
            state.tracker.session_id = session_id.clone();
            let commands = serde_json::to_value(&state.opened_commands)?;

            let _ = router.send_control(CrpEvent::SessionOpened {
                session_id,
                provider_session_id: Some(provider_session_id),
                supports_session_status: Some(true),
                commands: Some(commands),
                slash_commands: Some(state.opened_slash_commands.clone()),
                models: None,
                current_model_id: Some(current_model_id(
                    &state.default_model,
                    state.default_effort.as_deref(),
                )),
                agents: None,
                output_style: None,
                available_output_styles: None,
                skills: None,
                plugins: None,
                tools: None,
                permission_mode: None,
                mcp_servers: None,
                account: None,
                fast_mode_state: None,
            });

            *session = Some(state);
        }
        CrpCommand::SessionPrompt {
            session_id,
            turn_id,
            prompt,
            items,
            model,
            reasoning_effort,
            cwd,
        } => {
            let Some(state) = session.as_mut() else {
                warn!("session.prompt ignored: no active session");
                return Ok(());
            };
            if let Some(expected) = session_id.as_deref() {
                if expected != state.tracker.session_id {
                    warn!(%expected, "session.prompt ignored: session_id mismatch");
                    return Ok(());
                }
            }

            let items = match (items, prompt) {
                (Some(items), _) => items,
                (None, Some(prompt)) => vec![json!({
                    "type": "text",
                    "text": prompt,
                })],
                (None, None) => {
                    warn!("session.prompt ignored: missing prompt or items");
                    return Ok(());
                }
            };

            let cwd = cwd.unwrap_or_else(|| state.default_cwd.clone());
            let requested_model = model.unwrap_or_else(|| {
                current_model_id(&state.default_model, state.default_effort.as_deref())
            });
            let (model, effort_override) = split_model_and_effort(&requested_model);
            let effort = effort_override.or(reasoning_effort);
            let requested_turn_id = turn_id.clone();
            let app_server_input = match translate_prompt_items_for_app_server(items).await {
                Ok(items) => items,
                Err(err) => {
                    emit_turn_request_error(
                        router,
                        &state.tracker.session_id,
                        turn_id,
                        "turn_start_input_translation_failed",
                        err.to_string(),
                    );
                    return Ok(());
                }
            };

            match state
                .client
                .request::<TurnStartResponse>(
                    "turn/start",
                    json!({
                        "threadId": state.thread_id,
                        "input": app_server_input,
                        "cwd": cwd.to_string_lossy().to_string(),
                        "model": model,
                        "effort": effort,
                    }),
                )
                .await
            {
                Ok(response) => {
                    let _ = state
                        .turn_aliases
                        .bind_turn_alias(response.turn.id.clone(), requested_turn_id);
                }
                Err(err) => emit_turn_request_error(
                    router,
                    &state.tracker.session_id,
                    turn_id,
                    "turn_start_failed",
                    err.to_string(),
                ),
            }
        }
        CrpCommand::SessionCompact {
            session_id,
            turn_id,
        } => {
            let Some(state) = session.as_mut() else {
                warn!("session.compact ignored: no active session");
                return Ok(());
            };
            if let Some(expected) = session_id.as_deref() {
                if expected != state.tracker.session_id {
                    warn!(%expected, "session.compact ignored: session_id mismatch");
                    return Ok(());
                }
            }
            if let Some(turn_id) = turn_id.clone() {
                state.turn_aliases.pending_compact_turns.push_back(turn_id);
            }
            if let Err(err) = state
                .client
                .request::<Value>(
                    "thread/compact/start",
                    json!({ "threadId": state.thread_id }),
                )
                .await
            {
                emit_turn_request_error(
                    router,
                    &state.tracker.session_id,
                    turn_id,
                    "thread_compact_start_failed",
                    err.to_string(),
                );
            }
        }
        CrpCommand::SessionUndo {
            session_id,
            turn_id,
        } => {
            let Some(state) = session.as_mut() else {
                warn!("session.undo ignored: no active session");
                return Ok(());
            };
            if let Some(expected) = session_id.as_deref() {
                if expected != state.tracker.session_id {
                    warn!(%expected, "session.undo ignored: session_id mismatch");
                    return Ok(());
                }
            }
            match state
                .client
                .request::<Value>(
                    "thread/rollback",
                    json!({ "threadId": state.thread_id, "numTurns": 1 }),
                )
                .await
            {
                Ok(_) => {
                    if let Some(turn_id) = turn_id {
                        dispatch_event(
                            router,
                            CrpChannel::Control,
                            CrpEvent::TurnCompleted {
                                session_id: state.tracker.session_id.clone(),
                                turn_id,
                                status: CrpTurnStatus::Success,
                                context_window: None,
                                error: None,
                            },
                        );
                    }
                }
                Err(err) => emit_turn_request_error(
                    router,
                    &state.tracker.session_id,
                    turn_id,
                    "thread_rollback_failed",
                    err.to_string(),
                ),
            }
        }
        CrpCommand::SessionReview {
            session_id,
            turn_id,
            instructions,
        } => {
            let Some(state) = session.as_mut() else {
                warn!("session.review ignored: no active session");
                return Ok(());
            };
            if let Some(expected) = session_id.as_deref() {
                if expected != state.tracker.session_id {
                    warn!(%expected, "session.review ignored: session_id mismatch");
                    return Ok(());
                }
            }

            let target = instructions
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .map(|instructions| json!({"type": "custom", "instructions": instructions}))
                .unwrap_or_else(|| json!({"type": "uncommittedChanges"}));
            let requested_turn_id = turn_id.clone();
            match state
                .client
                .request::<serde_json::Value>(
                    "review/start",
                    json!({
                        "threadId": state.thread_id,
                        "target": target,
                    }),
                )
                .await
            {
                Ok(value) => {
                    let turn_id_from_response = value
                        .get("turn")
                        .and_then(|turn| turn.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if !turn_id_from_response.is_empty() {
                        let _ = state
                            .turn_aliases
                            .bind_turn_alias(turn_id_from_response, requested_turn_id);
                    }
                }
                Err(err) => emit_turn_request_error(
                    router,
                    &state.tracker.session_id,
                    turn_id,
                    "review_start_failed",
                    err.to_string(),
                ),
            }
        }
        CrpCommand::SessionSetModel {
            session_id,
            model_id,
        } => {
            let Some(state) = session.as_mut() else {
                warn!("session.set_model ignored: no active session");
                return Ok(());
            };
            if let Some(expected) = session_id.as_deref() {
                if expected != state.tracker.session_id {
                    warn!(%expected, "session.set_model ignored: session_id mismatch");
                    return Ok(());
                }
            }
            let Some(model_id) = model_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
            else {
                warn!("session.set_model ignored: missing model_id");
                return Ok(());
            };

            let response = state
                .client
                .request::<ModelListResponse>("model/list", json!({ "includeHidden": false }))
                .await;
            match response {
                Ok(response) => {
                    let (model, effort) = split_model_and_effort(&model_id);
                    let exists = response.data.iter().any(|candidate| candidate.id == model);
                    if exists {
                        state.default_model = model;
                        state.default_effort = effort;
                        dispatch_event(
                            router,
                            CrpChannel::Control,
                            CrpEvent::SessionNotice {
                                session_id: state.tracker.session_id.clone(),
                                turn_id: None,
                                code: "session_model_updated".to_string(),
                                severity: Some("info".to_string()),
                                message: Some(format!("session model updated to {model_id}")),
                                details: Some(json!({ "model_id": model_id })),
                                transient: Some(false),
                            },
                        );
                    } else {
                        dispatch_event(
                            router,
                            CrpChannel::Control,
                            CrpEvent::SessionNotice {
                                session_id: state.tracker.session_id.clone(),
                                turn_id: None,
                                code: "session_model_update_failed".to_string(),
                                severity: Some("error".to_string()),
                                message: Some(format!(
                                    "failed to update session model to {model_id}: model not found"
                                )),
                                details: Some(json!({ "model_id": model_id })),
                                transient: Some(false),
                            },
                        );
                    }
                }
                Err(err) => dispatch_event(
                    router,
                    CrpChannel::Control,
                    CrpEvent::SessionNotice {
                        session_id: state.tracker.session_id.clone(),
                        turn_id: None,
                        code: "session_model_update_failed".to_string(),
                        severity: Some("error".to_string()),
                        message: Some(format!(
                            "failed to update session model to {model_id}: {err}"
                        )),
                        details: Some(json!({ "model_id": model_id })),
                        transient: Some(false),
                    },
                ),
            }
        }
        CrpCommand::SessionAuthenticate {
            session_id,
            method_id,
        } => {
            let notice_session_id = session_id
                .or_else(|| {
                    session
                        .as_ref()
                        .map(|state| state.tracker.session_id.clone())
                })
                .unwrap_or_else(|| "unknown".to_string());
            warn!(
                session_id = %notice_session_id,
                ?method_id,
                "session.authenticate unsupported: app-server-backed runtime does not support CRP auth commands"
            );
            dispatch_event(
                router,
                CrpChannel::Control,
                CrpEvent::SessionNotice {
                    session_id: notice_session_id,
                    turn_id: None,
                    code: "auth_error".to_string(),
                    severity: Some("error".to_string()),
                    message: Some(
                        "session.authenticate is not supported by this runtime".to_string(),
                    ),
                    details: Some(json!({
                        "provider": "codex-crp",
                        "reason": "unsupported_command",
                    })),
                    transient: Some(false),
                },
            );
        }
        CrpCommand::SessionStatus { session_id } => {
            let Some(state) = session.as_mut() else {
                let failed_session_id = session_id.unwrap_or_else(|| "unknown".to_string());
                warn!(session_id = %failed_session_id, "session.status failed: no active session");
                dispatch_event(
                    router,
                    CrpChannel::Control,
                    CrpEvent::SessionNotice {
                        session_id: failed_session_id,
                        turn_id: None,
                        code: "session_status_failed".to_string(),
                        severity: Some("error".to_string()),
                        message: Some("session status query failed: no active session".to_string()),
                        details: None,
                        transient: Some(false),
                    },
                );
                return Ok(());
            };
            if let Some(expected) = session_id.as_deref() {
                if expected != state.tracker.session_id {
                    warn!(%expected, "session.status ignored: session_id mismatch");
                    return Ok(());
                }
            }

            match query_session_status(state).await {
                Ok(details) => dispatch_event(
                    router,
                    CrpChannel::Control,
                    CrpEvent::SessionNotice {
                        session_id: state.tracker.session_id.clone(),
                        turn_id: None,
                        code: "session_status".to_string(),
                        severity: Some("info".to_string()),
                        message: Some(if details["quiescent"] == json!(true) {
                            "session is quiescent".to_string()
                        } else {
                            "session is busy".to_string()
                        }),
                        details: Some(details),
                        transient: Some(false),
                    },
                ),
                Err(err) => dispatch_event(
                    router,
                    CrpChannel::Control,
                    CrpEvent::SessionNotice {
                        session_id: state.tracker.session_id.clone(),
                        turn_id: None,
                        code: "session_status_failed".to_string(),
                        severity: Some("error".to_string()),
                        message: Some(err.to_string()),
                        details: None,
                        transient: Some(false),
                    },
                ),
            }
        }
        CrpCommand::ModelsList { config } => {
            let models = probe_models(config.unwrap_or_default(), options).await?;
            let _ = router.send_control(CrpEvent::ModelsList {
                models: models.models,
                current_model_id: models.current_model_id,
                catalog_source: models.catalog_source,
            });
        }
        CrpCommand::SessionCancel {
            session_id,
            turn_id,
        } => {
            let Some(state) = session.as_mut() else {
                warn!("session.cancel ignored: no active session");
                return Ok(());
            };
            if let Some(expected) = session_id.as_deref() {
                if expected != state.tracker.session_id {
                    warn!(%expected, "session.cancel ignored: session_id mismatch");
                    return Ok(());
                }
            }
            let app_turn_id = state
                .turn_aliases
                .app_turn_id_for_crp(turn_id.as_deref())
                .or_else(|| state.turn_aliases.active_app_turn_id.clone());
            let Some(app_turn_id) = app_turn_id else {
                warn!("session.cancel ignored: no active turn to interrupt");
                return Ok(());
            };
            if let Err(err) = state
                .client
                .request::<Value>(
                    "turn/interrupt",
                    json!({ "threadId": state.thread_id, "turnId": app_turn_id }),
                )
                .await
            {
                dispatch_event(
                    router,
                    CrpChannel::Control,
                    CrpEvent::SessionNotice {
                        session_id: state.tracker.session_id.clone(),
                        turn_id,
                        code: "turn_interrupt_failed".to_string(),
                        severity: Some("error".to_string()),
                        message: Some(err.to_string()),
                        details: None,
                        transient: Some(false),
                    },
                );
            }
        }
        CrpCommand::ToolResult { .. } => {
            warn!("tool.result ignored: app-server-backed runtime owns tool execution");
        }
    }

    Ok(())
}
