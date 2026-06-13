use std::collections::HashMap;
use std::time::Duration;

use ctx_provider_install::install_state::{
    InstallErrorCode, InstallEventLevel, InstallId, InstallInfo, InstallProgressEvent,
    InstallState, InstallStateKind, InstallTarget,
};
use tokio::sync::broadcast;

use crate::ProviderRuntime;

const INSTALL_TIMEOUT_GRACE_SECS: u64 = 90;
const INSTALL_TIMEOUT_VENV_SECS: u64 = (5 * 60) + INSTALL_TIMEOUT_GRACE_SECS;
const INSTALL_TIMEOUT_DOWNLOAD_SECS: u64 = (15 * 60) + INSTALL_TIMEOUT_GRACE_SECS;
const INSTALL_TIMEOUT_PACKAGE_MANAGER_SECS: u64 = (12 * 60) + INSTALL_TIMEOUT_GRACE_SECS;
const INSTALL_TIMEOUT_PREPARE_SECS: u64 = 5 * 60;
const INSTALL_TIMEOUT_REGISTRY_SECS: u64 = 2 * 60;
const INSTALL_TIMEOUT_DEFAULT_SECS: u64 = 20 * 60;
const PREREQUISITE_PROGRESS_STAGE_FLOOR: &str = "start";
const PREREQUISITE_PROGRESS_VISIBILITY_MS: i64 = 1_200;

#[derive(Debug, Clone)]
pub struct ProviderInstallOpsEvent {
    pub level: &'static str,
    pub name: &'static str,
    pub install_id: InstallId,
    pub provider_id: String,
    pub target: Option<InstallTarget>,
    pub state: Option<InstallStateKind>,
    pub error: Option<String>,
    pub error_code: Option<InstallErrorCode>,
    pub ok: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct FindRunningInstallOutcome {
    pub install_id: Option<InstallId>,
    pub ops_events: Vec<ProviderInstallOpsEvent>,
}

#[derive(Debug, Clone)]
pub struct StartInstallOutcome {
    pub install_id: InstallId,
    pub started_new: bool,
    pub ops_events: Vec<ProviderInstallOpsEvent>,
}

#[derive(Debug, Clone)]
pub struct CancelInstallOutcome {
    pub info: InstallInfo,
    pub ops_events: Vec<ProviderInstallOpsEvent>,
}

#[derive(Debug, Clone)]
pub struct InstallInfoOutcome {
    pub info: Option<InstallInfo>,
    pub ops_events: Vec<ProviderInstallOpsEvent>,
}

#[derive(Debug, Clone)]
pub struct InstallEventsOutcome {
    pub events: Option<Vec<InstallProgressEvent>>,
    pub ops_events: Vec<ProviderInstallOpsEvent>,
}

impl ProviderRuntime {
    pub async fn find_running_install(
        &self,
        provider_id: &str,
        target: Option<InstallTarget>,
    ) -> FindRunningInstallOutcome {
        let mut ops_events = Vec::new();
        let install_id = self
            .with_install_map(|installs| {
                find_running_install_locked(installs, provider_id, target, &mut ops_events)
            })
            .await;
        FindRunningInstallOutcome {
            install_id,
            ops_events,
        }
    }

    pub async fn start_install(
        &self,
        provider_id: String,
        target: Option<InstallTarget>,
    ) -> StartInstallOutcome {
        let _start_gate = self.install_start_gate.lock().await;
        let mut ops_events = Vec::new();
        let mut started_new = false;
        let install_id = self
            .with_install_map(|installs| {
                if let Some(existing) =
                    find_running_install_locked(installs, &provider_id, target, &mut ops_events)
                {
                    ops_events.push(ProviderInstallOpsEvent {
                        level: "info",
                        name: "provider_install_joined",
                        install_id: existing,
                        provider_id: provider_id.clone(),
                        target,
                        state: None,
                        error: None,
                        error_code: None,
                        ok: None,
                    });
                    return existing;
                }

                let install_id = InstallId::new_v4();
                let mut state = InstallState::new(provider_id.clone(), target);
                let start_event = state.canonical_start_event(install_id);
                push_install_event_locked(&mut state, start_event);
                installs.insert(install_id, state);
                started_new = true;
                ops_events.push(ProviderInstallOpsEvent {
                    level: "info",
                    name: "provider_install_started",
                    install_id,
                    provider_id: provider_id.clone(),
                    target,
                    state: None,
                    error: None,
                    error_code: None,
                    ok: None,
                });
                install_id
            })
            .await;
        StartInstallOutcome {
            install_id,
            started_new,
            ops_events,
        }
    }

    pub async fn cancel_install(&self, install_id: InstallId) -> Option<CancelInstallOutcome> {
        enum CancelInstallUpdate {
            AlreadyFinished(Box<InstallInfo>),
            Cancelled {
                provider_id: String,
                target: Option<InstallTarget>,
            },
        }

        let update = self
            .with_install_map(|installs| {
                let st = installs.get_mut(&install_id)?;
                if !matches!(st.state, InstallStateKind::Running) {
                    return Some(CancelInstallUpdate::AlreadyFinished(Box::new(
                        st.info(install_id),
                    )));
                }

                st.state = InstallStateKind::Cancelled;
                st.error = Some("Install canceled by user".to_string());
                st.error_code = Some(InstallErrorCode::Cancelled);
                st.progress_pct_override = None;
                st.info_event_override = None;
                st.info_event_override_until = None;
                st.finished_at = Some(chrono::Utc::now());

                let event = InstallProgressEvent {
                    install_id,
                    provider_id: st.provider_id.clone(),
                    target: st.target,
                    at: chrono::Utc::now(),
                    stage: "cancelled".to_string(),
                    message: "Install canceled by user".to_string(),
                    level: InstallEventLevel::Warning,
                    bytes: None,
                    total_bytes: None,
                    attempt: None,
                    error_code: Some(InstallErrorCode::Cancelled),
                };
                push_install_event_locked(st, event);
                Some(CancelInstallUpdate::Cancelled {
                    provider_id: st.provider_id.clone(),
                    target: st.target,
                })
            })
            .await?;

        match update {
            CancelInstallUpdate::AlreadyFinished(info) => Some(CancelInstallOutcome {
                info: *info,
                ops_events: Vec::new(),
            }),
            CancelInstallUpdate::Cancelled {
                provider_id,
                target,
            } => {
                let info = self.get_install_info(install_id).await.info?;
                Some(CancelInstallOutcome {
                    info,
                    ops_events: vec![ProviderInstallOpsEvent {
                        level: "info",
                        name: "provider_install_cancel_requested",
                        install_id,
                        provider_id,
                        target,
                        state: None,
                        error: None,
                        error_code: None,
                        ok: None,
                    }],
                })
            }
        }
    }

    pub async fn finish_install(
        &self,
        install_id: InstallId,
        ok: bool,
        error: Option<String>,
        error_code: Option<InstallErrorCode>,
    ) -> Option<ProviderInstallOpsEvent> {
        let update = self
            .with_install_map(|installs| {
                let st = installs.get_mut(&install_id)?;
                if !matches!(st.state, InstallStateKind::Cancelled) {
                    st.state = if ok {
                        InstallStateKind::Succeeded
                    } else {
                        InstallStateKind::Failed
                    };
                }
                if !ok || matches!(st.state, InstallStateKind::Cancelled) {
                    st.error = error.or_else(|| {
                        if matches!(st.state, InstallStateKind::Cancelled) {
                            Some("Install canceled by user".to_string())
                        } else {
                            None
                        }
                    });
                    st.error_code = error_code.or({
                        if matches!(st.state, InstallStateKind::Cancelled) {
                            Some(InstallErrorCode::Cancelled)
                        } else {
                            None
                        }
                    });
                } else {
                    st.error = None;
                    st.error_code = None;
                }
                if ok {
                    st.progress_pct = Some(100);
                }
                st.progress_pct_override = None;
                st.info_event_override = None;
                st.info_event_override_until = None;
                st.finished_at = Some(chrono::Utc::now());
                Some((
                    st.provider_id.clone(),
                    st.target,
                    st.state,
                    st.error.clone(),
                    st.error_code,
                ))
            })
            .await?;

        let (provider_id, target, state, error, error_code) = update;
        let name = match state {
            InstallStateKind::Succeeded => "provider_install_succeeded",
            InstallStateKind::Failed => "provider_install_failed",
            InstallStateKind::Cancelled => "provider_install_cancelled",
            InstallStateKind::Running => "provider_install_running",
        };
        Some(ProviderInstallOpsEvent {
            level: if matches!(state, InstallStateKind::Failed) {
                "warn"
            } else {
                "info"
            },
            name,
            install_id,
            provider_id,
            target,
            state: Some(state),
            error,
            error_code,
            ok: Some(ok),
        })
    }

    pub async fn set_install_progress_pct_override(&self, install_id: InstallId, pct: Option<u8>) {
        self.with_install_map(|installs| {
            let Some(state) = installs.get_mut(&install_id) else {
                return;
            };
            state.progress_pct_override = pct;
        })
        .await;
    }

    pub async fn get_install_sender(
        &self,
        install_id: InstallId,
    ) -> Option<broadcast::Sender<InstallProgressEvent>> {
        self.with_install_map(|installs| installs.get(&install_id).map(|s| s.tx.clone()))
            .await
    }

    pub async fn get_install_info(&self, install_id: InstallId) -> InstallInfoOutcome {
        let mut ops_events = Vec::new();
        let info = self
            .with_install_map(|installs| {
                let st = installs.get_mut(&install_id)?;
                reconcile_stale_running_install_locked(install_id, st, &mut ops_events);
                Some(st.info(install_id))
            })
            .await;
        InstallInfoOutcome { info, ops_events }
    }

    pub async fn get_install_polling_info(&self, install_id: InstallId) -> InstallInfoOutcome {
        let mut ops_events = Vec::new();
        let info = self
            .with_install_map(|installs| {
                let st = installs.get_mut(&install_id)?;
                reconcile_stale_running_install_locked(install_id, st, &mut ops_events);
                Some(st.polling_info(install_id))
            })
            .await;
        InstallInfoOutcome { info, ops_events }
    }

    pub async fn get_install_events(&self, install_id: InstallId) -> InstallEventsOutcome {
        let mut ops_events = Vec::new();
        let events = self
            .with_install_map(|installs| {
                let st = installs.get_mut(&install_id)?;
                reconcile_stale_running_install_locked(install_id, st, &mut ops_events);
                Some(st.events.iter().cloned().collect())
            })
            .await;
        InstallEventsOutcome { events, ops_events }
    }

    pub async fn is_install_cancelled(&self, install_id: InstallId) -> bool {
        self.with_install_map(|installs| {
            installs
                .get(&install_id)
                .map(|st| matches!(st.state, InstallStateKind::Cancelled))
                .unwrap_or(false)
        })
        .await
    }

    pub async fn register_install_progress_mirror(
        &self,
        source_install_id: InstallId,
        mirror_install_id: InstallId,
    ) -> bool {
        self.with_install_map(|installs| {
            let (source_provider_id, source_target, inserted, last_event) = {
                let Some(source_state) = installs.get_mut(&source_install_id) else {
                    return false;
                };
                let inserted = source_state.mirrors.insert(mirror_install_id);
                let source_provider_id = source_state.provider_id.clone();
                let source_target = source_state.target;
                let last_event = source_state.events.back().cloned();
                (source_provider_id, source_target, inserted, last_event)
            };
            let Some(mirror_state) = installs.get_mut(&mirror_install_id) else {
                return false;
            };
            if inserted {
                let source_event = last_event.unwrap_or_else(|| InstallProgressEvent {
                    install_id: source_install_id,
                    provider_id: source_provider_id.clone(),
                    target: source_target,
                    at: chrono::Utc::now(),
                    stage: "start".to_string(),
                    message: "Waiting for tracked prerequisite install to report progress"
                        .to_string(),
                    level: InstallEventLevel::Info,
                    bytes: None,
                    total_bytes: None,
                    attempt: None,
                    error_code: None,
                });
                let mirrored_event = mirrored_install_event(
                    source_install_id,
                    &source_provider_id,
                    &source_event,
                    mirror_install_id,
                    mirror_state,
                );
                set_install_info_event_override_locked(mirror_state, &mirrored_event);
                push_install_event_locked(mirror_state, mirrored_event);
            }
            true
        })
        .await
    }

    pub async fn emit_install_event(&self, install_id: InstallId, event: InstallProgressEvent) {
        self.with_install_map(|installs| {
            let Some(st) = installs.get_mut(&install_id) else {
                return;
            };
            let source_provider_id = st.provider_id.clone();
            let mirrors = st.mirrors.iter().copied().collect::<Vec<_>>();
            push_install_event_locked(st, event.clone());
            for mirror_install_id in mirrors {
                let Some(mirror_state) = installs.get_mut(&mirror_install_id) else {
                    continue;
                };
                let mirrored_event = mirrored_install_event(
                    install_id,
                    &source_provider_id,
                    &event,
                    mirror_install_id,
                    mirror_state,
                );
                set_install_info_event_override_locked(mirror_state, &mirrored_event);
                push_install_event_locked(mirror_state, mirrored_event);
            }
        })
        .await;
    }

    pub async fn update_install_start_event(
        &self,
        install_id: InstallId,
        provider_id: &str,
        target: Option<InstallTarget>,
        message: String,
        only_if_default: bool,
    ) {
        self.with_install_map(|installs| {
            let Some(install) = installs.get_mut(&install_id) else {
                return;
            };
            if only_if_default && !install.canonical_start_event_is_default() {
                return;
            }
            let _ = install.update_canonical_start_event(provider_id, target, message);
        })
        .await;
    }

    pub async fn tracked_install_ids(
        &self,
        provider_id: &str,
        target: Option<InstallTarget>,
    ) -> Vec<InstallId> {
        self.with_install_map(|installs| {
            installs
                .iter()
                .filter_map(|(id, install)| {
                    (install.provider_id == provider_id && install.target == target).then_some(*id)
                })
                .collect()
        })
        .await
    }

    pub async fn install_count(&self) -> usize {
        self.with_install_map(|installs| installs.len()).await
    }

    #[doc(hidden)]
    pub async fn insert_install_state_for_testing(
        &self,
        install_id: InstallId,
        install: InstallState,
    ) {
        self.with_install_map(|installs| {
            installs.insert(install_id, install);
        })
        .await;
    }

    async fn with_install_map<R>(
        &self,
        f: impl FnOnce(&mut HashMap<InstallId, InstallState>) -> R,
    ) -> R {
        let mut installs = self.installs.lock().await;
        f(&mut installs)
    }
}

fn push_install_event_locked(st: &mut InstallState, event: InstallProgressEvent) {
    st.progress_pct = ctx_provider_install::install_state::heuristic_progress_pct_from_event(
        &event,
        st.progress_pct,
    );
    if st.events.len() >= 256 {
        st.events.pop_front();
    }
    st.events.push_back(event.clone());
    let _ = st.tx.send(event);
}

fn set_install_info_event_override_locked(st: &mut InstallState, event: &InstallProgressEvent) {
    st.info_event_override = Some(event.clone());
    st.info_event_override_until =
        Some(event.at + chrono::Duration::milliseconds(PREREQUISITE_PROGRESS_VISIBILITY_MS));
}

fn mirrored_install_event(
    source_install_id: InstallId,
    source_provider_id: &str,
    source_event: &InstallProgressEvent,
    mirror_install_id: InstallId,
    mirror_state: &InstallState,
) -> InstallProgressEvent {
    InstallProgressEvent {
        install_id: mirror_install_id,
        provider_id: mirror_state.provider_id.clone(),
        target: mirror_state.target,
        at: chrono::Utc::now(),
        stage: PREREQUISITE_PROGRESS_STAGE_FLOOR.to_string(),
        message: format!(
            "Prerequisite {source_provider_id} (install {source_install_id}, stage {}): {}",
            source_event.stage, source_event.message
        ),
        level: source_event.level,
        bytes: None,
        total_bytes: None,
        attempt: None,
        error_code: source_event.error_code,
    }
}

fn install_running_timeout_for_stage(stage: &str) -> Duration {
    match stage {
        "download" | "node_download" | "python_download" | "model_download"
        | "runtime_download" => Duration::from_secs(INSTALL_TIMEOUT_DOWNLOAD_SECS),
        "npm_install" | "dependency_npm_install" | "pip_install" => {
            Duration::from_secs(INSTALL_TIMEOUT_PACKAGE_MANAGER_SECS)
        }
        "venv" => Duration::from_secs(INSTALL_TIMEOUT_VENV_SECS),
        "registry" | "registry_load" | "registry_save" => {
            Duration::from_secs(INSTALL_TIMEOUT_REGISTRY_SECS)
        }
        "prepare" | "extract" | "node_extract" | "python_extract" | "runtime_extract" => {
            Duration::from_secs(INSTALL_TIMEOUT_PREPARE_SECS)
        }
        _ => Duration::from_secs(INSTALL_TIMEOUT_DEFAULT_SECS),
    }
}

fn format_install_timeout(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs >= 60 {
        let mins = secs / 60;
        let rem = secs % 60;
        if rem == 0 {
            format!("{mins}m")
        } else {
            format!("{mins}m {rem}s")
        }
    } else {
        format!("{secs}s")
    }
}

fn reconcile_stale_running_install_locked(
    install_id: InstallId,
    st: &mut InstallState,
    ops_events: &mut Vec<ProviderInstallOpsEvent>,
) -> bool {
    if !matches!(st.state, InstallStateKind::Running) {
        return false;
    }
    let now = chrono::Utc::now();
    let last_event = st.events.back().cloned();
    let stage = last_event
        .as_ref()
        .map(|event| event.stage.trim())
        .filter(|stage| !stage.is_empty())
        .unwrap_or("prepare");
    let anchor = last_event
        .as_ref()
        .map(|event| event.at)
        .unwrap_or(st.started_at);
    let Ok(inactive_for) = now.signed_duration_since(anchor).to_std() else {
        return false;
    };
    let timeout_after = install_running_timeout_for_stage(stage);
    if inactive_for <= timeout_after {
        return false;
    }

    let message = format!(
        "Install timed out during {stage} after {} without progress. Retry the install.",
        format_install_timeout(inactive_for)
    );
    st.state = InstallStateKind::Failed;
    st.finished_at = Some(now);
    st.error = Some(message.clone());
    st.error_code = Some(InstallErrorCode::Timeout);
    let event = InstallProgressEvent {
        install_id,
        provider_id: st.provider_id.clone(),
        target: st.target,
        at: now,
        stage: stage.to_string(),
        message: message.clone(),
        level: InstallEventLevel::Error,
        bytes: None,
        total_bytes: None,
        attempt: None,
        error_code: Some(InstallErrorCode::Timeout),
    };
    push_install_event_locked(st, event);
    ops_events.push(ProviderInstallOpsEvent {
        level: "warn",
        name: "provider_install_failed",
        install_id,
        provider_id: st.provider_id.clone(),
        target: st.target,
        state: Some(InstallStateKind::Failed),
        error: Some(message),
        error_code: Some(InstallErrorCode::Timeout),
        ok: Some(false),
    });
    true
}

fn find_running_install_locked(
    installs: &mut HashMap<InstallId, InstallState>,
    provider_id: &str,
    target: Option<InstallTarget>,
    ops_events: &mut Vec<ProviderInstallOpsEvent>,
) -> Option<InstallId> {
    installs.iter_mut().find_map(|(id, st)| {
        let _ = reconcile_stale_running_install_locked(*id, st, ops_events);
        if st.provider_id == provider_id
            && st.target == target
            && matches!(st.state, InstallStateKind::Running)
        {
            Some(*id)
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn find_running_install_reconciles_stale_running_venv_install() {
        let runtime = ProviderRuntime::new(HashMap::new());
        let install_id = InstallId::new_v4();
        let now = chrono::Utc::now();
        let mut install = InstallState::new("mistral".to_string(), Some(InstallTarget::Container));
        install.started_at = now - chrono::Duration::minutes(9);
        install.events.push_back(InstallProgressEvent {
            install_id,
            provider_id: "mistral".to_string(),
            target: Some(InstallTarget::Container),
            at: now - chrono::Duration::minutes(8),
            stage: "venv".to_string(),
            message: "Creating virtualenv...".to_string(),
            level: InstallEventLevel::Info,
            bytes: None,
            total_bytes: None,
            attempt: None,
            error_code: None,
        });
        runtime
            .insert_install_state_for_testing(install_id, install)
            .await;

        let running = runtime
            .find_running_install("mistral", Some(InstallTarget::Container))
            .await;
        assert!(running.install_id.is_none());
        assert_eq!(running.ops_events.len(), 1);
        assert_eq!(running.ops_events[0].name, "provider_install_failed");

        let info = runtime
            .get_install_info(install_id)
            .await
            .info
            .expect("missing install info");
        assert!(matches!(info.state, InstallStateKind::Failed));
        assert_eq!(info.error_code, Some(InstallErrorCode::Timeout));
        assert!(info
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("timed out during venv"));
    }

    #[tokio::test]
    async fn get_install_info_preserves_recent_running_install() {
        let runtime = ProviderRuntime::new(HashMap::new());
        let install_id = InstallId::new_v4();
        let now = chrono::Utc::now();
        let mut install = InstallState::new("codex".to_string(), Some(InstallTarget::Container));
        install.started_at = now - chrono::Duration::minutes(1);
        install.events.push_back(InstallProgressEvent {
            install_id,
            provider_id: "codex".to_string(),
            target: Some(InstallTarget::Container),
            at: now - chrono::Duration::seconds(30),
            stage: "download".to_string(),
            message: "downloading...".to_string(),
            level: InstallEventLevel::Info,
            bytes: Some(10),
            total_bytes: Some(100),
            attempt: None,
            error_code: None,
        });
        runtime
            .insert_install_state_for_testing(install_id, install)
            .await;

        let outcome = runtime.get_install_info(install_id).await;
        let info = outcome.info.expect("missing install info");
        assert!(matches!(info.state, InstallStateKind::Running));
        assert_eq!(info.error_code, None);
        assert!(outcome.ops_events.is_empty());
    }

    #[tokio::test]
    async fn start_install_dedupes_concurrent_requests_for_same_provider_target() {
        let runtime = std::sync::Arc::new(ProviderRuntime::new(HashMap::new()));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let runtime = runtime.clone();
            tasks.push(tokio::spawn(async move {
                runtime
                    .start_install("acp-crp-bridge".to_string(), Some(InstallTarget::Container))
                    .await
            }));
        }

        let mut install_ids = Vec::new();
        let mut started_new_count = 0usize;
        for task in tasks {
            let outcome = task.await.expect("join start_install task");
            install_ids.push(outcome.install_id);
            if outcome.started_new {
                started_new_count += 1;
            }
        }

        assert_eq!(
            started_new_count, 1,
            "concurrent start_install callers must share one tracked running install"
        );
        assert!(
            install_ids
                .windows(2)
                .all(|pair| pair.first() == pair.get(1)),
            "all concurrent start_install callers should receive the same install id: {install_ids:#?}"
        );
    }
}
