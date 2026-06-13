use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Result;
use serde_json::json;
use tokio::sync::broadcast;

use ctx_core::models::SessionEventType;

use crate::events::NormalizedEvent;

use super::super::super::auth_required_notice_payload_from_stderr;
use super::super::super::normalize::{
    event_matches_session, map_crp_event, unknown_event_observation, CachedToolInput,
};
use super::super::super::policy::{
    extract_auth_error_from_stderr_line, extract_auth_url_from_stderr_line,
};
use super::super::super::protocol::{CrpCommand, CrpEvent, KnownCrpEvent};
use super::super::open_handshake::{apply_session_opened_state, AuthSessionOpenRequest};
use super::super::CrpSessionPool;
use super::terminal::is_sweep_only_status_notice;

const CRP_AUTH_EVENT_FORWARD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60 * 10);

pub(in crate::crp) struct CrpAuthenticateSessionRequest {
    pub(in crate::crp) session_key: String,
    pub(in crate::crp) workdir: std::path::PathBuf,
    pub(in crate::crp) env: HashMap<String, String>,
    pub(in crate::crp) method_id: Option<String>,
    pub(in crate::crp) event_sink: tokio::sync::mpsc::Sender<NormalizedEvent>,
    pub(in crate::crp) provider_unknown_event: Option<crate::adapters::ProviderUnknownEventHook>,
    pub(in crate::crp) provider_session_ref_claim:
        Option<crate::adapters::ProviderSessionRefClaimHook>,
}

impl CrpSessionPool {
    pub(in crate::crp) async fn authenticate_session(
        self: &Arc<Self>,
        request: CrpAuthenticateSessionRequest,
    ) -> Result<()> {
        let CrpAuthenticateSessionRequest {
            session_key,
            workdir,
            env,
            method_id,
            event_sink,
            provider_unknown_event,
            provider_session_ref_claim,
        } = request;
        let busy_guard = self.session_busy_guard(session_key.clone());
        let session = self
            .get_or_create_session(&session_key, &workdir, &env)
            .await?;
        let mut rx = session.process.events.subscribe();
        let mut stderr_rx = session.process.stderr_lines.subscribe();
        let mut shutdown_rx = session.process.shutdown.subscribe();
        let auth_session_key = session_key.clone();
        let mut drain_after_auth = false;
        if !session.opened.load(Ordering::SeqCst) && !session.opening.load(Ordering::SeqCst) {
            match self
                .ensure_auth_session_open(AuthSessionOpenRequest {
                    session_key: &session_key,
                    session: &session,
                    workdir: &workdir,
                    env: &env,
                    event_sink: &event_sink,
                    provider_session_ref_claim: provider_session_ref_claim.as_ref(),
                    rx: &mut rx,
                    stderr_rx: &mut stderr_rx,
                    shutdown_rx: &mut shutdown_rx,
                })
                .await
            {
                Ok(outcome) => {
                    drain_after_auth = outcome.drain_after_auth;
                }
                Err(err) => {
                    session.opening.store(false, Ordering::SeqCst);
                    session.draining.store(true, Ordering::SeqCst);
                    drop(busy_guard);
                    self.drain_session_if_needed(&auth_session_key, &session)
                        .await;
                    self.trigger_background_reap();
                    return Err(err);
                }
            }
        }
        if let Err(err) = session
            .process
            .send(CrpCommand::SessionAuthenticate {
                session_id: Some(session_key),
                method_id,
            })
            .await
        {
            if !session.opened.load(Ordering::SeqCst) {
                session.opening.store(false, Ordering::SeqCst);
            }
            session.draining.store(true, Ordering::SeqCst);
            drop(busy_guard);
            self.drain_session_if_needed(&auth_session_key, &session)
                .await;
            self.trigger_background_reap();
            return Err(err);
        }
        let session_for_events = Arc::clone(&session);
        let pool_for_reap = Arc::clone(self);
        tokio::spawn(async move {
            let _busy_guard = busy_guard;
            let deadline = tokio::time::Instant::now() + CRP_AUTH_EVENT_FORWARD_TIMEOUT;
            let mut last_seq = 0u64;
            let mut tool_output_cache: HashMap<String, String> = HashMap::new();
            let mut tool_input_cache: HashMap<String, CachedToolInput> = HashMap::new();
            'auth_forward: loop {
                let now = tokio::time::Instant::now();
                if now >= deadline {
                    break;
                }
                let timeout_remaining = deadline.saturating_duration_since(now);
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        break;
                    }
                    _ = tokio::time::sleep(timeout_remaining) => {
                        break;
                    }
                    recv = rx.recv() => {
                        match recv {
                            Ok(env) => {
                                if !event_matches_session(&env.event, &auth_session_key) {
                                    continue;
                                }
                                if env.seq <= last_seq {
                                    continue;
                                }
                                last_seq = env.seq;
                                if is_sweep_only_status_notice(&env.event) {
                                    continue;
                                }
                                apply_session_opened_state(&session_for_events, &env.event);
                                let auth_terminal_event = matches!(
                                    &env.event,
                                    CrpEvent::Known(event)
                                        if matches!(
                                            event.as_ref(),
                                            KnownCrpEvent::SessionNotice { code, .. }
                                                if code == "auth_complete"
                                                    || code == "auth_completed"
                                                    || code == "auth_success"
                                                    || code == "authenticated"
                                                    || code == "auth_failed"
                                                    || code == "auth_error"
                                        )
                                );
                                if auth_terminal_event {
                                    session_for_events.opening.store(false, Ordering::SeqCst);
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
                                    (&provider_unknown_event, unknown_observation)
                                {
                                    hook(observation).await;
                                }
                                for event in mapped.events {
                                    if event_sink.send(event).await.is_err() {
                                        break 'auth_forward;
                                    }
                                }
                                if auth_terminal_event {
                                    break;
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => {
                                let _ = event_sink
                                    .send(NormalizedEvent {
                                        event_type: SessionEventType::Notice,
                                        payload_json: json!({
                                            "kind": "session_gap",
                                            "reason": "crp_receiver_lagged",
                                        }),
                                    })
                                    .await;
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                break;
                            }
                        }
                    }
                    stderr = stderr_rx.recv() => {
                        match stderr {
                            Ok(line) => {
                                if let Some(auth_url) = extract_auth_url_from_stderr_line(&line) {
                                    if event_sink
                                        .send(NormalizedEvent {
                                            event_type: SessionEventType::Notice,
                                            payload_json: auth_required_notice_payload_from_stderr(
                                                &auth_url,
                                            ),
                                        })
                                        .await
                                        .is_err()
                                    {
                                        break 'auth_forward;
                                    }
                                }
                                if let Some(message) = extract_auth_error_from_stderr_line(&line) {
                                    let _ = event_sink
                                        .send(NormalizedEvent {
                                            event_type: SessionEventType::Notice,
                                            payload_json: json!({
                                                "kind": "auth_error",
                                                "message": message,
                                                "source": "crp_stderr",
                                            }),
                                        })
                                        .await;
                                    break;
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => {}
                            Err(broadcast::error::RecvError::Closed) => {
                                break;
                            }
                        }
                    }
                }
            }
            if !session_for_events.opened.load(Ordering::SeqCst) {
                session_for_events.opening.store(false, Ordering::SeqCst);
            }
            if drain_after_auth {
                session_for_events.draining.store(true, Ordering::SeqCst);
            }
            drop(_busy_guard);
            pool_for_reap
                .drain_session_if_needed(&auth_session_key, &session_for_events)
                .await;
            pool_for_reap.trigger_background_reap();
        });
        Ok(())
    }
}
