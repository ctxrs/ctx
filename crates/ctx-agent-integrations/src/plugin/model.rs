use std::{
    env, fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

pub const MARKETPLACE_NAME: &str = "ctx";
pub const MARKETPLACE_SOURCE: &str = "ctxrs/ctx";
pub const PLUGIN_ID: &str = "ctx@ctx";
pub const LEGACY_PLUGIN_ID: &str = "ctx-agent-history-search@ctx";
pub const PLUGIN_MANAGER_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
pub const PLUGIN_MANAGER_OUTPUT_LIMIT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PluginAgent {
    Codex,
    ClaudeCode,
    Cursor,
}

impl PluginAgent {
    pub const ALL: &'static [Self] = &[Self::Codex, Self::ClaudeCode, Self::Cursor];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
        }
    }

    pub const fn executable(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude",
            Self::Cursor => "cursor",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginOperation {
    Install,
    Status,
    Remove,
}

impl PluginOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Status => "status",
            Self::Remove => "remove",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginScope {
    Global,
    Project,
}

impl PluginScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }

    pub(crate) const fn claude_scope(self) -> &'static str {
        match self {
            Self::Global => "user",
            Self::Project => "project",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSelection {
    Detected,
    Explicit,
    All,
}

#[derive(Debug, Clone)]
pub struct PluginRequest {
    pub agents: Vec<PluginAgent>,
    pub all_agents: bool,
    pub project: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginCapability {
    Automatic,
    ManualRequired,
    UnsupportedScope,
}

impl PluginCapability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::ManualRequired => "manual_required",
            Self::UnsupportedScope => "unsupported_scope",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginMarketplaceStatus {
    Present,
    Added,
    Missing,
    Conflict,
    NotApplicable,
    Error,
}

impl PluginMarketplaceStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Added => "added",
            Self::Missing => "missing",
            Self::Conflict => "conflict",
            Self::NotApplicable => "not_applicable",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginInstallStatus {
    Installed,
    LegacyInstalled,
    Missing,
    ManualRequired,
    UnsupportedScope,
    CliMissing,
    Error,
}

impl PluginInstallStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::LegacyInstalled => "legacy_installed",
            Self::Missing => "missing",
            Self::ManualRequired => "manual_required",
            Self::UnsupportedScope => "unsupported_scope",
            Self::CliMissing => "cli_missing",
            Self::Error => "error",
        }
    }

    pub const fn is_current(self) -> bool {
        matches!(self, Self::Installed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginResultAction {
    Inspected,
    Installed,
    Removed,
    AlreadyInstalled,
    AlreadyAbsent,
    MarketplaceAdded,
    LegacyRemoved,
    ManualRequired,
    UnsupportedScope,
    Failed,
}

impl PluginResultAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inspected => "inspected",
            Self::Installed => "installed",
            Self::Removed => "removed",
            Self::AlreadyInstalled => "already_installed",
            Self::AlreadyAbsent => "already_absent",
            Self::MarketplaceAdded => "marketplace_added",
            Self::LegacyRemoved => "legacy_removed",
            Self::ManualRequired => "manual_required",
            Self::UnsupportedScope => "unsupported_scope",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginCommandStage {
    MarketplaceList,
    MarketplaceAdd,
    PluginList,
    PluginInstall,
    PluginRemove,
}

impl PluginCommandStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MarketplaceList => "marketplace list",
            Self::MarketplaceAdd => "marketplace add",
            Self::PluginList => "plugin list",
            Self::PluginInstall => "plugin install",
            Self::PluginRemove => "plugin remove",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginCommandFailureKind {
    Spawn,
    Capture,
    Timeout,
    OutputLimit,
    NonZero,
    MalformedJson,
    UnexpectedJson,
}

/// Captured manager output is intentionally kept out of user-facing receipts.
/// Callers that own private diagnostics may inspect it without leaking it to
/// text or JSON presentation.
#[derive(Debug, Clone)]
pub struct PluginCommandDiagnostic {
    pub stage: PluginCommandStage,
    pub kind: PluginCommandFailureKind,
    pub exit_code: Option<i32>,
    captured_stdout: Vec<u8>,
    captured_stderr: Vec<u8>,
}

impl PluginCommandDiagnostic {
    pub(crate) fn new(
        stage: PluginCommandStage,
        kind: PluginCommandFailureKind,
        exit_code: Option<i32>,
        captured_stdout: Vec<u8>,
        captured_stderr: Vec<u8>,
    ) -> Self {
        Self {
            stage,
            kind,
            exit_code,
            captured_stdout,
            captured_stderr,
        }
    }

    pub fn captured_stdout_bytes(&self) -> &[u8] {
        &self.captured_stdout
    }

    pub fn captured_stderr_bytes(&self) -> &[u8] {
        &self.captured_stderr
    }

    pub(crate) fn concise_error(&self, agent: PluginAgent) -> String {
        match self.kind {
            PluginCommandFailureKind::Spawn => format!(
                "{} could not run its native plugin manager.",
                agent.display_name()
            ),
            PluginCommandFailureKind::Capture => format!(
                "{} native plugin manager output could not be captured safely.",
                agent.display_name()
            ),
            PluginCommandFailureKind::Timeout => format!(
                "{} {} command timed out.",
                agent.display_name(),
                self.stage.as_str()
            ),
            PluginCommandFailureKind::OutputLimit => format!(
                "{} {} command exceeded the output limit.",
                agent.display_name(),
                self.stage.as_str()
            ),
            PluginCommandFailureKind::NonZero => match self.exit_code {
                Some(code) => format!(
                    "{} {} command failed with exit code {code}.",
                    agent.display_name(),
                    self.stage.as_str()
                ),
                None => format!(
                    "{} {} command did not complete successfully.",
                    agent.display_name(),
                    self.stage.as_str()
                ),
            },
            PluginCommandFailureKind::MalformedJson => format!(
                "{} returned malformed JSON for {}.",
                agent.display_name(),
                self.stage.as_str()
            ),
            PluginCommandFailureKind::UnexpectedJson => format!(
                "{} returned an unsupported JSON shape for {}.",
                agent.display_name(),
                self.stage.as_str()
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PluginResult {
    pub agent: PluginAgent,
    pub scope: PluginScope,
    pub capability: PluginCapability,
    pub detected: bool,
    pub supported: bool,
    pub marketplace_status: PluginMarketplaceStatus,
    pub previous_status: PluginInstallStatus,
    pub status: PluginInstallStatus,
    pub action: PluginResultAction,
    pub installed_version: Option<String>,
    pub success: bool,
    pub modified: bool,
    pub instructions: Option<String>,
    pub error: Option<String>,
    pub diagnostic: Option<PluginCommandDiagnostic>,
    pub reconciliation_diagnostic: Option<PluginCommandDiagnostic>,
}

impl PluginResult {
    pub fn is_operational_failure(&self) -> bool {
        !self.success && self.status != PluginInstallStatus::ManualRequired
    }
}

#[derive(Debug, Clone)]
pub struct PluginReceipt {
    pub operation: PluginOperation,
    pub scope: PluginScope,
    pub selection: PluginSelection,
    pub results: Vec<PluginResult>,
    pub failed: usize,
    pub operational_failures: usize,
    pub modified: usize,
}

#[derive(Debug, Clone)]
pub struct PluginContext {
    cwd: PathBuf,
    codex: Option<PathBuf>,
    claude: Option<PathBuf>,
    cursor: Option<PathBuf>,
    command_timeout: Duration,
    output_limit_bytes: usize,
}

impl PluginContext {
    /// Discovers host executables from PATH only. It never probes host config,
    /// caches, skill directories, or settings files.
    pub fn from_env() -> io::Result<Self> {
        let path = env::var_os("PATH");
        Ok(Self {
            cwd: env::current_dir()?,
            codex: find_executable(PluginAgent::Codex.executable(), path.as_deref()),
            claude: find_executable(PluginAgent::ClaudeCode.executable(), path.as_deref()),
            cursor: find_executable(PluginAgent::Cursor.executable(), path.as_deref()),
            command_timeout: PLUGIN_MANAGER_COMMAND_TIMEOUT,
            output_limit_bytes: PLUGIN_MANAGER_OUTPUT_LIMIT_BYTES,
        })
    }

    pub fn for_tests(
        cwd: PathBuf,
        codex: Option<PathBuf>,
        claude: Option<PathBuf>,
        cursor: Option<PathBuf>,
    ) -> Self {
        Self {
            cwd,
            codex,
            claude,
            cursor,
            command_timeout: PLUGIN_MANAGER_COMMAND_TIMEOUT,
            output_limit_bytes: PLUGIN_MANAGER_OUTPUT_LIMIT_BYTES,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_command_limits_for_tests(
        mut self,
        command_timeout: Duration,
        output_limit_bytes: usize,
    ) -> Self {
        self.command_timeout = command_timeout;
        self.output_limit_bytes = output_limit_bytes;
        self
    }

    pub fn detected(&self, agent: PluginAgent) -> bool {
        self.command(agent).is_some()
    }

    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub(crate) fn command(&self, agent: PluginAgent) -> Option<&Path> {
        match agent {
            PluginAgent::Codex => self.codex.as_deref(),
            PluginAgent::ClaudeCode => self.claude.as_deref(),
            PluginAgent::Cursor => self.cursor.as_deref(),
        }
    }

    pub(crate) const fn command_timeout(&self) -> Duration {
        self.command_timeout
    }

    pub(crate) const fn output_limit_bytes(&self) -> usize {
        self.output_limit_bytes
    }
}

fn find_executable(name: &str, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let path = path?;
    let cwd = env::current_dir().ok()?;
    for directory in env::split_paths(path) {
        let directory = if directory.is_absolute() {
            directory
        } else {
            cwd.join(directory)
        };
        for candidate in executable_candidates(&directory, name) {
            let Ok(metadata) = fs::metadata(&candidate) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                if metadata.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            if let Ok(path) = fs::canonicalize(candidate) {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn executable_candidates(directory: &Path, name: &str) -> Vec<PathBuf> {
    vec![directory.join(name)]
}

#[cfg(windows)]
fn executable_candidates(directory: &Path, name: &str) -> Vec<PathBuf> {
    native_windows_extensions(env::var_os("PATHEXT").as_deref())
        .into_iter()
        .map(|extension| directory.join(format!("{name}{extension}")))
        .collect()
}

#[cfg(windows)]
fn native_windows_extensions(path_ext: Option<&std::ffi::OsStr>) -> Vec<String> {
    let configured = path_ext
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".COM;.EXE".to_owned());
    let mut extensions = Vec::new();
    for extension in configured
        .split(';')
        .filter(|extension| !extension.is_empty())
    {
        if !matches!(extension.to_ascii_uppercase().as_str(), ".COM" | ".EXE") {
            continue;
        }
        if !extensions
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(extension))
        {
            extensions.push(extension.to_owned());
        }
    }
    extensions
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::{ffi::OsStr, path::Path};

    use super::{executable_candidates, native_windows_extensions};

    #[test]
    fn executable_candidates_exclude_command_processor_wrappers() {
        let extensions =
            native_windows_extensions(Some(OsStr::new(".BAT;.EXE;.CMD;.COM;.PS1;.exe")));
        assert_eq!(extensions, [".EXE", ".COM"]);

        let candidates = executable_candidates(Path::new(r"C:\\tools"), "codex");
        assert!(candidates.iter().all(|candidate| matches!(
            candidate.extension().and_then(OsStr::to_str),
            Some("exe" | "com" | "EXE" | "COM")
        )));
    }
}
