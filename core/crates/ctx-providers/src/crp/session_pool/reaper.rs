use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::sync::broadcast;

use crate::adapters::ProviderSessionSweepStats;

use super::super::normalize::event_matches_session;
use super::super::protocol::{CrpCommand, CrpEvent, KnownCrpEvent};
use super::state::{CrpSessionStatusDetails, SessionSnapshot};
use super::{CrpSession, CrpSessionPool};

const CRP_SESSION_STATUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl CrpSessionPool {
    pub(in crate::crp) fn trigger_background_reap(self: &Arc<Self>) {
        self.reap_requested.store(true, Ordering::SeqCst);
        if self.reap_in_flight.swap(true, Ordering::SeqCst) {
            return;
        }

        let pool = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                pool.reap_requested.store(false, Ordering::SeqCst);
                let _ = pool.reap_idle_sessions(pool.default_sweep_config).await;
                if pool.reap_requested.swap(false, Ordering::SeqCst) {
                    continue;
                }
                pool.reap_in_flight.store(false, Ordering::SeqCst);
                if pool.reap_requested.swap(false, Ordering::SeqCst)
                    && !pool.reap_in_flight.swap(true, Ordering::SeqCst)
                {
                    continue;
                }
                break;
            }
        });
    }

    pub(in crate::crp) async fn reap_idle_sessions(
        &self,
        config: crate::adapters::ProviderSessionSweepConfig,
    ) -> ProviderSessionSweepStats {
        let active = self.busy_session_snapshot();
        let pinned = self.pinned_session_snapshot();
        let now = Instant::now();
        let sessions = {
            let guard = self.sessions.lock().await;
            guard
                .iter()
                .map(|(session_key, session)| SessionSnapshot {
                    session_key: session_key.clone(),
                    session: Arc::clone(session),
                    last_used: session.last_used(),
                    draining: session.draining.load(Ordering::SeqCst),
                    shutdown_reason: super::session_shutdown_reason(session),
                })
                .collect::<Vec<_>>()
        };

        let mut stats = ProviderSessionSweepStats::default();
        let mut dead_candidates = Vec::new();
        let mut idle_candidates = Vec::new();
        for snapshot in sessions {
            if snapshot.shutdown_reason.is_some() {
                dead_candidates.push(snapshot);
                continue;
            }
            if snapshot.draining {
                continue;
            }
            if active.contains(&snapshot.session_key) {
                continue;
            }
            if pinned.contains(&snapshot.session_key) {
                continue;
            }
            idle_candidates.push(snapshot);
        }

        if !dead_candidates.is_empty() {
            let mut guard = self.sessions.lock().await;
            for candidate in dead_candidates {
                let should_remove = matches!(
                    guard.get(&candidate.session_key),
                    Some(current)
                        if Arc::ptr_eq(current, &candidate.session)
                            && super::session_shutdown_reason(current).is_some()
                );
                if should_remove && guard.remove(&candidate.session_key).is_some() {
                    stats.dead_removed += 1;
                }
            }
        }

        idle_candidates.sort_by_key(|candidate| candidate.last_used);
        let mut to_reap = Vec::new();
        let mut remaining_idle = idle_candidates.len();
        for candidate in idle_candidates {
            let ttl_expired = now.duration_since(candidate.last_used) >= config.idle_ttl;
            let cap_expired = remaining_idle > config.max_idle_sessions;
            if !ttl_expired && !cap_expired {
                break;
            }
            if !candidate.session.opened.load(Ordering::SeqCst)
                && !candidate.session.opening.load(Ordering::SeqCst)
            {
                to_reap.push(candidate);
                remaining_idle = remaining_idle.saturating_sub(1);
                continue;
            }
            if !candidate.session.status_supported.load(Ordering::SeqCst) {
                continue;
            }

            match self
                .query_session_status(&candidate.session_key, &candidate.session)
                .await
            {
                Ok(status) if status.quiescent => {
                    to_reap.push(candidate);
                    remaining_idle = remaining_idle.saturating_sub(1);
                }
                Ok(_) => stats.skipped_busy += 1,
                Err(_) => stats.status_errors += 1,
            }
        }

        for candidate in to_reap {
            if self
                .busy_session_snapshot()
                .contains(&candidate.session_key)
            {
                continue;
            }
            let removed = {
                let mut guard = self.sessions.lock().await;
                match guard.get(&candidate.session_key) {
                    Some(current)
                        if Arc::ptr_eq(current, &candidate.session)
                            && !current.draining.load(Ordering::SeqCst)
                            && current.last_used() == candidate.last_used =>
                    {
                        guard.remove(&candidate.session_key);
                        true
                    }
                    _ => false,
                }
            };
            if removed {
                candidate
                    .session
                    .process
                    .shutdown(&format!("idle session reap ({})", candidate.session_key))
                    .await;
                stats.reaped += 1;
            }
        }

        stats
    }

    async fn query_session_status(
        &self,
        session_key: &str,
        session: &Arc<CrpSession>,
    ) -> Result<CrpSessionStatusDetails> {
        let mut rx = session.process.events.subscribe();
        let mut shutdown_rx = session.process.shutdown.subscribe();
        session
            .process
            .send(CrpCommand::SessionStatus {
                session_id: Some(session_key.to_string()),
            })
            .await?;

        tokio::time::timeout(CRP_SESSION_STATUS_TIMEOUT, async {
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        let reason = shutdown_rx.borrow().clone().unwrap_or_else(|| "crp_shutdown".to_string());
                        anyhow::bail!("CRP runtime shut down while querying session status: {reason}");
                    }
                    recv = rx.recv() => {
                        match recv {
                            Ok(env) => {
                                if !event_matches_session(&env.event, session_key) {
                                    continue;
                                }
                                if let CrpEvent::Known(event) = env.event {
                                    match *event {
                                        KnownCrpEvent::SessionNotice { code, details, .. }
                                            if code == "session_status" =>
                                        {
                                            let details = details.ok_or_else(|| {
                                                anyhow::anyhow!("session_status notice missing details")
                                            })?;
                                            return serde_json::from_value::<CrpSessionStatusDetails>(
                                                details,
                                            )
                                            .context("parsing session status details");
                                        }
                                        KnownCrpEvent::SessionNotice { code, message, .. }
                                            if code == "session_status_failed" =>
                                        {
                                            anyhow::bail!(message.unwrap_or_else(|| {
                                                "session status query failed".to_string()
                                            }));
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => {}
                            Err(broadcast::error::RecvError::Closed) => {
                                anyhow::bail!("CRP runtime closed while waiting for session status");
                            }
                        }
                    }
                }
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for session status"))?
    }
}
