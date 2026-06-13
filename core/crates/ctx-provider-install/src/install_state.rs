use std::collections::{HashSet, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

pub type InstallId = Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InstallTarget {
    Host,
    Container,
    LinuxAarch64,
    LinuxX8664,
}

impl InstallTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Container => "container",
            Self::LinuxAarch64 => "linux-aarch64",
            Self::LinuxX8664 => "linux-x86_64",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallEventLevel {
    Info,
    Warning,
    Error,
    Success,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstallErrorCode {
    InvalidTarget,
    UnsupportedTarget,
    DownloadFailed,
    ChecksumMismatch,
    CommandFailed,
    Timeout,
    MatrixMismatch,
    HealthCheckFailed,
    RegistryWriteFailed,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallProgressEvent {
    pub install_id: InstallId,
    pub provider_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<InstallTarget>,
    pub at: DateTime<Utc>,
    pub stage: String,
    pub message: String,
    pub level: InstallEventLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<InstallErrorCode>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallStateKind {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallInfo {
    pub install_id: InstallId,
    pub provider_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<InstallTarget>,
    pub state: InstallStateKind,
    pub started_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<InstallErrorCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_pct: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event: Option<InstallProgressEvent>,
}

pub struct InstallState {
    pub provider_id: String,
    pub target: Option<InstallTarget>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub state: InstallStateKind,
    pub error: Option<String>,
    pub error_code: Option<InstallErrorCode>,
    pub events: VecDeque<InstallProgressEvent>,
    pub mirrors: HashSet<InstallId>,
    pub progress_pct: Option<u8>,
    pub progress_pct_override: Option<u8>,
    pub info_event_override: Option<InstallProgressEvent>,
    pub info_event_override_until: Option<DateTime<Utc>>,
    pub tx: broadcast::Sender<InstallProgressEvent>,
}

impl InstallState {
    pub fn new(provider_id: String, target: Option<InstallTarget>) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            provider_id,
            target,
            started_at: Utc::now(),
            finished_at: None,
            state: InstallStateKind::Running,
            error: None,
            error_code: None,
            events: VecDeque::with_capacity(256),
            mirrors: HashSet::new(),
            progress_pct: None,
            progress_pct_override: None,
            info_event_override: None,
            info_event_override_until: None,
            tx,
        }
    }

    pub fn canonical_start_event(&self, install_id: InstallId) -> InstallProgressEvent {
        InstallProgressEvent {
            install_id,
            provider_id: self.provider_id.clone(),
            target: self.target,
            at: self.started_at,
            stage: "start".to_string(),
            message: format_install_start_message(&self.provider_id, self.target),
            level: InstallEventLevel::Info,
            bytes: None,
            total_bytes: None,
            attempt: None,
            error_code: None,
        }
    }

    pub fn canonical_start_event_is_default(&self) -> bool {
        let Some(event) = self.events.front() else {
            return false;
        };
        event.stage == "start"
            && event.message == format_install_start_message(&self.provider_id, self.target)
    }

    pub fn update_canonical_start_event(
        &mut self,
        provider_id: &str,
        target: Option<InstallTarget>,
        message: String,
    ) -> bool {
        let Some(event) = self.events.front_mut() else {
            return false;
        };
        if event.stage != "start" {
            return false;
        }
        event.provider_id = provider_id.to_string();
        event.target = target;
        event.message = message;
        true
    }

    pub fn info(&self, install_id: InstallId) -> InstallInfo {
        self.build_info(install_id, self.events.back().cloned())
    }

    pub fn polling_info(&self, install_id: InstallId) -> InstallInfo {
        let current_last_event = self.events.back().cloned();
        let override_visible = matches!(self.state, InstallStateKind::Running)
            && self
                .info_event_override_until
                .is_some_and(|visible_until| Utc::now() <= visible_until)
            && self.should_expose_info_event_override(current_last_event.as_ref());
        let last_event = if override_visible {
            self.info_event_override
                .clone()
                .or_else(|| current_last_event.clone())
        } else {
            current_last_event
        };
        self.build_info(install_id, last_event)
    }

    fn should_expose_info_event_override(
        &self,
        current_last_event: Option<&InstallProgressEvent>,
    ) -> bool {
        if self.info_event_override.is_none() {
            return false;
        }
        let Some(current_last_event) = current_last_event else {
            return true;
        };
        current_last_event.message.starts_with("Prerequisite ")
            || matches!(current_last_event.stage.as_str(), "start" | "prerequisites")
    }

    fn build_info(
        &self,
        install_id: InstallId,
        last_event: Option<InstallProgressEvent>,
    ) -> InstallInfo {
        InstallInfo {
            install_id,
            provider_id: self.provider_id.clone(),
            target: self.target,
            state: self.state,
            started_at: self.started_at,
            finished_at: self.finished_at,
            error: self.error.clone(),
            error_code: self.error_code,
            progress_pct: self.progress_pct_override.or(self.progress_pct),
            last_event,
        }
    }
}

fn format_install_start_message(provider_id: &str, target: Option<InstallTarget>) -> String {
    match target {
        Some(target) => format!("Starting install for {provider_id} ({})", target.as_str()),
        None => format!("Starting install for {provider_id}"),
    }
}

fn stage_progress_pct(stage: &str) -> Option<u8> {
    match stage {
        "start" => Some(2),
        "prerequisites" => Some(4),
        "dependencies" => Some(6),
        "download" => Some(10),
        "node" => Some(15),
        "node_download" => Some(18),
        "node_extract" => Some(22),
        "prepare" => Some(25),
        "venv" => Some(35),
        "npm_install" => Some(65),
        "pip_install" => Some(70),
        "extract" => Some(78),
        "entrypoint" => Some(80),
        "inspect" => Some(90),
        "refresh" => Some(95),
        "registry" => Some(98),
        "done" => Some(100),
        _ => None,
    }
}

pub fn heuristic_progress_pct_from_event(
    event: &InstallProgressEvent,
    previous_pct: Option<u8>,
) -> Option<u8> {
    if let (Some(bytes), Some(total_bytes)) = (event.bytes, event.total_bytes) {
        if total_bytes > 0 {
            let raw =
                (((bytes as f64 / total_bytes as f64) * 100.0).round() as i64).clamp(0, 100) as u8;
            if event.stage.contains("download") {
                let download_scaled =
                    (((raw as f64 / 100.0) * 75.0).round() as i64).clamp(0, 100) as u8;
                return Some(
                    previous_pct.map_or(download_scaled, |prev| prev.max(download_scaled)),
                );
            }
            return Some(previous_pct.map_or(raw, |prev| prev.max(raw)));
        }
    }
    stage_progress_pct(&event.stage)
        .map(|candidate| previous_pct.map_or(candidate, |prev| prev.max(candidate)))
        .or(previous_pct)
}

pub fn truncate_for_storage(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    let mut out = s.chars().take(max_len).collect::<String>();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polling_info_surfaces_real_start_event_for_running_installs() {
        let install_id = InstallId::new_v4();
        let mut state =
            InstallState::new("acp-crp-bridge".to_string(), Some(InstallTarget::Container));
        state
            .events
            .push_back(state.canonical_start_event(install_id));

        let info = state.polling_info(install_id);

        assert!(matches!(info.state, InstallStateKind::Running));
        assert_eq!(
            info.last_event.as_ref().map(|event| event.stage.as_str()),
            Some("start")
        );
        assert_eq!(
            info.last_event.as_ref().map(|event| event.message.as_str()),
            Some("Starting install for acp-crp-bridge (container)")
        );
    }

    #[test]
    fn polling_info_prefers_real_progress_events_over_start_event() {
        let install_id = InstallId::new_v4();
        let mut state =
            InstallState::new("acp-crp-bridge".to_string(), Some(InstallTarget::Container));
        state
            .events
            .push_back(state.canonical_start_event(install_id));
        state.events.push_back(InstallProgressEvent {
            install_id,
            provider_id: "acp-crp-bridge".to_string(),
            target: Some(InstallTarget::Container),
            at: Utc::now(),
            stage: "download".to_string(),
            message: "Downloading bridge".to_string(),
            level: InstallEventLevel::Info,
            bytes: Some(32),
            total_bytes: Some(64),
            attempt: Some(1),
            error_code: None,
        });

        let info = state.polling_info(install_id);

        assert_eq!(
            info.last_event.as_ref().map(|event| event.stage.as_str()),
            Some("download")
        );
    }
}
