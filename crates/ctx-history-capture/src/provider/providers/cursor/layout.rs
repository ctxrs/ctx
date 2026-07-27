use std::{
    collections::{BTreeSet, VecDeque},
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, provider_metadata_is_link_like,
};

const AGENT_TRANSCRIPTS: &str = "agent-transcripts";
pub(super) const CURSOR_MAX_DIRECTORY_DEPTH: usize = 128;
pub(super) const CURSOR_MAX_DIRECTORY_ENTRIES: usize = 1_024;
pub(super) const CURSOR_MAX_TRAVERSAL_ENTRIES: usize = 4_096;
pub(super) const CURSOR_MAX_DISCOVERY_ISSUE_SAMPLES: usize = 128;
pub(super) const CURSOR_MAX_TRANSCRIPTS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorTranscriptPath {
    projects_root: PathBuf,
    project: PathBuf,
    session_id: String,
    path: PathBuf,
}

impl CursorTranscriptPath {
    pub(crate) fn parse(projects_root: &Path, path: &Path) -> Result<Self, &'static str> {
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
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorRootInventory {
    pub(crate) input: PathBuf,
    pub(crate) projects_roots: Vec<PathBuf>,
    pub(crate) transcripts: Vec<CursorTranscriptPath>,
    pub(crate) issues: Vec<CursorDiscoveryIssue>,
    pub(crate) completed: bool,
    pub(crate) stats: CursorDiscoveryStats,
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
}

pub(crate) fn discover_cursor_transcripts(input: &Path) -> CursorRootInventory {
    let mut inventory = CursorRootInventory::new(input);
    if let Err(error) = ensure_provider_path_parents_are_not_symlinks(input) {
        inventory.reject(
            input.to_path_buf(),
            CursorDiscoveryIssueKind::Symlink,
            error.to_string(),
            true,
        );
        return inventory;
    }
    let metadata = match fs::symlink_metadata(input) {
        Ok(metadata) => metadata,
        Err(error) => {
            let kind = if error.kind() == std::io::ErrorKind::NotFound {
                CursorDiscoveryIssueKind::NotFound
            } else {
                CursorDiscoveryIssueKind::Io
            };
            inventory.reject(input.to_path_buf(), kind, error.to_string(), true);
            return inventory;
        }
    };
    if provider_metadata_is_link_like(&metadata) {
        inventory.reject(
            input.to_path_buf(),
            CursorDiscoveryIssueKind::Symlink,
            "symlinked Cursor transcript roots are rejected",
            true,
        );
        return inventory;
    }
    if metadata.file_type().is_file() {
        inventory.stats.entries_visited = 1;
        inventory.stats.regular_files_visited = 1;
        inspect_file(input, input, &mut inventory, true);
    } else if metadata.file_type().is_dir() {
        visit_directories(input, &mut inventory);
    } else {
        inventory.reject(
            input.to_path_buf(),
            CursorDiscoveryIssueKind::UnsupportedFileType,
            "Cursor discovery input must be a regular file or directory",
            true,
        );
    }
    inventory.finish();
    inventory
}

fn visit_directories(input: &Path, inventory: &mut CursorRootInventory) {
    let mut pending = VecDeque::from([(input.to_path_buf(), 0_usize)]);
    while let Some((directory, depth)) = pending.pop_front() {
        if inventory.stats.entries_visited >= CURSOR_MAX_TRAVERSAL_ENTRIES {
            inventory.reject(
                directory,
                CursorDiscoveryIssueKind::LimitExceeded,
                format!(
                    "Cursor discovery exceeds the {CURSOR_MAX_TRAVERSAL_ENTRIES}-entry traversal limit"
                ),
                true,
            );
            break;
        }
        inventory.stats.directories_visited = inventory.stats.directories_visited.saturating_add(1);
        let entries = read_directory_entries(&directory, inventory);
        for entry in entries {
            let path = entry.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    inventory.reject(path, CursorDiscoveryIssueKind::Io, error.to_string(), true);
                    continue;
                }
            };
            if provider_metadata_is_link_like(&metadata) {
                inventory.reject(
                    path,
                    CursorDiscoveryIssueKind::Symlink,
                    "symlinked Cursor transcript entries are rejected",
                    true,
                );
            } else if metadata.file_type().is_dir() {
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
                    pending.push_back((path, depth.saturating_add(1)));
                }
            } else if metadata.file_type().is_file() {
                inventory.stats.regular_files_visited =
                    inventory.stats.regular_files_visited.saturating_add(1);
                inspect_file(input, &path, inventory, false);
            }
        }
    }
}

fn read_directory_entries(
    directory: &Path,
    inventory: &mut CursorRootInventory,
) -> Vec<fs::DirEntry> {
    let reader = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            inventory.reject(
                directory.to_path_buf(),
                CursorDiscoveryIssueKind::Io,
                error.to_string(),
                true,
            );
            return Vec::new();
        }
    };
    let mut entries = Vec::new();
    for entry in reader {
        if entries.len() >= CURSOR_MAX_DIRECTORY_ENTRIES {
            inventory.reject(
                directory.to_path_buf(),
                CursorDiscoveryIssueKind::LimitExceeded,
                format!("Cursor directory exceeds the {CURSOR_MAX_DIRECTORY_ENTRIES}-entry limit"),
                true,
            );
            entries.clear();
            break;
        }
        if inventory.stats.entries_visited >= CURSOR_MAX_TRAVERSAL_ENTRIES {
            inventory.reject(
                directory.to_path_buf(),
                CursorDiscoveryIssueKind::LimitExceeded,
                format!(
                    "Cursor discovery exceeds the {CURSOR_MAX_TRAVERSAL_ENTRIES}-entry traversal limit"
                ),
                true,
            );
            entries.clear();
            break;
        }
        match entry {
            Ok(entry) => {
                inventory.stats.entries_visited = inventory.stats.entries_visited.saturating_add(1);
                entries.push(entry);
            }
            Err(error) => {
                inventory.reject(
                    directory.to_path_buf(),
                    CursorDiscoveryIssueKind::Io,
                    error.to_string(),
                    true,
                );
                entries.clear();
                break;
            }
        }
    }
    entries.sort_by_key(|entry| entry.file_name());
    entries
}

fn inspect_file(
    input: &Path,
    path: &Path,
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
    match CursorTranscriptPath::parse(projects_root, path) {
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
