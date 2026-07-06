use std::path::PathBuf;

use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub(super) struct InstallMarker {
    pub(super) install_path: PathBuf,
    pub(super) platform: String,
    pub(super) channel: String,
    pub(super) version: String,
    pub(super) sha256: String,
}

#[derive(Debug, Clone)]
pub(super) struct ReleaseMetadata {
    pub(super) version: String,
    pub(super) base_url: String,
    pub(super) artifact: String,
    pub(super) sha256: String,
    pub(super) source_commit: Option<String>,
    pub(super) published_at: Option<String>,
    pub(super) self_upgrade_allowed: bool,
    pub(super) auto_upgrade_allowed: bool,
    pub(super) store_schema_version: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct UpgradePlan {
    pub(super) current_version: String,
    pub(super) latest_version: String,
    pub(super) channel: String,
    pub(super) platform: String,
    pub(super) metadata_url: String,
    pub(super) artifact_url: String,
    pub(super) artifact_sha256: String,
    pub(super) install_path: PathBuf,
    pub(super) update_available: bool,
    pub(super) managed: bool,
    pub(super) warnings: Vec<String>,
    pub(super) path: PathDiagnostics,
    pub(super) metadata: ReleaseMetadata,
}

#[derive(Debug, Clone)]
pub(super) struct PathDiagnostics {
    pub(super) current_exe: PathBuf,
    pub(super) entries: Vec<PathDiagnosticEntry>,
    pub(super) warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct PathDiagnosticEntry {
    pub(super) path: PathBuf,
    pub(super) version: Option<String>,
    pub(super) current: bool,
}

#[derive(Debug, Clone)]
pub(super) struct UpgradeOutcome {
    pub(super) command: &'static str,
    pub(super) status: &'static str,
    pub(super) message: String,
    pub(super) plan: Option<UpgradePlan>,
    pub(super) applied: bool,
    pub(super) dry_run: bool,
    pub(super) warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ApplyResult {
    Applied,
    Scheduled,
}

impl UpgradeOutcome {
    pub(super) fn json(&self) -> Value {
        let plan = self.plan.as_ref();
        json!({
            "schema_version": 1,
            "command": self.command,
            "ok": true,
            "status": self.status,
            "message": self.message,
            "current_version": plan.map(|plan| plan.current_version.as_str()),
            "latest_version": plan.map(|plan| plan.latest_version.as_str()),
            "update_available": plan.map(|plan| plan.update_available).unwrap_or(false),
            "channel": plan.map(|plan| plan.channel.as_str()),
            "platform": plan.map(|plan| plan.platform.as_str()),
            "metadata_url": plan.map(|plan| plan.metadata_url.as_str()),
            "artifact_url": plan.map(|plan| plan.artifact_url.as_str()),
            "install_path": plan.map(|plan| plan.install_path.display().to_string()),
            "managed": plan.map(|plan| plan.managed).unwrap_or(false),
            "path": plan.map(|plan| plan.path.json()),
            "applied": self.applied,
            "dry_run": self.dry_run,
            "warnings": self.warnings,
        })
    }
}

impl PathDiagnostics {
    pub(super) fn json(&self) -> Value {
        json!({
            "current_exe": self.current_exe.display().to_string(),
            "first_ctx": self.entries.first().map(|entry| entry.path.display().to_string()),
            "entries": self.entries.iter().map(|entry| {
                json!({
                    "path": entry.path.display().to_string(),
                    "version": entry.version.as_deref(),
                    "current": entry.current,
                })
            }).collect::<Vec<_>>(),
            "warnings": self.warnings,
        })
    }
}
