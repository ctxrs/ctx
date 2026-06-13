use super::*;
pub(super) use ctx_desktop_ipc::{
    DesktopAppRestartResp, DesktopAppUpdateApplyReq, DesktopAppUpdateApplyResp,
    DesktopAppUpdateAttemptResp, DesktopAppUpdateAttemptStageResp, DesktopAppUpdateCheckReq,
    DesktopAppUpdateCheckResp, DesktopAppUpdateStateResp,
};

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex as AsyncMutex;

mod apply;
mod attempts;
mod recovery;
mod restart;
mod staged;
mod support;
#[cfg(test)]
mod tests;
mod transaction;

const RESTART_MARKER_FILENAME: &str = "desktop_update_restart_required.json";
const STAGED_UPDATE_META_FILENAME: &str = "desktop_update_staged.v1.json";
const STAGED_UPDATE_BYTES_FILENAME: &str = "desktop_update_staged.v1.bin";
const LAST_ATTEMPT_FILENAME: &str = "desktop_update_attempt_last.v1.json";
const RESTART_READY_MESSAGE: &str =
    "Update takes ~1 second and preserves data. Active agents will be paused.";
const REMOTE_BOOTSTRAP_UPDATE_REQUIRED_PREFIX: &str =
    "Desktop app update required before remote bootstrap.";
const REMOTE_BOOTSTRAP_FRESHNESS_UNVERIFIED_PREFIX: &str =
    "Desktop app freshness could not be verified before remote bootstrap.";
static STAGING_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
static APPLY_IN_PROGRESS: OnceLock<AsyncMutex<()>> = OnceLock::new();

fn resolve_app_update_channel(
    app: &tauri::AppHandle,
    requested: Option<&str>,
) -> Result<String, String> {
    resolve_desktop_update_channel(app, requested)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DesktopAppUpdatePhase {
    Idle,
    Staging,
    StagedReady,
    RestartRequired,
    Failed,
}

impl DesktopAppUpdatePhase {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Staging => "staging",
            Self::StagedReady => "staged_ready",
            Self::RestartRequired => "restart_required",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DesktopStagedUpdateMeta {
    version: String,
    target: String,
    endpoint: String,
    channel: String,
    download_url: String,
    signature: String,
    sha256: String,
    downloaded_at_ms: u64,
    size_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DesktopUpdateAttemptResult {
    InProgress,
    Succeeded,
    Failed,
}

impl DesktopUpdateAttemptResult {
    fn as_str(&self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DesktopUpdateAttemptStage {
    stage: String,
    started_at_ms: u64,
    finished_at_ms: Option<u64>,
    result: DesktopUpdateAttemptResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DesktopUpdateAttempt {
    attempt_id: String,
    channel: String,
    current_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_version: Option<String>,
    started_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    finished_at_ms: Option<u64>,
    result: DesktopUpdateAttemptResult,
    stages: Vec<DesktopUpdateAttemptStage>,
}

#[derive(Debug, Clone)]
struct DesktopNativeUpdaterConfig {
    target: String,
    endpoint: String,
    pubkey: Option<String>,
}

impl From<DesktopUpdateAttempt> for DesktopAppUpdateAttemptResp {
    fn from(value: DesktopUpdateAttempt) -> Self {
        Self {
            attempt_id: value.attempt_id,
            channel: value.channel,
            current_version: value.current_version,
            target_version: value.target_version,
            started_at_ms: value.started_at_ms,
            finished_at_ms: value.finished_at_ms,
            result: value.result.as_str().to_string(),
            stages: value
                .stages
                .into_iter()
                .map(DesktopAppUpdateAttemptStageResp::from)
                .collect(),
        }
    }
}

impl From<DesktopUpdateAttemptStage> for DesktopAppUpdateAttemptStageResp {
    fn from(value: DesktopUpdateAttemptStage) -> Self {
        Self {
            stage: value.stage,
            started_at_ms: value.started_at_ms,
            finished_at_ms: value.finished_at_ms,
            result: value.result.as_str().to_string(),
            error_code: value.error_code,
            error_message: value.error_message,
        }
    }
}

#[tauri::command]
pub(super) async fn desktop_check_app_update(
    app: tauri::AppHandle,
    req: DesktopAppUpdateCheckReq,
) -> Result<DesktopAppUpdateCheckResp, String> {
    let state = desktop_get_app_update_state(app, req).await?;
    Ok(DesktopAppUpdateCheckResp {
        configured: state.configured,
        available: state.available,
        restart_required: state.restart_required,
        phase: state.phase,
        staged: state.staged,
        current_version: state.current_version,
        latest_version: state.latest_version,
        target: state.target,
        endpoint: state.endpoint,
        message: state.message,
        last_attempt_id: state.last_attempt_id,
        last_error: state.last_error,
    })
}

#[tauri::command]
pub(super) async fn desktop_get_app_update_state(
    app: tauri::AppHandle,
    req: DesktopAppUpdateCheckReq,
) -> Result<DesktopAppUpdateStateResp, String> {
    let channel = resolve_app_update_channel(&app, req.channel.as_deref())?;
    recovery::resolve_desktop_update_state(&app, &channel).await
}

#[tauri::command]
pub(super) fn desktop_get_last_app_update_attempt(
    app: tauri::AppHandle,
) -> Result<Option<DesktopAppUpdateAttemptResp>, String> {
    let raw = attempts::read_last_attempt_for_app(&app)?;
    Ok(raw.map(DesktopAppUpdateAttemptResp::from))
}

#[tauri::command]
pub(super) async fn desktop_apply_app_update(
    app: tauri::AppHandle,
    req: DesktopAppUpdateApplyReq,
) -> Result<DesktopAppUpdateApplyResp, String> {
    apply::apply_app_update(app, req).await
}

#[tauri::command]
pub(super) fn desktop_restart_app(app: tauri::AppHandle) -> Result<DesktopAppRestartResp, String> {
    restart::desktop_restart_app(app)
}

pub(super) async fn ensure_desktop_app_current_for_remote_bootstrap(
    app: &tauri::AppHandle,
    channel: &str,
) -> Result<(), String> {
    recovery::ensure_desktop_app_current_for_remote_bootstrap_impl(app, channel).await
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn app_data_root_for_app(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("resolving app_data_dir: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating app_data_dir: {e}"))?;
    Ok(dir)
}

fn staged_meta_path_for_app(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app_data_root_for_app(app)?.join(STAGED_UPDATE_META_FILENAME))
}

fn staged_bytes_path_for_app(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app_data_root_for_app(app)?.join(STAGED_UPDATE_BYTES_FILENAME))
}

fn last_attempt_path_for_app(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app_data_root_for_app(app)?.join(LAST_ATTEMPT_FILENAME))
}

fn restart_marker_path_for_app(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app_data_root_for_app(app)?.join(RESTART_MARKER_FILENAME))
}
