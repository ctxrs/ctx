use std::{
    collections::BTreeSet,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use ctx_history_provider_runtime::source_io::{
    open_provider_source_path, OpenedProviderSourceFile, OpenedProviderSourcePath,
    ProviderSourceDirectory, ProviderSourceRoot, PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES,
    PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS, PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES,
    PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
};
use ctx_history_provider_runtime::CaptureError;

#[path = "layout_helpers.rs"]
mod helpers;
use helpers::*;

pub(super) const AGENT_TRANSCRIPTS: &str = "agent-transcripts";
pub(super) const PROJECTS: &str = "projects";
pub(super) const CURSOR_MAX_DISCOVERY_ISSUE_SAMPLES: usize = 128;

fn is_literal_projects_root(path: &Path) -> bool {
    path.file_name() == Some(OsStr::new(PROJECTS))
}

fn require_literal_projects_root(
    candidate: &Path,
    projects_root: &Path,
    inventory: &mut CursorRootInventory,
) -> bool {
    if is_literal_projects_root(projects_root) {
        return true;
    }
    inventory.reject(
        candidate.to_path_buf(),
        CursorDiscoveryIssueKind::InvalidLayout,
        "Cursor transcript entry points must be beneath a literal projects directory",
        true,
    );
    false
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CursorInventoryLimits {
    pub(super) max_directories: usize,
    pub(super) max_transcripts: usize,
    pub(super) max_metadata_entries: usize,
    pub(super) max_path_bytes: usize,
}

impl Default for CursorInventoryLimits {
    fn default() -> Self {
        // Cursor's fixed shape needs no recursive depth budget. Reuse the
        // ordinary source-I/O inventory ceilings for the remaining dimensions
        // instead of maintaining smaller provider-local corpus ceilings.
        Self {
            max_directories: PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES,
            max_transcripts: PROVIDER_JSONL_INVENTORY_MAX_ELIGIBLE_PATHS,
            max_metadata_entries: PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES,
            max_path_bytes: PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorScanGoal {
    CompleteInventory,
    FirstTranscript,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorTranscriptAvailability {
    Found,
    NotFound,
    BudgetExhausted,
    IoError,
}

#[derive(Debug, Clone)]
pub(crate) struct CursorTranscriptPath {
    projects_root: PathBuf,
    project: PathBuf,
    session_id: String,
    path: PathBuf,
    authority_relative_path: PathBuf,
    ordinary_file_token: [u8; 32],
    authority: ProviderSourceRoot,
}

impl CursorTranscriptPath {
    fn parse(
        projects_root: &Path,
        path: &Path,
        authority_relative_path: PathBuf,
        ordinary_file_token: [u8; 32],
        authority: ProviderSourceRoot,
    ) -> Result<Self, &'static str> {
        if !is_literal_projects_root(projects_root) {
            return Err("Cursor transcript must be beneath a literal projects directory");
        }
        let relative = path
            .strip_prefix(projects_root)
            .map_err(|_| "Cursor transcript is outside the selected projects root")?;
        let components = relative
            .components()
            .map(|component| component.as_os_str())
            .collect::<Vec<_>>();
        if components.len() != 4 || components[1] != OsStr::new(AGENT_TRANSCRIPTS) {
            return Err(
                "Cursor transcript must match <project>/agent-transcripts/<session>/<session>.jsonl",
            );
        }
        if components[0].is_empty() {
            return Err("Cursor project directory must not be empty");
        }
        let session_id = components[2]
            .to_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or("Cursor session directory must be nonempty UTF-8")?;
        let file_name = components[3]
            .to_str()
            .ok_or("Cursor transcript file name must be UTF-8")?;
        let expected_file_name = format!("{session_id}.jsonl");
        if file_name != expected_file_name {
            return Err("Cursor transcript file and directory session IDs must match");
        }
        Ok(Self {
            projects_root: projects_root.to_path_buf(),
            project: PathBuf::from(components[0]),
            session_id: session_id.to_owned(),
            path: path.to_path_buf(),
            authority_relative_path,
            ordinary_file_token,
            authority,
        })
    }

    pub(crate) fn native_session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn authority_relative_path(&self) -> &Path {
        &self.authority_relative_path
    }

    pub(crate) fn authority(&self) -> &ProviderSourceRoot {
        &self.authority
    }
}

impl PartialEq for CursorTranscriptPath {
    fn eq(&self, other: &Self) -> bool {
        self.projects_root == other.projects_root
            && self.project == other.project
            && self.session_id == other.session_id
            && self.path == other.path
            && self.authority_relative_path == other.authority_relative_path
            && self.ordinary_file_token == other.ordinary_file_token
    }
}

impl Eq for CursorTranscriptPath {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CursorDiscoveryIssueKind {
    Io,
    InvalidLayout,
    NotFound,
    Symlink,
    SpecialFile,
    LimitExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorDiscoveryIssue {
    pub(crate) path: PathBuf,
    pub(crate) kind: CursorDiscoveryIssueKind,
    pub(crate) reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CursorDiscoveryStats {
    pub(crate) directories_visited: usize,
    pub(crate) entries_visited: usize,
    pub(crate) regular_files_visited: usize,
    pub(crate) selected_transcripts: usize,
    pub(crate) rejected_candidates: usize,
}

#[derive(Debug, Clone)]
pub struct CursorRootInventory {
    pub(crate) input: PathBuf,
    pub(crate) projects_roots: Vec<PathBuf>,
    pub(crate) transcripts: Vec<CursorTranscriptPath>,
    pub(crate) issues: Vec<CursorDiscoveryIssue>,
    pub(crate) completed: bool,
    pub(crate) stats: CursorDiscoveryStats,
    authority: Option<ProviderSourceRoot>,
    issue_kinds: BTreeSet<CursorDiscoveryIssueKind>,
}

impl CursorRootInventory {
    pub fn completed(&self) -> bool {
        self.completed
    }

    pub fn has_issue_kind(&self, expected: CursorDiscoveryIssueKind) -> bool {
        self.issue_kinds.contains(&expected)
    }

    pub fn has_transcripts(&self) -> bool {
        !self.transcripts.is_empty()
    }

    fn new(input: &Path) -> Self {
        Self {
            input: input.to_path_buf(),
            projects_roots: Vec::new(),
            transcripts: Vec::new(),
            issues: Vec::new(),
            completed: true,
            stats: CursorDiscoveryStats::default(),
            authority: None,
            issue_kinds: BTreeSet::new(),
        }
    }

    fn reject(
        &mut self,
        path: PathBuf,
        kind: CursorDiscoveryIssueKind,
        reason: impl Into<String>,
        invalidates_completion: bool,
    ) {
        self.stats.rejected_candidates = self.stats.rejected_candidates.saturating_add(1);
        self.issue_kinds.insert(kind);
        if self.issues.len() < CURSOR_MAX_DISCOVERY_ISSUE_SAMPLES {
            self.issues.push(CursorDiscoveryIssue {
                path,
                kind,
                reason: reason.into(),
            });
        }
        if invalidates_completion {
            self.completed = false;
        }
    }

    fn finish(&mut self) {
        self.transcripts
            .sort_by(|left, right| left.path.cmp(&right.path));
        self.transcripts
            .dedup_by(|left, right| left.path == right.path);
        self.stats.selected_transcripts = self.transcripts.len();
        let roots = self
            .transcripts
            .iter()
            .map(|source| source.projects_root.clone())
            .collect::<BTreeSet<_>>();
        self.projects_roots = roots.into_iter().collect();
    }

    pub(crate) fn revalidate(&self) -> ctx_history_provider_runtime::Result<()> {
        match self.authority.as_ref() {
            Some(root) => root.revalidate(),
            None => Err(CaptureError::InvalidProviderTranscriptPath {
                path: self.input.clone(),
                reason: "Cursor discovery has no retained source authority",
            }),
        }
    }

    pub(crate) fn authority(&self) -> Option<&ProviderSourceRoot> {
        self.authority.as_ref()
    }
}

impl PartialEq for CursorRootInventory {
    fn eq(&self, other: &Self) -> bool {
        self.input == other.input
            && self.projects_roots == other.projects_roots
            && self.transcripts == other.transcripts
            && self.issues == other.issues
            && self.completed == other.completed
            && self.stats == other.stats
            && self.issue_kinds == other.issue_kinds
    }
}

impl Eq for CursorRootInventory {}

pub fn probe_cursor_transcript_availability(input: &Path) -> CursorTranscriptAvailability {
    probe_cursor_transcript_availability_with_limits(input, CursorInventoryLimits::default())
}

pub(super) fn probe_cursor_transcript_availability_with_limits(
    input: &Path,
    limits: CursorInventoryLimits,
) -> CursorTranscriptAvailability {
    let inventory = scan_cursor_transcripts(input, limits, CursorScanGoal::FirstTranscript);
    if inventory.has_transcripts() {
        CursorTranscriptAvailability::Found
    } else if inventory.has_issue_kind(CursorDiscoveryIssueKind::LimitExceeded) {
        CursorTranscriptAvailability::BudgetExhausted
    } else if inventory.has_issue_kind(CursorDiscoveryIssueKind::NotFound) {
        CursorTranscriptAvailability::NotFound
    } else if !inventory.completed() {
        CursorTranscriptAvailability::IoError
    } else {
        CursorTranscriptAvailability::NotFound
    }
}

pub fn discover_cursor_transcripts(input: &Path) -> CursorRootInventory {
    discover_cursor_transcripts_with_limits(input, CursorInventoryLimits::default())
}

pub(super) fn discover_cursor_transcripts_with_limits(
    input: &Path,
    limits: CursorInventoryLimits,
) -> CursorRootInventory {
    scan_cursor_transcripts(input, limits, CursorScanGoal::CompleteInventory)
}

fn scan_cursor_transcripts(
    input: &Path,
    limits: CursorInventoryLimits,
    goal: CursorScanGoal,
) -> CursorRootInventory {
    let mut inventory = CursorRootInventory::new(input);
    if !admit_metadata_entry(input, limits, &mut inventory) {
        return inventory;
    }
    let opened = match open_provider_source_path(input) {
        Ok(opened) => opened,
        Err(error) => {
            let kind = match &error {
                error if capture_error_is_not_found(error) => CursorDiscoveryIssueKind::NotFound,
                CaptureError::InvalidProviderTranscriptPath { .. } => {
                    CursorDiscoveryIssueKind::Symlink
                }
                _ => CursorDiscoveryIssueKind::Io,
            };
            inventory.reject(input.to_path_buf(), kind, error.to_string(), true);
            return inventory;
        }
    };
    match opened {
        OpenedProviderSourcePath::File(file) => {
            inventory.stats.regular_files_visited = 1;
            let (authority, relative_path) = match cursor_explicit_authority(input) {
                Ok(admitted) => admitted,
                Err(error) => {
                    inventory.reject(
                        input.to_path_buf(),
                        CursorDiscoveryIssueKind::Io,
                        error.to_string(),
                        true,
                    );
                    return inventory;
                }
            };
            inventory.authority = Some(authority.clone());
            inspect_file(
                input,
                relative_path,
                file,
                authority,
                &mut inventory,
                true,
                limits,
            );
        }
        OpenedProviderSourcePath::Directory(directory) => {
            if !admit_directory(input, limits, &mut inventory) {
                return inventory;
            }
            inventory.authority = Some(directory.authority_root());
            discover_from_directory(input, directory, limits, goal, &mut inventory);
        }
    }
    inventory.finish();
    if inventory.completed
        || (goal == CursorScanGoal::FirstTranscript && inventory.has_transcripts())
    {
        if let Err(error) = inventory.revalidate() {
            inventory.reject(
                inventory.input.clone(),
                CursorDiscoveryIssueKind::Io,
                error.to_string(),
                true,
            );
            if goal == CursorScanGoal::FirstTranscript {
                inventory.transcripts.clear();
                inventory.finish();
            }
        }
    }
    inventory
}

fn cursor_explicit_authority(
    input: &Path,
) -> ctx_history_provider_runtime::Result<(ProviderSourceRoot, PathBuf)> {
    let parent = input
        .parent()
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: input.to_path_buf(),
            reason: "explicit Cursor transcript has no parent authority",
        })?;
    let relative_path = input.file_name().map(PathBuf::from).ok_or_else(|| {
        CaptureError::InvalidProviderTranscriptPath {
            path: input.to_path_buf(),
            reason: "explicit Cursor transcript has no authority-relative name",
        }
    })?;
    Ok((ProviderSourceRoot::open(parent)?, relative_path))
}

#[derive(Debug)]
enum CursorDirectoryEntryPoint {
    Session { projects_root: PathBuf },
    AgentTranscripts { projects_root: PathBuf },
    Project { projects_root: PathBuf },
    Projects,
    ProviderRoot,
}

fn cursor_directory_entry_points(input: &Path) -> Vec<CursorDirectoryEntryPoint> {
    let mut entry_points = Vec::with_capacity(5);
    if let Some(agent_transcripts) = input
        .parent()
        .filter(|parent| parent.file_name() == Some(OsStr::new(AGENT_TRANSCRIPTS)))
    {
        if let Some(projects_root) = agent_transcripts
            .parent()
            .and_then(Path::parent)
            .filter(|root| is_literal_projects_root(root))
        {
            entry_points.push(CursorDirectoryEntryPoint::Session {
                projects_root: projects_root.to_path_buf(),
            });
        }
    }
    if input.file_name() == Some(OsStr::new(AGENT_TRANSCRIPTS)) {
        if let Some(projects_root) = input
            .parent()
            .and_then(Path::parent)
            .filter(|root| is_literal_projects_root(root))
        {
            entry_points.push(CursorDirectoryEntryPoint::AgentTranscripts {
                projects_root: projects_root.to_path_buf(),
            });
        }
    }
    if let Some(projects_root) = input.parent().filter(|root| is_literal_projects_root(root)) {
        entry_points.push(CursorDirectoryEntryPoint::Project {
            projects_root: projects_root.to_path_buf(),
        });
    }
    if is_literal_projects_root(input) {
        entry_points.push(CursorDirectoryEntryPoint::Projects);
    }
    entry_points.push(CursorDirectoryEntryPoint::ProviderRoot);
    entry_points
}

fn discover_from_directory(
    input: &Path,
    directory: ProviderSourceDirectory,
    limits: CursorInventoryLimits,
    goal: CursorScanGoal,
    inventory: &mut CursorRootInventory,
) {
    let fingerprint = directory.authority_fingerprint();
    let baseline = inventory.clone();
    let mut first_directory = Some(directory);
    let mut fallback = None;
    for entry_point in cursor_directory_entry_points(input) {
        let Some(directory) = first_directory
            .take()
            .or_else(|| reopen_cursor_directory(input, fingerprint, limits, inventory))
        else {
            return;
        };
        let authority = directory.authority_root();
        inventory.authority = Some(authority.clone());
        scan_directory_entry_point(
            input,
            directory,
            authority,
            entry_point,
            limits,
            goal,
            inventory,
        );
        if inventory.has_transcripts() {
            return;
        }
        fallback.get_or_insert_with(|| inventory.clone());
        if inventory.has_issue_kind(CursorDiscoveryIssueKind::LimitExceeded) {
            return;
        }
        let work = inventory.stats.clone();
        *inventory = baseline.clone();
        inventory.stats.directories_visited = work.directories_visited;
        inventory.stats.entries_visited = work.entries_visited;
        inventory.stats.regular_files_visited = work.regular_files_visited;
    }
    if let Some(mut selected) = fallback {
        selected.stats.directories_visited = inventory.stats.directories_visited;
        selected.stats.entries_visited = inventory.stats.entries_visited;
        selected.stats.regular_files_visited = inventory.stats.regular_files_visited;
        *inventory = selected;
    }
}

fn reopen_cursor_directory(
    input: &Path,
    expected_fingerprint: [u8; 32],
    limits: CursorInventoryLimits,
    inventory: &mut CursorRootInventory,
) -> Option<ProviderSourceDirectory> {
    if !admit_metadata_entry(input, limits, inventory) {
        return None;
    }
    let directory = match open_provider_source_path(input) {
        Ok(OpenedProviderSourcePath::Directory(directory)) => directory,
        Ok(OpenedProviderSourcePath::File(_)) => {
            inventory.reject(
                input.to_path_buf(),
                CursorDiscoveryIssueKind::Io,
                "Cursor directory entry point changed into a file during discovery",
                true,
            );
            return None;
        }
        Err(error) => {
            inventory.reject(
                input.to_path_buf(),
                CursorDiscoveryIssueKind::Io,
                error.to_string(),
                true,
            );
            return None;
        }
    };
    if directory.authority_fingerprint() != expected_fingerprint {
        inventory.reject(
            input.to_path_buf(),
            CursorDiscoveryIssueKind::Io,
            "Cursor directory entry point changed while resolving its fixed-shape layout",
            true,
        );
        return None;
    }
    admit_directory(input, limits, inventory).then_some(directory)
}

#[allow(clippy::too_many_arguments)]
fn scan_directory_entry_point(
    input: &Path,
    directory: ProviderSourceDirectory,
    authority: ProviderSourceRoot,
    entry_point: CursorDirectoryEntryPoint,
    limits: CursorInventoryLimits,
    goal: CursorScanGoal,
    inventory: &mut CursorRootInventory,
) {
    match entry_point {
        CursorDirectoryEntryPoint::Session { projects_root } => {
            let Some(session_id) = input
                .file_name()
                .and_then(OsStr::to_str)
                .filter(|value| !value.trim().is_empty())
            else {
                inventory.reject(
                    input.to_path_buf(),
                    CursorDiscoveryIssueKind::InvalidLayout,
                    "Cursor session directory must be nonempty UTF-8",
                    false,
                );
                revalidate_directory(input, &directory, goal, inventory);
                return;
            };
            scan_session(
                &projects_root,
                input,
                session_id,
                directory,
                authority,
                limits,
                goal,
                inventory,
            );
        }
        CursorDirectoryEntryPoint::AgentTranscripts { projects_root } => {
            scan_agent_transcripts(
                &projects_root,
                input,
                directory,
                authority,
                limits,
                goal,
                inventory,
            );
        }
        CursorDirectoryEntryPoint::Project { projects_root } => scan_project(
            &projects_root,
            input,
            directory,
            authority,
            limits,
            goal,
            inventory,
        ),
        CursorDirectoryEntryPoint::Projects => {
            scan_projects(input, directory, authority, limits, goal, inventory);
        }
        CursorDirectoryEntryPoint::ProviderRoot => match open_direct_child(
            input,
            &directory,
            OsStr::new(PROJECTS),
            true,
            true,
            limits,
            inventory,
        ) {
            DirectChild::Directory(projects) => {
                let projects_path = input.join(PROJECTS);
                inventory.input = projects_path.clone();
                scan_projects(&projects_path, projects, authority, limits, goal, inventory);
                revalidate_directory(input, &directory, goal, inventory);
            }
            DirectChild::Failed => revalidate_directory(input, &directory, goal, inventory),
            DirectChild::File(_) => {
                inventory.reject(
                    input.join(PROJECTS),
                    CursorDiscoveryIssueKind::InvalidLayout,
                    "Cursor projects component must be a directory",
                    true,
                );
                revalidate_directory(input, &directory, goal, inventory);
            }
            DirectChild::Missing if input.file_name() == Some(OsStr::new(".cursor")) => {
                revalidate_directory(input, &directory, goal, inventory);
            }
            DirectChild::Missing => {
                inventory.reject(
                    input.to_path_buf(),
                    CursorDiscoveryIssueKind::InvalidLayout,
                    "Cursor directory entry point must be a provider root containing projects or be beneath a literal projects directory",
                    true,
                );
                revalidate_directory(input, &directory, goal, inventory);
            }
        },
    }
}

fn scan_projects(
    projects_root: &Path,
    projects: ProviderSourceDirectory,
    authority: ProviderSourceRoot,
    limits: CursorInventoryLimits,
    goal: CursorScanGoal,
    inventory: &mut CursorRootInventory,
) {
    if !require_literal_projects_root(projects_root, projects_root, inventory) {
        revalidate_directory(projects_root, &projects, goal, inventory);
        return;
    }
    let entries = read_directory_entries(projects_root, &projects, limits, inventory);
    for name in entries {
        if scan_should_stop(goal, inventory) {
            break;
        }
        let project_path = projects_root.join(&name);
        match open_enumerated_child(
            &project_path,
            &projects,
            &name,
            true,
            false,
            limits,
            inventory,
        ) {
            Some(OpenedProviderSourcePath::Directory(project)) => scan_project(
                projects_root,
                &project_path,
                project,
                authority.clone(),
                limits,
                goal,
                inventory,
            ),
            Some(OpenedProviderSourcePath::File(_)) | None => {}
        }
    }
    revalidate_directory(projects_root, &projects, goal, inventory);
}

fn scan_project(
    projects_root: &Path,
    project_path: &Path,
    project: ProviderSourceDirectory,
    authority: ProviderSourceRoot,
    limits: CursorInventoryLimits,
    goal: CursorScanGoal,
    inventory: &mut CursorRootInventory,
) {
    if !require_literal_projects_root(project_path, projects_root, inventory) {
        revalidate_directory(project_path, &project, goal, inventory);
        return;
    }
    match open_direct_child(
        project_path,
        &project,
        OsStr::new(AGENT_TRANSCRIPTS),
        true,
        true,
        limits,
        inventory,
    ) {
        DirectChild::Directory(agent_transcripts) => scan_agent_transcripts(
            projects_root,
            &project_path.join(AGENT_TRANSCRIPTS),
            agent_transcripts,
            authority,
            limits,
            goal,
            inventory,
        ),
        DirectChild::File(_) => inventory.reject(
            project_path.join(AGENT_TRANSCRIPTS),
            CursorDiscoveryIssueKind::InvalidLayout,
            "Cursor agent-transcripts component must be a directory",
            true,
        ),
        DirectChild::Missing | DirectChild::Failed => {}
    }
    revalidate_directory(project_path, &project, goal, inventory);
}

fn scan_agent_transcripts(
    projects_root: &Path,
    agent_transcripts_path: &Path,
    agent_transcripts: ProviderSourceDirectory,
    authority: ProviderSourceRoot,
    limits: CursorInventoryLimits,
    goal: CursorScanGoal,
    inventory: &mut CursorRootInventory,
) {
    if !require_literal_projects_root(agent_transcripts_path, projects_root, inventory) {
        revalidate_directory(agent_transcripts_path, &agent_transcripts, goal, inventory);
        return;
    }
    let entries = read_directory_entries(
        agent_transcripts_path,
        &agent_transcripts,
        limits,
        inventory,
    );
    for name in entries {
        if scan_should_stop(goal, inventory) {
            break;
        }
        let session_path = agent_transcripts_path.join(&name);
        let Some(session_id) = name.to_str().filter(|value| !value.trim().is_empty()) else {
            inventory.reject(
                session_path,
                CursorDiscoveryIssueKind::InvalidLayout,
                "Cursor session directory must be nonempty UTF-8",
                false,
            );
            continue;
        };
        match open_enumerated_child(
            &session_path,
            &agent_transcripts,
            &name,
            true,
            false,
            limits,
            inventory,
        ) {
            Some(OpenedProviderSourcePath::Directory(session)) => scan_session(
                projects_root,
                &session_path,
                session_id,
                session,
                authority.clone(),
                limits,
                goal,
                inventory,
            ),
            Some(OpenedProviderSourcePath::File(_)) | None => {}
        }
    }
    revalidate_directory(agent_transcripts_path, &agent_transcripts, goal, inventory);
}

#[allow(clippy::too_many_arguments)]
fn scan_session(
    projects_root: &Path,
    session_path: &Path,
    session_id: &str,
    session: ProviderSourceDirectory,
    authority: ProviderSourceRoot,
    limits: CursorInventoryLimits,
    goal: CursorScanGoal,
    inventory: &mut CursorRootInventory,
) {
    if !require_literal_projects_root(session_path, projects_root, inventory) {
        revalidate_directory(session_path, &session, goal, inventory);
        return;
    }
    let transcript_name = format!("{session_id}.jsonl");
    let transcript_path = session_path.join(&transcript_name);
    match open_direct_child(
        session_path,
        &session,
        OsStr::new(&transcript_name),
        true,
        true,
        limits,
        inventory,
    ) {
        DirectChild::File(file) => inspect_file(
            &transcript_path,
            session.relative_path().join(&transcript_name),
            file,
            authority,
            inventory,
            false,
            limits,
        ),
        DirectChild::Directory(_) => inventory.reject(
            transcript_path,
            CursorDiscoveryIssueKind::InvalidLayout,
            "Cursor transcript leaf must be a regular file",
            true,
        ),
        DirectChild::Missing | DirectChild::Failed => {}
    }
    revalidate_directory(session_path, &session, goal, inventory);
}

fn inspect_file(
    path: &Path,
    authority_relative_path: PathBuf,
    source_file: OpenedProviderSourceFile,
    authority: ProviderSourceRoot,
    inventory: &mut CursorRootInventory,
    explicit_file: bool,
    limits: CursorInventoryLimits,
) {
    if path.extension().and_then(OsStr::to_str) != Some("jsonl") {
        if explicit_file {
            inventory.reject(
                path.to_path_buf(),
                CursorDiscoveryIssueKind::InvalidLayout,
                "explicit Cursor transcript files must use the .jsonl extension",
                false,
            );
        }
        return;
    }
    let Some(session_directory) = path.parent() else {
        return;
    };
    let Some(agent_transcripts) = session_directory.parent() else {
        return;
    };
    if agent_transcripts.file_name() != Some(OsStr::new(AGENT_TRANSCRIPTS)) {
        if explicit_file {
            inventory.reject(
                path.to_path_buf(),
                CursorDiscoveryIssueKind::InvalidLayout,
                "explicit Cursor transcript does not have an agent-transcripts parent",
                false,
            );
        }
        return;
    }
    let Some(project) = agent_transcripts.parent() else {
        return;
    };
    let Some(projects_root) = project.parent() else {
        return;
    };
    if !require_literal_projects_root(path, projects_root, inventory) {
        return;
    }
    let selected_root = if explicit_file { path } else { projects_root };
    if ![
        projects_root,
        project,
        agent_transcripts,
        session_directory,
        path,
    ]
    .contains(&selected_root)
    {
        inventory.reject(
            path.to_path_buf(),
            CursorDiscoveryIssueKind::InvalidLayout,
            "Cursor transcript is a loose nested lookalike below the selected root",
            false,
        );
        return;
    }
    let ordinary_file_token = match cursor_catalog_token(
        &source_file,
        &authority,
        &authority_relative_path,
        explicit_file,
    ) {
        Ok(token) => token,
        Err(error) => {
            inventory.reject(
                path.to_path_buf(),
                CursorDiscoveryIssueKind::Io,
                error.to_string(),
                true,
            );
            return;
        }
    };
    match CursorTranscriptPath::parse(
        projects_root,
        path,
        authority_relative_path,
        ordinary_file_token,
        authority,
    ) {
        Ok(source) if inventory.transcripts.len() < limits.max_transcripts => {
            inventory.transcripts.push(source)
        }
        Ok(_) => inventory.reject(
            path.to_path_buf(),
            CursorDiscoveryIssueKind::LimitExceeded,
            format!(
                "Cursor discovery exceeds the {}-transcript inventory limit",
                limits.max_transcripts
            ),
            true,
        ),
        Err(reason) => inventory.reject(
            path.to_path_buf(),
            CursorDiscoveryIssueKind::InvalidLayout,
            reason,
            false,
        ),
    }
}

fn cursor_catalog_token(
    source_file: &OpenedProviderSourceFile,
    authority: &ProviderSourceRoot,
    authority_relative_path: &Path,
    explicit_file: bool,
) -> ctx_history_provider_runtime::Result<[u8; 32]> {
    let token = source_file.ordinary_file_token();
    source_file.revalidate_leaf()?;
    if explicit_file {
        let reopened = authority.open_file(authority_relative_path)?;
        if reopened.ordinary_file_token() != token {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        reopened.revalidate_leaf()?;
    }
    Ok(token)
}
