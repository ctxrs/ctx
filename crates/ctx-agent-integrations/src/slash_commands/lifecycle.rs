use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};

use super::{
    combined_file_status, ensure_path_inside, legacy_metadata_manages_hash, metadata_manages_hash,
    selected_agents, sha256_hex, CommandFileTarget, PathContext, SlashCommandAgent,
    SlashCommandInstallStatus, SlashCommandMetadata, SlashCommandPlan, SlashCommandScope,
    COMMAND_NAME, LEGACY_COMMAND_NAME, METADATA_FILE,
};

mod directory_fence;

use directory_fence::DirectoryFence;

#[derive(Debug, Clone)]
pub struct SlashCommandStatusRequest {
    pub agents: Vec<SlashCommandAgent>,
    pub all_agents: bool,
    pub project: bool,
}

#[derive(Debug)]
pub struct SlashCommandStatusReceipt {
    pub request: SlashCommandStatusRequest,
    pub selected_agents: usize,
    pub failed: usize,
    pub results: Vec<SlashCommandStatusResult>,
}

#[derive(Debug)]
pub struct SlashCommandStatusResult {
    pub agent: SlashCommandAgent,
    pub scope: Option<SlashCommandScope>,
    pub path: Option<PathBuf>,
    pub legacy_path: Option<PathBuf>,
    pub success: bool,
    pub status: SlashCommandInstallStatus,
    pub force_required: bool,
    pub error: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SlashCommandRemoveRequest {
    pub agents: Vec<SlashCommandAgent>,
    pub all_agents: bool,
    pub project: bool,
    pub force: bool,
}

#[derive(Debug)]
pub struct SlashCommandRemoveReceipt {
    pub project: bool,
    pub selected_agents: usize,
    pub failed: usize,
    pub modified_targets: usize,
    pub results: Vec<SlashCommandRemoveResult>,
}

#[derive(Debug)]
pub struct SlashCommandRemoveResult {
    pub agent: SlashCommandAgent,
    pub scope: Option<SlashCommandScope>,
    pub path: Option<PathBuf>,
    pub legacy_path: Option<PathBuf>,
    pub success: bool,
    pub previous_status: SlashCommandInstallStatus,
    pub status: SlashCommandInstallStatus,
    pub already_absent: bool,
    pub modified: bool,
    pub current_removed: bool,
    pub legacy_removed: bool,
    pub metadata_removed: bool,
    pub force_required: bool,
    pub error: Option<String>,
    pub note: Option<String>,
}

pub fn execute_status(
    request: SlashCommandStatusRequest,
    context: &PathContext,
) -> SlashCommandStatusReceipt {
    let agents = selected_agents(
        &request.agents,
        request.all_agents,
        request.project,
        context,
    );
    let results = agents
        .iter()
        .copied()
        .map(|agent| {
            status_plan(
                agent.install_plan(request.project, context),
                &authority_root(agent, request.project, context),
            )
        })
        .collect::<Vec<_>>();
    SlashCommandStatusReceipt {
        selected_agents: agents.len(),
        failed: results.iter().filter(|result| !result.success).count(),
        results,
        request,
    }
}

pub fn execute_remove(
    request: SlashCommandRemoveRequest,
    context: &PathContext,
) -> SlashCommandRemoveReceipt {
    let agents = selected_agents(
        &request.agents,
        request.all_agents,
        request.project,
        context,
    );
    let results = agents
        .iter()
        .copied()
        .map(|agent| {
            remove_plan(
                agent.install_plan(request.project, context),
                &authority_root(agent, request.project, context),
                request.force,
            )
        })
        .collect::<Vec<_>>();
    SlashCommandRemoveReceipt {
        project: request.project,
        selected_agents: agents.len(),
        failed: results.iter().filter(|result| !result.success).count(),
        modified_targets: results.iter().filter(|result| result.modified).count(),
        results,
    }
}

fn status_plan(plan: SlashCommandPlan, authority_root: &Path) -> SlashCommandStatusResult {
    match plan {
        SlashCommandPlan::File(target) => match FilePreflight::read(&target, authority_root) {
            Ok(preflight) => status_file_result(&target, &preflight),
            Err(error) => SlashCommandStatusResult {
                agent: target.agent,
                scope: Some(target.scope),
                path: Some(target.command_path()),
                legacy_path: safe_existing_path(authority_root, target.legacy_command_path()),
                success: false,
                status: SlashCommandInstallStatus::Modified,
                force_required: false,
                error: Some(format!("{error:#}")),
                note: None,
            },
        },
        SlashCommandPlan::SkillOnly { agent, note } => informational_status(
            agent,
            SlashCommandInstallStatus::SkillOnly,
            note.replace("<agent>", agent.id()),
        ),
        SlashCommandPlan::ManualOnly { agent, note } => informational_status(
            agent,
            SlashCommandInstallStatus::ManualOnly,
            note.to_owned(),
        ),
    }
}

fn status_file_result(
    target: &CommandFileTarget,
    preflight: &FilePreflight,
) -> SlashCommandStatusResult {
    SlashCommandStatusResult {
        agent: target.agent,
        scope: Some(target.scope),
        path: Some(target.command_path()),
        legacy_path: preflight
            .legacy
            .exists()
            .then(|| preflight.legacy.path.clone()),
        success: true,
        status: preflight.status,
        force_required: preflight.has_unowned_file(),
        error: None,
        note: None,
    }
}

fn informational_status(
    agent: SlashCommandAgent,
    status: SlashCommandInstallStatus,
    note: String,
) -> SlashCommandStatusResult {
    SlashCommandStatusResult {
        agent,
        scope: None,
        path: None,
        legacy_path: None,
        success: true,
        status,
        force_required: false,
        error: None,
        note: Some(note),
    }
}

fn remove_plan(
    plan: SlashCommandPlan,
    authority_root: &Path,
    force: bool,
) -> SlashCommandRemoveResult {
    match plan {
        SlashCommandPlan::File(target) => remove_file_target(&target, authority_root, force),
        SlashCommandPlan::SkillOnly { agent, note } => informational_remove(
            agent,
            SlashCommandInstallStatus::SkillOnly,
            note.replace("<agent>", agent.id()),
        ),
        SlashCommandPlan::ManualOnly { agent, note } => informational_remove(
            agent,
            SlashCommandInstallStatus::ManualOnly,
            note.to_owned(),
        ),
    }
}

fn informational_remove(
    agent: SlashCommandAgent,
    status: SlashCommandInstallStatus,
    note: String,
) -> SlashCommandRemoveResult {
    SlashCommandRemoveResult {
        agent,
        scope: None,
        path: None,
        legacy_path: None,
        success: true,
        previous_status: status,
        status,
        already_absent: true,
        modified: false,
        current_removed: false,
        legacy_removed: false,
        metadata_removed: false,
        force_required: false,
        error: None,
        note: Some(note),
    }
}

fn remove_file_target(
    target: &CommandFileTarget,
    authority_root: &Path,
    force: bool,
) -> SlashCommandRemoveResult {
    let preflight = match FilePreflight::read(target, authority_root) {
        Ok(preflight) => preflight,
        Err(error) => {
            return remove_failure(
                target,
                authority_root,
                SlashCommandInstallStatus::Modified,
                SlashCommandInstallStatus::Modified,
                RemovalProgress::default(),
                false,
                format!("{error:#}"),
            )
        }
    };
    if preflight.status == SlashCommandInstallStatus::Missing {
        return remove_success(
            target,
            authority_root,
            preflight.status,
            true,
            RemovalProgress::default(),
        );
    }
    if !force && preflight.has_unowned_file() {
        return remove_failure(
            target,
            authority_root,
            preflight.status,
            preflight.status,
            RemovalProgress::default(),
            true,
            "local or unowned slash-command files were preserved; rerun with --force to remove the exact observed files"
                .to_owned(),
        );
    }

    let remove_metadata = preflight.metadata_exclusively_owns_removed_entry(target);
    let mut progress = RemovalProgress::default();
    if let Err(error) = remove_snapshot(
        &preflight.current,
        preflight.directory.as_ref(),
        &mut progress.current,
    ) {
        return mutation_failure(
            target,
            authority_root,
            preflight.status,
            progress,
            false,
            format!("{error:#}"),
        );
    }
    if let Err(error) = remove_snapshot(
        &preflight.legacy,
        preflight.directory.as_ref(),
        &mut progress.legacy,
    ) {
        return mutation_failure(
            target,
            authority_root,
            preflight.status,
            progress,
            false,
            format!("{error:#}"),
        );
    }

    if remove_metadata {
        if let Err(error) = remove_snapshot(
            &preflight.metadata,
            preflight.directory.as_ref(),
            &mut progress.metadata,
        ) {
            return mutation_failure(
                target,
                authority_root,
                preflight.status,
                progress,
                false,
                format!("{error:#}"),
            );
        }
    }
    match FilePreflight::read(target, authority_root) {
        Ok(final_state) if final_state.status == SlashCommandInstallStatus::Missing => {
            remove_success(target, authority_root, preflight.status, false, progress)
        }
        Ok(final_state) => remove_failure(
            target,
            authority_root,
            preflight.status,
            final_state.status,
            progress,
            false,
            "slash-command target changed during removal".to_owned(),
        ),
        Err(error) => remove_failure(
            target,
            authority_root,
            preflight.status,
            SlashCommandInstallStatus::Modified,
            progress,
            false,
            format!("reinspect slash-command target after removal: {error:#}"),
        ),
    }
}

fn remove_snapshot(
    snapshot: &FileSnapshot,
    directory: Option<&DirectoryFence>,
    removed: &mut bool,
) -> Result<()> {
    let Some(body) = snapshot.regular_body() else {
        return Ok(());
    };
    let directory = directory.ok_or_else(|| {
        anyhow!(
            "slash-command directory disappeared before removing {}",
            snapshot.path.display()
        )
    })?;
    *removed = directory
        .atomic_remove_if_unchanged(&snapshot.name, &snapshot.path, body)
        .with_context(|| format!("remove slash command {}", snapshot.path.display()))?;
    Ok(())
}

fn remove_success(
    target: &CommandFileTarget,
    authority_root: &Path,
    previous_status: SlashCommandInstallStatus,
    already_absent: bool,
    progress: RemovalProgress,
) -> SlashCommandRemoveResult {
    SlashCommandRemoveResult {
        agent: target.agent,
        scope: Some(target.scope),
        path: Some(target.command_path()),
        legacy_path: safe_existing_path(authority_root, target.legacy_command_path()),
        success: true,
        previous_status,
        status: SlashCommandInstallStatus::Missing,
        already_absent,
        modified: progress.modified(),
        current_removed: progress.current,
        legacy_removed: progress.legacy,
        metadata_removed: progress.metadata,
        force_required: false,
        error: None,
        note: None,
    }
}

fn remove_failure(
    target: &CommandFileTarget,
    authority_root: &Path,
    previous_status: SlashCommandInstallStatus,
    status: SlashCommandInstallStatus,
    progress: RemovalProgress,
    force_required: bool,
    error: String,
) -> SlashCommandRemoveResult {
    SlashCommandRemoveResult {
        agent: target.agent,
        scope: Some(target.scope),
        path: Some(target.command_path()),
        legacy_path: safe_existing_path(authority_root, target.legacy_command_path()),
        success: false,
        previous_status,
        status,
        already_absent: false,
        modified: progress.modified(),
        current_removed: progress.current,
        legacy_removed: progress.legacy,
        metadata_removed: progress.metadata,
        force_required,
        error: Some(error),
        note: None,
    }
}

fn mutation_failure(
    target: &CommandFileTarget,
    authority_root: &Path,
    previous_status: SlashCommandInstallStatus,
    progress: RemovalProgress,
    force_required: bool,
    error: String,
) -> SlashCommandRemoveResult {
    let status = FilePreflight::read(target, authority_root)
        .map_or(SlashCommandInstallStatus::Modified, |state| state.status);
    remove_failure(
        target,
        authority_root,
        previous_status,
        status,
        progress,
        force_required,
        error,
    )
}

#[derive(Clone, Copy, Default)]
struct RemovalProgress {
    current: bool,
    legacy: bool,
    metadata: bool,
}

impl RemovalProgress {
    const fn modified(self) -> bool {
        self.current || self.legacy || self.metadata
    }
}

struct FilePreflight {
    directory: Option<DirectoryFence>,
    current: FileSnapshot,
    legacy: FileSnapshot,
    metadata: FileSnapshot,
    current_status: SlashCommandInstallStatus,
    legacy_status: SlashCommandInstallStatus,
    status: SlashCommandInstallStatus,
}

impl FilePreflight {
    fn read(target: &CommandFileTarget, authority_root: &Path) -> Result<Self> {
        let current_path = target.command_path();
        let legacy_path = target.legacy_command_path();
        let metadata_path = target.base_dir.join(METADATA_FILE);
        ensure_path_inside(&target.base_dir, &current_path)?;
        ensure_path_inside(&target.base_dir, &legacy_path)?;
        ensure_path_inside(&target.base_dir, &metadata_path)?;
        validate_parent_chain(authority_root, &current_path)?;
        validate_parent_chain(authority_root, &legacy_path)?;
        validate_parent_chain(authority_root, &metadata_path)?;

        let directory = DirectoryFence::open_existing(authority_root, &target.base_dir)?;
        let metadata =
            FileSnapshot::read(directory.as_ref(), METADATA_FILE.to_owned(), metadata_path)?;
        let parsed_metadata = metadata
            .regular_body()
            .and_then(|body| serde_json::from_slice::<SlashCommandMetadata>(body).ok());
        let current =
            FileSnapshot::read(directory.as_ref(), target.filename.clone(), current_path)?;
        let legacy = FileSnapshot::read(directory.as_ref(), target.legacy_filename(), legacy_path)?;
        let current_status = classify_current(target, &current, parsed_metadata.as_ref());
        let legacy_status = classify_legacy(target, &legacy, parsed_metadata.as_ref());
        let legacy_result = legacy.exists().then(|| super::LegacyStatusResult {
            path: legacy.path.clone(),
            status: legacy_status,
            body: legacy.regular_body().map(<[u8]>::to_vec),
        });
        let status = combined_file_status(current_status, legacy_result.as_ref());
        if let Some(directory) = &directory {
            directory.revalidate()?;
        }
        Ok(Self {
            directory,
            current,
            legacy,
            metadata,
            current_status,
            legacy_status,
            status,
        })
    }

    fn has_unowned_file(&self) -> bool {
        (self.current.exists() && self.current_status == SlashCommandInstallStatus::Modified)
            || (self.legacy.exists() && self.legacy_status == SlashCommandInstallStatus::Modified)
    }

    fn metadata_exclusively_owns_removed_entry(&self, target: &CommandFileTarget) -> bool {
        let Some(body) = self.metadata.regular_body() else {
            return false;
        };
        let Ok(metadata) = serde_json::from_slice::<SlashCommandMetadata>(body) else {
            return false;
        };
        if metadata.schema_version != 1
            || metadata.installer != "ctx-cli"
            || metadata.files.len() != 1
        {
            return false;
        }
        if metadata.command_name == COMMAND_NAME {
            return self.current.regular_body().is_some()
                && metadata
                    .files
                    .get(&target.filename)
                    .is_some_and(|hash| self.current.hash().as_deref() == Some(hash.as_str()));
        }
        if metadata.command_name == LEGACY_COMMAND_NAME {
            let filename = target.legacy_filename();
            return self.legacy.regular_body().is_some()
                && metadata
                    .files
                    .get(&filename)
                    .is_some_and(|hash| self.legacy.hash().as_deref() == Some(hash.as_str()));
        }
        false
    }
}

fn classify_current(
    target: &CommandFileTarget,
    snapshot: &FileSnapshot,
    metadata: Option<&SlashCommandMetadata>,
) -> SlashCommandInstallStatus {
    match &snapshot.kind {
        FileSnapshotKind::Missing => SlashCommandInstallStatus::Missing,
        FileSnapshotKind::Regular(body) => {
            let hash = sha256_hex(body);
            if !metadata_manages_hash(target, metadata, &hash) {
                SlashCommandInstallStatus::Modified
            } else if hash == target.bundled_hash() {
                SlashCommandInstallStatus::Current
            } else {
                SlashCommandInstallStatus::Stale
            }
        }
    }
}

fn classify_legacy(
    target: &CommandFileTarget,
    snapshot: &FileSnapshot,
    metadata: Option<&SlashCommandMetadata>,
) -> SlashCommandInstallStatus {
    match &snapshot.kind {
        FileSnapshotKind::Missing => SlashCommandInstallStatus::Missing,
        FileSnapshotKind::Regular(body) => {
            let hash = sha256_hex(body);
            if legacy_metadata_manages_hash(target, metadata, &hash) {
                SlashCommandInstallStatus::Stale
            } else {
                SlashCommandInstallStatus::Modified
            }
        }
    }
}

#[derive(Debug)]
struct FileSnapshot {
    name: String,
    path: PathBuf,
    kind: FileSnapshotKind,
}

impl FileSnapshot {
    fn read(directory: Option<&DirectoryFence>, name: String, path: PathBuf) -> Result<Self> {
        let kind = match directory {
            Some(directory) => directory
                .read_optional_regular_file(&name, &path)?
                .map_or(FileSnapshotKind::Missing, FileSnapshotKind::Regular),
            None => FileSnapshotKind::Missing,
        };
        Ok(Self { name, path, kind })
    }

    fn exists(&self) -> bool {
        !matches!(self.kind, FileSnapshotKind::Missing)
    }

    fn regular_body(&self) -> Option<&[u8]> {
        match &self.kind {
            FileSnapshotKind::Regular(body) => Some(body),
            FileSnapshotKind::Missing => None,
        }
    }

    fn hash(&self) -> Option<String> {
        self.regular_body().map(sha256_hex)
    }
}

#[derive(Debug)]
enum FileSnapshotKind {
    Missing,
    Regular(Vec<u8>),
}

fn authority_root(agent: SlashCommandAgent, project: bool, context: &PathContext) -> PathBuf {
    if project {
        return context.cwd.clone();
    }
    match agent {
        SlashCommandAgent::OpenCode => context.xdg_config_home.clone(),
        SlashCommandAgent::MiMoCode => context.mimocode_config_dir(),
        SlashCommandAgent::GeminiCli | SlashCommandAgent::QwenCode => context.home.clone(),
        SlashCommandAgent::Codex
        | SlashCommandAgent::GrokBuild
        | SlashCommandAgent::ClaudeCode
        | SlashCommandAgent::Cursor
        | SlashCommandAgent::Antigravity
        | SlashCommandAgent::GitHubCopilot
        | SlashCommandAgent::Pi
        | SlashCommandAgent::Goose
        | SlashCommandAgent::Continue => context.home.clone(),
    }
}

fn validate_parent_chain(authority_root: &Path, path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("slash-command path has no parent: {}", path.display()))?;
    ensure_path_inside(authority_root, parent)?;
    let relative = parent
        .strip_prefix(authority_root)
        .map_err(|_| anyhow!("slash-command path escapes authority root"))?;
    let mut current = authority_root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata)
                if metadata.file_type().is_symlink() || is_windows_reparse_point(&metadata) =>
            {
                return Err(anyhow!(
                    "slash-command path traverses a symlink or reparse point: {}",
                    current.display()
                ));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(anyhow!(
                    "slash-command path component is not a directory: {}",
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

fn safe_existing_path(authority_root: &Path, path: PathBuf) -> Option<PathBuf> {
    validate_parent_chain(authority_root, &path)
        .ok()
        .and_then(|()| fs::symlink_metadata(&path).ok())
        .map(|_| path)
}

#[cfg(test)]
mod tests;
