use super::*;
pub(crate) use ctx_desktop_ipc::{
    DesktopGitBranchReq, DesktopRemoteDaemonUpdateReq, DesktopRemoteDaemonUpdateResp,
    DesktopRemotePrewarmReq, DesktopSshConnectJobStatus, DesktopSshConnectPollReq, DesktopSshHost,
    DesktopSshPathEntry, DesktopSshPathReq, DesktopSshTestReq, SshConnectReq,
};

pub(super) const MANAGED_REMOTE_CTX_BIN: &str = "~/.ctx/bin/ctx";
pub(super) const WINDOWS_REMOTE_UNSUPPORTED_MSG: &str =
    "Remote Windows hosts are not supported yet. Use a Linux host (x86_64 or arm64).";
pub(super) const REMOTE_BOOTSTRAP_CAPABILITY_MSG: &str =
    "Remote daemon bootstrap failed while retrieving managed daemon artifact. Check network connectivity and release metadata.";
pub(super) const PLATFORM_PROBE_OS_MARKER: &str = "__CTX_PLATFORM_OS__";
pub(super) const PLATFORM_PROBE_ARCH_MARKER: &str = "__CTX_PLATFORM_ARCH__";
pub(super) const SSH_CONFIG_OVERRIDE_ENV: &str = "CTX_DESKTOP_SSH_CONFIG_PATH";
pub(super) const SSH_TUNNEL_BOOTSTRAP_HEALTH_RETRIES: usize = 12;
pub(super) const SSH_TUNNEL_BOOTSTRAP_HEALTH_BASE_DELAY_MS: u64 = 150;
pub(super) const SSH_TUNNEL_LOG_BYTES: usize = 4096;
pub(super) const DEFAULT_DOWNLOAD_BASE_URL: &str = "https://api.ctx.rs/functions/v1";
pub(super) const REMOTE_DAEMON_DOWNLOAD_TIMEOUT_SECS: u64 = 180;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RemoteLinuxPlatform {
    pub(super) arch: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RemoteAuthBootstrap {
    None,
    PasswordOncePubkeyInstall,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RemoteProbe {
    pub(super) platform: RemoteLinuxPlatform,
    pub(super) auth_bootstrap_used: RemoteAuthBootstrap,
    pub(super) managed_binary_present: bool,
    pub(super) existing_daemon_reachable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SshConnectTarget {
    pub(super) host: String,
    pub(super) user: Option<String>,
    pub(super) password_once: Option<String>,
    pub(super) remote_port: u16,
    pub(super) start_remote: bool,
    pub(super) remote_data_dir: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConnectJobPhase {
    Queued,
    Probing,
    Planning,
    InstallingManagedDaemon,
    StartingRemoteDaemon,
    OpeningTunnel,
    ReadingAuth,
    HandingOffConnection,
    Succeeded,
    Failed,
}

impl ConnectJobPhase {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Probing => "probing",
            Self::Planning => "planning",
            Self::InstallingManagedDaemon => "installing_managed_daemon",
            Self::StartingRemoteDaemon => "starting_remote_daemon",
            Self::OpeningTunnel => "opening_tunnel",
            Self::ReadingAuth => "reading_auth",
            Self::HandingOffConnection => "handing_off_connection",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    pub(super) fn status(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            _ => "pending",
        }
    }
}

pub(super) fn normalize_optional_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
}

pub(super) fn normalize_update_channel_with_env(
    raw: Option<&str>,
    env_channel: Option<&str>,
) -> Result<String, String> {
    normalize_update_channel_with_sources(raw, env_channel, None)
}

pub(super) fn normalize_update_channel_with_sources(
    raw: Option<&str>,
    env_channel: Option<&str>,
    preference_channel: Option<&str>,
) -> Result<String, String> {
    normalize_desktop_update_channel_with_sources(raw, env_channel, preference_channel)
}
