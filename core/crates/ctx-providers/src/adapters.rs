use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::oneshot;
use tokio::task::AbortHandle;

use ctx_core::boolish::parse_boolish;
use ctx_core::models::MessageAttachment;

use crate::events::{NormalizedEvent, ProviderUnknownEventObservation};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub stream_events: bool,
    pub stream_format: String,

    pub has_turn_boundaries: bool,
    pub has_tool_call_ids: bool,
    pub has_file_change_events: bool,
    pub has_command_events: bool,

    pub supports_resume: bool,
    pub supports_stable_session_id: bool,
    pub supports_fork_or_rewind: bool,

    pub supports_headless: bool,
    pub supports_server_mode: bool,
    pub supports_interactive_tui: bool,

    pub supports_private_state_dir: bool,

    pub supports_sandbox_flags: bool,
    pub supports_approval_flags: bool,

    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderHealth {
    Ok,
    Missing,
    Misconfigured,
    UnsupportedVersion,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUsabilityStatus {
    Ready,
    Installable,
    #[default]
    Blocked,
    Unsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRecommendedAction {
    #[default]
    None,
    Install,
    ResolveDependency,
    ConfigureRuntime,
    SwitchTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ProviderUsability {
    pub usable: bool,
    pub status: ProviderUsabilityStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocking_provider_ids: Vec<String>,
    #[serde(default)]
    pub recommended_action: ProviderRecommendedAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub provider_id: String,
    pub installed: bool,
    pub detected_path: Option<String>,
    pub version: Option<String>,
    pub capabilities: Option<ProviderCapabilities>,
    pub health: ProviderHealth,
    pub diagnostics: Vec<String>,
    #[serde(default)]
    pub details: HashMap<String, String>,
    #[serde(default)]
    pub usability: ProviderUsability,
}

impl ProviderStatus {
    pub fn detail_flag(&self, key: &str) -> Option<bool> {
        self.details.get(key).and_then(|value| parse_boolish(value))
    }

    pub fn is_usable(&self) -> bool {
        self.usability.usable
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderProcessInfo {
    pub provider_id: String,
    pub pid: u32,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRestartMode {
    Immediate,
    Drain,
}

impl ProviderRestartMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Immediate => "immediate",
            Self::Drain => "drain",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TurnInput {
    pub content: String,
    pub attachments: Vec<MessageAttachment>,
    pub context_blocks: Vec<Value>,
    pub model_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProviderSessionRefClaim {
    pub requested_provider_session_ref: Option<String>,
    pub returned_provider_session_ref: Option<String>,
}

pub type ProviderSessionRefClaimFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

pub type ProviderSessionRefClaimHook =
    Arc<dyn Fn(ProviderSessionRefClaim) -> ProviderSessionRefClaimFuture + Send + Sync>;

pub type ProviderUnknownEventFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

pub type ProviderUnknownEventHook =
    Arc<dyn Fn(ProviderUnknownEventObservation) -> ProviderUnknownEventFuture + Send + Sync>;

#[derive(Clone, Default)]
pub struct ProviderRunHooks {
    pub provider_session_ref_claim: Option<ProviderSessionRefClaimHook>,
    pub provider_unknown_event: Option<ProviderUnknownEventHook>,
}

impl std::fmt::Debug for ProviderRunHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRunHooks")
            .field(
                "provider_session_ref_claim",
                &self.provider_session_ref_claim.is_some(),
            )
            .field(
                "provider_unknown_event",
                &self.provider_unknown_event.is_some(),
            )
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderTurnStatus {
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderTurnOutcome {
    pub status: ProviderTurnStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_cancelled: Option<bool>,
    #[serde(default)]
    pub terminal_event_emitted: bool,
}

impl ProviderTurnOutcome {
    pub fn completed() -> Self {
        Self {
            status: ProviderTurnStatus::Completed,
            message: None,
            reason: None,
            details: None,
            kind: None,
            provider_cancelled: None,
            terminal_event_emitted: false,
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            status: ProviderTurnStatus::Failed,
            message: Some(message.into()),
            reason: None,
            details: None,
            kind: None,
            provider_cancelled: None,
            terminal_event_emitted: true,
        }
    }

    pub fn failed_with_context(
        message: impl Into<String>,
        reason: Option<String>,
        details: Option<Value>,
        kind: Option<Value>,
        terminal_event_emitted: bool,
    ) -> Self {
        Self {
            status: ProviderTurnStatus::Failed,
            message: Some(message.into()),
            reason,
            details,
            kind,
            provider_cancelled: None,
            terminal_event_emitted,
        }
    }

    pub fn interrupted(reason: impl Into<String>, provider_cancelled: bool) -> Self {
        Self {
            status: ProviderTurnStatus::Interrupted,
            message: None,
            reason: Some(reason.into()),
            details: None,
            kind: None,
            provider_cancelled: Some(provider_cancelled),
            terminal_event_emitted: true,
        }
    }

    pub fn protocol_violation(reason: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: ProviderTurnStatus::Failed,
            message: Some(message.into()),
            reason: Some(reason.into()),
            details: None,
            kind: Some(json!("provider_protocol_violation")),
            provider_cancelled: None,
            terminal_event_emitted: false,
        }
    }
}

#[derive(Debug)]
pub struct RunHandle {
    pub done: oneshot::Receiver<()>,
    pub outcome: oneshot::Receiver<ProviderTurnOutcome>,
    pub cancel: Option<oneshot::Sender<()>>,
    pub abort: Option<AbortHandle>,
}

const DEFAULT_PROVIDER_WORKER_IDLE_SECS: u64 = 15 * 60;
const DEFAULT_PROVIDER_WORKER_MAX_IDLE_SESSIONS: usize = 8;
const DEFAULT_PROVIDER_WORKER_SWEEP_INTERVAL_SECS: u64 = 60;

#[derive(Debug, Clone, Copy)]
pub struct ProviderSessionSweepConfig {
    pub idle_ttl: Duration,
    pub max_idle_sessions: usize,
    pub interval: Duration,
}

impl ProviderSessionSweepConfig {
    pub fn from_env() -> Self {
        let idle_secs = std::env::var("CTX_PROVIDER_WORKER_IDLE_SECS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_PROVIDER_WORKER_IDLE_SECS);
        let max_idle_sessions = std::env::var("CTX_PROVIDER_WORKER_MAX_IDLE_SESSIONS")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_PROVIDER_WORKER_MAX_IDLE_SESSIONS);
        let interval_secs = std::env::var("CTX_PROVIDER_WORKER_SWEEP_INTERVAL_SECS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_PROVIDER_WORKER_SWEEP_INTERVAL_SECS);
        Self {
            idle_ttl: Duration::from_secs(idle_secs),
            max_idle_sessions,
            interval: Duration::from_secs(interval_secs.max(15)),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ProviderSessionSweepStats {
    pub reaped: usize,
    pub skipped_busy: usize,
    pub dead_removed: usize,
    pub status_errors: usize,
}

impl ProviderSessionSweepStats {
    pub fn total_actions(self) -> usize {
        self.reaped + self.dead_removed
    }
}

#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    async fn inspect(&self) -> Result<ProviderStatus>;

    async fn run(
        &self,
        input: TurnInput,
        workdir: PathBuf,
        env: HashMap<String, String>,
        event_sink: tokio::sync::mpsc::Sender<NormalizedEvent>,
        hooks: ProviderRunHooks,
    ) -> Result<RunHandle>;

    async fn cancel(&self, handle: &mut RunHandle) -> Result<()>;

    /// Best-effort provider process discovery (used for resource utilization).
    async fn list_processes(&self) -> Vec<ProviderProcessInfo> {
        Vec::new()
    }

    /// Best-effort provider restart (used for memory recovery).
    async fn restart(&self, _reason: &str, mode: ProviderRestartMode) -> Result<()> {
        anyhow::bail!("provider does not support {} restart", mode.as_str());
    }

    /// Whether this adapter can honor a restart request for the given mode.
    fn supports_restart_mode(&self, _mode: ProviderRestartMode) -> bool {
        false
    }

    /// Whether this adapter has an in-memory live provider session for the given key.
    async fn has_live_session(&self, _session_key: &str) -> bool {
        false
    }

    /// Whether the provider supports native session resume (without replay).
    fn supports_resume(&self) -> bool {
        false
    }

    /// Best-effort pinning for sessions that ctx considers actively owned.
    async fn set_session_pinned(&self, _session_key: String, _pinned: bool) -> Result<()> {
        Ok(())
    }

    /// Best-effort per-session model selection (only for providers that support it via ACP).
    async fn set_session_model(&self, _session_key: String, _model_id: String) -> Result<()> {
        anyhow::bail!("provider does not support session model selection");
    }

    /// Best-effort per-session mode selection (only for providers that support it via ACP).
    async fn set_session_mode(&self, _session_key: String, _mode_id: String) -> Result<()> {
        anyhow::bail!("provider does not support session mode selection");
    }

    /// Best-effort session authentication (ACP `authenticate`).
    async fn authenticate_session(
        &self,
        _session_key: String,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        _method_id: Option<String>,
        _event_sink: tokio::sync::mpsc::Sender<NormalizedEvent>,
        _hooks: ProviderRunHooks,
    ) -> Result<()> {
        anyhow::bail!("provider does not support authenticate");
    }

    /// Best-effort idle provider-session reaping for adapters that keep live provider workers.
    async fn reap_idle_sessions(
        &self,
        _config: ProviderSessionSweepConfig,
    ) -> Result<ProviderSessionSweepStats> {
        Ok(ProviderSessionSweepStats::default())
    }
}
