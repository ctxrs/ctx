use super::CountBucket;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationAction {
    Install,
    Remove,
    Status,
}

impl IntegrationAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Remove => "remove",
            Self::Status => "status",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationTarget {
    Mcp,
    Skills,
    SlashCommands,
}

impl IntegrationTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Skills => "skills",
            Self::SlashCommands => "slash_commands",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationScope {
    Global,
    Project,
}

impl IntegrationScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSelection {
    All,
    Detected,
    Explicit,
    Picker,
    Fallback,
}

impl TargetSelection {
    pub fn as_str(self) -> &'static str {
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
pub enum IntegrationResult {
    Ok,
    PartialError,
    AllCurrent,
    NoneCurrent,
    PartiallyCurrent,
}

impl IntegrationResult {
    pub fn as_str(self) -> &'static str {
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
pub struct IntegrationTelemetry {
    pub action: Option<IntegrationAction>,
    pub target: Option<IntegrationTarget>,
    pub scope: Option<IntegrationScope>,
    pub selection: Option<TargetSelection>,
    pub force: Option<bool>,
    pub target_agents: Option<CountBucket>,
    pub resolved_agents: Option<CountBucket>,
    pub result: Option<IntegrationResult>,
    pub modified_targets: Option<CountBucket>,
    pub already_installed: Option<bool>,
    pub updated: Option<bool>,
    pub current_targets: Option<CountBucket>,
    pub missing_targets: Option<CountBucket>,
    pub conflicting_targets: Option<CountBucket>,
    pub invalid_targets: Option<CountBucket>,
    pub unsupported_targets: Option<CountBucket>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeMode {
    Manual,
    Auto,
}

impl UpgradeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeOperation {
    Apply,
    Check,
    Status,
    Enable,
    Disable,
}

impl UpgradeOperation {
    pub fn as_str(self) -> &'static str {
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
pub enum UpgradeStatus {
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
    pub fn from_safe_summary(value: &str) -> Self {
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

    pub fn as_str(self) -> &'static str {
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
pub enum UpgradeChannel {
    Stable,
    Beta,
    Canary,
    Dev,
    Other,
}

impl UpgradeChannel {
    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "stable" => Self::Stable,
            "beta" => Self::Beta,
            "canary" => Self::Canary,
            "dev" => Self::Dev,
            _ => Self::Other,
        }
    }

    pub fn as_str(self) -> &'static str {
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
pub enum UpgradeFailureKind {
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
    pub fn as_str(self) -> &'static str {
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
pub struct UpgradeTelemetry {
    pub mode: UpgradeMode,
    pub operation: UpgradeOperation,
    pub dry_run: bool,
    pub suppress_event: bool,
    pub status: Option<UpgradeStatus>,
    pub applied: Option<bool>,
    pub scheduled: Option<bool>,
    pub update_available: Option<bool>,
    pub update_was_available: Option<bool>,
    pub upgrade_attempt_id: Option<String>,
    pub managed_install: Option<bool>,
    pub self_upgrade_allowed: Option<bool>,
    pub auto_upgrade_allowed: Option<bool>,
    pub warning_count: Option<CountBucket>,
    pub channel: Option<UpgradeChannel>,
    pub failure_kind: Option<UpgradeFailureKind>,
}

#[derive(Debug, Default)]
pub struct DoctorTelemetry {
    pub finding_count: Option<CountBucket>,
    pub healthy: Option<bool>,
}
