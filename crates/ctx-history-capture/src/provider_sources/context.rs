use std::{
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

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
    "FILE_STORE",
    "FILE_STORE_PATH",
    "FLATPAK_XDG_DATA_HOME",
    "FORGE_CONFIG",
    "GEMINI_CLI_HOME",
    "GOOSE_PATH_ROOT",
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
    cwd: Option<PathBuf>,
    data_root: Option<PathBuf>,
    platform: DiscoveryPlatform,
    platform_dirs: DiscoveryPlatformDirs,
    inherited_env: BTreeMap<&'static str, OsString>,
}

impl DiscoveryContext {
    pub fn from_process(home: impl Into<PathBuf>) -> Self {
        let inherited_env = DISCOVERY_ENV_ALLOWLIST
            .iter()
            .filter_map(|name| env::var_os(name).map(|value| (*name, value)))
            .collect();
        Self {
            home: home.into(),
            cwd: env::current_dir().ok(),
            data_root: None,
            platform: DiscoveryPlatform::current(),
            platform_dirs: DiscoveryPlatformDirs::from_process(),
            inherited_env,
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
            cwd: Some(cwd.into()),
            data_root: None,
            platform,
            platform_dirs,
            inherited_env: BTreeMap::new(),
        }
    }

    pub fn without_cwd(
        home: impl Into<PathBuf>,
        platform: DiscoveryPlatform,
        platform_dirs: DiscoveryPlatformDirs,
    ) -> Self {
        Self {
            home: home.into(),
            cwd: None,
            data_root: None,
            platform,
            platform_dirs,
            inherited_env: BTreeMap::new(),
        }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    pub fn data_root(&self) -> Option<&Path> {
        self.data_root.as_deref()
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
            "FILE_STORE",
            "FLATPAK_XDG_DATA_HOME",
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
            "OPENAI_API_KEY",
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
