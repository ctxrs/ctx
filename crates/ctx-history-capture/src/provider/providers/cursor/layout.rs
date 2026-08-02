use std::{
    collections::{BTreeSet, VecDeque},
    ffi::OsStr,
    path::{Path, PathBuf},
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorDiscoveryIssueKind {
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
pub(crate) struct CursorRootInventory {
    pub(crate) input: PathBuf,
    pub(crate) projects_roots: Vec<PathBuf>,
    pub(crate) transcripts: Vec<CursorTranscriptPath>,
    pub(crate) issues: Vec<CursorDiscoveryIssue>,
    pub(crate) completed: bool,
    pub(crate) stats: CursorDiscoveryStats,
    authority: Option<ProviderSourceRoot>,
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
            inventory.stats.entries_visited = 1;
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
                input,
                relative_path,
                file,
                authority,
                &mut inventory,
                true,
            );
        }
        OpenedProviderSourcePath::Directory(directory) => {
            let authority = directory.authority_root();
            inventory.authority = Some(authority.clone());
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

fn cursor_explicit_authority(input: &Path) -> crate::Result<(ProviderSourceRoot, PathBuf)> {
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
                    inspect_file(
                        input,
                        &path,
                        relative_path.join(name),
                        file,
                        authority.clone(),
                        inventory,
                        false,
                    );
                }
                Err(error) => {
                    // A non-regular entry (Unix-domain socket, FIFO, device
                    // node) is safely refused by the authority walk. Skip it
                    // without invalidating completion: its mere presence beside
                    // valid transcripts must not mark the whole Cursor source
                    // unreadable, which would otherwise fail the full refresh.
                    if crate::common::io::is_non_regular_source_rejection(&error) {
                        inventory.reject(
                            path,
                            CursorDiscoveryIssueKind::SpecialFile,
                            error.to_string(),
                            false,
                        );
                    } else {
                        inventory.reject(
                            path,
                            CursorDiscoveryIssueKind::Symlink,
                            error.to_string(),
                            true,
                        );
                    }
                }
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
    authority_relative_path: PathBuf,
    source_file: OpenedProviderSourceFile,
    authority: ProviderSourceRoot,
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

fn cursor_catalog_token(
    source_file: &OpenedProviderSourceFile,
    authority: &ProviderSourceRoot,
    authority_relative_path: &Path,
    explicit_file: bool,
) -> crate::Result<[u8; 32]> {
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    // Creates a non-regular special file (FIFO) at `path`. A FIFO shares the
    // exact authority-walk rejection path as the reported Cursor `worker.sock`
    // Unix-domain socket (both classify as NON_REGULAR_PROVIDER_SOURCE_REASON),
    // and unlike a bound socket it is not constrained by the platform sun_path
    // length limit under a deep temp directory. The socket-specific errno
    // mapping is covered separately in the root_handle unix tests.
    fn make_special_file(path: &Path) {
        let raw = CString::new(path.as_os_str().as_bytes()).unwrap();
        let result = unsafe { libc::mkfifo(raw.as_ptr(), 0o600) };
        assert_eq!(result, 0, "mkfifo {}", path.display());
    }

    #[test]
    fn cursor_special_file_beside_transcript_is_skipped_without_failing_discovery() {
        // Regression: a live special file (for example Cursor's `worker.sock`)
        // sitting beside valid transcripts must be safely skipped, not treated
        // as an unreadable source. Previously it invalidated completion, marked
        // the whole Cursor provider `unknown`, and failed the full refresh so
        // no lexical index was published for any provider.
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path();
        let session_directory = root
            .join("projects")
            .join("acme")
            .join(AGENT_TRANSCRIPTS)
            .join("session-a");
        std::fs::create_dir_all(&session_directory).unwrap();
        std::fs::write(session_directory.join("session-a.jsonl"), b"{}\n").unwrap();
        make_special_file(&session_directory.join("worker.sock"));

        let inventory = discover_cursor_transcripts(root);

        assert!(
            inventory.completed,
            "special-file entry must not invalidate completion: {:?}",
            inventory.issues
        );
        assert_eq!(
            inventory.transcripts.len(),
            1,
            "the valid transcript is still discovered alongside the special file"
        );
        assert!(
            inventory
                .issues
                .iter()
                .any(|issue| issue.kind == CursorDiscoveryIssueKind::SpecialFile),
            "the skipped special file is recorded as a non-fatal issue: {:?}",
            inventory.issues
        );
    }
}
