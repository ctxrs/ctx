use super::CountBucket;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntegrationAction {
    Install,
    Status,
}

impl IntegrationAction {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Status => "status",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntegrationTarget {
    Mcp,
    Skills,
    SlashCommands,
}

impl IntegrationTarget {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Skills => "skills",
            Self::SlashCommands => "slash_commands",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntegrationScope {
    Global,
    Project,
}

impl IntegrationScope {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetSelection {
    All,
    Detected,
    Explicit,
    Picker,
    Fallback,
}

impl TargetSelection {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Detected => "detected",
            Self::Explicit => "explicit",
            Self::Picker => "picker",
            Self::Fallback => "fallback",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntegrationResult {
    Ok,
    PartialError,
    AllCurrent,
    NoneCurrent,
    PartiallyCurrent,
}

impl IntegrationResult {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::PartialError => "partial_error",
            Self::AllCurrent => "all_current",
            Self::NoneCurrent => "none_current",
            Self::PartiallyCurrent => "partially_current",
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct IntegrationTelemetry {
    pub(crate) action: Option<IntegrationAction>,
    pub(crate) target: Option<IntegrationTarget>,
    pub(crate) scope: Option<IntegrationScope>,
    pub(crate) selection: Option<TargetSelection>,
    pub(crate) force: Option<bool>,
    pub(crate) target_agents: Option<CountBucket>,
    pub(crate) resolved_agents: Option<CountBucket>,
    pub(crate) result: Option<IntegrationResult>,
    pub(crate) modified_targets: Option<CountBucket>,
    pub(crate) already_installed: Option<bool>,
    pub(crate) updated: Option<bool>,
    pub(crate) current_targets: Option<CountBucket>,
    pub(crate) missing_targets: Option<CountBucket>,
    pub(crate) conflicting_targets: Option<CountBucket>,
    pub(crate) invalid_targets: Option<CountBucket>,
    pub(crate) unsupported_targets: Option<CountBucket>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpgradeMode {
    Manual,
    Auto,
}

impl UpgradeMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpgradeOperation {
    Apply,
    Check,
    Status,
    Enable,
    Disable,
}

impl UpgradeOperation {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Check => "check",
            Self::Status => "status",
            Self::Enable => "enable",
            Self::Disable => "disable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpgradeStatus {
    Available,
    UpToDate,
    Applied,
    Scheduled,
    DryRun,
    StatusChecked,
    AutoEnabled,
    AutoDisabled,
    Locked,
    Skipped,
    Failed,
    Unknown,
}

impl UpgradeStatus {
    pub(crate) fn from_safe_summary(value: &str) -> Self {
        match value {
            "available" => Self::Available,
            "up_to_date" => Self::UpToDate,
            "applied" => Self::Applied,
            "scheduled" => Self::Scheduled,
            "dry_run" => Self::DryRun,
            "status_checked" => Self::StatusChecked,
            "auto_enabled" => Self::AutoEnabled,
            "auto_disabled" => Self::AutoDisabled,
            "locked" => Self::Locked,
            "skipped" => Self::Skipped,
            "failed" => Self::Failed,
            _ => Self::Unknown,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::UpToDate => "up_to_date",
            Self::Applied => "applied",
            Self::Scheduled => "scheduled",
            Self::DryRun => "dry_run",
            Self::StatusChecked => "status_checked",
            Self::AutoEnabled => "auto_enabled",
            Self::AutoDisabled => "auto_disabled",
            Self::Locked => "locked",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpgradeChannel {
    Stable,
    Beta,
    Canary,
    Dev,
    Other,
}

impl UpgradeChannel {
    pub(crate) fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "stable" => Self::Stable,
            "beta" => Self::Beta,
            "canary" => Self::Canary,
            "dev" => Self::Dev,
            _ => Self::Other,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
            Self::Canary => "canary",
            Self::Dev => "dev",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpgradeFailureKind {
    LockFailed,
    UnmanagedInstall,
    MetadataFetch,
    SignatureVerify,
    MetadataInvalid,
    ArtifactVerify,
    ArtifactDownload,
    PolicyDisallowed,
    ApplyFailed,
}

impl UpgradeFailureKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::LockFailed => "lock_failed",
            Self::UnmanagedInstall => "unmanaged_install",
            Self::MetadataFetch => "metadata_fetch",
            Self::SignatureVerify => "signature_verify",
            Self::MetadataInvalid => "metadata_invalid",
            Self::ArtifactVerify => "artifact_verify",
            Self::ArtifactDownload => "artifact_download",
            Self::PolicyDisallowed => "policy_disallowed",
            Self::ApplyFailed => "apply_failed",
        }
    }
}

#[derive(Debug)]
pub(crate) struct UpgradeTelemetry {
    pub(crate) mode: UpgradeMode,
    pub(crate) operation: UpgradeOperation,
    pub(crate) dry_run: bool,
    pub(crate) status: Option<UpgradeStatus>,
    pub(crate) applied: Option<bool>,
    pub(crate) scheduled: Option<bool>,
    pub(crate) update_available: Option<bool>,
    pub(crate) managed_install: Option<bool>,
    pub(crate) self_upgrade_allowed: Option<bool>,
    pub(crate) auto_upgrade_allowed: Option<bool>,
    pub(crate) warning_count: Option<CountBucket>,
    pub(crate) channel: Option<UpgradeChannel>,
    pub(crate) failure_kind: Option<UpgradeFailureKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutoUpgradeSpawnStatus {
    AutoDisabled,
    Ci,
    BackgroundChild,
    NotDue,
    MarkerInvalid,
    CurrentExeError,
    Spawned,
    SpawnFailed,
}

impl AutoUpgradeSpawnStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::AutoDisabled => "auto_disabled",
            Self::Ci => "ci",
            Self::BackgroundChild => "background_child",
            Self::NotDue => "not_due",
            Self::MarkerInvalid => "marker_invalid",
            Self::CurrentExeError => "current_exe_error",
            Self::Spawned => "spawned",
            Self::SpawnFailed => "spawn_failed",
        }
    }
}

#[derive(Debug)]
pub(crate) struct AutoUpgradeTelemetry {
    pub(crate) due: bool,
    pub(crate) spawned: bool,
    pub(crate) status: AutoUpgradeSpawnStatus,
    pub(crate) channel: UpgradeChannel,
}

#[derive(Debug, Default)]
pub(crate) struct DoctorTelemetry {
    pub(crate) finding_count: Option<CountBucket>,
    pub(crate) healthy: Option<bool>,
}
