use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

pub const BUILD_IDENTITY_PATH_ENV: &str = "CTX_BUILD_IDENTITY_PATH";
pub const APPIMAGE_PATH_ENV: &str = "CTX_APPIMAGE_PATH";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildIdentity {
    pub schema_version: u32,
    pub exact_version: String,
    pub build_id: String,
    pub compatibility_token: String,
}

pub fn current_build_identity(package_version: &'static str) -> Result<BuildIdentity> {
    if let Some(path) = std::env::var_os(BUILD_IDENTITY_PATH_ENV) {
        let bytes = std::fs::read(path)?;
        let identity = serde_json::from_slice(&bytes)?;
        return Ok(identity);
    }
    Ok(BuildIdentity {
        schema_version: 1,
        exact_version: package_version.to_string(),
        build_id: package_version.to_string(),
        compatibility_token: package_version.to_string(),
    })
}

pub fn normalize_release_channel(raw: &str) -> Result<String> {
    let value = raw.trim();
    if value.is_empty()
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(anyhow!("release channel must be a simple channel name"));
    }
    Ok(value.to_string())
}

pub fn default_download_base_url() -> String {
    "https://api.ctx.rs/functions/v1".to_string()
}

pub fn platform_key() -> Option<&'static str> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Some("linux-x64")
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        Some("linux-arm64")
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Some("macos-arm64")
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Some("macos-x64")
    }
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64")
    )))]
    {
        None
    }
}

pub fn appimage_path_env() -> Option<PathBuf> {
    std::env::var_os(APPIMAGE_PATH_ENV).map(PathBuf::from)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReleaseManifest {
    pub channel: String,
    pub latest_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_supported_version: Option<String>,
    #[serde(default)]
    pub platforms: BTreeMap<String, ReleasePlatformArtifacts>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReleasePlatformArtifacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appimage: Option<ReleaseArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon: Option<ReleaseArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseArtifact {
    pub url_path: String,
    pub sha256: String,
}

pub async fn fetch_latest_manifest_with_params(
    _base_url: &str,
    channel: &str,
    query: Option<&[(&str, String)]>,
) -> Result<ReleaseManifest> {
    let latest_version = query
        .unwrap_or_default()
        .iter()
        .find_map(|(key, value)| (*key == "current_version").then(|| value.clone()))
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    Ok(ReleaseManifest {
        channel: normalize_release_channel(channel)?,
        latest_version,
        min_supported_version: None,
        platforms: BTreeMap::new(),
    })
}

pub async fn fetch_latest_manifest(base_url: &str, channel: &str) -> Result<ReleaseManifest> {
    fetch_latest_manifest_with_params(base_url, channel, None).await
}

pub fn platform_supported(manifest: &ReleaseManifest, platform: Option<&str>) -> bool {
    platform.is_some_and(|platform| manifest.platforms.contains_key(platform))
}

pub fn in_place_update_capability(
    _manifest: &ReleaseManifest,
    _platform: Option<&str>,
    platform_supported: bool,
) -> (bool, Option<String>) {
    if !platform_supported {
        return (
            false,
            Some("updates are unavailable in the public ADE export".to_string()),
        );
    }
    if appimage_path_env().is_none() {
        return (false, Some("CTX_APPIMAGE_PATH is not set".to_string()));
    }
    (
        false,
        Some("updates are unavailable in the public ADE export".to_string()),
    )
}

pub fn is_update_available(
    current_version: &str,
    latest_version: &str,
    platform_supported: bool,
) -> bool {
    platform_supported && current_version != latest_version
}

pub fn release_manifest_url(base_url: &str, channel: &str) -> String {
    format!(
        "{}/releases/{}/latest.json",
        base_url.trim_end_matches('/'),
        channel
    )
}

pub fn resolve_release_artifact_url(base_url: &str, url_path: &str) -> Result<String> {
    if url_path.starts_with("http://") || url_path.starts_with("https://") {
        return Ok(url_path.to_string());
    }
    if !url_path.starts_with('/') {
        return Err(anyhow!("release artifact url_path must be absolute"));
    }
    Ok(format!("{}{}", base_url.trim_end_matches('/'), url_path))
}

#[derive(Debug)]
pub struct AppImageCandidateRequest<'a> {
    pub data_root: &'a Path,
    pub target_path: &'a Path,
    pub channel: &'a str,
    pub platform: &'a str,
    pub target_version: &'a str,
    pub current_version: &'a str,
    pub artifact_url: &'a str,
    pub artifact_url_path: &'a str,
    pub manifest_url: &'a str,
    pub base_url: &'a str,
    pub sha256: &'a str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiedAppImageCandidateMeta {
    pub schema_version: u32,
    pub channel: String,
    pub platform: String,
    pub target_version: String,
    pub current_version: String,
    pub artifact_url: String,
    pub artifact_url_path: String,
    pub manifest_url: String,
    pub base_url: String,
    pub sha256: String,
    pub target_path: String,
    pub candidate_path: PathBuf,
    pub downloaded_at: DateTime<Utc>,
}

impl VerifiedAppImageCandidateMeta {
    pub const SCHEMA_VERSION: u32 = 1;
}

pub fn appimage_candidate_path(data_root: &Path) -> PathBuf {
    data_root.join("updates").join("appimage.candidate")
}

pub fn appimage_candidate_meta_path(data_root: &Path) -> PathBuf {
    data_root.join("updates").join("appimage.candidate.json")
}

pub async fn download_verified_appimage_candidate(
    _request: AppImageCandidateRequest<'_>,
) -> Result<VerifiedAppImageCandidateMeta> {
    Err(anyhow!("updates are unavailable in the public ADE export"))
}

pub async fn read_verified_appimage_candidate_meta(
    data_root: &Path,
) -> Result<VerifiedAppImageCandidateMeta> {
    let bytes = tokio::fs::read(appimage_candidate_meta_path(data_root)).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub async fn validate_verified_appimage_candidate(
    data_root: &Path,
    target: &Path,
    channel: &str,
    platform: &str,
    base_url: &str,
    current_version: &str,
) -> Result<(PathBuf, VerifiedAppImageCandidateMeta)> {
    let meta = read_verified_appimage_candidate_meta(data_root).await?;
    if meta.channel != channel
        || meta.platform != platform
        || meta.base_url != base_url
        || meta.current_version != current_version
        || meta.target_path != target.to_string_lossy()
    {
        return Err(anyhow!(
            "verified update candidate metadata does not match request"
        ));
    }
    let candidate = appimage_candidate_path(data_root);
    Ok((candidate, meta))
}

pub async fn atomic_replace_file(target: &Path, downloaded: &Path) -> Result<()> {
    tokio::fs::rename(downloaded, target).await?;
    Ok(())
}

pub async fn clear_appimage_candidate(data_root: &Path) {
    let _ = tokio::fs::remove_file(appimage_candidate_path(data_root)).await;
    let _ = tokio::fs::remove_file(appimage_candidate_meta_path(data_root)).await;
}

pub async fn self_update_daemon(
    _channel: &str,
    _base_url: &str,
    _current_version: &str,
    _yes: bool,
    _check: bool,
) -> Result<()> {
    Err(anyhow!(
        "self-update is unavailable in the public ADE export"
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateDrainSnapshot {
    pub reason: String,
    pub owner: String,
    pub acquired_at: DateTime<Utc>,
}

#[derive(Debug, Default)]
pub struct UpdateDrainCoordinator {
    state: Mutex<Option<UpdateDrainSnapshot>>,
}

impl UpdateDrainCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn acquire(
        &self,
        reason: impl Into<String>,
        owner: impl Into<String>,
    ) -> Option<UpdateDrainSnapshot> {
        let mut state = self.state.lock().await;
        if state.is_some() {
            return None;
        }
        let snapshot = UpdateDrainSnapshot {
            reason: reason.into(),
            owner: owner.into(),
            acquired_at: Utc::now(),
        };
        *state = Some(snapshot.clone());
        Some(snapshot)
    }

    pub async fn release(&self) -> bool {
        self.state.lock().await.take().is_some()
    }

    pub async fn snapshot(&self) -> Option<UpdateDrainSnapshot> {
        self.state.lock().await.clone()
    }

    pub async fn reject_if_draining(&self) -> Result<()> {
        if let Some(snapshot) = self.snapshot().await {
            return Err(anyhow!("daemon update drain active: {}", snapshot.reason));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ManagedDaemonAutoUpdateConfig {
    pub data_root: PathBuf,
    pub bind: Vec<String>,
    pub current_version: String,
}

#[async_trait::async_trait]
pub trait ManagedDaemonAutoUpdateHooks: Send + Sync {
    async fn acquire_update_drain(&self, reason: &str, owner: &str) -> bool;
    async fn release_update_drain(&self);
    async fn daemon_is_idle(&self) -> Result<bool>;
}

pub fn managed_daemon_auto_update_configured_from_env() -> bool {
    false
}

pub fn spawn_managed_daemon_auto_update(
    _config: ManagedDaemonAutoUpdateConfig,
    _hooks: Arc<dyn ManagedDaemonAutoUpdateHooks>,
) {
}

pub async fn managed_daemon_auto_update_status_snapshot(
    _data_root: &Path,
) -> route_contract::ManagedDaemonAutoUpdateStatusSnapshot {
    route_contract::ManagedDaemonAutoUpdateStatusSnapshot {
        configured: false,
        running: false,
        last_error: None,
    }
}

pub mod route_contract {
    use super::UpdateDrainSnapshot;
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ActiveTurnRecord {
        pub workspace_id: String,
        pub session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub run_id: Option<String>,
        pub turn_id: String,
        pub status: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct DaemonTurnActivitySummary {
        pub idle: bool,
        pub active_turn_count: usize,
        pub queued_turn_count: usize,
        pub running_turn_count: usize,
        pub scanned_workspace_count: usize,
        pub turns: Vec<ActiveTurnRecord>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub update_drain: Option<UpdateDrainSnapshot>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct DaemonSandboxWorkActivitySummary {
        pub active: bool,
        pub active_sandbox_turn_count: usize,
        pub queued_sandbox_turn_count: usize,
        pub running_sandbox_turn_count: usize,
        pub running_container_backed_terminal: bool,
        pub running_workspace_container_count: usize,
        pub runtime_operation_count: usize,
        pub prewarm_artifact_operation_count: usize,
        pub scanned_workspace_count: usize,
        pub turns: Vec<ActiveTurnRecord>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ManagedDaemonAutoUpdateStatusSnapshot {
        pub configured: bool,
        pub running: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub last_error: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct UpdateActivitySnapshot {
        pub activity: DaemonTurnActivitySummary,
        pub managed_daemon_auto_update: ManagedDaemonAutoUpdateStatusSnapshot,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct UpdateCheckSnapshot {
        pub channel: String,
        pub base_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub platform: Option<String>,
        pub current_version: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub latest_version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub min_supported_version: Option<String>,
        pub platform_supported: bool,
        pub in_place_update_supported: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub in_place_update_reason: Option<String>,
        pub update_available: bool,
        pub manifest: Value,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct DownloadAppImageUpdateRequest {
        #[serde(default)]
        channel: Option<String>,
    }

    impl DownloadAppImageUpdateRequest {
        pub fn channel(&self) -> Option<&str> {
            self.channel.as_deref()
        }
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct DownloadAppImageUpdateResult {
        pub downloaded_path: String,
        pub can_apply_in_place: bool,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct ApplyAppImageUpdateRequest {
        #[serde(default)]
        channel: Option<String>,
        #[serde(default)]
        confirm: bool,
    }

    impl ApplyAppImageUpdateRequest {
        pub fn channel(&self) -> Option<&str> {
            self.channel.as_deref()
        }

        pub fn confirm(&self) -> bool {
            self.confirm
        }
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct ApplyAppImageUpdateResult {
        pub applied: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub target_path: Option<String>,
        pub message: String,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct BeginUpdateDrainRouteRequest {
        #[serde(default)]
        confirm: bool,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        owner: Option<String>,
    }

    impl BeginUpdateDrainRouteRequest {
        pub fn confirm(&self) -> bool {
            self.confirm
        }

        pub fn into_reason_owner(self) -> (Option<String>, Option<String>) {
            (self.reason, self.owner)
        }
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct BeginUpdateDrainRouteResult {
        pub acquired: bool,
        pub activity: DaemonTurnActivitySummary,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct ReleaseUpdateDrainRouteRequest {
        #[serde(default)]
        confirm: bool,
    }

    impl ReleaseUpdateDrainRouteRequest {
        pub fn confirm(&self) -> bool {
            self.confirm
        }
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct ReleaseUpdateDrainRouteResult {
        pub released: bool,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct ShutdownDaemonRouteRequest {
        #[serde(default)]
        confirm: bool,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        shutdown_token: Option<String>,
    }

    impl ShutdownDaemonRouteRequest {
        pub fn confirm(&self) -> bool {
            self.confirm
        }

        pub fn reason(&self) -> Option<String> {
            self.reason.clone()
        }

        pub fn supplied_shutdown_token(&self) -> Option<&str> {
            self.shutdown_token.as_deref()
        }

        pub fn with_supplied_shutdown_token(mut self, token: Option<String>) -> Self {
            self.shutdown_token = token;
            self
        }
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct ShutdownDaemonRouteResult {
        pub accepted: bool,
        pub activity: DaemonTurnActivitySummary,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum UpdateRouteErrorKind {
        BadRequest,
        BadGateway,
        Internal,
    }

    #[derive(Debug)]
    pub struct UpdateRouteError {
        kind: UpdateRouteErrorKind,
        message: String,
    }

    impl UpdateRouteError {
        pub fn new(kind: UpdateRouteErrorKind, message: impl Into<String>) -> Self {
            Self {
                kind,
                message: message.into(),
            }
        }

        pub fn bad_request(message: impl ToString) -> Self {
            Self::new(UpdateRouteErrorKind::BadRequest, message.to_string())
        }

        pub fn bad_gateway(message: impl ToString) -> Self {
            Self::new(UpdateRouteErrorKind::BadGateway, message.to_string())
        }

        pub fn internal(message: impl ToString) -> Self {
            Self::new(UpdateRouteErrorKind::Internal, message.to_string())
        }

        pub fn kind(&self) -> UpdateRouteErrorKind {
            self.kind
        }

        pub fn message(&self) -> &str {
            &self.message
        }
    }

    impl std::fmt::Display for UpdateRouteError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.message)
        }
    }

    impl std::error::Error for UpdateRouteError {}

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum MaintenanceRouteErrorKind {
        BadRequest,
        Conflict,
        Forbidden,
        Internal,
    }

    #[derive(Debug)]
    pub struct MaintenanceRouteError {
        kind: MaintenanceRouteErrorKind,
        message: String,
    }

    impl MaintenanceRouteError {
        pub fn new(kind: MaintenanceRouteErrorKind, message: impl Into<String>) -> Self {
            Self {
                kind,
                message: message.into(),
            }
        }

        pub fn bad_request(message: impl ToString) -> Self {
            Self::new(MaintenanceRouteErrorKind::BadRequest, message.to_string())
        }

        pub fn conflict(message: impl ToString) -> Self {
            Self::new(MaintenanceRouteErrorKind::Conflict, message.to_string())
        }

        pub fn forbidden(message: impl ToString) -> Self {
            Self::new(MaintenanceRouteErrorKind::Forbidden, message.to_string())
        }

        pub fn internal(message: impl ToString) -> Self {
            Self::new(MaintenanceRouteErrorKind::Internal, message.to_string())
        }

        pub fn kind(&self) -> MaintenanceRouteErrorKind {
            self.kind
        }

        pub fn message(&self) -> &str {
            &self.message
        }
    }

    impl std::fmt::Display for MaintenanceRouteError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.message)
        }
    }

    impl std::error::Error for MaintenanceRouteError {}

    #[allow(dead_code)]
    fn _assert_dates_are_used(_: DateTime<Utc>) {}
}
