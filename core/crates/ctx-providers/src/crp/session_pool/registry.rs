use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{Context, Result};
use ctx_core::provider_policy::{CTX_CRP_LAUNCH_POLICY_ENV, CTX_CRP_LAUNCH_POLICY_FULL};

use crate::adapters::ProviderProcessInfo;

use super::super::runtime::CrpProcess;
use super::{session_shutdown_reason, state::session_is_live, CrpSession, CrpSessionPool};

fn env_has_scoped_mcp_token(env: &HashMap<String, String>) -> bool {
    env.get("CTX_MCP_TOKEN")
        .is_some_and(|value| !value.trim().is_empty())
}

fn env_is_live_session_field(key: &str) -> bool {
    // These fields are delivered through session.open/session.prompt, updated
    // through CRP commands, or handled by explicit refresh paths below. They
    // should not force a valid already-open process to restart.
    matches!(
        key,
        "CTX_AUTH_TOKEN"
            | "CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN"
            | "CTX_MCP_TOKEN"
            | "CTX_MODEL_ID"
            | "CTX_ORG_ID"
            | "CTX_POLICY_VERSION"
            | "CTX_PROVIDER_SESSION_REF"
            | "CTX_RUN_GRANT_ID"
            | "CTX_SYSTEM_PROMPT_APPEND"
    )
}

fn env_launch_signature(env: &HashMap<String, String>) -> u64 {
    let mut entries = env
        .iter()
        .filter(|(key, _)| !env_is_live_session_field(key))
        .collect::<Vec<_>>();
    entries.sort_by(|(left_key, _), (right_key, _)| left_key.cmp(right_key));

    let mut hasher = DefaultHasher::new();
    for (key, value) in entries {
        key.hash(&mut hasher);
        value.hash(&mut hasher);
    }
    hasher.finish()
}

fn env_launch_policy_signature(env: &HashMap<String, String>) -> Result<Option<String>> {
    let Some(value) = env
        .get(CTX_CRP_LAUNCH_POLICY_ENV)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    if value != CTX_CRP_LAUNCH_POLICY_FULL {
        anyhow::bail!("unsupported {CTX_CRP_LAUNCH_POLICY_ENV}: {value}");
    }
    Ok(Some(value.to_string()))
}

pub(super) struct ActivePromptGuard {
    _busy_guard: BusySessionGuard,
    session_key: String,
    active_prompts: Arc<StdMutex<HashSet<String>>>,
}

impl ActivePromptGuard {
    pub(super) fn new(
        active_prompts: Arc<StdMutex<HashSet<String>>>,
        busy_sessions: Arc<StdMutex<HashMap<String, usize>>>,
        session_key: String,
    ) -> Result<Self> {
        if let Ok(mut active) = active_prompts.lock() {
            if active.contains(&session_key) {
                anyhow::bail!("session {session_key} already has an active prompt");
            }
            active.insert(session_key.clone());
        }
        Ok(Self {
            _busy_guard: BusySessionGuard::new(busy_sessions, session_key.clone()),
            session_key,
            active_prompts,
        })
    }
}

impl Drop for ActivePromptGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active_prompts.lock() {
            active.remove(&self.session_key);
        }
    }
}

pub(super) struct BusySessionGuard {
    session_key: String,
    busy_sessions: Arc<StdMutex<HashMap<String, usize>>>,
}

impl BusySessionGuard {
    pub(super) fn new(
        busy_sessions: Arc<StdMutex<HashMap<String, usize>>>,
        session_key: String,
    ) -> Self {
        if let Ok(mut guard) = busy_sessions.lock() {
            *guard.entry(session_key.clone()).or_default() += 1;
        }
        Self {
            session_key,
            busy_sessions,
        }
    }
}

impl Drop for BusySessionGuard {
    fn drop(&mut self) {
        let Ok(mut guard) = self.busy_sessions.lock() else {
            return;
        };
        let Some(count) = guard.get_mut(&self.session_key) else {
            return;
        };
        if *count > 1 {
            *count -= 1;
            return;
        }
        guard.remove(&self.session_key);
    }
}

impl CrpSessionPool {
    pub(in crate::crp) async fn list_processes(&self) -> Vec<ProviderProcessInfo> {
        let sessions = self.sessions.lock().await;
        let mut out = Vec::new();
        for (session_id, session) in sessions.iter() {
            if let Some(pid) = session.process.pid().await {
                out.push(ProviderProcessInfo {
                    provider_id: self.agent.provider_id.clone(),
                    pid,
                    label: Some(session_id.clone()),
                });
            }
        }
        out
    }

    #[cfg(test)]
    pub(in crate::crp) async fn session_count_for_test(&self) -> usize {
        self.sessions.lock().await.len()
    }

    pub(in crate::crp::session_pool) async fn prune_dead_sessions(&self) -> usize {
        let mut guard = self.sessions.lock().await;
        let dead_session_keys = guard
            .iter()
            .filter_map(|(session_key, session)| {
                session_shutdown_reason(session).map(|_| session_key.clone())
            })
            .collect::<Vec<_>>();
        let mut removed = 0usize;
        for session_key in dead_session_keys {
            if guard.remove(&session_key).is_some() {
                removed += 1;
            }
        }
        removed
    }

    pub(in crate::crp) async fn has_session(&self, session_key: &str) -> bool {
        let sessions = self.sessions.lock().await;
        match sessions.get(session_key) {
            Some(session) => session_is_live(session),
            None => false,
        }
    }

    pub(in crate::crp) async fn require_open_session(
        &self,
        session_key: &str,
    ) -> Result<Arc<CrpSession>> {
        let sessions = self.sessions.lock().await;
        let session = sessions
            .get(session_key)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("provider session {session_key} is not live"))?;
        drop(sessions);
        if !session_is_live(&session) {
            anyhow::bail!("provider session {session_key} is not live");
        }
        if !session.opened.load(Ordering::SeqCst) {
            anyhow::bail!("provider session {session_key} is not open");
        }
        Ok(session)
    }

    pub(in crate::crp) async fn restart_immediate(&self, reason: &str) {
        let sessions = {
            let mut guard = self.sessions.lock().await;
            guard.drain().collect::<Vec<_>>()
        };
        for (session_key, session) in sessions {
            session
                .process
                .shutdown(&format!("{reason}: immediate restart ({session_key})"))
                .await;
        }
    }

    pub(in crate::crp) async fn restart_drain(&self, reason: &str) {
        let active = self.busy_session_snapshot();
        let sessions_to_kill = {
            let mut guard = self.sessions.lock().await;
            let mut to_kill = Vec::new();
            for (session_key, session) in guard.iter() {
                session.draining.store(true, Ordering::SeqCst);
                if !active.contains(session_key) {
                    to_kill.push((session_key.clone(), Arc::clone(session)));
                }
            }
            for (session_key, _) in &to_kill {
                guard.remove(session_key);
            }
            to_kill
        };

        for (session_key, session) in sessions_to_kill {
            session
                .process
                .shutdown(&format!("{reason}: drain idle ({session_key})"))
                .await;
        }
    }

    pub(in crate::crp::session_pool) fn busy_session_snapshot(&self) -> HashSet<String> {
        let Ok(guard) = self.busy_sessions.lock() else {
            return HashSet::new();
        };
        guard.keys().cloned().collect()
    }

    pub(in crate::crp::session_pool) fn pinned_session_snapshot(&self) -> HashSet<String> {
        let Ok(guard) = self.pinned_sessions.lock() else {
            return HashSet::new();
        };
        guard.iter().cloned().collect()
    }

    pub(in crate::crp) fn set_session_pinned(&self, session_key: String, pinned: bool) {
        let Ok(mut guard) = self.pinned_sessions.lock() else {
            return;
        };
        if pinned {
            guard.insert(session_key);
        } else {
            guard.remove(&session_key);
        }
    }

    pub(in crate::crp::session_pool) fn session_busy_guard(
        &self,
        session_key: String,
    ) -> BusySessionGuard {
        BusySessionGuard::new(Arc::clone(&self.busy_sessions), session_key)
    }

    pub(in crate::crp::session_pool) async fn drain_session_if_needed(
        &self,
        session_key: &str,
        session: &Arc<CrpSession>,
    ) {
        if !session.draining.load(Ordering::SeqCst) {
            return;
        }
        let should_remove = {
            let mut guard = self.sessions.lock().await;
            let Some(current) = guard.get(session_key) else {
                return;
            };
            if !Arc::ptr_eq(current, session) {
                return;
            }
            guard.remove(session_key);
            true
        };
        if should_remove {
            session
                .process
                .shutdown(&format!("drain completed ({session_key})"))
                .await;
        }
    }

    pub(in crate::crp) async fn get_or_create_session(
        &self,
        session_key: &str,
        workdir: &PathBuf,
        env: &HashMap<String, String>,
    ) -> Result<Arc<CrpSession>> {
        let needs_fresh_scoped_mcp_session = env_has_scoped_mcp_token(env);
        let launch_policy_signature = env_launch_policy_signature(env)?;
        let launch_env_signature = env_launch_signature(env);
        let replaced = {
            let mut sessions = self.sessions.lock().await;
            if let Some(existing) = sessions.get(session_key) {
                let shutdown_reason = session_shutdown_reason(existing);
                let launch_policy_changed =
                    existing.launch_policy_signature != launch_policy_signature;
                let launch_env_changed = existing.launch_env_signature != launch_env_signature;
                if !needs_fresh_scoped_mcp_session
                    && !launch_policy_changed
                    && !launch_env_changed
                    && !existing.draining.load(Ordering::SeqCst)
                    && shutdown_reason.is_none()
                {
                    existing.touch();
                    return Ok(Arc::clone(existing));
                }
                sessions
                    .remove(session_key)
                    .map(|session| (session, shutdown_reason))
            } else {
                None
            }
        };
        if let Some((existing, shutdown_reason)) = replaced {
            if shutdown_reason.is_none() {
                let reason = if needs_fresh_scoped_mcp_session {
                    format!("scoped MCP token refresh ({session_key})")
                } else if existing.launch_policy_signature != launch_policy_signature {
                    format!("CRP launch policy refresh ({session_key})")
                } else if existing.launch_env_signature != launch_env_signature {
                    format!("CRP launch environment refresh ({session_key})")
                } else {
                    format!("drain replace ({session_key})")
                };
                existing.process.shutdown(&reason).await;
            }
        }

        if self.sessions.lock().await.len() >= self.default_sweep_config.max_idle_sessions.max(1) {
            let _ = self.prune_dead_sessions().await;
        }

        let process = CrpProcess::spawn(&self.agent, workdir, env)
            .await
            .with_context(|| format!("spawning CRP runtime {}", self.agent.command))?;
        let session = Arc::new(CrpSession::new(
            process,
            self.supports_session_status,
            launch_policy_signature,
            launch_env_signature,
        ));
        let mut sessions = self.sessions.lock().await;
        sessions.insert(session_key.to_string(), Arc::clone(&session));
        Ok(session)
    }
}
