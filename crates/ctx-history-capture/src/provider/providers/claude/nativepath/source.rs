use std::{
    ffi::OsStr,
    fs::{self, File, Metadata, OpenOptions},
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
};

const CLAUDE_OBSERVATION_DOMAIN: &[u8] = b"ctx-claude-nativepath-observation-v1\0";
const CLAUDE_INVENTORY_DOMAIN: &[u8] = b"ctx-claude-nativepath-inventory-v1\0";
pub(super) const CLAUDE_MAX_DIRECTORY_ENTRIES: usize = 4_096;
pub(super) const CLAUDE_MAX_TRAVERSAL_ENTRIES: usize = 16_384;

#[derive(Debug, Error)]
pub(crate) enum ClaudeNativePathError {
    #[error("Claude NativePath I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid Claude projects layout at {path}: {reason}")]
    InvalidLayout { path: PathBuf, reason: String },
    #[error("Claude source changed after discovery: {path}")]
    StaleDiscovery { path: PathBuf },
    #[error("Claude source changed while it was parsed: {path}")]
    SourceChanged { path: PathBuf },
    #[error("Claude inventory changed after discovery: {path}")]
    InventoryChanged { path: PathBuf },
    #[error("invalid Claude NativePath checkpoint: {reason}")]
    InvalidCheckpoint { reason: String },
    #[error("Claude NativePath byte or ordinal position overflow")]
    PositionOverflow,
}

impl ClaudeNativePathError {
    fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    fn invalid(path: &Path, reason: impl Into<String>) -> Self {
        Self::InvalidLayout {
            path: path.to_path_buf(),
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeSessionKey {
    pub(crate) root_session_id: String,
    pub(crate) workflow_run_id: Option<String>,
    pub(crate) agent_id: Option<String>,
}

impl ClaudeSessionKey {
    pub(crate) fn provider_session_id(&self) -> String {
        match (&self.workflow_run_id, &self.agent_id) {
            (None, None) => self.root_session_id.clone(),
            (None, Some(agent_id)) => {
                format!("{}/subagents/{agent_id}", self.root_session_id)
            }
            (Some(run_id), Some(agent_id)) => format!(
                "{}/subagents/workflows/{run_id}/{agent_id}",
                self.root_session_id
            ),
            (Some(_), None) => self.root_session_id.clone(),
        }
    }

    pub(crate) fn parent_provider_session_id(&self) -> Option<&str> {
        self.agent_id
            .as_ref()
            .map(|_| self.root_session_id.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionLayout {
    Primary,
    Subagent,
    WorkflowSubagent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudePhysicalFileId {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeFileFingerprint {
    pub(crate) len: u64,
    pub(crate) modified: Option<SystemTime>,
    pub(crate) created: Option<SystemTime>,
    pub(crate) change_marker: Option<(i64, i64)>,
    pub(crate) physical_file_id: Option<ClaudePhysicalFileId>,
}

impl ClaudeFileFingerprint {
    pub(super) fn from_metadata(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        let physical_file_id = {
            use std::os::unix::fs::MetadataExt;
            Some(ClaudePhysicalFileId {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        };
        #[cfg(not(unix))]
        let physical_file_id = None;
        #[cfg(unix)]
        let change_marker = {
            use std::os::unix::fs::MetadataExt;
            Some((metadata.ctime(), metadata.ctime_nsec()))
        };
        #[cfg(not(unix))]
        let change_marker = None;

        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
            change_marker,
            physical_file_id,
        }
    }

    pub(super) fn observation_sha256(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(CLAUDE_OBSERVATION_DOMAIN);
        hasher.update(self.len.to_be_bytes());
        update_time(&mut hasher, self.modified);
        update_time(&mut hasher, self.created);
        match self.change_marker {
            Some((seconds, nanoseconds)) => {
                hasher.update([1]);
                hasher.update(seconds.to_be_bytes());
                hasher.update(nanoseconds.to_be_bytes());
            }
            None => hasher.update([0]),
        }
        match self.physical_file_id {
            Some(file_id) => {
                hasher.update([1]);
                hasher.update(file_id.device.to_be_bytes());
                hasher.update(file_id.inode.to_be_bytes());
            }
            None => hasher.update([0]),
        }
        hasher.finalize().into()
    }
}

fn update_time(hasher: &mut Sha256, value: Option<SystemTime>) {
    let Some(value) = value else {
        hasher.update([0]);
        return;
    };
    match value.duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            hasher.update([1, 1]);
            hasher.update(duration.as_secs().to_be_bytes());
            hasher.update(duration.subsec_nanos().to_be_bytes());
        }
        Err(error) => {
            let duration = error.duration();
            hasher.update([1, 0]);
            hasher.update(duration.as_secs().to_be_bytes());
            hasher.update(duration.subsec_nanos().to_be_bytes());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoveredClaudeSession {
    pub(crate) project_dir: PathBuf,
    pub(crate) path: PathBuf,
    pub(crate) canonical_path: PathBuf,
    pub(crate) key: ClaudeSessionKey,
    pub(crate) layout: SessionLayout,
    pub(crate) fingerprint: ClaudeFileFingerprint,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ClaudeDiscoveryStats {
    pub(crate) project_directories: usize,
    pub(crate) directory_entries: usize,
    pub(crate) selected_sessions: usize,
}

/// Evidence that one bounded traversal completed without suppressing a route.
///
/// Only a revalidated complete certificate can authorize missing-source
/// candidates. The digest contains route identities and observations, not
/// transcript content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeInventoryCertificate {
    pub(crate) root: PathBuf,
    pub(crate) canonical_root: PathBuf,
    pub(crate) route_count: usize,
    pub(crate) routes_sha256: [u8; 32],
    pub(crate) complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeDiscovery {
    pub(crate) root: PathBuf,
    pub(crate) sessions: Vec<DiscoveredClaudeSession>,
    pub(crate) stats: ClaudeDiscoveryStats,
    pub(crate) inventory: ClaudeInventoryCertificate,
}

impl ClaudeDiscovery {
    pub(crate) fn revalidate_inventory(&self) -> Result<(), ClaudeNativePathError> {
        let current = discover_projects(&self.root)?;
        if current.inventory != self.inventory {
            return Err(ClaudeNativePathError::InventoryChanged {
                path: self.root.clone(),
            });
        }
        Ok(())
    }
}

/// Provider-owned source lifecycle evidence. The short names in the comments
/// are the NativePath review matrix: N/R/A/W/Rw/X/M/C/D.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaudeSourceLifecycle {
    New,         // N
    Replay,      // R
    Append,      // A
    Rewrite,     // W
    Rewind,      // Rw
    Replacement, // X
    Move,        // M
    Copy,        // C
    #[allow(dead_code)]
    DeletionCandidate, // D
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct ClaudeDeletionCandidate {
    pub(crate) lifecycle: ClaudeSourceLifecycle,
    pub(crate) session_key: ClaudeSessionKey,
    pub(crate) canonical_route: PathBuf,
    pub(crate) inventory: ClaudeInventoryCertificate,
}

#[derive(Debug, Default)]
struct TraversalBudget {
    scanned: usize,
}

impl TraversalBudget {
    fn observe(&mut self, path: &Path) -> Result<(), ClaudeNativePathError> {
        self.scanned = self
            .scanned
            .checked_add(1)
            .ok_or(ClaudeNativePathError::PositionOverflow)?;
        if self.scanned > CLAUDE_MAX_TRAVERSAL_ENTRIES {
            return Err(ClaudeNativePathError::invalid(
                path,
                format!(
                    "Claude discovery exceeds the {CLAUDE_MAX_TRAVERSAL_ENTRIES}-entry traversal limit"
                ),
            ));
        }
        Ok(())
    }
}

pub(crate) fn discover_projects(root: &Path) -> Result<ClaudeDiscovery, ClaudeNativePathError> {
    ensure_unlinked_path(root)?;
    let metadata =
        fs::symlink_metadata(root).map_err(|error| ClaudeNativePathError::io(root, error))?;
    if metadata.file_type().is_symlink() {
        return Err(ClaudeNativePathError::invalid(
            root,
            "symlinked roots are rejected",
        ));
    }

    let canonical_root =
        fs::canonicalize(root).map_err(|error| ClaudeNativePathError::io(root, error))?;
    let mut discovery = ClaudeDiscovery {
        root: root.to_path_buf(),
        sessions: Vec::new(),
        stats: ClaudeDiscoveryStats::default(),
        inventory: ClaudeInventoryCertificate {
            root: root.to_path_buf(),
            canonical_root,
            route_count: 0,
            routes_sha256: Sha256::digest(CLAUDE_INVENTORY_DOMAIN).into(),
            complete: false,
        },
    };
    if metadata.is_file() {
        discovery.sessions.push(discover_explicit_file(root)?);
        discovery.stats.selected_sessions = 1;
        finalize_inventory(&mut discovery)?;
        return Ok(discovery);
    }
    if !metadata.is_dir() {
        return Err(ClaudeNativePathError::invalid(
            root,
            "the import root is neither a directory nor a regular JSONL file",
        ));
    }

    let projects_root = if root.file_name() == Some(OsStr::new(".claude")) {
        let projects = root.join("projects");
        ensure_directory(&projects)?;
        projects
    } else {
        root.to_path_buf()
    };
    let mut budget = TraversalBudget::default();
    if projects_root.file_name() == Some(OsStr::new("projects")) {
        for project_dir in
            sorted_directory_paths(&projects_root, &mut discovery.stats, &mut budget)?
        {
            let metadata = fs::symlink_metadata(&project_dir)
                .map_err(|error| ClaudeNativePathError::io(&project_dir, error))?;
            if metadata.file_type().is_symlink() {
                return Err(ClaudeNativePathError::invalid(
                    &project_dir,
                    "symlinked project directories are rejected",
                ));
            }
            if metadata.is_dir() {
                discover_project(&project_dir, &mut discovery, &mut budget)?;
            }
        }
    } else {
        discover_project(&projects_root, &mut discovery, &mut budget)?;
    }

    discovery.sessions.sort_unstable_by(|left, right| {
        left.project_dir
            .cmp(&right.project_dir)
            .then_with(|| left.key.cmp(&right.key))
            .then_with(|| left.canonical_path.cmp(&right.canonical_path))
    });
    discovery.stats.selected_sessions = discovery.sessions.len();
    finalize_inventory(&mut discovery)?;
    Ok(discovery)
}

fn finalize_inventory(discovery: &mut ClaudeDiscovery) -> Result<(), ClaudeNativePathError> {
    let mut hasher = Sha256::new();
    hasher.update(CLAUDE_INVENTORY_DOMAIN);
    hasher.update(
        u64::try_from(discovery.sessions.len())
            .map_err(|_| ClaudeNativePathError::PositionOverflow)?
            .to_be_bytes(),
    );
    for source in &discovery.sessions {
        update_inventory_field(
            &mut hasher,
            source.canonical_path.as_os_str().as_encoded_bytes(),
        )?;
        let key = serde_json::to_vec(&source.key).map_err(|error| {
            ClaudeNativePathError::InvalidLayout {
                path: source.path.clone(),
                reason: format!("session identity cannot be certified: {error}"),
            }
        })?;
        update_inventory_field(&mut hasher, &key)?;
        hasher.update(source.fingerprint.observation_sha256());
    }
    discovery.inventory.route_count = discovery.sessions.len();
    discovery.inventory.routes_sha256 = hasher.finalize().into();
    discovery.inventory.complete = true;
    Ok(())
}

fn update_inventory_field(hasher: &mut Sha256, bytes: &[u8]) -> Result<(), ClaudeNativePathError> {
    hasher.update(
        u64::try_from(bytes.len())
            .map_err(|_| ClaudeNativePathError::PositionOverflow)?
            .to_be_bytes(),
    );
    hasher.update(bytes);
    Ok(())
}

fn discover_project(
    project_dir: &Path,
    discovery: &mut ClaudeDiscovery,
    budget: &mut TraversalBudget,
) -> Result<(), ClaudeNativePathError> {
    ensure_directory(project_dir)?;
    discovery.stats.project_directories += 1;
    for path in sorted_directory_paths(project_dir, &mut discovery.stats, budget)? {
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| ClaudeNativePathError::io(&path, error))?;
        if metadata.file_type().is_symlink() {
            if is_jsonl(&path) {
                return Err(ClaudeNativePathError::invalid(
                    &path,
                    "symlinked session files are rejected",
                ));
            }
            if has_primary_session_sibling(&path)? {
                return Err(ClaudeNativePathError::invalid(
                    &path,
                    "symlinked session directories are rejected",
                ));
            }
            continue;
        }
        if metadata.is_file() && is_jsonl(&path) {
            let root_session_id = utf8_file_stem(&path)?.to_owned();
            discovery.sessions.push(discover_file(
                project_dir,
                &path,
                SessionLayout::Primary,
                ClaudeSessionKey {
                    root_session_id,
                    workflow_run_id: None,
                    agent_id: None,
                },
            )?);
        } else if metadata.is_dir() {
            discover_session_subagents(project_dir, &path, discovery, budget)?;
        }
    }
    Ok(())
}

fn has_primary_session_sibling(path: &Path) -> Result<bool, ClaudeNativePathError> {
    let Some(name) = path.file_name() else {
        return Ok(false);
    };
    let mut primary_name = name.to_os_string();
    primary_name.push(".jsonl");
    let primary = path.with_file_name(primary_name);
    match fs::symlink_metadata(&primary) {
        Ok(metadata) => Ok(metadata.is_file() && !metadata.file_type().is_symlink()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(ClaudeNativePathError::io(&primary, error)),
    }
}

fn discover_session_subagents(
    project_dir: &Path,
    session_dir: &Path,
    discovery: &mut ClaudeDiscovery,
    budget: &mut TraversalBudget,
) -> Result<(), ClaudeNativePathError> {
    let root_session_id = utf8_file_name(session_dir)?.to_owned();
    let subagents = session_dir.join("subagents");
    let Ok(metadata) = fs::symlink_metadata(&subagents) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        return Err(ClaudeNativePathError::invalid(
            &subagents,
            "symlinked subagent directories are rejected",
        ));
    }
    if !metadata.is_dir() {
        return Ok(());
    }

    for candidate in sorted_directory_paths(&subagents, &mut discovery.stats, budget)? {
        let metadata = fs::symlink_metadata(&candidate)
            .map_err(|error| ClaudeNativePathError::io(&candidate, error))?;
        if candidate.file_name() == Some(OsStr::new("workflows")) {
            if metadata.file_type().is_symlink() {
                return Err(ClaudeNativePathError::invalid(
                    &candidate,
                    "symlinked workflow directories are rejected",
                ));
            }
            if metadata.is_dir() {
                discover_workflow_subagents(
                    project_dir,
                    &candidate,
                    &root_session_id,
                    discovery,
                    budget,
                )?;
            }
        } else if metadata.file_type().is_symlink() && is_subagent_jsonl(&candidate) {
            return Err(ClaudeNativePathError::invalid(
                &candidate,
                "symlinked subagent session files are rejected",
            ));
        } else if metadata.is_file() && is_subagent_jsonl(&candidate) {
            let agent_id = utf8_file_stem(&candidate)?.to_owned();
            discovery.sessions.push(discover_file(
                project_dir,
                &candidate,
                SessionLayout::Subagent,
                ClaudeSessionKey {
                    root_session_id: root_session_id.clone(),
                    workflow_run_id: None,
                    agent_id: Some(agent_id),
                },
            )?);
        }
    }
    Ok(())
}

fn discover_workflow_subagents(
    project_dir: &Path,
    workflows_dir: &Path,
    root_session_id: &str,
    discovery: &mut ClaudeDiscovery,
    budget: &mut TraversalBudget,
) -> Result<(), ClaudeNativePathError> {
    for run_dir in sorted_directory_paths(workflows_dir, &mut discovery.stats, budget)? {
        let metadata = fs::symlink_metadata(&run_dir)
            .map_err(|error| ClaudeNativePathError::io(&run_dir, error))?;
        if metadata.file_type().is_symlink() {
            return Err(ClaudeNativePathError::invalid(
                &run_dir,
                "symlinked workflow run directories are rejected",
            ));
        }
        if !metadata.is_dir() {
            continue;
        }
        let run_id = utf8_file_name(&run_dir)?.to_owned();
        for candidate in sorted_directory_paths(&run_dir, &mut discovery.stats, budget)? {
            let metadata = fs::symlink_metadata(&candidate)
                .map_err(|error| ClaudeNativePathError::io(&candidate, error))?;
            if metadata.file_type().is_symlink() && is_subagent_jsonl(&candidate) {
                return Err(ClaudeNativePathError::invalid(
                    &candidate,
                    "symlinked workflow subagent files are rejected",
                ));
            }
            if metadata.is_file() && is_subagent_jsonl(&candidate) {
                let agent_id = utf8_file_stem(&candidate)?.to_owned();
                discovery.sessions.push(discover_file(
                    project_dir,
                    &candidate,
                    SessionLayout::WorkflowSubagent,
                    ClaudeSessionKey {
                        root_session_id: root_session_id.to_owned(),
                        workflow_run_id: Some(run_id.clone()),
                        agent_id: Some(agent_id),
                    },
                )?);
            }
        }
    }
    Ok(())
}

fn discover_explicit_file(path: &Path) -> Result<DiscoveredClaudeSession, ClaudeNativePathError> {
    if !is_jsonl(path) {
        return Err(ClaudeNativePathError::invalid(
            path,
            "explicit Claude session files must use the .jsonl extension",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| ClaudeNativePathError::invalid(path, "session file has no parent"))?;
    let file_stem = utf8_file_stem(path)?.to_owned();
    if parent.file_name() == Some(OsStr::new("subagents")) {
        if !is_subagent_jsonl(path) {
            return Err(ClaudeNativePathError::invalid(
                path,
                "subagent session filenames must match agent-*.jsonl",
            ));
        }
        let session_dir = parent
            .parent()
            .ok_or_else(|| ClaudeNativePathError::invalid(path, "incomplete subagent layout"))?;
        let project_dir = session_dir
            .parent()
            .ok_or_else(|| ClaudeNativePathError::invalid(path, "incomplete subagent layout"))?;
        return discover_file(
            project_dir,
            path,
            SessionLayout::Subagent,
            ClaudeSessionKey {
                root_session_id: utf8_file_name(session_dir)?.to_owned(),
                workflow_run_id: None,
                agent_id: Some(file_stem),
            },
        );
    }

    if parent
        .parent()
        .is_some_and(|value| value.file_name() == Some(OsStr::new("workflows")))
        && parent
            .parent()
            .and_then(Path::parent)
            .is_some_and(|value| value.file_name() == Some(OsStr::new("subagents")))
    {
        if !is_subagent_jsonl(path) {
            return Err(ClaudeNativePathError::invalid(
                path,
                "workflow subagent filenames must match agent-*.jsonl",
            ));
        }
        let workflows = parent
            .parent()
            .ok_or_else(|| ClaudeNativePathError::invalid(path, "incomplete workflow layout"))?;
        let subagents = workflows
            .parent()
            .ok_or_else(|| ClaudeNativePathError::invalid(path, "incomplete workflow layout"))?;
        let session_dir = subagents
            .parent()
            .ok_or_else(|| ClaudeNativePathError::invalid(path, "incomplete workflow layout"))?;
        let project_dir = session_dir
            .parent()
            .ok_or_else(|| ClaudeNativePathError::invalid(path, "incomplete workflow layout"))?;
        return discover_file(
            project_dir,
            path,
            SessionLayout::WorkflowSubagent,
            ClaudeSessionKey {
                root_session_id: utf8_file_name(session_dir)?.to_owned(),
                workflow_run_id: Some(utf8_file_name(parent)?.to_owned()),
                agent_id: Some(file_stem),
            },
        );
    }

    discover_file(
        parent,
        path,
        SessionLayout::Primary,
        ClaudeSessionKey {
            root_session_id: file_stem,
            workflow_run_id: None,
            agent_id: None,
        },
    )
}

fn discover_file(
    project_dir: &Path,
    path: &Path,
    layout: SessionLayout,
    key: ClaudeSessionKey,
) -> Result<DiscoveredClaudeSession, ClaudeNativePathError> {
    ensure_unlinked_file(path)?;
    let metadata =
        fs::symlink_metadata(path).map_err(|error| ClaudeNativePathError::io(path, error))?;
    let canonical_path =
        fs::canonicalize(path).map_err(|error| ClaudeNativePathError::io(path, error))?;
    Ok(DiscoveredClaudeSession {
        project_dir: project_dir.to_path_buf(),
        path: path.to_path_buf(),
        canonical_path,
        key,
        layout,
        fingerprint: ClaudeFileFingerprint::from_metadata(&metadata),
    })
}

pub(super) fn open_discovered_file(
    source: &DiscoveredClaudeSession,
) -> Result<File, ClaudeNativePathError> {
    ensure_unlinked_file(&source.path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let file = options
        .open(&source.path)
        .map_err(|error| ClaudeNativePathError::io(&source.path, error))?;
    let fingerprint = ClaudeFileFingerprint::from_metadata(
        &file
            .metadata()
            .map_err(|error| ClaudeNativePathError::io(&source.path, error))?,
    );
    if fingerprint != source.fingerprint {
        return Err(ClaudeNativePathError::StaleDiscovery {
            path: source.path.clone(),
        });
    }
    Ok(file)
}

pub(super) fn revalidate_open_file(
    source: &DiscoveredClaudeSession,
    file: &File,
    expected: &ClaudeFileFingerprint,
) -> Result<(), ClaudeNativePathError> {
    let open_fingerprint = ClaudeFileFingerprint::from_metadata(
        &file
            .metadata()
            .map_err(|error| ClaudeNativePathError::io(&source.path, error))?,
    );
    let path_fingerprint = ClaudeFileFingerprint::from_metadata(
        &fs::symlink_metadata(&source.path)
            .map_err(|error| ClaudeNativePathError::io(&source.path, error))?,
    );
    if &open_fingerprint != expected || &path_fingerprint != expected {
        return Err(ClaudeNativePathError::SourceChanged {
            path: source.path.clone(),
        });
    }
    Ok(())
}

pub(crate) fn revalidate_discovered_source(
    source: &DiscoveredClaudeSession,
) -> Result<(), ClaudeNativePathError> {
    let file = open_discovered_file(source)?;
    revalidate_open_file(source, &file, &source.fingerprint)
}

fn sorted_directory_paths(
    directory: &Path,
    stats: &mut ClaudeDiscoveryStats,
    budget: &mut TraversalBudget,
) -> Result<Vec<PathBuf>, ClaudeNativePathError> {
    let entries =
        fs::read_dir(directory).map_err(|error| ClaudeNativePathError::io(directory, error))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| ClaudeNativePathError::io(directory, error))?;
        let path = entry.path();
        budget.observe(&path)?;
        if paths.len() >= CLAUDE_MAX_DIRECTORY_ENTRIES {
            return Err(ClaudeNativePathError::invalid(
                directory,
                format!(
                    "directory exceeds the {CLAUDE_MAX_DIRECTORY_ENTRIES}-entry Claude discovery limit"
                ),
            ));
        }
        stats.directory_entries += 1;
        paths.push(path);
    }
    paths.sort_unstable();
    Ok(paths)
}

fn ensure_directory(path: &Path) -> Result<(), ClaudeNativePathError> {
    ensure_unlinked_path(path)?;
    let metadata =
        fs::symlink_metadata(path).map_err(|error| ClaudeNativePathError::io(path, error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ClaudeNativePathError::invalid(
            path,
            "expected an unlinked directory",
        ));
    }
    Ok(())
}

fn ensure_unlinked_file(path: &Path) -> Result<(), ClaudeNativePathError> {
    ensure_regular_provider_transcript_file(path)
        .map_err(|error| ClaudeNativePathError::invalid(path, error.to_string()))
}

fn ensure_unlinked_path(path: &Path) -> Result<(), ClaudeNativePathError> {
    ensure_provider_path_parents_are_not_symlinks(path)
        .map_err(|error| ClaudeNativePathError::invalid(path, error.to_string()))
}

fn utf8_file_stem(path: &Path) -> Result<&str, ClaudeNativePathError> {
    path.file_stem()
        .and_then(OsStr::to_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ClaudeNativePathError::invalid(path, "session filename is not valid UTF-8"))
}

fn utf8_file_name(path: &Path) -> Result<&str, ClaudeNativePathError> {
    path.file_name()
        .and_then(OsStr::to_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ClaudeNativePathError::invalid(path, "layout name is not valid UTF-8"))
}

fn is_jsonl(path: &Path) -> bool {
    path.extension() == Some(OsStr::new("jsonl"))
}

fn is_subagent_jsonl(path: &Path) -> bool {
    is_jsonl(path)
        && path
            .file_stem()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with("agent-") && name.len() > "agent-".len())
}

#[allow(dead_code)]
pub(crate) fn authoritative_deletion_candidates(
    discovery: &ClaudeDiscovery,
    known: &[super::checkpoint::ParseCheckpoint],
) -> Result<Vec<ClaudeDeletionCandidate>, ClaudeNativePathError> {
    if !discovery.inventory.complete {
        return Err(ClaudeNativePathError::InvalidCheckpoint {
            reason: "deletion authority requires a complete Claude inventory".to_owned(),
        });
    }
    if known.len() > CLAUDE_MAX_TRAVERSAL_ENTRIES {
        return Err(ClaudeNativePathError::InvalidCheckpoint {
            reason: format!(
                "deletion authority exceeds the {CLAUDE_MAX_TRAVERSAL_ENTRIES}-source Claude bound"
            ),
        });
    }
    discovery.revalidate_inventory()?;
    let present = discovery
        .sessions
        .iter()
        .map(|source| &source.canonical_path)
        .collect::<std::collections::BTreeSet<_>>();
    let mut missing_routes = std::collections::BTreeSet::new();
    Ok(known
        .iter()
        .filter(|checkpoint| {
            !present.contains(&checkpoint.canonical_route)
                && missing_routes.insert(checkpoint.canonical_route.clone())
        })
        .map(|checkpoint| ClaudeDeletionCandidate {
            lifecycle: ClaudeSourceLifecycle::DeletionCandidate,
            session_key: checkpoint.session_key.clone(),
            canonical_route: checkpoint.canonical_route.clone(),
            inventory: discovery.inventory.clone(),
        })
        .collect())
}
