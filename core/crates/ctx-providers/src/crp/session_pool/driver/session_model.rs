use std::sync::Arc;

use anyhow::Result;
use tokio::sync::broadcast;

use super::super::super::normalize::event_matches_session;
use super::super::super::protocol::{CrpCommand, CrpEvent, KnownCrpEvent};
use super::super::CrpSessionPool;

const CRP_SESSION_MODEL_UPDATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl CrpSessionPool {
    pub(in crate::crp) async fn set_session_model(
        self: &Arc<Self>,
        session_key: String,
        model_id: String,
    ) -> Result<()> {
        let busy_guard = self.session_busy_guard(session_key.clone());
        let session = self.require_open_session(&session_key).await?;
        session.touch();
        let mut rx = session.process.events.subscribe();
        let mut shutdown_rx = session.process.shutdown.subscribe();
        if let Err(err) = session
            .process
            .send(CrpCommand::SessionSetModel {
                session_id: Some(session_key.clone()),
                model_id: Some(model_id.clone()),
            })
            .await
        {
            drop(busy_guard);
            self.drain_session_if_needed(&session_key, &session).await;
            self.trigger_background_reap();
            return Err(err);
        }

        let result = tokio::time::timeout(CRP_SESSION_MODEL_UPDATE_TIMEOUT, async {
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        let reason = shutdown_rx.borrow().clone().unwrap_or_else(|| "crp_shutdown".to_string());
                        anyhow::bail!("CRP runtime shut down while setting model: {reason}");
                    }
                    recv = rx.recv() => {
                        match recv {
                            Ok(env) => {
                                if !event_matches_session(&env.event, &session_key) {
                                    continue;
                                }
                                if let CrpEvent::Known(event) = env.event {
                                    if let KnownCrpEvent::SessionNotice { code, message, details, .. } = *event {
                                        if code == "session_model_updated" {
                                            let selected = details
                                                .as_ref()
                                                .and_then(|value| value.get("model_id"))
                                                .and_then(|value| value.as_str())
                                                .unwrap_or(model_id.as_str());
                                            if selected == model_id {
                                                return Ok(());
                                            }
                                        }
                                        if code == "session_model_update_failed" {
                                            let detail = message.unwrap_or_else(|| {
                                                format!("provider rejected session model '{model_id}'")
                                            });
                                            anyhow::bail!("{detail}");
                                        }
                                    }
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => {}
                            Err(broadcast::error::RecvError::Closed) => {
                                anyhow::bail!("CRP runtime closed while waiting for session model update");
                            }
                        }
                    }
                }
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for session model update"));
        drop(busy_guard);
        self.drain_session_if_needed(&session_key, &session).await;
        self.trigger_background_reap();
        result??;
        Ok(())
    }
}
