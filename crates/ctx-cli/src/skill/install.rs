use std::{fs, path::Path};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::utc_now;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    analytics::{count_bucket, IntegrationResult, IntegrationTelemetry, TargetSelection},
    ui::{fields, outcome, section, Document, Field, Outcome, OutcomeState, RenderContext, Ui},
};

use super::{
    paths::{bundled_hash, ensure_path_inside, sha256_hex},
    selection::{
        install_agent_selection, status_agent_selection, SkillAgentSelection, SkillSelectionSource,
    },
    target::{resolve_targets_for_agents, SkillTarget},
    SkillInstallArgs, SkillStatusArgs, BUNDLED_SKILL_BODY, BUNDLED_SKILL_NAME,
    LEGACY_BUNDLED_SKILL_HASHES, METADATA_FILE,
};

pub(super) fn run_install(
    args: SkillInstallArgs,
    context: &super::paths::PathContext,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let selection = install_agent_selection(&args, context)?;
    insert_selection_analytics(telemetry, &selection);
    let targets = resolve_targets_for_agents(&selection.agents, args.project, context)?;
    let mut results = Vec::with_capacity(targets.len());
    let modified_preserve_is_fatal = modified_preserve_is_fatal(selection.source);
    for target in &targets {
        results.push(install_target(
            target,
            args.force,
            modified_preserve_is_fatal,
        )?);
    }
    let fatal_failures = results.iter().filter(|result| result.fatal).count();
    let already_installed = results.iter().all(|result| result.already_installed);
    let updated = results.iter().any(|result| result.updated);
    telemetry.result = Some(if fatal_failures == 0 {
        IntegrationResult::Ok
    } else {
        IntegrationResult::PartialError
    });
    telemetry.already_installed = Some(already_installed);
    telemetry.updated = Some(updated);
    telemetry.modified_targets = Some(count_bucket(
        results.iter().filter(|result| result.updated).count() as u64,
    ));
    if args.format.is_json() {
        println!(
            "{}",
            json!({
                "skill": BUNDLED_SKILL_NAME,
                "scope": if args.project { "project" } else { "global" },
                "results": results.iter().map(InstallResult::to_json).collect::<Vec<_>>(),
            })
        );
    } else {
        let document = render_install_results(ui.stdout_context(), &results);
        ui.write_stdout(&document)?;
    }
    if fatal_failures > 0 {
        return Err(anyhow!(
            "failed to install skill for {fatal_failures} target(s)"
        ));
    }
    Ok(())
}

pub(super) fn run_status(
    args: SkillStatusArgs,
    context: &super::paths::PathContext,
    telemetry: &mut IntegrationTelemetry,
    ui: &mut Ui,
) -> Result<()> {
    let selection = status_agent_selection(&args, context);
    insert_selection_analytics(telemetry, &selection);
    let targets = resolve_targets_for_agents(&selection.agents, args.project, context)?;
    let results = targets
        .iter()
        .map(status_target)
        .collect::<Result<Vec<_>>>()?;
    let current_count = results
        .iter()
        .filter(|result| result.status == SkillInstallStatus::Current)
        .count();
    telemetry.result = Some(if current_count == results.len() {
        IntegrationResult::AllCurrent
    } else if current_count == 0 {
        IntegrationResult::NoneCurrent
    } else {
        IntegrationResult::PartiallyCurrent
    });
    telemetry.current_targets = Some(count_bucket(current_count as u64));
    telemetry.missing_targets = Some(count_bucket(
        results
            .iter()
            .filter(|result| result.status == SkillInstallStatus::Missing)
            .count() as u64,
    ));
    telemetry.conflicting_targets = Some(count_bucket(
        results
            .iter()
            .filter(|result| result.status == SkillInstallStatus::Modified)
            .count() as u64,
    ));
    if args.format.is_json() {
        println!(
            "{}",
            json!({
                "skill": BUNDLED_SKILL_NAME,
                "scope": if args.project { "project" } else { "global" },
                "results": results.iter().map(StatusResult::to_json).collect::<Vec<_>>(),
            })
        );
    } else {
        let document = render_status_results(ui.stdout_context(), &results);
        ui.write_stdout(&document)?;
    }
    Ok(())
}

fn insert_selection_analytics(
    telemetry: &mut IntegrationTelemetry,
    selection: &SkillAgentSelection,
) {
    telemetry.selection = Some(match selection.source {
        SkillSelectionSource::Explicit => TargetSelection::Explicit,
        SkillSelectionSource::All => TargetSelection::All,
        SkillSelectionSource::Picker => TargetSelection::Picker,
        SkillSelectionSource::Detected => TargetSelection::Detected,
        SkillSelectionSource::Fallback => TargetSelection::Fallback,
    });
    telemetry.resolved_agents = Some(count_bucket(selection.agents.len() as u64));
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
    fatal: bool,
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

fn install_target(
    target: &SkillTarget,
    force: bool,
    modified_preserve_is_fatal: bool,
) -> Result<InstallResult> {
    let previous = status_target(target)?;
    if previous.status == SkillInstallStatus::Current {
        if !metadata_is_current(previous.metadata.as_ref()) {
            write_metadata(target)?;
        }
        return Ok(InstallResult {
            target: target.clone(),
            success: true,
            fatal: false,
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
            fatal: modified_preserve_is_fatal,
            previous_status: previous.status,
            status: previous.status,
            already_installed: false,
            updated: false,
            error: Some(format!(
                "preserved existing {} skill; use --force to replace",
                target.agent.display_name()
            )),
        });
    }
    write_skill_dir(target)?;
    Ok(InstallResult {
        target: target.clone(),
        success: true,
        fatal: false,
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
        Some(hash) if is_legacy_bundled_hash(hash) => SkillInstallStatus::Stale,
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

fn modified_preserve_is_fatal(source: SkillSelectionSource) -> bool {
    !matches!(
        source,
        SkillSelectionSource::Detected | SkillSelectionSource::Fallback
    )
}

fn is_legacy_bundled_hash(hash: &str) -> bool {
    LEGACY_BUNDLED_SKILL_HASHES.contains(&hash)
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
            && metadata.ctx_cli_version == env!("CARGO_PKG_VERSION")
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

fn render_install_results(context: &RenderContext, results: &[InstallResult]) -> Document {
    let all_success = !results.is_empty() && results.iter().all(|result| result.success);
    let all_current = all_success && results.iter().all(|result| result.already_installed);
    let any_updated = results
        .iter()
        .any(|result| result.success && result.updated);
    let any_installed = results
        .iter()
        .any(|result| result.success && !result.already_installed && !result.updated);
    let title = if all_current {
        "Agent skill is already installed"
    } else if all_success && any_updated && !any_installed {
        "Agent skill updated"
    } else if all_success {
        "Agent skill installed"
    } else {
        "Agent skill needs attention"
    };
    let mut document = outcome(
        context,
        Outcome {
            state: if all_success {
                OutcomeState::Success
            } else {
                OutcomeState::Warning
            },
            title,
            detail: None,
        },
    );
    document.push_blank();
    document.append(fields(context, &[Field::new("Skill", BUNDLED_SKILL_NAME)]));

    let rows = results
        .iter()
        .map(|result| {
            let status = if result.already_installed {
                "current"
            } else if !result.success {
                "skipped"
            } else if result.updated {
                "updated"
            } else {
                "installed"
            };
            let mut detail = result.target.agent.display_name().to_owned();
            if let Some(error) = &result.error {
                detail.push_str(" - ");
                detail.push_str(error);
            }
            (status, detail)
        })
        .collect::<Vec<_>>();
    let target_fields = rows
        .iter()
        .map(|(status, detail)| Field::new(status, detail))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("Targets", fields(context, &target_fields)));
    document
}

fn render_status_results(context: &RenderContext, results: &[StatusResult]) -> Document {
    let all_current = !results.is_empty()
        && results
            .iter()
            .all(|result| result.status == SkillInstallStatus::Current);
    let mut document = outcome(
        context,
        Outcome {
            state: if all_current {
                OutcomeState::Success
            } else {
                OutcomeState::Warning
            },
            title: if all_current {
                "Agent skill is current"
            } else {
                "Agent skill needs attention"
            },
            detail: None,
        },
    );
    document.push_blank();
    document.append(fields(context, &[Field::new("Skill", BUNDLED_SKILL_NAME)]));

    let rows = results
        .iter()
        .map(|result| {
            (
                result.status.as_str(),
                format!(
                    "{} ({}) -> {}",
                    result.target.agent.display_name(),
                    result.target.scope.as_str(),
                    result.target.skill_dir.display()
                ),
            )
        })
        .collect::<Vec<_>>();
    let target_fields = rows
        .iter()
        .map(|(status, detail)| Field::new(status, detail))
        .collect::<Vec<_>>();
    document.push_blank();
    document.append(section("Targets", fields(context, &target_fields)));
    document
}

#[cfg(test)]
mod render_tests {
    use std::io::Write as _;

    use super::*;
    use crate::{
        skill::agents::SkillAgentArg,
        ui::{ColorMode, StreamKind, TestContext},
    };

    fn render_context(width: usize, color: ColorMode) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
    }

    fn strip_ansi(rendered: &str) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        stream.write_all(rendered.as_bytes()).unwrap();
        String::from_utf8(stream.into_inner()).unwrap()
    }

    #[test]
    fn human_install_and_status_results_use_the_typed_ui() {
        let temp = tempfile::tempdir().unwrap();
        let path_context = super::super::paths::PathContext::for_tests(
            temp.path().join("home"),
            temp.path().join("repo"),
        );
        let target = resolve_targets_for_agents(&[SkillAgentArg::Universal], false, &path_context)
            .unwrap()
            .remove(0);
        let missing = status_target(&target).unwrap();
        let installed = install_target(&target, false, true).unwrap();
        let current = status_target(&target).unwrap();

        for (document, expected) in [
            (
                render_install_results(&render_context(80, ColorMode::Never), &[installed]),
                "Agent skill installed",
            ),
            (
                render_status_results(&render_context(80, ColorMode::Never), &[missing]),
                "Agent skill needs attention",
            ),
            (
                render_status_results(&render_context(80, ColorMode::Never), &[current]),
                "Agent skill is current",
            ),
        ] {
            let plain = document.render_plain();
            assert!(plain.contains(expected), "{plain}");
            assert!(plain.contains("Skill"), "{plain}");
            assert!(plain.contains("Targets"), "{plain}");
        }

        let color = render_context(80, ColorMode::Always);
        let document = render_status_results(&color, &[status_target(&target).unwrap()]);
        let styled = document.render(&color);
        assert!(styled.as_bytes().contains(&0x1b), "{styled:?}");
        assert_eq!(strip_ansi(&styled), document.render_plain());
    }
}
