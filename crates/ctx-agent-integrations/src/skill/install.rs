use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::utc_now;
use serde::{Deserialize, Serialize};

use super::{
    paths::{bundled_hash, ensure_path_inside, sha256_hex},
    selection::{SkillAgentSelection, SkillSelectionSource},
    target::{resolve_targets_for_agents, single_target, SkillTarget},
    SkillAgentArg, BUNDLED_SKILL_BODY, BUNDLED_SKILL_NAME, LEGACY_BUNDLED_SKILL_HASHES,
    LEGACY_BUNDLED_SKILL_NAME, METADATA_FILE,
};
use crate::filesystem::{atomic_remove_if_unchanged, atomic_update};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillInstallStatus {
    Current,
    Stale,
    Modified,
    Missing,
}

impl SkillInstallStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Modified => "modified",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug)]
pub struct StatusResult {
    pub target: SkillTarget,
    pub status: SkillInstallStatus,
    pub metadata: Option<SkillMetadata>,
    pub installed_hash: Option<String>,
    pub legacy_skill_dir: Option<PathBuf>,
    pub legacy_status: Option<SkillInstallStatus>,
    legacy_snapshot: Option<LegacySkillSnapshot>,
}

#[derive(Debug)]
struct LegacySkillSnapshot {
    status: SkillInstallStatus,
    body: Vec<u8>,
    managed_metadata_body: Option<Vec<u8>>,
}

#[derive(Debug)]
pub struct InstallResult {
    pub target: SkillTarget,
    pub success: bool,
    pub fatal: bool,
    pub previous_status: SkillInstallStatus,
    pub status: SkillInstallStatus,
    pub already_installed: bool,
    pub updated: bool,
    pub migrated: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub schema_version: u32,
    pub installer: String,
    pub skill_name: String,
    pub skill_hash: String,
    pub ctx_cli_version: String,
    pub installed_at: String,
}

impl SkillMetadata {
    pub fn current(product_version: &str) -> Self {
        Self {
            schema_version: 1,
            installer: "ctx-cli".to_owned(),
            skill_name: BUNDLED_SKILL_NAME.to_owned(),
            skill_hash: bundled_hash(),
            ctx_cli_version: product_version.to_owned(),
            installed_at: utc_now().to_rfc3339(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SkillInstallRequest {
    pub selection: SkillAgentSelection,
    pub project: bool,
    pub force: bool,
    pub product_version: String,
}

#[derive(Debug)]
pub struct SkillInstallReceipt {
    pub project: bool,
    pub selection: SkillAgentSelection,
    pub results: Vec<InstallResult>,
    pub fatal_failures: usize,
    pub already_installed: bool,
    pub updated: bool,
    pub modified_targets: usize,
}

#[derive(Debug, Clone)]
pub struct SkillStatusRequest {
    pub selection: SkillAgentSelection,
    pub project: bool,
}

#[derive(Debug)]
pub struct SkillStatusReceipt {
    pub project: bool,
    pub selection: SkillAgentSelection,
    pub results: Vec<StatusResult>,
    pub current_count: usize,
}

pub fn execute_install(
    request: SkillInstallRequest,
    context: &super::PathContext,
) -> Result<SkillInstallReceipt> {
    let selection = include_installed_targets(request.selection, request.project, context)?;
    let targets = resolve_targets_for_agents(&selection.agents, request.project, context)?;
    let preserve_is_fatal = !matches!(
        selection.source,
        SkillSelectionSource::Detected | SkillSelectionSource::Fallback
    );
    let results = targets
        .iter()
        .map(|target| {
            install_target(
                target,
                request.force,
                preserve_is_fatal,
                &request.product_version,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(SkillInstallReceipt {
        project: request.project,
        fatal_failures: results.iter().filter(|result| result.fatal).count(),
        already_installed: results.iter().all(|result| result.already_installed),
        updated: results.iter().any(|result| result.updated),
        modified_targets: results.iter().filter(|result| result.updated).count(),
        selection,
        results,
    })
}

pub fn execute_status(
    request: SkillStatusRequest,
    context: &super::PathContext,
) -> Result<SkillStatusReceipt> {
    let selection = include_installed_targets(request.selection, request.project, context)?;
    let targets = resolve_targets_for_agents(&selection.agents, request.project, context)?;
    let results = targets
        .iter()
        .map(status_target)
        .collect::<Result<Vec<_>>>()?;
    let current_count = results
        .iter()
        .filter(|result| result.status == SkillInstallStatus::Current)
        .count();
    Ok(SkillStatusReceipt {
        project: request.project,
        selection,
        results,
        current_count,
    })
}

pub fn default_maintenance_selection(
    project: bool,
    context: &super::PathContext,
) -> Result<SkillAgentSelection> {
    include_installed_targets(super::default_agent_selection(context), project, context)
}

/// Extends default maintenance to every recognized location that already has
/// a current or legacy skill. Explicit and picker selections remain exact.
fn include_installed_targets(
    mut selection: SkillAgentSelection,
    project: bool,
    context: &super::PathContext,
) -> Result<SkillAgentSelection> {
    if !matches!(
        selection.source,
        SkillSelectionSource::Detected | SkillSelectionSource::Fallback
    ) {
        return Ok(selection);
    }
    let mut selected_skill_dirs = resolve_targets_for_agents(&selection.agents, project, context)?
        .into_iter()
        .map(|target| target.skill_dir)
        .collect::<Vec<_>>();
    for agent in SkillAgentArg::ALL.iter().copied() {
        let target = single_target(agent, project, context)?;
        if selected_skill_dirs.contains(&target.skill_dir) {
            continue;
        }
        if !has_installed_skill(&target)? {
            continue;
        }
        selection.agents.push(agent);
        selected_skill_dirs.push(target.skill_dir);
    }
    Ok(selection)
}

fn has_installed_skill(target: &SkillTarget) -> Result<bool> {
    let legacy_dir = legacy_skill_dir(target)?;
    let current = inspect_discovered_skill_path(&target.authority_root, &target.skill_dir)?;
    let legacy = inspect_discovered_skill_path(&target.authority_root, &legacy_dir)?;
    Ok(!matches!(
        (current, legacy),
        (DiscoveredSkillPath::Unsafe, _) | (_, DiscoveredSkillPath::Unsafe)
    ) && matches!(
        (current, legacy),
        (DiscoveredSkillPath::Present, _) | (_, DiscoveredSkillPath::Present)
    ))
}

#[derive(Clone, Copy)]
enum DiscoveredSkillPath {
    Present,
    Missing,
    Unsafe,
}

fn inspect_discovered_skill_path(
    authority_root: &Path,
    skill_dir: &Path,
) -> Result<DiscoveredSkillPath> {
    ensure_path_inside(authority_root, skill_dir)?;
    let relative = skill_dir
        .strip_prefix(authority_root)
        .map_err(|_| anyhow!("skill path escapes authority root"))?;
    let mut current = authority_root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata)
                if metadata.file_type().is_symlink() || is_windows_reparse_point(&metadata) =>
            {
                return Ok(DiscoveredSkillPath::Unsafe)
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Ok(DiscoveredSkillPath::Unsafe),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(DiscoveredSkillPath::Missing)
            }
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", current.display()))
            }
        }
    }
    let skill_file = skill_dir.join("SKILL.md");
    match fs::symlink_metadata(&skill_file) {
        Ok(metadata) if metadata.file_type().is_file() && !is_windows_reparse_point(&metadata) => {
            Ok(DiscoveredSkillPath::Present)
        }
        Ok(_) => Ok(DiscoveredSkillPath::Unsafe),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DiscoveredSkillPath::Missing),
        Err(error) => Err(error).with_context(|| format!("inspect skill {}", skill_file.display())),
    }
}

#[cfg(windows)]
fn is_windows_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_windows_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

pub fn install_target(
    target: &SkillTarget,
    force: bool,
    modified_preserve_is_fatal: bool,
    product_version: &str,
) -> Result<InstallResult> {
    let previous = status_target(target)?;
    let bundled_hash = bundled_hash();
    let current_content = previous.installed_hash.as_deref() == Some(bundled_hash.as_str());
    let migrates_legacy = previous.legacy_status.is_some();
    if previous.status == SkillInstallStatus::Modified && !force {
        let detail = if previous.legacy_status == Some(SkillInstallStatus::Modified) {
            format!(
                "preserved locally modified legacy {LEGACY_BUNDLED_SKILL_NAME} skill; use --force to migrate"
            )
        } else {
            format!(
                "preserved existing {} skill; use --force to replace",
                target.agent.display_name()
            )
        };
        return Ok(InstallResult {
            target: target.clone(),
            success: false,
            fatal: modified_preserve_is_fatal,
            previous_status: previous.status,
            status: previous.status,
            already_installed: false,
            updated: false,
            migrated: false,
            error: Some(detail),
        });
    }
    if current_content {
        if migrates_legacy {
            remove_legacy_skill_files(
                target,
                previous
                    .legacy_snapshot
                    .as_ref()
                    .expect("legacy status has a snapshot"),
            )?;
        }
        if !metadata_is_current(previous.metadata.as_ref(), product_version) {
            write_metadata(target, product_version)?;
        }
        return Ok(InstallResult {
            target: target.clone(),
            success: true,
            fatal: false,
            previous_status: previous.status,
            status: SkillInstallStatus::Current,
            already_installed: !migrates_legacy,
            updated: migrates_legacy,
            migrated: migrates_legacy,
            error: None,
        });
    }
    if migrates_legacy {
        let prior_body = read_optional_regular_file(&target.skill_dir.join("SKILL.md"))?;
        write_skill_body(target)?;
        if let Err(cleanup) = remove_legacy_skill_files(
            target,
            previous
                .legacy_snapshot
                .as_ref()
                .expect("legacy status has a snapshot"),
        ) {
            if let Err(rollback) = rollback_skill_body(target, prior_body.as_deref()) {
                return Err(anyhow!(
                    "{cleanup:#}; failed to roll back {} after migration cleanup failed: {rollback:#}",
                    target.skill_dir.join("SKILL.md").display()
                ));
            }
            return Err(cleanup);
        }
        write_metadata(target, product_version)?;
    } else {
        write_skill_files(target, product_version)?;
    }
    Ok(InstallResult {
        target: target.clone(),
        success: true,
        fatal: false,
        previous_status: previous.status,
        status: SkillInstallStatus::Current,
        already_installed: false,
        updated: migrates_legacy
            || matches!(
                previous.status,
                SkillInstallStatus::Stale | SkillInstallStatus::Modified
            ),
        migrated: migrates_legacy,
        error: None,
    })
}

pub fn status_target(target: &SkillTarget) -> Result<StatusResult> {
    ensure_path_inside(&target.base_dir, &target.skill_dir)?;
    validate_directory_path(&target.authority_root, &target.skill_dir)?;
    let (current_status, metadata, installed_hash) = inspect_current_skill(&target.skill_dir)?;
    let legacy_dir = legacy_skill_dir(target)?;
    validate_directory_path(&target.authority_root, &legacy_dir)?;
    let legacy = inspect_legacy_skill(&legacy_dir)?;
    let legacy_status = legacy.as_ref().map(|legacy| legacy.status);
    let status = if current_status == SkillInstallStatus::Modified
        || legacy_status == Some(SkillInstallStatus::Modified)
    {
        SkillInstallStatus::Modified
    } else if legacy_status.is_some() {
        SkillInstallStatus::Stale
    } else {
        current_status
    };
    Ok(StatusResult {
        target: target.clone(),
        status,
        metadata,
        installed_hash,
        legacy_skill_dir: legacy.as_ref().map(|_| legacy_dir),
        legacy_status,
        legacy_snapshot: legacy,
    })
}

fn inspect_current_skill(
    skill_dir: &Path,
) -> Result<(SkillInstallStatus, Option<SkillMetadata>, Option<String>)> {
    let skill_file = skill_dir.join("SKILL.md");
    let metadata = read_metadata(skill_dir);
    let installed_hash = read_skill_hash(&skill_file)?;
    let status = match installed_hash.as_deref() {
        None => SkillInstallStatus::Missing,
        Some(hash) if hash == bundled_hash() && metadata_manages_hash(metadata.as_ref(), hash) => {
            SkillInstallStatus::Current
        }
        Some(hash) if hash == bundled_hash() => SkillInstallStatus::Stale,
        Some(hash) if LEGACY_BUNDLED_SKILL_HASHES.contains(&hash) => SkillInstallStatus::Stale,
        Some(hash) => match metadata.as_ref() {
            Some(metadata) if metadata.skill_hash == hash => SkillInstallStatus::Stale,
            _ => SkillInstallStatus::Modified,
        },
    };
    Ok((status, metadata, installed_hash))
}

fn inspect_legacy_skill(skill_dir: &Path) -> Result<Option<LegacySkillSnapshot>> {
    let Some(body) = read_optional_regular_file(&skill_dir.join("SKILL.md"))? else {
        return Ok(None);
    };
    let installed_hash = sha256_hex(&body);
    let metadata_body = read_optional_regular_file(&skill_dir.join(METADATA_FILE))
        .ok()
        .flatten();
    let metadata = metadata_body
        .as_deref()
        .and_then(|body| serde_json::from_slice(body).ok());
    let metadata_is_managed = metadata_manages_legacy_hash(metadata.as_ref(), &installed_hash);
    let status =
        if LEGACY_BUNDLED_SKILL_HASHES.contains(&installed_hash.as_str()) || metadata_is_managed {
            SkillInstallStatus::Stale
        } else {
            SkillInstallStatus::Modified
        };
    Ok(Some(LegacySkillSnapshot {
        status,
        body,
        managed_metadata_body: metadata_is_managed.then_some(metadata_body).flatten(),
    }))
}

fn read_skill_hash(skill_file: &Path) -> Result<Option<String>> {
    Ok(read_optional_regular_file(skill_file)?.map(|body| sha256_hex(&body)))
}

fn ensure_safe_skill_directory(target: &SkillTarget) -> Result<()> {
    ensure_path_inside(&target.base_dir, &target.skill_dir)?;
    validate_directory_path(&target.authority_root, &target.skill_dir)?;
    fs::create_dir_all(&target.skill_dir)
        .with_context(|| format!("create {}", target.skill_dir.display()))
}

fn validate_directory_path(authority_root: &Path, path: &Path) -> Result<()> {
    ensure_path_inside(authority_root, path)?;
    let relative = path
        .strip_prefix(authority_root)
        .map_err(|_| anyhow!("skill path escapes authority root"))?;
    let mut current = authority_root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata)
                if metadata.file_type().is_symlink() || is_windows_reparse_point(&metadata) =>
            {
                return Err(anyhow!(
                    "skill path traverses a symlink or reparse point: {}",
                    current.display()
                ));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(anyhow!(
                    "skill path component is not a directory: {}",
                    current.display()
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", current.display()))
            }
        }
    }
    Ok(())
}

fn legacy_skill_dir(target: &SkillTarget) -> Result<PathBuf> {
    let path = target.base_dir.join(LEGACY_BUNDLED_SKILL_NAME);
    ensure_path_inside(&target.base_dir, &path)?;
    Ok(path)
}

fn remove_legacy_skill_files(target: &SkillTarget, legacy: &LegacySkillSnapshot) -> Result<()> {
    let legacy_dir = legacy_skill_dir(target)?;
    validate_directory_path(&target.authority_root, &legacy_dir)?;
    atomic_remove_if_unchanged(&legacy_dir.join("SKILL.md"), &legacy.body)
        .with_context(|| format!("remove {}", legacy_dir.join("SKILL.md").display()))?;
    if let Some(metadata_body) = &legacy.managed_metadata_body {
        // The skill file is the active integration surface. Metadata that was
        // concurrently edited is no longer installer-owned, so preserve it.
        let _ = atomic_remove_if_unchanged(&legacy_dir.join(METADATA_FILE), metadata_body);
    }
    Ok(())
}

fn read_optional_regular_file(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && !is_windows_reparse_point(&metadata) => {
            fs::read(path)
                .map(Some)
                .with_context(|| format!("read {}", path.display()))
        }
        Ok(_) => Err(anyhow!("target is not a regular file: {}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

fn rollback_skill_body(target: &SkillTarget, prior_body: Option<&[u8]>) -> Result<()> {
    let path = target.skill_dir.join("SKILL.md");
    match prior_body {
        Some(prior_body) => atomic_update(&path, |existing| {
            if existing != Some(BUNDLED_SKILL_BODY.as_bytes()) {
                return Err(anyhow!(
                    "refusing to overwrite concurrently changed target {}",
                    path.display()
                ));
            }
            Ok(prior_body.to_vec())
        }),
        None => atomic_remove_if_unchanged(&path, BUNDLED_SKILL_BODY.as_bytes()).map(|_| ()),
    }
}

fn write_skill_files(target: &SkillTarget, product_version: &str) -> Result<()> {
    write_skill_body(target)?;
    write_metadata(target, product_version)
}

fn write_skill_body(target: &SkillTarget) -> Result<()> {
    ensure_safe_skill_directory(target)?;
    atomic_update(&target.skill_dir.join("SKILL.md"), |_| {
        Ok(BUNDLED_SKILL_BODY.as_bytes().to_vec())
    })
}

fn write_metadata(target: &SkillTarget, product_version: &str) -> Result<()> {
    ensure_safe_skill_directory(target)?;
    let metadata = serde_json::to_vec_pretty(&SkillMetadata::current(product_version))?;
    atomic_update(&target.skill_dir.join(METADATA_FILE), |_| Ok(metadata))
}

fn read_metadata(skill_dir: &Path) -> Option<SkillMetadata> {
    let body = read_optional_regular_file(&skill_dir.join(METADATA_FILE))
        .ok()
        .flatten()?;
    serde_json::from_slice(&body).ok()
}

fn metadata_is_current(metadata: Option<&SkillMetadata>, product_version: &str) -> bool {
    let hash = bundled_hash();
    metadata_manages_hash(metadata, &hash)
        && metadata.is_some_and(|metadata| metadata.ctx_cli_version == product_version)
}

fn metadata_manages_hash(metadata: Option<&SkillMetadata>, hash: &str) -> bool {
    metadata.is_some_and(|metadata| {
        metadata.schema_version == 1
            && metadata.installer == "ctx-cli"
            && metadata.skill_name == BUNDLED_SKILL_NAME
            && metadata.skill_hash == hash
    })
}

fn metadata_manages_legacy_hash(metadata: Option<&SkillMetadata>, hash: &str) -> bool {
    metadata.is_some_and(|metadata| {
        metadata.schema_version == 1
            && metadata.installer == "ctx-cli"
            && metadata.skill_name == LEGACY_BUNDLED_SKILL_NAME
            && metadata.skill_hash == hash
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASED_LEGACY_SKILL_V0_17_0: &[u8] = include_bytes!("testdata/legacy_skill_v0_17_0.md");

    #[cfg(unix)]
    fn link_directory(target: &Path, link: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).unwrap();
        true
    }

    #[cfg(windows)]
    fn link_directory(target: &Path, link: &Path) -> bool {
        let status = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .status()
            .expect("create Windows directory junction");
        assert!(status.success(), "failed to create Windows junction");
        true
    }

    fn write_managed_legacy_skill(target: &SkillTarget, body: &[u8]) -> PathBuf {
        let legacy_dir = target.base_dir.join(LEGACY_BUNDLED_SKILL_NAME);
        fs::create_dir_all(&legacy_dir).unwrap();
        fs::write(legacy_dir.join("SKILL.md"), body).unwrap();
        let metadata = SkillMetadata {
            schema_version: 1,
            installer: "ctx-cli".to_owned(),
            skill_name: LEGACY_BUNDLED_SKILL_NAME.to_owned(),
            skill_hash: sha256_hex(body),
            ctx_cli_version: "0.9.0".to_owned(),
            installed_at: "2026-01-01T00:00:00Z".to_owned(),
        };
        fs::write(
            legacy_dir.join(METADATA_FILE),
            serde_json::to_vec_pretty(&metadata).unwrap(),
        )
        .unwrap();
        legacy_dir
    }

    #[test]
    fn default_install_includes_legacy_native_targets_in_global_and_project_scope() {
        for (agent, project) in [
            (super::super::SkillAgentArg::Codex, false),
            (super::super::SkillAgentArg::GrokBuild, true),
        ] {
            let root = tempfile::tempdir().unwrap();
            let context = super::super::PathContext::for_tests(
                root.path().join("home"),
                root.path().join("repo"),
            );
            let native_target = super::super::single_target(agent, project, &context).unwrap();
            let legacy_dir = write_managed_legacy_skill(&native_target, b"managed legacy\n");
            let selection = super::super::default_agent_selection(&context);

            let receipt = execute_install(
                SkillInstallRequest {
                    selection,
                    project,
                    force: false,
                    product_version: "1.0.0".to_owned(),
                },
                &context,
            )
            .unwrap();

            assert!(
                receipt.selection.agents.contains(&agent),
                "missing legacy target {} in project={project}",
                agent.id()
            );
            assert!(!legacy_dir.join("SKILL.md").exists());
            assert!(native_target.skill_dir.join("SKILL.md").is_file());
        }
    }

    #[cfg(unix)]
    #[test]
    fn default_maintenance_does_not_select_an_unowned_fifo_skill_file() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt as _};

        let root = tempfile::tempdir().unwrap();
        let context = super::super::PathContext::for_tests(
            root.path().join("home"),
            root.path().join("repo"),
        );
        let cursor =
            super::super::single_target(super::super::SkillAgentArg::Cursor, false, &context)
                .unwrap();
        fs::create_dir_all(&cursor.skill_dir).unwrap();
        let fifo = cursor.skill_dir.join("SKILL.md");
        let raw = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);

        let selection = include_installed_targets(
            super::super::default_agent_selection(&context),
            false,
            &context,
        )
        .unwrap();

        assert!(!selection
            .agents
            .contains(&super::super::SkillAgentArg::Cursor));
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn automatic_discovery_skips_a_safe_copy_with_an_unsafe_legacy_sibling() {
        let root = tempfile::tempdir().unwrap();
        let context = super::super::PathContext::for_tests(
            root.path().join("home"),
            root.path().join("repo"),
        );
        let cursor =
            super::super::single_target(super::super::SkillAgentArg::Cursor, false, &context)
                .unwrap();
        fs::create_dir_all(&cursor.skill_dir).unwrap();
        fs::write(cursor.skill_dir.join("SKILL.md"), BUNDLED_SKILL_BODY).unwrap();
        let outside = root.path().join("outside-legacy");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("SKILL.md"), BUNDLED_SKILL_BODY).unwrap();
        let legacy_dir = cursor.base_dir.join(LEGACY_BUNDLED_SKILL_NAME);
        if !link_directory(&outside, &legacy_dir) {
            return;
        }

        let selection = include_installed_targets(
            super::super::default_agent_selection(&context),
            false,
            &context,
        )
        .unwrap();

        assert!(!selection
            .agents
            .contains(&super::super::SkillAgentArg::Cursor));
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn every_selected_project_source_rejects_a_linked_ancestor() {
        for source in [
            SkillSelectionSource::Fallback,
            SkillSelectionSource::Explicit,
            SkillSelectionSource::Picker,
        ] {
            let root = tempfile::tempdir().unwrap();
            let repo = root.path().join("repo");
            let outside = root.path().join("outside-agents");
            fs::create_dir_all(&repo).unwrap();
            fs::create_dir_all(&outside).unwrap();
            if !link_directory(&outside, &repo.join(".agents")) {
                return;
            }
            let context = super::super::PathContext::for_tests(root.path().join("home"), repo);
            let selection = SkillAgentSelection {
                agents: vec![super::super::SkillAgentArg::Universal],
                source,
            };

            let status_error = execute_status(
                SkillStatusRequest {
                    selection: selection.clone(),
                    project: true,
                },
                &context,
            )
            .unwrap_err();
            let install_error = execute_install(
                SkillInstallRequest {
                    selection,
                    project: true,
                    force: false,
                    product_version: "1.0.0".to_owned(),
                },
                &context,
            )
            .unwrap_err();

            assert!(
                format!("{status_error:#}").contains("symlink or reparse point"),
                "{status_error:#}"
            );
            assert!(
                format!("{install_error:#}").contains("symlink or reparse point"),
                "{install_error:#}"
            );
            assert!(!outside.join("skills/ctx/SKILL.md").exists());
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn unrelated_override_cannot_authorize_another_agents_linked_root() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let cursor_root = home.join(".cursor");
        let outside = root.path().join("outside-cursor");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&outside).unwrap();
        if !link_directory(&outside, &cursor_root) {
            return;
        }
        let context = super::super::PathContext::for_tests(home, root.path().join("repo"))
            .with_env_override("CODEX_HOME", cursor_root);
        let selection = SkillAgentSelection {
            agents: vec![super::super::SkillAgentArg::Cursor],
            source: SkillSelectionSource::Explicit,
        };

        let error = execute_install(
            SkillInstallRequest {
                selection,
                project: false,
                force: false,
                product_version: "1.0.0".to_owned(),
            },
            &context,
        )
        .unwrap_err();

        assert!(
            format!("{error:#}").contains("symlink or reparse point"),
            "{error:#}"
        );
        assert!(!outside.join("skills/ctx/SKILL.md").exists());
    }

    #[test]
    fn metadata_free_released_legacy_skill_is_migrated() {
        let root = tempfile::tempdir().unwrap();
        let context = super::super::PathContext::for_tests(
            root.path().join("home"),
            root.path().join("repo"),
        );
        let target =
            super::super::single_target(super::super::SkillAgentArg::Universal, false, &context)
                .unwrap();
        let legacy_dir = target.base_dir.join(LEGACY_BUNDLED_SKILL_NAME);
        fs::create_dir_all(&legacy_dir).unwrap();
        fs::write(legacy_dir.join("SKILL.md"), RELEASED_LEGACY_SKILL_V0_17_0).unwrap();
        assert_eq!(
            sha256_hex(RELEASED_LEGACY_SKILL_V0_17_0),
            "sha256:d76cd55f506f6d8605f2fed933a16e4ab995b3a4ab8e6d96bfd84d469872b3d6"
        );
        assert!(!legacy_dir.join(METADATA_FILE).exists());

        let before = status_target(&target).unwrap();
        assert_eq!(before.status, SkillInstallStatus::Stale);
        assert_eq!(before.legacy_status, Some(SkillInstallStatus::Stale));

        let result = install_target(&target, false, true, "1.0.0").unwrap();

        assert!(result.success);
        assert!(result.migrated);
        assert_eq!(result.status, SkillInstallStatus::Current);
        assert!(!legacy_dir.join("SKILL.md").exists());
        assert_eq!(
            fs::read(target.skill_dir.join("SKILL.md")).unwrap(),
            BUNDLED_SKILL_BODY.as_bytes()
        );
        assert!(target.skill_dir.join(METADATA_FILE).is_file());
    }

    #[test]
    fn current_and_managed_legacy_copies_coexist_until_one_way_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let context = super::super::PathContext::for_tests(
            root.path().join("home"),
            root.path().join("repo"),
        );
        let target =
            super::super::single_target(super::super::SkillAgentArg::Universal, false, &context)
                .unwrap();
        install_target(&target, false, true, "1.0.0").unwrap();
        let legacy_dir = write_managed_legacy_skill(&target, b"managed legacy\n");
        let before = status_target(&target).unwrap();
        assert_eq!(before.status, SkillInstallStatus::Stale);
        assert_eq!(before.legacy_status, Some(SkillInstallStatus::Stale));
        let result = install_target(&target, false, true, "1.0.0").unwrap();
        assert!(result.success);
        assert!(result.migrated);
        assert!(result.updated);
        assert!(!legacy_dir.join("SKILL.md").exists());
        assert_eq!(
            fs::read(target.skill_dir.join("SKILL.md")).unwrap(),
            BUNDLED_SKILL_BODY.as_bytes()
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_legacy_cleanup_rolls_back_the_new_skill_body() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().unwrap();
        let context = super::super::PathContext::for_tests(
            root.path().join("home"),
            root.path().join("repo"),
        );
        let target =
            super::super::single_target(super::super::SkillAgentArg::Universal, false, &context)
                .unwrap();
        let legacy_dir = write_managed_legacy_skill(&target, b"managed legacy\n");
        fs::set_permissions(&legacy_dir, fs::Permissions::from_mode(0o555)).unwrap();

        let error = install_target(&target, false, true, "1.0.0").unwrap_err();

        fs::set_permissions(&legacy_dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(format!("{error:#}").contains("transaction lock"));
        assert!(legacy_dir.join("SKILL.md").is_file());
        assert!(!target.skill_dir.join("SKILL.md").exists());
    }

    #[test]
    fn grok_build_native_install_is_current_at_the_override_path() {
        let root = tempfile::tempdir().unwrap();
        let grok_home = root.path().join("grok-home");
        let context = super::super::PathContext::for_tests(
            root.path().join("home"),
            root.path().join("repo"),
        )
        .with_env_override("GROK_HOME", grok_home.clone());
        let target =
            super::super::single_target(super::super::SkillAgentArg::GrokBuild, false, &context)
                .unwrap();

        assert_eq!(
            target.skill_dir,
            grok_home.join("skills").join(BUNDLED_SKILL_NAME)
        );
        let result = install_target(&target, false, true, "1.0.0-test").unwrap();
        assert!(result.success);
        assert_eq!(
            status_target(&target).unwrap().status,
            SkillInstallStatus::Current
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn force_update_preserves_unrelated_skill_files() {
        let root = tempfile::tempdir().unwrap();
        let context = super::super::PathContext::for_tests(
            root.path().join("home"),
            root.path().join("repo"),
        );
        let target =
            super::super::single_target(super::super::SkillAgentArg::Universal, true, &context)
                .unwrap();
        fs::create_dir_all(&target.skill_dir).unwrap();
        fs::write(target.skill_dir.join("notes.txt"), "keep").unwrap();
        fs::write(target.skill_dir.join("SKILL.md"), "modified").unwrap();

        let result = install_target(&target, true, true, "1.0.0").unwrap();
        assert!(result.success);
        assert_eq!(
            fs::read_to_string(target.skill_dir.join("notes.txt")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn interrupted_content_then_metadata_publication_is_stale_and_recoverable() {
        let root = tempfile::tempdir().unwrap();
        let context = super::super::PathContext::for_tests(
            root.path().join("home"),
            root.path().join("repo"),
        );
        let target =
            super::super::single_target(super::super::SkillAgentArg::Universal, true, &context)
                .unwrap();
        fs::create_dir_all(target.skill_dir.join(METADATA_FILE)).unwrap();

        let error = install_target(&target, false, true, "1.0.0").unwrap_err();
        assert!(error.to_string().contains("non-regular file"));
        assert_eq!(
            fs::read(target.skill_dir.join("SKILL.md")).unwrap(),
            BUNDLED_SKILL_BODY.as_bytes()
        );
        assert_eq!(
            status_target(&target).unwrap().status,
            SkillInstallStatus::Stale
        );

        fs::remove_dir(target.skill_dir.join(METADATA_FILE)).unwrap();
        let repaired = install_target(&target, false, true, "1.0.0").unwrap();
        assert!(repaired.already_installed);
        assert!(!repaired.updated);
        assert_eq!(
            status_target(&target).unwrap().status,
            SkillInstallStatus::Current
        );
    }
}
