use std::{fs, path::Path};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::utc_now;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{analytics, AnalyticsProperties};

use super::{
    paths::{bundled_hash, ensure_path_inside, sha256_hex},
    selection::{install_agent_selection, status_agent_selection, SkillAgentSelection},
    target::{resolve_targets_for_agents, SkillTarget},
    SkillInstallArgs, SkillStatusArgs, BUNDLED_SKILL_BODY, BUNDLED_SKILL_NAME, METADATA_FILE,
};

pub(super) fn run_install(
    args: SkillInstallArgs,
    context: &super::paths::PathContext,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let selection = install_agent_selection(&args, context)?;
    insert_selection_analytics(analytics_properties, &selection);
    let targets = resolve_targets_for_agents(&selection.agents, args.project, context)?;
    let mut results = Vec::with_capacity(targets.len());
    for target in &targets {
        results.push(install_target(target, args.force)?);
    }
    let failed = results.iter().filter(|result| !result.success).count();
    let already_installed = results.iter().all(|result| result.already_installed);
    let updated = results.iter().any(|result| result.updated);
    analytics::insert_str(
        analytics_properties,
        "install_result",
        if failed == 0 { "ok" } else { "partial_error" },
    );
    analytics::insert_bool(analytics_properties, "already_installed", already_installed);
    analytics::insert_bool(analytics_properties, "updated", updated);
    if args.json {
        println!(
            "{}",
            json!({
                "skill": BUNDLED_SKILL_NAME,
                "scope": if args.project { "project" } else { "global" },
                "results": results.iter().map(InstallResult::to_json).collect::<Vec<_>>(),
            })
        );
    } else {
        print_install_results(&results);
    }
    if failed > 0 {
        return Err(anyhow!("failed to install skill for {failed} target(s)"));
    }
    Ok(())
}

pub(super) fn run_status(
    args: SkillStatusArgs,
    context: &super::paths::PathContext,
    analytics_properties: &mut AnalyticsProperties,
) -> Result<()> {
    let selection = status_agent_selection(&args, context);
    insert_selection_analytics(analytics_properties, &selection);
    let targets = resolve_targets_for_agents(&selection.agents, args.project, context)?;
    let results = targets
        .iter()
        .map(status_target)
        .collect::<Result<Vec<_>>>()?;
    let current_count = results
        .iter()
        .filter(|result| result.status == SkillInstallStatus::Current)
        .count();
    analytics::insert_str(
        analytics_properties,
        "status_result",
        if current_count == results.len() {
            "all_current"
        } else if current_count == 0 {
            "none_current"
        } else {
            "partially_current"
        },
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "current_targets_bucket",
        current_count as u64,
    );
    if args.json {
        println!(
            "{}",
            json!({
                "skill": BUNDLED_SKILL_NAME,
                "scope": if args.project { "project" } else { "global" },
                "results": results.iter().map(StatusResult::to_json).collect::<Vec<_>>(),
            })
        );
    } else {
        print_status_results(&results);
    }
    Ok(())
}

fn insert_selection_analytics(
    analytics_properties: &mut AnalyticsProperties,
    selection: &SkillAgentSelection,
) {
    analytics::insert_str(
        analytics_properties,
        "target_agent_group",
        selection.source.as_str(),
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "target_agents_count_bucket",
        selection.agents.len() as u64,
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SkillInstallStatus {
    Current,
    Stale,
    Modified,
    Missing,
}

impl SkillInstallStatus {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Modified => "modified",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug)]
pub(super) struct StatusResult {
    pub(super) target: SkillTarget,
    pub(super) status: SkillInstallStatus,
    pub(super) metadata: Option<SkillMetadata>,
    installed_hash: Option<String>,
}

impl StatusResult {
    fn to_json(&self) -> Value {
        json!({
            "agent": self.target.agent.id(),
            "agent_display_name": self.target.agent.display_name(),
            "scope": self.target.scope.as_str(),
            "status": self.status.as_str(),
            "path": self.target.skill_dir,
            "installed_hash": self.installed_hash,
            "bundled_hash": bundled_hash(),
            "metadata": self.metadata.as_ref().map(|metadata| json!({
                "schema_version": metadata.schema_version,
                "skill_name": metadata.skill_name,
                "skill_hash": metadata.skill_hash,
                "ctx_cli_version": metadata.ctx_cli_version,
            })),
        })
    }
}

#[derive(Debug)]
struct InstallResult {
    target: SkillTarget,
    success: bool,
    previous_status: SkillInstallStatus,
    status: SkillInstallStatus,
    already_installed: bool,
    updated: bool,
    error: Option<String>,
}

impl InstallResult {
    fn to_json(&self) -> Value {
        json!({
            "agent": self.target.agent.id(),
            "agent_display_name": self.target.agent.display_name(),
            "scope": self.target.scope.as_str(),
            "path": self.target.skill_dir,
            "success": self.success,
            "previous_status": self.previous_status.as_str(),
            "status": self.status.as_str(),
            "already_installed": self.already_installed,
            "updated": self.updated,
            "error": self.error,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SkillMetadata {
    schema_version: u32,
    installer: String,
    skill_name: String,
    pub(super) skill_hash: String,
    ctx_cli_version: String,
    installed_at: String,
}

impl SkillMetadata {
    pub(super) fn current() -> Self {
        Self {
            schema_version: 1,
            installer: "ctx-cli".to_owned(),
            skill_name: BUNDLED_SKILL_NAME.to_owned(),
            skill_hash: bundled_hash(),
            ctx_cli_version: env!("CARGO_PKG_VERSION").to_owned(),
            installed_at: utc_now().to_rfc3339(),
        }
    }
}

fn install_target(target: &SkillTarget, force: bool) -> Result<InstallResult> {
    let previous = status_target(target)?;
    if previous.status == SkillInstallStatus::Current {
        if !metadata_is_current(previous.metadata.as_ref()) {
            write_metadata(target)?;
        }
        return Ok(InstallResult {
            target: target.clone(),
            success: true,
            previous_status: previous.status,
            status: SkillInstallStatus::Current,
            already_installed: true,
            updated: false,
            error: None,
        });
    }
    if previous.status == SkillInstallStatus::Modified && !force {
        return Ok(InstallResult {
            target: target.clone(),
            success: false,
            previous_status: previous.status,
            status: previous.status,
            already_installed: false,
            updated: false,
            error: Some("local skill edits detected; rerun with --force to overwrite".to_owned()),
        });
    }
    write_skill_dir(target)?;
    Ok(InstallResult {
        target: target.clone(),
        success: true,
        previous_status: previous.status,
        status: SkillInstallStatus::Current,
        already_installed: false,
        updated: matches!(
            previous.status,
            SkillInstallStatus::Stale | SkillInstallStatus::Modified
        ),
        error: None,
    })
}

pub(super) fn status_target(target: &SkillTarget) -> Result<StatusResult> {
    ensure_path_inside(&target.base_dir, &target.skill_dir)?;
    let skill_file = target.skill_dir.join("SKILL.md");
    let metadata = read_metadata(&target.skill_dir);
    let installed_hash = match fs::read(&skill_file) {
        Ok(body) => Some(sha256_hex(&body)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err).with_context(|| format!("read {}", skill_file.display())),
    };
    let status = match installed_hash.as_deref() {
        None => SkillInstallStatus::Missing,
        Some(hash) if hash == bundled_hash() => SkillInstallStatus::Current,
        Some(hash) => match metadata.as_ref() {
            Some(metadata) if metadata.skill_hash == hash => SkillInstallStatus::Stale,
            _ => SkillInstallStatus::Modified,
        },
    };
    Ok(StatusResult {
        target: target.clone(),
        status,
        metadata,
        installed_hash,
    })
}

fn read_metadata(skill_dir: &Path) -> Option<SkillMetadata> {
    let path = skill_dir.join(METADATA_FILE);
    let body = fs::read(path).ok()?;
    serde_json::from_slice(&body).ok()
}

fn metadata_is_current(metadata: Option<&SkillMetadata>) -> bool {
    metadata.is_some_and(|metadata| {
        metadata.schema_version == 1
            && metadata.installer == "ctx-cli"
            && metadata.skill_name == BUNDLED_SKILL_NAME
            && metadata.skill_hash == bundled_hash()
    })
}

pub(super) fn write_skill_dir(target: &SkillTarget) -> Result<()> {
    ensure_path_inside(&target.base_dir, &target.skill_dir)?;
    remove_existing_target(&target.skill_dir)
        .with_context(|| format!("remove existing {}", target.skill_dir.display()))?;
    fs::create_dir_all(&target.skill_dir)
        .with_context(|| format!("create {}", target.skill_dir.display()))?;
    fs::write(target.skill_dir.join("SKILL.md"), BUNDLED_SKILL_BODY)
        .with_context(|| format!("write {}", target.skill_dir.join("SKILL.md").display()))?;
    write_metadata(target)
}

fn write_metadata(target: &SkillTarget) -> Result<()> {
    fs::create_dir_all(&target.skill_dir)
        .with_context(|| format!("create {}", target.skill_dir.display()))?;
    let metadata = serde_json::to_vec_pretty(&SkillMetadata::current())?;
    fs::write(target.skill_dir.join(METADATA_FILE), metadata)
        .with_context(|| format!("write {}", target.skill_dir.join(METADATA_FILE).display()))
}

fn remove_existing_target(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            fs::remove_file(path)?;
        }
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(path)?;
        }
        Ok(_) => {
            fs::remove_file(path)?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

fn print_install_results(results: &[InstallResult]) {
    let all_success = !results.is_empty() && results.iter().all(|result| result.success);
    let all_current = all_success && results.iter().all(|result| result.already_installed);
    let any_updated = results
        .iter()
        .any(|result| result.success && result.updated);
    let any_installed = results
        .iter()
        .any(|result| result.success && !result.already_installed && !result.updated);
    let heading = if all_current {
        "Agent skill already installed"
    } else if all_success && any_updated && !any_installed {
        "Agent skill updated"
    } else if all_success {
        "Agent skill installed"
    } else {
        "Agent skill"
    };
    println!("{heading}: {BUNDLED_SKILL_NAME}");
    for result in results {
        let verb = if result.already_installed {
            "current"
        } else if !result.success {
            "skipped"
        } else if result.updated {
            "updated"
        } else {
            "installed"
        };
        let detail = result
            .error
            .as_deref()
            .map(|error| format!(" - {error}"))
            .unwrap_or_default();
        println!("  {verb}: {}{}", result.target.agent.display_name(), detail);
    }
}

fn print_status_results(results: &[StatusResult]) {
    println!("ctx skill status: {BUNDLED_SKILL_NAME}");
    for result in results {
        println!(
            "  {}: {} ({}) -> {}",
            result.status.as_str(),
            result.target.agent.display_name(),
            result.target.scope.as_str(),
            result.target.skill_dir.display()
        );
    }
}
