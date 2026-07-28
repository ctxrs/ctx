use std::{
    collections::{BTreeSet, VecDeque},
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::common::io::{
    open_provider_source_path, OpenedProviderSourceFile, OpenedProviderSourcePath,
    ProviderSourceDirectory, ProviderSourceRoot,
};
use crate::CaptureError;

const AGENT_TRANSCRIPTS: &str = "agent-transcripts";
pub(super) const CURSOR_MAX_DIRECTORY_DEPTH: usize = 128;
pub(super) const CURSOR_MAX_DIRECTORY_ENTRIES: usize = 1_024;
pub(super) const CURSOR_MAX_TRAVERSAL_ENTRIES: usize = 4_096;
pub(super) const CURSOR_MAX_DISCOVERY_ISSUE_SAMPLES: usize = 128;
pub(super) const CURSOR_MAX_TRANSCRIPTS: usize = 128;

#[derive(Debug, Clone)]
pub(crate) struct CursorTranscriptPath {
    projects_root: PathBuf,
    project: PathBuf,
    session_id: String,
    path: PathBuf,
    source_file: Arc<OpenedProviderSourceFile>,
}

impl CursorTranscriptPath {
    fn parse(
        projects_root: &Path,
        path: &Path,
        source_file: Arc<OpenedProviderSourceFile>,
    ) -> Result<Self, &'static str> {
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
            source_file,
        })
    }

    pub(crate) fn projects_root(&self) -> &Path {
        &self.projects_root
    }

    pub(crate) fn project(&self) -> &Path {
        &self.project
    }

    pub(crate) fn native_session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn source_file(&self) -> &Arc<OpenedProviderSourceFile> {
        &self.source_file
    }
}

impl PartialEq for CursorTranscriptPath {
    fn eq(&self, other: &Self) -> bool {
        self.projects_root == other.projects_root
            && self.project == other.project
            && self.session_id == other.session_id
            && self.path == other.path
    }
}

impl Eq for CursorTranscriptPath {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorDiscoveryIssueKind {
    Io,
    InvalidLayout,
    NotFound,
    Symlink,
    UnsupportedFileType,
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
pub(crate) struct CursorRootInventory {
    pub(crate) input: PathBuf,
    pub(crate) projects_roots: Vec<PathBuf>,
    pub(crate) transcripts: Vec<CursorTranscriptPath>,
    pub(crate) issues: Vec<CursorDiscoveryIssue>,
    pub(crate) completed: bool,
    pub(crate) stats: CursorDiscoveryStats,
    authority: Option<CursorInventoryAuthority>,
}

#[derive(Debug, Clone)]
enum CursorInventoryAuthority {
    File(Arc<OpenedProviderSourceFile>),
    Root(ProviderSourceRoot),
}

impl CursorRootInventory {
    fn new(input: &Path) -> Self {
        Self {
            input: input.to_path_buf(),
            projects_roots: Vec::new(),
            transcripts: Vec::new(),
            issues: Vec::new(),
            completed: true,
            stats: CursorDiscoveryStats::default(),
            authority: None,
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

    pub(crate) fn revalidate(&self) -> crate::Result<()> {
        match self.authority.as_ref() {
            Some(CursorInventoryAuthority::File(file)) => file.revalidate(),
            Some(CursorInventoryAuthority::Root(root)) => root.revalidate(),
            None => Err(CaptureError::InvalidProviderTranscriptPath {
                path: self.input.clone(),
                reason: "Cursor discovery has no retained source authority",
            }),
        }
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
    }
}

impl Eq for CursorRootInventory {}

pub(crate) fn discover_cursor_transcripts(input: &Path) -> CursorRootInventory {
    let mut inventory = CursorRootInventory::new(input);
    let opened = match open_provider_source_path(input) {
        Ok(opened) => opened,
        Err(error) => {
            let kind = match &error {
                CaptureError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    CursorDiscoveryIssueKind::NotFound
                }
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
            let file = Arc::new(file);
            inventory.authority = Some(CursorInventoryAuthority::File(file.clone()));
            inventory.stats.entries_visited = 1;
            inventory.stats.regular_files_visited = 1;
            inspect_file(input, input, file, &mut inventory, true);
        }
        OpenedProviderSourcePath::Directory(directory) => {
            let authority = directory.authority_root();
            inventory.authority = Some(CursorInventoryAuthority::Root(authority.clone()));
            let (scan_path, scan_directory) =
                select_projects_directory(input, directory, &mut inventory);
            inventory.input = scan_path.clone();
            visit_directories(&scan_path, scan_directory, authority, &mut inventory);
        }
    }
    inventory.finish();
    if inventory.completed {
        if let Err(error) = inventory.revalidate() {
            inventory.reject(
                inventory.input.clone(),
                CursorDiscoveryIssueKind::Io,
                error.to_string(),
                true,
            );
        }
    }
    inventory
}

fn select_projects_directory(
    input: &Path,
    directory: ProviderSourceDirectory,
    inventory: &mut CursorRootInventory,
) -> (PathBuf, ProviderSourceDirectory) {
    let names = match directory.entries(CURSOR_MAX_DIRECTORY_ENTRIES.saturating_add(1)) {
        Ok(names) => names,
        Err(error) => {
            inventory.reject(
                input.to_path_buf(),
                CursorDiscoveryIssueKind::Io,
                error.to_string(),
                true,
            );
            return (input.to_path_buf(), directory);
        }
    };
    if names.iter().any(|name| name == OsStr::new("projects")) {
        if let Ok(OpenedProviderSourcePath::Directory(projects)) =
            directory.open_child(OsStr::new("projects"))
        {
            return (input.join("projects"), projects);
        }
    }
    (input.to_path_buf(), directory)
}

fn visit_directories(
    input: &Path,
    first_directory: ProviderSourceDirectory,
    authority: ProviderSourceRoot,
    inventory: &mut CursorRootInventory,
) {
    let first_relative = first_directory.relative_path().to_path_buf();
    let mut pending = VecDeque::from([(input.to_path_buf(), first_relative, 0_usize)]);
    drop(first_directory);
    while let Some((directory_path, relative_path, depth)) = pending.pop_front() {
        if inventory.stats.entries_visited >= CURSOR_MAX_TRAVERSAL_ENTRIES {
            inventory.reject(
                directory_path,
                CursorDiscoveryIssueKind::LimitExceeded,
                format!(
                    "Cursor discovery exceeds the {CURSOR_MAX_TRAVERSAL_ENTRIES}-entry traversal limit"
                ),
                true,
            );
            break;
        }
        inventory.stats.directories_visited = inventory.stats.directories_visited.saturating_add(1);
        let directory = match authority.open_directory(&relative_path) {
            Ok(directory) => directory,
            Err(error) => {
                inventory.reject(
                    directory_path,
                    CursorDiscoveryIssueKind::Io,
                    error.to_string(),
                    true,
                );
                continue;
            }
        };
        let entries = read_directory_entries(&directory_path, &directory, inventory);
        for name in entries {
            let path = directory_path.join(&name);
            match directory.open_child(&name) {
                Ok(OpenedProviderSourcePath::Directory(child_directory)) => {
                    if depth >= CURSOR_MAX_DIRECTORY_DEPTH {
                        inventory.reject(
                        path,
                        CursorDiscoveryIssueKind::LimitExceeded,
                        format!(
                            "Cursor discovery exceeds the {CURSOR_MAX_DIRECTORY_DEPTH}-level directory depth limit"
                        ),
                        true,
                    );
                    } else {
                        pending.push_back((
                            path,
                            child_directory.relative_path().to_path_buf(),
                            depth.saturating_add(1),
                        ));
                    }
                }
                Ok(OpenedProviderSourcePath::File(file)) => {
                    inventory.stats.regular_files_visited =
                        inventory.stats.regular_files_visited.saturating_add(1);
                    inspect_file(input, &path, Arc::new(file), inventory, false);
                }
                Err(error) => inventory.reject(
                    path,
                    CursorDiscoveryIssueKind::Symlink,
                    error.to_string(),
                    true,
                ),
            }
        }
        if let Err(error) = directory.revalidate() {
            inventory.reject(
                directory_path,
                CursorDiscoveryIssueKind::Io,
                error.to_string(),
                true,
            );
        }
    }
}

fn read_directory_entries(
    directory_path: &Path,
    directory: &ProviderSourceDirectory,
    inventory: &mut CursorRootInventory,
) -> Vec<std::ffi::OsString> {
    let reader = match directory.entries(CURSOR_MAX_DIRECTORY_ENTRIES.saturating_add(1)) {
        Ok(entries) => entries,
        Err(error) => {
            inventory.reject(
                directory_path.to_path_buf(),
                CursorDiscoveryIssueKind::Io,
                error.to_string(),
                true,
            );
            return Vec::new();
        }
    };
    let mut entries = Vec::new();
    for name in reader {
        if entries.len() >= CURSOR_MAX_DIRECTORY_ENTRIES {
            inventory.reject(
                directory_path.to_path_buf(),
                CursorDiscoveryIssueKind::LimitExceeded,
                format!("Cursor directory exceeds the {CURSOR_MAX_DIRECTORY_ENTRIES}-entry limit"),
                true,
            );
            entries.clear();
            break;
        }
        if inventory.stats.entries_visited >= CURSOR_MAX_TRAVERSAL_ENTRIES {
            inventory.reject(
                directory_path.to_path_buf(),
                CursorDiscoveryIssueKind::LimitExceeded,
                format!(
                    "Cursor discovery exceeds the {CURSOR_MAX_TRAVERSAL_ENTRIES}-entry traversal limit"
                ),
                true,
            );
            entries.clear();
            break;
        }
        inventory.stats.entries_visited = inventory.stats.entries_visited.saturating_add(1);
        entries.push(name);
    }
    entries
}

fn inspect_file(
    input: &Path,
    path: &Path,
    source_file: Arc<OpenedProviderSourceFile>,
    inventory: &mut CursorRootInventory,
    explicit_file: bool,
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
    if ![
        projects_root,
        project,
        agent_transcripts,
        session_directory,
        path,
    ]
    .contains(&input)
    {
        inventory.reject(
            path.to_path_buf(),
            CursorDiscoveryIssueKind::InvalidLayout,
            "Cursor transcript is a loose nested lookalike below the selected root",
            false,
        );
        return;
    }
    match CursorTranscriptPath::parse(projects_root, path, source_file) {
        Ok(source) if inventory.transcripts.len() < CURSOR_MAX_TRANSCRIPTS => {
            inventory.transcripts.push(source);
        }
        Ok(_) => inventory.reject(
            path.to_path_buf(),
            CursorDiscoveryIssueKind::LimitExceeded,
            format!("Cursor discovery exceeds the {CURSOR_MAX_TRANSCRIPTS}-transcript limit"),
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
