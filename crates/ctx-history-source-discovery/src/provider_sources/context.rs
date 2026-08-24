use std::{
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use ctx_history_capture_model::ProviderRootDefinition;
use directories::BaseDirs;

/// Process environment keys that discovery may inherit.
///
/// Resolver lanes must request values through [`DiscoveryContext::env`]. Keys
/// outside this list are never captured, including auth, token, and `.env`
/// inputs.
pub const DISCOVERY_ENV_ALLOWLIST: &[&str] = &[
    "APPDATA",
    "ASTRBOT_ROOT",
    "CLAUDE_CONFIG_DIR",
    "CLINE_DATA_DIR",
    "CLINE_DB_DATA_DIR",
    "CLINE_DIR",
    "CLINE_SANDBOX",
    "CLINE_SANDBOX_DATA_DIR",
    "CLINE_SESSION_DATA_DIR",
    "CODEBUDDY_CONFIG_DIR",
    "CODEX_HOME",
    "CONTINUE_GLOBAL_DIR",
    "COPILOT_HOME",
    "CRUSH_GLOBAL_CONFIG",
    "CRUSH_GLOBAL_DATA",
    "CURSOR_DATA_DIR",
    "DSH_HOME",
    "FILE_STORE",
    "FILE_STORE_PATH",
    "FLATPAK_XDG_DATA_HOME",
    "FORGE_CONFIG",
    "GEMINI_CLI_HOME",
    "GOOSE_PATH_ROOT",
    "GROK_HOME",
    "HERMES_HOME",
    "JUNIE_HOME",
    "KILO_DB",
    "KIMI_CODE_HOME",
    "KIRO_HOME",
    "MIMOCODE_DB",
    "MIMOCODE_HOME",
    "MUX_ROOT",
    "NODE_ENV",
    "OH_PERSISTENCE_DIR",
    "OPENCLAW_HOME",
    "OPENCLAW_STATE_DIR",
    "OPENHANDS_CONVERSATIONS_DIR",
    "OPENHANDS_PERSISTENCE_DIR",
    "OPENHANDS_USER_ID",
    "OPENCODE_DB",
    "PI_CODING_AGENT_DIR",
    "PI_CODING_AGENT_SESSION_DIR",
    "QODER_CONFIG_DIR",
    "QWEN_CODE_SYSTEM_DEFAULTS_PATH",
    "QWEN_CODE_SYSTEM_SETTINGS_PATH",
    "QWEN_CODE_TRUSTED_FOLDERS_PATH",
    "QWEN_HOME",
    "QWEN_RUNTIME_DIR",
    "SHARED_EVENT_STORAGE_PROVIDER",
    "VIBE_HOME",
    "VIBE_SESSION_LOGGING",
    "VIBE_SESSION_LOGGING__SAVE_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "ZED_STATELESS",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryPlatform {
    Linux,
    MacOS,
    Windows,
    OtherUnix,
}

impl DiscoveryPlatform {
    fn current() -> Self {
        #[cfg(target_os = "linux")]
        {
            Self::Linux
        }
        #[cfg(target_os = "macos")]
        {
            Self::MacOS
        }
        #[cfg(target_os = "windows")]
        {
            Self::Windows
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            Self::OtherUnix
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiscoveryPlatformDirs {
    pub data: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub state: Option<PathBuf>,
    pub local_data: Option<PathBuf>,
}

impl DiscoveryPlatformDirs {
    fn from_process() -> Self {
        let Some(dirs) = BaseDirs::new() else {
            return Self::default();
        };
        Self {
            data: Some(dirs.data_dir().to_path_buf()),
            config: Some(dirs.config_dir().to_path_buf()),
            state: dirs.state_dir().map(Path::to_path_buf),
            local_data: Some(dirs.data_local_dir().to_path_buf()),
        }
    }
}

/// All process-observable state available to provider discovery.
///
/// An unavailable process CWD is represented as `None`; project resolvers must
/// then suppress CWD-derived candidates instead of guessing a replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryContext {
    home: PathBuf,
    home_directory_available: bool,
    cwd: Option<PathBuf>,
    data_root: Option<PathBuf>,
    effective_uid: Option<u32>,
    platform: DiscoveryPlatform,
    platform_dirs: DiscoveryPlatformDirs,
    inherited_env: BTreeMap<&'static str, OsString>,
    automatic_provider_discovery: bool,
    configured_provider_roots: Vec<ProviderRootDefinition>,
}

impl DiscoveryContext {
    pub fn from_process(home: impl Into<PathBuf>) -> Self {
        let inherited_env = DISCOVERY_ENV_ALLOWLIST
            .iter()
            .filter_map(|name| env::var_os(name).map(|value| (*name, value)))
            .collect();
        Self {
            home: home.into(),
            home_directory_available: true,
            cwd: env::current_dir().ok(),
            data_root: None,
            effective_uid: process_effective_uid(),
            platform: DiscoveryPlatform::current(),
            platform_dirs: DiscoveryPlatformDirs::from_process(),
            inherited_env,
            automatic_provider_discovery: true,
            configured_provider_roots: Vec::new(),
        }
    }

    pub fn new(
        home: impl Into<PathBuf>,
        cwd: impl Into<PathBuf>,
        platform: DiscoveryPlatform,
        platform_dirs: DiscoveryPlatformDirs,
    ) -> Self {
        Self {
            home: home.into(),
            home_directory_available: true,
            cwd: Some(cwd.into()),
            data_root: None,
            effective_uid: None,
            platform,
            platform_dirs,
            inherited_env: BTreeMap::new(),
            automatic_provider_discovery: true,
            configured_provider_roots: Vec::new(),
        }
    }

    pub fn without_cwd(
        home: impl Into<PathBuf>,
        platform: DiscoveryPlatform,
        platform_dirs: DiscoveryPlatformDirs,
    ) -> Self {
        Self {
            home: home.into(),
            home_directory_available: true,
            cwd: None,
            data_root: None,
            effective_uid: None,
            platform,
            platform_dirs,
            inherited_env: BTreeMap::new(),
            automatic_provider_discovery: true,
            configured_provider_roots: Vec::new(),
        }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Marks the supplied home path as a non-discoverable placeholder.
    ///
    /// Absolute configured provider roots remain usable when the process has
    /// no resolvable home directory, while home- and environment-derived
    /// automatic discovery stays conservatively disabled.
    pub fn with_home_directory_available(mut self, available: bool) -> Self {
        self.home_directory_available = available;
        self
    }

    pub const fn home_directory_available(&self) -> bool {
        self.home_directory_available
    }

    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    pub fn data_root(&self) -> Option<&Path> {
        self.data_root.as_deref()
    }

    /// Returns the effective Unix user ID captured at discovery startup.
    ///
    /// Synthetic contexts leave this unavailable unless a test or embedding
    /// caller supplies explicit process authority with [`Self::with_effective_uid`].
    pub fn effective_uid(&self) -> Option<u32> {
        self.effective_uid
    }

    /// Returns the same bounded process snapshot scoped to one authorized activity locator.
    ///
    /// Passing `None` suppresses all project-relative resolver behavior while retaining the
    /// already captured platform directories and allowlisted environment.
    pub fn with_cwd(mut self, cwd: Option<PathBuf>) -> Self {
        self.cwd = cwd;
        self
    }

    /// Supplies the caller-selected ctx authority for transient provider
    /// SQLite snapshots. Discovery without this authority remains read-only
    /// and reports live-WAL structural probes as unavailable.
    pub fn with_data_root(mut self, data_root: impl Into<PathBuf>) -> Self {
        self.data_root = Some(data_root.into());
        self
    }

    /// Supplies effective-user authority for a synthetic discovery context.
    pub fn with_effective_uid(mut self, effective_uid: u32) -> Self {
        self.effective_uid = Some(effective_uid);
        self
    }

    pub fn platform(&self) -> DiscoveryPlatform {
        self.platform
    }

    pub fn platform_dirs(&self) -> &DiscoveryPlatformDirs {
        &self.platform_dirs
    }

    pub fn env(&self, name: &str) -> Option<&OsStr> {
        self.inherited_env.get(name).map(OsString::as_os_str)
    }

    pub fn with_env(mut self, name: &'static str, value: impl Into<OsString>) -> Self {
        if DISCOVERY_ENV_ALLOWLIST.contains(&name) {
            self.inherited_env.insert(name, value.into());
        }
        self
    }

    pub fn with_configured_provider_roots(
        mut self,
        mut roots: Vec<ProviderRootDefinition>,
    ) -> Self {
        roots.sort_by(|left, right| left.id.cmp(&right.id));
        self.configured_provider_roots = roots;
        self
    }

    pub fn configured_provider_roots(&self) -> &[ProviderRootDefinition] {
        &self.configured_provider_roots
    }

    pub fn with_automatic_provider_discovery(mut self, enabled: bool) -> Self {
        self.automatic_provider_discovery = enabled;
        self
    }

    pub const fn automatic_provider_discovery_enabled(&self) -> bool {
        self.automatic_provider_discovery
    }

    pub const fn automatic_provider_inference_enabled(&self) -> bool {
        self.automatic_provider_discovery && self.home_directory_available
    }
}

#[cfg(unix)]
fn process_effective_uid() -> Option<u32> {
    // SAFETY: `geteuid` takes no arguments and has no failure mode.
    Some(unsafe { libc::geteuid() })
}

#[cfg(not(unix))]
fn process_effective_uid() -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_environment_accepts_only_frozen_discovery_keys() {
        let mut context = DiscoveryContext::new(
            "/home/test",
            "/work/test",
            DiscoveryPlatform::Linux,
            DiscoveryPlatformDirs::default(),
        );
        let newly_required = [
            "CLINE_SANDBOX",
            "CLINE_SANDBOX_DATA_DIR",
            "DSH_HOME",
            "FILE_STORE",
            "FLATPAK_XDG_DATA_HOME",
            "GROK_HOME",
            "NODE_ENV",
            "QWEN_CODE_SYSTEM_DEFAULTS_PATH",
            "QWEN_CODE_SYSTEM_SETTINGS_PATH",
            "QWEN_CODE_TRUSTED_FOLDERS_PATH",
            "SHARED_EVENT_STORAGE_PROVIDER",
            "VIBE_SESSION_LOGGING",
            "VIBE_SESSION_LOGGING__SAVE_DIR",
            "ZED_STATELESS",
        ];
        for name in newly_required {
            context = context.with_env(name, "/allowed-selector");
        }
        let rejected = [
            "JUNIE_SESSIONS_DIR",
            "KILO_DISABLE_CHANNEL_DB",
            "KIMI_SHARE_DIR",
            "LOCALAPPDATA",
            "MIMOCODE_DISABLE_CHANNEL_DB",
            "DEEPSEEK_API_KEY",
            "OPENAI_API_KEY",
            "OPENROUTER_API_KEY",
            "XAI_API_KEY",
            "ROO_CLINE_DATA_DIR",
            "ROO_CODE_DATA_DIR",
            "ROO_DATA_DIR",
            "SHELLEY_DB",
            "TABNINE_CLI_HOME",
        ];
        for name in rejected {
            context = context.with_env(name, "must-not-be-captured");
        }

        for name in newly_required {
            assert_eq!(context.env(name), Some(OsStr::new("/allowed-selector")));
        }
        for name in rejected {
            assert_eq!(context.env(name), None);
        }
    }
}
