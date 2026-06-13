use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{broadcast, mpsc, watch};

use crate::adapters::{ProviderSessionRefClaim, ProviderSessionRefClaimHook};
use crate::container_exec::container_exec_spec;
use crate::events::NormalizedEvent;

use super::super::config::{build_crp_auth_session_config, build_crp_session_config};
use super::super::normalize::{event_matches_session, map_crp_event, CachedToolInput};
use super::super::policy::extract_runtime_fatal_error_from_stderr_line;
use super::super::protocol::{
    CrpCommand, CrpEvent, CrpEventEnvelope, CrpSessionConfig, KnownCrpEvent,
};
use super::{AuthSessionOpenMode, CrpSession, CrpSessionPool};

const CRP_FIRST_EVENT_TIMEOUT_HOST: std::time::Duration = std::time::Duration::from_secs(15);
const CRP_FIRST_EVENT_TIMEOUT_CONTAINER: std::time::Duration = std::time::Duration::from_secs(120);
const CRP_FIRST_EVENT_TIMEOUT_EXACT_RESUME: std::time::Duration =
    std::time::Duration::from_secs(120);
const CRP_FIRST_EVENT_TIMEOUT_ENV: &str = "CTX_CRP_FIRST_EVENT_TIMEOUT_MS";

pub(super) fn apply_session_opened_state(session: &CrpSession, event: &CrpEvent) {
    if let CrpEvent::Known(event) = event {
        let KnownCrpEvent::SessionOpened {
            supports_session_status,
            ..
        } = event.as_ref()
        else {
            return;
        };
        session.opened.store(true, Ordering::SeqCst);
        session.opening.store(false, Ordering::SeqCst);
        let default_support = session.status_supported.load(Ordering::SeqCst);
        session.status_supported.store(
            (*supports_session_status).unwrap_or(default_support),
            Ordering::SeqCst,
        );
    }
}

pub(super) fn session_opened_provider_session_id(event: &CrpEvent) -> Option<Option<String>> {
    let CrpEvent::Known(event) = event else {
        return None;
    };
    let KnownCrpEvent::SessionOpened {
        provider_session_id,
        ..
    } = event.as_ref()
    else {
        return None;
    };
    Some(provider_session_id.clone())
}

pub(super) async fn validate_provider_session_open(
    requested_provider_session_ref: Option<&str>,
    returned_provider_session_ref: Option<String>,
    claim_hook: Option<&ProviderSessionRefClaimHook>,
) -> Result<()> {
    if let Some(requested) = requested_provider_session_ref {
        if returned_provider_session_ref.as_deref() != Some(requested) {
            anyhow::bail!(
                "provider session resume mismatch: requested provider ref `{}` but runtime opened `{}`",
                requested,
                returned_provider_session_ref
                    .as_deref()
                    .unwrap_or("<none>")
            );
        }
    }

    if let Some(hook) = claim_hook {
        hook(ProviderSessionRefClaim {
            requested_provider_session_ref: requested_provider_session_ref.map(str::to_string),
            returned_provider_session_ref,
        })
        .await?;
    }

    Ok(())
}

pub(super) fn crp_first_event_timeout(env: &HashMap<String, String>) -> std::time::Duration {
    let default = if container_exec_spec(env).is_some() {
        CRP_FIRST_EVENT_TIMEOUT_CONTAINER
    } else if env
        .get("CTX_PROVIDER_SESSION_REF")
        .is_some_and(|value| !value.trim().is_empty())
    {
        CRP_FIRST_EVENT_TIMEOUT_EXACT_RESUME
    } else {
        CRP_FIRST_EVENT_TIMEOUT_HOST
    };
    let configured = env
        .get(CRP_FIRST_EVENT_TIMEOUT_ENV)
        .cloned()
        .or_else(|| std::env::var(CRP_FIRST_EVENT_TIMEOUT_ENV).ok());
    configured
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(std::time::Duration::from_millis)
        .unwrap_or(default)
}

pub(super) fn crp_runtime_label(env: &HashMap<String, String>) -> &'static str {
    if container_exec_spec(env).is_some() {
        "container"
    } else {
        "host"
    }
}

pub(super) fn duration_millis_u64(duration: std::time::Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

pub(in crate::crp::session_pool) struct AuthSessionOpenOutcome {
    pub drain_after_auth: bool,
}

pub(in crate::crp::session_pool) struct AuthSessionOpenRequest<'a> {
    pub session_key: &'a str,
    pub session: &'a Arc<CrpSession>,
    pub workdir: &'a Path,
    pub env: &'a HashMap<String, String>,
    pub event_sink: &'a mpsc::Sender<NormalizedEvent>,
    pub provider_session_ref_claim: Option<&'a ProviderSessionRefClaimHook>,
    pub rx: &'a mut broadcast::Receiver<CrpEventEnvelope>,
    pub stderr_rx: &'a mut broadcast::Receiver<String>,
    pub shutdown_rx: &'a mut watch::Receiver<Option<String>>,
}

impl CrpSessionPool {
    pub(super) async fn send_session_open(
        &self,
        session: &Arc<CrpSession>,
        session_key: &str,
        provider_session_id: Option<String>,
        config: CrpSessionConfig,
    ) -> Result<()> {
        session.opening.store(true, Ordering::SeqCst);
        if let Err(err) = session
            .process
            .send(CrpCommand::SessionOpen {
                session_id: Some(session_key.to_string()),
                provider_session_id,
                config: Some(config),
            })
            .await
        {
            session.opening.store(false, Ordering::SeqCst);
            return Err(err);
        }
        Ok(())
    }

    pub(in crate::crp::session_pool) async fn ensure_auth_session_open(
        self: &Arc<Self>,
        request: AuthSessionOpenRequest<'_>,
    ) -> Result<AuthSessionOpenOutcome> {
        let AuthSessionOpenRequest {
            session_key,
            session,
            workdir,
            env,
            event_sink,
            provider_session_ref_claim,
            rx,
            stderr_rx,
            shutdown_rx,
        } = request;
        let auth_session_open_mode = self.auth_session_open_mode;
        let config = match auth_session_open_mode {
            AuthSessionOpenMode::Standard => build_crp_session_config(env, workdir)?,
            AuthSessionOpenMode::OmitMcpThenDrain => build_crp_auth_session_config(env, workdir)?,
        };
        let provider_session_id = env
            .get("CTX_PROVIDER_SESSION_REF")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self.send_session_open(session, session_key, provider_session_id.clone(), config)
            .await?;

        let first_event_timeout = crp_first_event_timeout(env);
        let first_event_deadline = tokio::time::Instant::now() + first_event_timeout;
        let mut last_seq = 0u64;
        let mut tool_output_cache: HashMap<String, String> = HashMap::new();
        let mut tool_input_cache: HashMap<String, CachedToolInput> = HashMap::new();
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(first_event_deadline) => {
                    session.opening.store(false, Ordering::SeqCst);
                    anyhow::bail!("CRP runtime did not emit session.opened before authentication");
                }
                shutdown = shutdown_rx.changed() => {
                    let reason = match shutdown {
                        Ok(()) => shutdown_rx
                            .borrow()
                            .clone()
                            .unwrap_or_else(|| "crp_shutdown".to_string()),
                        Err(_) => "crp_shutdown".to_string(),
                    };
                    anyhow::bail!("CRP runtime shut down before authentication: {reason}");
                }
                stderr = stderr_rx.recv() => {
                    match stderr {
                        Ok(line) => {
                            if let Some(message) = extract_runtime_fatal_error_from_stderr_line(&line) {
                                session.process.shutdown("crp_runtime_fatal_stderr").await;
                                anyhow::bail!("{message}");
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
                            anyhow::bail!("CRP event stream closed before session.opened");
                        }
                    };
                    if !event_matches_session(&env.event, session_key) {
                        continue;
                    }
                    if env.seq <= last_seq {
                        continue;
                    }
                    last_seq = env.seq;
                    if let Some(returned_provider_session_ref) =
                        session_opened_provider_session_id(&env.event)
                    {
                        if auth_session_open_mode == AuthSessionOpenMode::Standard {
                            if let Err(err) = validate_provider_session_open(
                                provider_session_id.as_deref(),
                                returned_provider_session_ref,
                                provider_session_ref_claim,
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
                        }
                        apply_session_opened_state(session, &env.event);
                        if auth_session_open_mode == AuthSessionOpenMode::Standard {
                            let mapped = map_crp_event(
                                env.event,
                                env.channel,
                                env.seq,
                                &mut tool_output_cache,
                                &mut tool_input_cache,
                            );
                            for event in mapped.events {
                                if event_sink.send(event).await.is_err() {
                                    break;
                                }
                            }
                        }
                        return Ok(AuthSessionOpenOutcome {
                            drain_after_auth: auth_session_open_mode
                                == AuthSessionOpenMode::OmitMcpThenDrain,
                        });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crp_first_event_timeout_uses_container_cold_start_budget() {
        let mut env = HashMap::new();
        assert_eq!(crp_first_event_timeout(&env), CRP_FIRST_EVENT_TIMEOUT_HOST);

        env.insert(
            "CTX_HARNESS_CONTAINER_ID".to_string(),
            "ctx-harness-test".to_string(),
        );
        assert_eq!(
            crp_first_event_timeout(&env),
            CRP_FIRST_EVENT_TIMEOUT_CONTAINER
        );
        assert_eq!(
            CRP_FIRST_EVENT_TIMEOUT_CONTAINER,
            std::time::Duration::from_secs(120)
        );
    }

    #[test]
    fn crp_first_event_timeout_uses_longer_host_budget_for_exact_resume() {
        let env = HashMap::from([(
            "CTX_PROVIDER_SESSION_REF".to_string(),
            "provider-thread-1".to_string(),
        )]);
        assert_eq!(
            crp_first_event_timeout(&env),
            CRP_FIRST_EVENT_TIMEOUT_EXACT_RESUME
        );
        assert_eq!(
            CRP_FIRST_EVENT_TIMEOUT_EXACT_RESUME,
            std::time::Duration::from_secs(120)
        );
    }

    #[test]
    fn crp_first_event_timeout_env_override_still_wins() {
        let mut env = HashMap::from([
            (
                "CTX_HARNESS_CONTAINER_ID".to_string(),
                "ctx-harness-test".to_string(),
            ),
            (
                "CTX_CRP_FIRST_EVENT_TIMEOUT_MS".to_string(),
                "2500".to_string(),
            ),
        ]);
        assert_eq!(
            crp_first_event_timeout(&env),
            std::time::Duration::from_millis(2500)
        );

        env.insert(
            "CTX_CRP_FIRST_EVENT_TIMEOUT_MS".to_string(),
            "0".to_string(),
        );
        assert_eq!(
            crp_first_event_timeout(&env),
            CRP_FIRST_EVENT_TIMEOUT_CONTAINER
        );
    }
}
