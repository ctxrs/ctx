use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Result;
use serde_json::json;
use tokio::sync::broadcast;

use ctx_core::models::SessionEventType;

use crate::container_exec::translate_thread_cwd_for_container;
use crate::events::NormalizedEvent;

use super::super::super::config::{
    build_crp_session_config, build_prompt_items, flatten_prompt_items_as_text,
    model_override_disabled, provider_requires_flattened_text_prompt, split_model_id_and_effort,
};
use super::super::super::normalize::{
    event_matches_session, event_turn_id, map_crp_event, unknown_event_observation, CachedToolInput,
};
use super::super::super::policy::{
    parse_native_crp_slash_command_for_provider, validate_provider_slash_command_support,
    CrpSlashCommand,
};
use super::super::super::protocol::{CrpCommand, CrpEvent, KnownCrpEvent};
use super::super::super::CRP_CANCEL_DRAIN_TIMEOUT;
use super::super::open_handshake::{
    apply_session_opened_state, crp_first_event_timeout, crp_runtime_label, duration_millis_u64,
    session_opened_provider_session_id, validate_provider_session_open,
};
use super::super::{registry::ActivePromptGuard, CrpPromptRequest, CrpSessionPool};
use super::startup_stderr::handle_startup_stderr_line;
use super::terminal::{
    interrupted_outcome_without_event, is_sweep_only_status_notice, update_terminal_outcome,
};
use crate::adapters::ProviderTurnOutcome;

impl CrpSessionPool {
    pub(in crate::crp) async fn prompt(
        self: &Arc<Self>,
        req: CrpPromptRequest,
    ) -> Result<ProviderTurnOutcome> {
        let _guard = ActivePromptGuard::new(
            Arc::clone(&self.active_prompts),
            Arc::clone(&self.busy_sessions),
            req.session_key.clone(),
        )?;
        let session = self
            .get_or_create_session(&req.session_key, &req.workdir, &req.env)
            .await?;

        let turn_id = format!("crp-{}", uuid::Uuid::new_v4());
        let mut rx = session.process.events.subscribe();
        let mut stderr_rx = session.process.stderr_lines.subscribe();
        let mut shutdown_rx = session.process.shutdown.subscribe();
        let shutdown_reason = shutdown_rx.borrow().clone();
        if let Some(reason) = shutdown_reason {
            let _ = req
                .event_sink
                .send(NormalizedEvent {
                    event_type: SessionEventType::TurnInterrupted,
                    payload_json: json!({
                        "reason": reason,
                        "provider_cancelled": true,
                    }),
                })
                .await;
            self.drain_session_if_needed(&req.session_key, &session)
                .await;
            return Ok(ProviderTurnOutcome::interrupted(reason, true));
        }

        let result: Result<ProviderTurnOutcome> = async {
            let mut cancel_rx = req.cancel_rx;
            let needs_session_open =
                !session.opened.load(Ordering::SeqCst) && !session.opening.load(Ordering::SeqCst);
            let mut last_seq = 0u64;
            let mut tool_output_cache: HashMap<String, String> = HashMap::new();
            let mut tool_input_cache: HashMap<String, CachedToolInput> = HashMap::new();
            let dump_norm_path = std::env::var("CTX_CRP_DUMP_NORMALIZED_EVENTS_PATH")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let mut dump_norm_file = dump_norm_path.as_deref().and_then(|path| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .ok()
            });
            let mut outcome: Option<ProviderTurnOutcome> = None;
            let first_event_timeout = crp_first_event_timeout(&req.env);
            if needs_session_open {
                let config = build_crp_session_config(&req.env, &req.workdir)?;
                let provider_session_id = req
                    .env
                    .get("CTX_PROVIDER_SESSION_REF")
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty());
                self.send_session_open(
                    &session,
                    &req.session_key,
                    provider_session_id.clone(),
                    config,
                )
                .await?;
                let first_event_deadline = tokio::time::Instant::now() + first_event_timeout;
                loop {
                    tokio::select! {
                        _ = &mut cancel_rx => {
                            session.opening.store(false, Ordering::SeqCst);
                            let _ = req
                                .event_sink
                                .send(NormalizedEvent {
                                    event_type: SessionEventType::TurnInterrupted,
                                    payload_json: json!({
                                        "reason": "cancelled",
                                        "provider_cancelled": true,
                                    }),
                                })
                                .await;
                            return Ok(ProviderTurnOutcome::interrupted("cancelled".to_string(), true));
                        }
                        _ = tokio::time::sleep_until(first_event_deadline) => {
                            let timeout_ms = duration_millis_u64(first_event_timeout);
                            let runtime = crp_runtime_label(&req.env);
                            let message = format!(
                                "CRP runtime did not emit session.opened within {timeout_ms}ms after launching {runtime} provider session",
                            );
                            session.opening.store(false, Ordering::SeqCst);
                            session.process.shutdown("crp_session_open_timeout").await;
                            return Ok(ProviderTurnOutcome::failed_with_context(
                                message,
                                Some("provider_startup_timeout".to_string()),
                                Some(json!({
                                    "timeout_ms": timeout_ms,
                                    "runtime": runtime,
                                    "provider_id": self.agent.provider_id,
                                })),
                                Some(json!("provider_startup_timeout")),
                                false,
                            ));
                        }
                        shutdown = shutdown_rx.changed() => {
                            let reason = match shutdown {
                                Ok(()) => shutdown_rx
                                    .borrow()
                                    .clone()
                                    .unwrap_or_else(|| "crp_shutdown".to_string()),
                                Err(_) => "crp_shutdown".to_string(),
                            };
                            return Ok(ProviderTurnOutcome::interrupted(reason, true));
                        }
                        stderr = stderr_rx.recv() => {
                            match stderr {
                                Ok(line) => {
                                    if let Some(startup) =
                                        handle_startup_stderr_line(&line, &session, &req.event_sink)
                                            .await?
                                    {
                                        return Ok(startup.outcome);
                                    }
                                }
                                Err(broadcast::error::RecvError::Lagged(_)) => {}
                                Err(broadcast::error::RecvError::Closed) => {}
                            }
                        }
                        recv = rx.recv() => {
                            let env = match recv {
                                Ok(env) => env,
                                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                                Err(broadcast::error::RecvError::Closed) => {
                                    return Ok(ProviderTurnOutcome::protocol_violation(
                                        "provider_protocol_violation_event_stream_closed",
                                        "CRP event stream closed before session.opened",
                                    ));
                                }
                            };
                            if !event_matches_session(&env.event, &req.session_key) {
                                continue;
                            }
                            if env.seq <= last_seq {
                                continue;
                            }
                            last_seq = env.seq;
                            if is_sweep_only_status_notice(&env.event) {
                                continue;
                            }
                            if let Some(returned_provider_session_ref) =
                                session_opened_provider_session_id(&env.event)
                            {
                                if let Err(err) = validate_provider_session_open(
                                    provider_session_id.as_deref(),
                                    returned_provider_session_ref,
                                    req.provider_session_ref_claim.as_ref(),
                                )
                                .await
                                {
                                    session
                                        .process
                                        .shutdown("provider_session_open_validation_failed")
                                        .await;
                                    let _ = self.prune_dead_sessions().await;
                                    return Err(err);
                                }
                                apply_session_opened_state(&session, &env.event);
                                let unknown_observation =
                                    unknown_event_observation(&env.event, env.channel, env.seq);
                                let mapped = map_crp_event(
                                    env.event,
                                    env.channel,
                                    env.seq,
                                    &mut tool_output_cache,
                                    &mut tool_input_cache,
                                );
                                if let (Some(hook), Some(observation)) =
                                    (&req.provider_unknown_event, unknown_observation)
                                {
                                    hook(observation).await;
                                }
                                update_terminal_outcome(&mut outcome, &mapped.events, mapped.done);
                                for event in mapped.events {
                                    if let Some(file) = dump_norm_file.as_mut() {
                                        let _ = writeln!(
                                            file,
                                            "{}",
                                            json!({
                                                "session_key": req.session_key,
                                                "turn_id": turn_id,
                                                "crp_seq": env.seq,
                                                "event_type": format!("{:?}", event.event_type),
                                                "payload_json": event.payload_json,
                                            })
                                        );
                                    }
                                    let _ = req.event_sink.send(event).await;
                                }
                                break;
                            }
                        }
                    }
                }
            }

            let send_result: Result<()> = async {
                validate_provider_slash_command_support(&self.agent.provider_id, &req.input.content)?;
                match parse_native_crp_slash_command_for_provider(
                    &self.agent.provider_id,
                    &req.input.content,
                ) {
                    Some(CrpSlashCommand::Compact) => {
                        session
                            .process
                            .send(CrpCommand::SessionCompact {
                                session_id: Some(req.session_key.clone()),
                                turn_id: Some(turn_id.clone()),
                            })
                            .await?;
                    }
                    Some(CrpSlashCommand::Undo) => {
                        session
                            .process
                            .send(CrpCommand::SessionUndo {
                                session_id: Some(req.session_key.clone()),
                                turn_id: Some(turn_id.clone()),
                            })
                            .await?;
                    }
                    Some(CrpSlashCommand::Review { instructions }) => {
                        session
                            .process
                            .send(CrpCommand::SessionReview {
                                session_id: Some(req.session_key.clone()),
                                turn_id: Some(turn_id.clone()),
                                instructions,
                            })
                            .await?;
                    }
                    None => {
                        let items = build_prompt_items(&req.input, &req.workdir, &req.env).await?;
                        let (prompt_items, prompt) =
                            if provider_requires_flattened_text_prompt(&self.agent.provider_id) {
                                (None, Some(flatten_prompt_items_as_text(&items)?))
                            } else {
                                (Some(items), Some(req.input.content.clone()))
                            };
                        let (model, reasoning_effort) = if model_override_disabled(&req.env) {
                            (None, None)
                        } else {
                            req.input
                                .model_id
                                .as_deref()
                                .map(split_model_id_and_effort)
                                .unwrap_or((None, None))
                        };
                        let prompt_cwd =
                            translate_thread_cwd_for_container(&req.env, &req.workdir)?;
                        session
                            .process
                            .send(CrpCommand::SessionPrompt {
                                session_id: Some(req.session_key.clone()),
                                turn_id: Some(turn_id.clone()),
                                items: prompt_items,
                                prompt,
                                model,
                                reasoning_effort,
                                cwd: Some(prompt_cwd),
                            })
                            .await?;
                    }
                }
                Ok(())
            }
            .await;
            if let Err(err) = send_result {
                if session.opened.load(Ordering::SeqCst) {
                    session.draining.store(true, Ordering::SeqCst);
                }
                return Err(err);
            }

            let mut cancel_requested = false;
            let mut cancel_deadline: Option<tokio::time::Instant> = None;
            let first_event_deadline =
                (!session.opened.load(Ordering::SeqCst))
                    .then(|| tokio::time::Instant::now() + first_event_timeout);
            loop {
                tokio::select! {
                    _ = &mut cancel_rx, if !cancel_requested => {
                        let _ = session.process.send(CrpCommand::SessionCancel {
                            session_id: Some(req.session_key.clone()),
                            turn_id: Some(turn_id.clone()),
                        }).await;
                        cancel_requested = true;
                        cancel_deadline = Some(tokio::time::Instant::now() + CRP_CANCEL_DRAIN_TIMEOUT);
                    }
                    _ = async {
                        if let Some(deadline) = cancel_deadline {
                            tokio::time::sleep_until(deadline).await;
                        }
                    }, if cancel_requested && cancel_deadline.is_some() => {
                        outcome = Some(interrupted_outcome_without_event("cancelled", true));
                        break;
                    }
                    _ = async {
                        if let Some(deadline) = first_event_deadline {
                            tokio::time::sleep_until(deadline).await;
                        }
                    }, if first_event_deadline.is_some() && last_seq == 0 => {
                        let timeout_ms = duration_millis_u64(first_event_timeout);
                        let runtime = crp_runtime_label(&req.env);
                        let message = format!(
                            "CRP runtime did not emit any events within {timeout_ms}ms after launching {runtime} provider session",
                        );
                        session.opening.store(false, Ordering::SeqCst);
                        session.process.shutdown("crp_first_event_timeout").await;
                        outcome = Some(ProviderTurnOutcome::failed_with_context(
                            message,
                            Some("provider_startup_timeout".to_string()),
                            Some(json!({
                                "timeout_ms": timeout_ms,
                                "runtime": runtime,
                                "provider_id": self.agent.provider_id,
                            })),
                            Some(json!("provider_startup_timeout")),
                            false,
                        ));
                        break;
                    }
                    shutdown = shutdown_rx.changed() => {
                        let reason = match shutdown {
                            Ok(()) => shutdown_rx
                                .borrow()
                                .clone()
                                .unwrap_or_else(|| "crp_shutdown".to_string()),
                            Err(_) => "crp_shutdown".to_string(),
                        };
                        let _ = req
                            .event_sink
                            .send(NormalizedEvent {
                                event_type: SessionEventType::TurnInterrupted,
                                payload_json: json!({
                                    "reason": reason,
                                    "provider_cancelled": true,
                                }),
                            })
                            .await;
                        outcome = Some(ProviderTurnOutcome::interrupted(reason, true));
                        break;
                    }
                    stderr = stderr_rx.recv() => {
                        match stderr {
                            Ok(line) => {
                                if last_seq == 0 {
                                    if let Some(startup) =
                                        handle_startup_stderr_line(&line, &session, &req.event_sink)
                                            .await?
                                    {
                                        update_terminal_outcome(
                                            &mut outcome,
                                            &startup.terminal_events,
                                            false,
                                        );
                                        outcome = Some(startup.outcome);
                                        break;
                                    }
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => {}
                            Err(broadcast::error::RecvError::Closed) => {}
                        }
                    }
                    recv = rx.recv() => {
                        match recv {
                            Ok(env) => {
                                if !event_matches_session(&env.event, &req.session_key) {
                                    continue;
                                }
                                if let Some(event_turn_id) = event_turn_id(&env.event) {
                                    if event_turn_id != turn_id {
                                        continue;
                                    }
                                }
                                if env.seq <= last_seq {
                                    continue;
                                }
                                last_seq = env.seq;
                                if is_sweep_only_status_notice(&env.event) {
                                    continue;
                                }
                                apply_session_opened_state(&session, &env.event);
                                let auth_required = matches!(
                                    &env.event,
                                    CrpEvent::Known(event)
                                        if matches!(
                                            event.as_ref(),
                                            KnownCrpEvent::SessionNotice { code, .. }
                                                if code == "auth_required"
                                        )
                                );
                                if auth_required {
                                    session.opening.store(false, Ordering::SeqCst);
                                }
                                let unknown_observation =
                                    unknown_event_observation(&env.event, env.channel, env.seq);
                                let mapped = map_crp_event(
                                    env.event,
                                    env.channel,
                                    env.seq,
                                    &mut tool_output_cache,
                                    &mut tool_input_cache,
                                );
                                if let (Some(hook), Some(observation)) =
                                    (&req.provider_unknown_event, unknown_observation)
                                {
                                    hook(observation).await;
                                }
                                update_terminal_outcome(&mut outcome, &mapped.events, mapped.done);
                                for event in mapped.events {
                                    if let Some(file) = dump_norm_file.as_mut() {
                                        let _ = writeln!(
                                            file,
                                            "{}",
                                            json!({
                                                "session_key": req.session_key,
                                                "turn_id": turn_id,
                                                "crp_seq": env.seq,
                                                "event_type": format!("{:?}", event.event_type),
                                                "payload_json": event.payload_json,
                                            })
                                        );
                                    }
                                    let _ = req.event_sink.send(event).await;
                                }
                                if auth_required {
                                    let _ = req
                                        .event_sink
                                        .send(NormalizedEvent {
                                            event_type: SessionEventType::TurnInterrupted,
                                            payload_json: json!({
                                                "reason": "auth_required",
                                            }),
                                        })
                                        .await;
                                    update_terminal_outcome(
                                        &mut outcome,
                                        &[NormalizedEvent {
                                            event_type: SessionEventType::TurnInterrupted,
                                            payload_json: json!({
                                                "reason": "auth_required",
                                            }),
                                        }],
                                        false,
                                    );
                                    break;
                                }
                                if mapped.done {
                                    break;
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => {
                                let payload = json!({
                                    "kind": "session_gap",
                                    "reason": "crp_receiver_lagged",
                                });
                                let _ = req
                                    .event_sink
                                    .send(NormalizedEvent {
                                        event_type: SessionEventType::Notice,
                                        payload_json: payload,
                                    })
                                    .await;
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                outcome = Some(ProviderTurnOutcome::protocol_violation(
                                    "provider_protocol_violation_event_stream_closed",
                                    "CRP event stream closed before the turn reported an outcome",
                                ));
                                break;
                            }
                        }
                    }
                }
            }
            Ok(outcome.unwrap_or_else(|| {
                ProviderTurnOutcome::protocol_violation(
                    "provider_protocol_violation_no_terminal_outcome",
                    "CRP prompt ended without a terminal outcome",
                )
            }))
        }
        .await;

        if req
            .env
            .get("CTX_MCP_TOKEN")
            .is_some_and(|value| !value.trim().is_empty())
        {
            session.draining.store(true, Ordering::SeqCst);
        }
        if !session.opened.load(Ordering::SeqCst) {
            session.opening.store(false, Ordering::SeqCst);
        }
        session.touch();
        self.drain_session_if_needed(&req.session_key, &session)
            .await;
        result
    }
}
