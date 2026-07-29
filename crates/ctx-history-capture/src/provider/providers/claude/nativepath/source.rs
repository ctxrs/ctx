use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs::{File, Metadata},
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    common::io::{
        open_provider_source_path, OpenedProviderSourceFile, OpenedProviderSourcePath,
        ProviderSourceDirectory, ProviderSourceRoot,
    },
    CaptureError,
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

#[derive(Debug, Clone)]
pub(crate) struct DiscoveredClaudeSession {
    pub(crate) project_dir: PathBuf,
    pub(crate) path: PathBuf,
    pub(crate) canonical_path: PathBuf,
    pub(crate) key: ClaudeSessionKey,
    pub(crate) layout: SessionLayout,
    pub(crate) fingerprint: ClaudeFileFingerprint,
    pub(crate) opened: Arc<OpenedProviderSourceFile>,
}

impl PartialEq for DiscoveredClaudeSession {
    fn eq(&self, other: &Self) -> bool {
        self.project_dir == other.project_dir
            && self.path == other.path
            && self.canonical_path == other.canonical_path
            && self.key == other.key
            && self.layout == other.layout
            && self.fingerprint == other.fingerprint
    }
}

impl Eq for DiscoveredClaudeSession {}

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

#[derive(Debug, Clone)]
pub(crate) struct ClaudeDiscovery {
    pub(crate) root: PathBuf,
    pub(crate) sessions: Vec<DiscoveredClaudeSession>,
    pub(crate) stats: ClaudeDiscoveryStats,
    pub(crate) inventory: ClaudeInventoryCertificate,
    authority: ClaudeDiscoveryAuthority,
}

#[derive(Debug, Clone)]
enum ClaudeDiscoveryAuthority {
    Root(ProviderSourceRoot),
    File(Arc<OpenedProviderSourceFile>),
}

impl PartialEq for ClaudeDiscovery {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root
            && self.sessions == other.sessions
            && self.stats == other.stats
            && self.inventory == other.inventory
    }
}

impl Eq for ClaudeDiscovery {}

impl ClaudeDiscovery {
    pub(crate) fn revalidate_inventory(&self) -> Result<(), ClaudeNativePathError> {
        let current = self.rediscover()?;
        if current.inventory != self.inventory {
            return Err(ClaudeNativePathError::InventoryChanged {
                path: self.root.clone(),
            });
        }
        Ok(())
    }

    pub(crate) fn rediscover(&self) -> Result<Self, ClaudeNativePathError> {
        discover_from_authority(&self.root, self.authority.clone())
    }

    pub(crate) fn has_directory_authority(&self) -> bool {
        matches!(self.authority, ClaudeDiscoveryAuthority::Root(_))
    }
}

#[derive(Debug, Default)]
struct ClaudeOpenedTree {
    directories: BTreeSet<PathBuf>,
    files: BTreeMap<PathBuf, Arc<OpenedProviderSourceFile>>,
    rejected_links: Vec<PathBuf>,
    stats: ClaudeDiscoveryStats,
    traversal_entries: usize,
}

pub(crate) fn discover_projects(root: &Path) -> Result<ClaudeDiscovery, ClaudeNativePathError> {
    let root = std::path::absolute(root).map_err(|error| ClaudeNativePathError::io(root, error))?;
    let opened =
        open_provider_source_path(&root).map_err(|error| map_capture_error(&root, error))?;
    match opened {
        OpenedProviderSourcePath::File(file) => {
            discover_from_authority(&root, ClaudeDiscoveryAuthority::File(Arc::new(file)))
        }
        OpenedProviderSourcePath::Directory(directory) => discover_from_authority(
            &root,
            ClaudeDiscoveryAuthority::Root(directory.authority_root()),
        ),
    }
}

fn discover_from_authority(
    root: &Path,
    authority: ClaudeDiscoveryAuthority,
) -> Result<ClaudeDiscovery, ClaudeNativePathError> {
    let canonical_root = root.to_path_buf();
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
        authority: authority.clone(),
    };
    match authority {
        ClaudeDiscoveryAuthority::File(opened) => {
            opened
                .revalidate()
                .map_err(|error| map_capture_error(root, error))?;
            discovery
                .sessions
                .push(discover_explicit_opened(root, opened)?);
            discovery.stats.selected_sessions = 1;
        }
        ClaudeDiscoveryAuthority::Root(authority) => {
            let mut tree = ClaudeOpenedTree::default();
            let directory = authority
                .directory()
                .map_err(|error| map_capture_error(root, error))?;
            inventory_claude_tree(root, directory, &mut tree)?;
            authority
                .revalidate()
                .map_err(|error| map_capture_error(root, error))?;
            discovery.stats = tree.stats;
            bind_opened_tree(root, tree, &mut discovery)?;
        }
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

fn inventory_claude_tree(
    display_path: &Path,
    directory: ProviderSourceDirectory,
    tree: &mut ClaudeOpenedTree,
) -> Result<(), ClaudeNativePathError> {
    tree.directories.insert(display_path.to_path_buf());
    let names = directory
        .entries(CLAUDE_MAX_DIRECTORY_ENTRIES.saturating_add(1))
        .map_err(|error| map_capture_error(display_path, error))?;
    if names.len() > CLAUDE_MAX_DIRECTORY_ENTRIES {
        return Err(ClaudeNativePathError::invalid(
            display_path,
            format!(
                "directory exceeds the {CLAUDE_MAX_DIRECTORY_ENTRIES}-entry Claude discovery limit"
            ),
        ));
    }
    tree.stats.directory_entries = tree
        .stats
        .directory_entries
        .checked_add(names.len())
        .ok_or(ClaudeNativePathError::PositionOverflow)?;
    tree.traversal_entries = tree
        .traversal_entries
        .checked_add(names.len())
        .ok_or(ClaudeNativePathError::PositionOverflow)?;
    if tree.traversal_entries > CLAUDE_MAX_TRAVERSAL_ENTRIES {
        return Err(ClaudeNativePathError::invalid(
            display_path,
            format!(
                "Claude discovery exceeds the {CLAUDE_MAX_TRAVERSAL_ENTRIES}-entry traversal limit"
            ),
        ));
    }
    for name in names {
        let path = display_path.join(&name);
        let opened = match directory.open_child(&name) {
            Ok(opened) => opened,
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => {
                tree.rejected_links.push(path);
                continue;
            }
            Err(error) => return Err(map_capture_error(&path, error)),
        };
        match opened {
            OpenedProviderSourcePath::Directory(child) => {
                inventory_claude_tree(&path, child, tree)?;
            }
            OpenedProviderSourcePath::File(file) => {
                tree.files.insert(path, Arc::new(file));
            }
        }
    }
    directory
        .revalidate()
        .map_err(|error| map_capture_error(display_path, error))?;
    Ok(())
}

fn bind_opened_tree(
    root: &Path,
    tree: ClaudeOpenedTree,
    discovery: &mut ClaudeDiscovery,
) -> Result<(), ClaudeNativePathError> {
    let projects_root = if root.file_name() == Some(OsStr::new(".claude")) {
        let projects = root.join("projects");
        if !tree.directories.contains(&projects) {
            return Err(ClaudeNativePathError::invalid(
                &projects,
                "expected an unlinked directory",
            ));
        }
        projects
    } else {
        root.to_path_buf()
    };
    let projects_container = projects_root.file_name() == Some(OsStr::new("projects"));
    for path in &tree.rejected_links {
        let Ok(relative) = path.strip_prefix(&projects_root) else {
            continue;
        };
        let components = relative
            .components()
            .map(|component| component.as_os_str())
            .collect::<Vec<_>>();
        let (_, layout, key) =
            classify_claude_relative(&projects_root, projects_container, &components, path)?;
        if layout.is_some() && key.is_some() {
            let reason = match layout {
                Some(SessionLayout::Primary) => "symlinked session files are rejected",
                Some(SessionLayout::Subagent) => "symlinked subagent session files are rejected",
                Some(SessionLayout::WorkflowSubagent) => {
                    "symlinked workflow subagent files are rejected"
                }
                None => unreachable!(),
            };
            return Err(ClaudeNativePathError::invalid(path, reason));
        }
        let mut primary_name = path
            .file_name()
            .ok_or_else(|| ClaudeNativePathError::invalid(path, "layout name is missing"))?
            .to_os_string();
        primary_name.push(".jsonl");
        if tree.files.contains_key(&path.with_file_name(primary_name)) {
            return Err(ClaudeNativePathError::invalid(
                path,
                "symlinked session directories are rejected",
            ));
        }
    }
    let mut projects = BTreeSet::new();
    for (path, opened) in tree.files {
        let Ok(relative) = path.strip_prefix(&projects_root) else {
            continue;
        };
        let components = relative
            .components()
            .map(|component| component.as_os_str())
            .collect::<Vec<_>>();
        let (project_dir, layout, key) =
            classify_claude_relative(&projects_root, projects_container, &components, &path)?;
        let Some((project_dir, layout, key)) = project_dir
            .zip(layout)
            .zip(key)
            .map(|((project_dir, layout), key)| (project_dir, layout, key))
        else {
            continue;
        };
        projects.insert(project_dir.clone());
        discovery.sessions.push(discover_file_from_opened(
            project_dir,
            path,
            layout,
            key,
            opened,
        )?);
    }
    discovery.stats.project_directories = projects.len();
    Ok(())
}

fn classify_claude_relative(
    projects_root: &Path,
    projects_container: bool,
    components: &[&OsStr],
    path: &Path,
) -> Result<
    (
        Option<PathBuf>,
        Option<SessionLayout>,
        Option<ClaudeSessionKey>,
    ),
    ClaudeNativePathError,
> {
    let (project_dir, inner) = if projects_container {
        let Some(project) = components.first() else {
            return Ok((None, None, None));
        };
        (projects_root.join(project), &components[1..])
    } else {
        (projects_root.to_path_buf(), components)
    };
    if inner.len() == 1 && is_jsonl(path) {
        let root_session_id = utf8_file_stem(path)?.to_owned();
        return Ok((
            Some(project_dir),
            Some(SessionLayout::Primary),
            Some(ClaudeSessionKey {
                root_session_id,
                workflow_run_id: None,
                agent_id: None,
            }),
        ));
    }
    if inner.len() == 3 && inner[1] == OsStr::new("subagents") && is_subagent_jsonl(path) {
        return Ok((
            Some(project_dir),
            Some(SessionLayout::Subagent),
            Some(ClaudeSessionKey {
                root_session_id: inner[0]
                    .to_str()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        ClaudeNativePathError::invalid(path, "session directory is not valid UTF-8")
                    })?
                    .to_owned(),
                workflow_run_id: None,
                agent_id: Some(utf8_file_stem(path)?.to_owned()),
            }),
        ));
    }
    if inner.len() == 5
        && inner[1] == OsStr::new("subagents")
        && inner[2] == OsStr::new("workflows")
        && is_subagent_jsonl(path)
    {
        return Ok((
            Some(project_dir),
            Some(SessionLayout::WorkflowSubagent),
            Some(ClaudeSessionKey {
                root_session_id: inner[0]
                    .to_str()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        ClaudeNativePathError::invalid(path, "session directory is not valid UTF-8")
                    })?
                    .to_owned(),
                workflow_run_id: Some(
                    inner[3]
                        .to_str()
                        .filter(|value| !value.trim().is_empty())
                        .ok_or_else(|| {
                            ClaudeNativePathError::invalid(
                                path,
                                "workflow directory is not valid UTF-8",
                            )
                        })?
                        .to_owned(),
                ),
                agent_id: Some(utf8_file_stem(path)?.to_owned()),
            }),
        ));
    }
    Ok((None, None, None))
}

fn discover_explicit_opened(
    path: &Path,
    opened: Arc<OpenedProviderSourceFile>,
) -> Result<DiscoveredClaudeSession, ClaudeNativePathError> {
    let (project_dir, layout, key) = explicit_claude_layout(path)?;
    discover_file_from_opened(project_dir, path.to_path_buf(), layout, key, opened)
}

fn discover_file_from_opened(
    project_dir: PathBuf,
    path: PathBuf,
    layout: SessionLayout,
    key: ClaudeSessionKey,
    opened: Arc<OpenedProviderSourceFile>,
) -> Result<DiscoveredClaudeSession, ClaudeNativePathError> {
    if !is_jsonl(&path) {
        return Err(ClaudeNativePathError::invalid(
            &path,
            "explicit Claude session files must use the .jsonl extension",
        ));
    }
    let fingerprint = ClaudeFileFingerprint::from_metadata(opened.metadata());
    opened
        .revalidate()
        .map_err(|error| map_capture_error(&path, error))?;
    Ok(DiscoveredClaudeSession {
        project_dir,
        canonical_path: path.clone(),
        path,
        key,
        layout,
        fingerprint,
        opened,
    })
}

fn explicit_claude_layout(
    path: &Path,
) -> Result<(PathBuf, SessionLayout, ClaudeSessionKey), ClaudeNativePathError> {
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
        let session_dir = parent
            .parent()
            .ok_or_else(|| ClaudeNativePathError::invalid(path, "incomplete subagent layout"))?;
        let project_dir = session_dir
            .parent()
            .ok_or_else(|| ClaudeNativePathError::invalid(path, "incomplete subagent layout"))?;
        return Ok((
            project_dir.to_path_buf(),
            SessionLayout::Subagent,
            ClaudeSessionKey {
                root_session_id: utf8_file_name(session_dir)?.to_owned(),
                workflow_run_id: None,
                agent_id: Some(file_stem),
            },
        ));
    }
    if parent
        .parent()
        .is_some_and(|value| value.file_name() == Some(OsStr::new("workflows")))
        && parent
            .parent()
            .and_then(Path::parent)
            .is_some_and(|value| value.file_name() == Some(OsStr::new("subagents")))
    {
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
        return Ok((
            project_dir.to_path_buf(),
            SessionLayout::WorkflowSubagent,
            ClaudeSessionKey {
                root_session_id: utf8_file_name(session_dir)?.to_owned(),
                workflow_run_id: Some(utf8_file_name(parent)?.to_owned()),
                agent_id: Some(file_stem),
            },
        ));
    }
    Ok((
        parent.to_path_buf(),
        SessionLayout::Primary,
        ClaudeSessionKey {
            root_session_id: file_stem,
            workflow_run_id: None,
            agent_id: None,
        },
    ))
}

fn map_capture_error(path: &Path, error: CaptureError) -> ClaudeNativePathError {
    match error {
        CaptureError::Io(source) => ClaudeNativePathError::io(path, source),
        other => ClaudeNativePathError::invalid(path, other.to_string()),
    }
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

pub(super) fn open_discovered_file(
    source: &DiscoveredClaudeSession,
) -> Result<File, ClaudeNativePathError> {
    source
        .opened
        .revalidate()
        .map_err(|_| ClaudeNativePathError::StaleDiscovery {
            path: source.path.clone(),
        })?;
    let file = source
        .opened
        .file()
        .try_clone()
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
    if source.opened.revalidate().is_err() || &open_fingerprint != expected {
        return Err(ClaudeNativePathError::SourceChanged {
            path: source.path.clone(),
        });
    }
    Ok(())
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
