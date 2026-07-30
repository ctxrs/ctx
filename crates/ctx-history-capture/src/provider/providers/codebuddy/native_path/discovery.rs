use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

use crate::provider::source_backed::family::document::{
    CompleteDocumentTree, DocumentLeafFingerprint, ObservedDocumentLeaf,
};

use super::*;

const CATALOG_MAX_DEPTH: usize = 16;
const CATALOG_MAX_ENTRIES: usize = 16_384;
const CATALOG_MAX_PATH_BYTES: usize = 4 * 1024;
const LEAF_DOMAIN: &[u8] = b"ctx.codebuddy.document-leaf.v1\0";
const TREE_DOMAIN: &[u8] = b"ctx.codebuddy.document-tree.v1\0";

#[cfg(test)]
std::thread_local! {
    static BODY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_body_reads() {
    BODY_READS.with(|reads| reads.set(0));
}

#[cfg(test)]
pub(super) fn body_reads() -> usize {
    BODY_READS.with(std::cell::Cell::get)
}

pub(super) fn note_body_read() {
    #[cfg(test)]
    BODY_READS.with(|reads| reads.set(reads.get().saturating_add(1)));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogSelection {
    ExactFile { inventory_parent: bool },
    Directory,
}

impl CatalogSelection {
    fn tag(self) -> u8 {
        match self {
            Self::ExactFile {
                inventory_parent: false,
            } => 1,
            Self::ExactFile {
                inventory_parent: true,
            } => 2,
            Self::Directory => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteKind {
    File,
    Directory,
}

impl RouteKind {
    fn tag(self) -> u8 {
        match self {
            Self::File => 1,
            Self::Directory => 2,
        }
    }
}

#[derive(Debug, Clone)]
struct CatalogRoute {
    relative_path: PathBuf,
    display_path: PathBuf,
    kind: RouteKind,
    authority_fingerprint: [u8; 32],
    frozen: Option<CodeBuddyFrozenFile>,
}

impl CatalogRoute {
    fn observed_file(&self) -> Option<CodeBuddyObservedFile> {
        Some(CodeBuddyObservedFile {
            relative_path: self.relative_path.clone(),
            display_path: self.display_path.clone(),
            frozen: self.frozen.clone()?,
            authority_fingerprint: self.authority_fingerprint,
        })
    }
}

#[derive(Debug)]
pub(super) struct CodeBuddyDocumentLeaf {
    pub(super) source: SourceKey,
    pub(super) session_ordinal: usize,
    pub(super) kind: DocumentLeafKind,
}

#[derive(Debug)]
pub(super) enum DocumentLeafKind {
    Cli {
        selected: CodeBuddyObservedFile,
        aliases: Vec<PathBuf>,
    },
    Extension {
        session_dir: PathBuf,
        session_index: CodeBuddyObservedFile,
        project_index: Option<CodeBuddyObservedFile>,
        messages: BTreeMap<String, CodeBuddyObservedFile>,
    },
}

impl CodeBuddyDocumentLeaf {
    pub(super) fn logical_path(&self) -> &Path {
        match &self.kind {
            DocumentLeafKind::Cli { selected, .. } => &selected.display_path,
            DocumentLeafKind::Extension { session_dir, .. } => session_dir,
        }
    }

    fn aliases(&self) -> impl Iterator<Item = &Path> {
        let paths = match &self.kind {
            DocumentLeafKind::Cli { aliases, .. } => aliases.as_slice(),
            DocumentLeafKind::Extension { session_dir, .. } => std::slice::from_ref(session_dir),
        };
        paths.iter().map(PathBuf::as_path)
    }
}

#[derive(Debug)]
pub(super) struct CodeBuddyTreeAuthority {
    root: ProviderSourceRoot,
    selected_path: PathBuf,
    selected_relative_path: PathBuf,
    selection: CatalogSelection,
    routes: Vec<CatalogRoute>,
}

impl CodeBuddyTreeAuthority {
    #[cfg(test)]
    pub(super) fn retained_handles(&self) -> usize {
        1
    }

    #[cfg(test)]
    pub(super) fn route_count(&self) -> usize {
        self.routes.len()
    }
}

pub(super) type CodeBuddyDocumentTree =
    CompleteDocumentTree<CodeBuddyDocumentLeaf, CodeBuddyTreeAuthority>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodeBuddyInventoryStatus {
    Complete,
    Unavailable,
}

pub(super) struct CodeBuddyInventory {
    pub(super) status: CodeBuddyInventoryStatus,
    tree: Option<CodeBuddyDocumentTree>,
}

impl CodeBuddyInventory {
    pub(super) fn into_complete_tree(self) -> Option<CodeBuddyDocumentTree> {
        self.tree
    }
}

pub(super) fn discover_codebuddy_tree(selected: &Path) -> Result<CodeBuddyInventory> {
    let selected_path = absolute_path(selected)?;
    validate_path(&selected_path)?;
    let opened = match open_provider_source_path(&selected_path) {
        Ok(opened) => opened,
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(CodeBuddyInventory {
                status: CodeBuddyInventoryStatus::Unavailable,
                tree: None,
            });
        }
        Err(error) => return Err(error),
    };
    let (root, relative_path, selection, opening) = match opened {
        OpenedProviderSourcePath::File(file) => {
            let name = selected_path
                .file_name()
                .map(PathBuf::from)
                .ok_or_else(|| invalid(&selected_path, "selected file has no name"))?;
            let parent = selected_path
                .parent()
                .ok_or_else(|| invalid(&selected_path, "selected file has no parent"))?;
            let opening = file.authority_fingerprint();
            let inventory_parent =
                selected_path.file_name().and_then(OsStr::to_str) == Some("index.json");
            drop(file);
            (
                ProviderSourceRoot::open(parent)?,
                name,
                CatalogSelection::ExactFile { inventory_parent },
                opening,
            )
        }
        OpenedProviderSourcePath::Directory(directory) => {
            let opening = directory.authority_fingerprint();
            let root = directory.authority_root();
            drop(directory);
            (root, PathBuf::new(), CatalogSelection::Directory, opening)
        }
    };
    let tree = complete_tree(selected_path, relative_path.clone(), selection, root)?;
    let observed = tree
        .authority
        .routes
        .iter()
        .find(|route| route.relative_path == relative_path)
        .map(|route| route.authority_fingerprint);
    if observed != Some(opening) {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(CodeBuddyInventory {
        status: CodeBuddyInventoryStatus::Complete,
        tree: Some(tree),
    })
}

pub(super) fn revalidate_codebuddy_tree(tree: &CodeBuddyDocumentTree) -> Result<[u8; 32]> {
    let current = complete_tree(
        tree.authority.selected_path.clone(),
        tree.authority.selected_relative_path.clone(),
        tree.authority.selection,
        tree.authority.root.clone(),
    )?;
    Ok(current.tree_fingerprint)
}

fn complete_tree(
    selected_path: PathBuf,
    selected_relative_path: PathBuf,
    selection: CatalogSelection,
    root: ProviderSourceRoot,
) -> Result<CodeBuddyDocumentTree> {
    let mut state = DiscoveryState {
        routes: Vec::new(),
        entries: 0,
    };
    match selection {
        CatalogSelection::Directory
        | CatalogSelection::ExactFile {
            inventory_parent: true,
        } => discover_directory(&root, root.directory()?, 0, &mut state)?,
        CatalogSelection::ExactFile {
            inventory_parent: false,
        } => {
            let directory = root.directory()?;
            observe_directory(&root, &directory, &mut state)?;
            directory.revalidate()?;
            state.entries = 1;
            admit_file(
                &root,
                selected_relative_path.clone(),
                root.open_file(&selected_relative_path)?,
                &mut state,
            )?;
        }
    }
    root.revalidate()?;
    state
        .routes
        .sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let leaves = logical_leaves(
        &selected_path,
        &selected_relative_path,
        selection,
        &state.routes,
    )?;
    let tree_fingerprint =
        tree_fingerprint(selection, &selected_relative_path, &state.routes, &leaves);
    Ok(CompleteDocumentTree::new(
        tree_fingerprint,
        leaves,
        CodeBuddyTreeAuthority {
            root,
            selected_path,
            selected_relative_path,
            selection,
            routes: state.routes,
        },
    ))
}

struct DiscoveryState {
    routes: Vec<CatalogRoute>,
    entries: usize,
}

fn discover_directory(
    root: &ProviderSourceRoot,
    directory: ProviderSourceDirectory,
    depth: usize,
    state: &mut DiscoveryState,
) -> Result<()> {
    if depth > CATALOG_MAX_DEPTH {
        return Err(invalid(
            &root.named_path().join(directory.relative_path()),
            "CodeBuddy source tree exceeds its depth bound",
        ));
    }
    observe_directory(root, &directory, state)?;
    let remaining = CATALOG_MAX_ENTRIES.saturating_sub(state.entries);
    let names = directory.entries(remaining.saturating_add(1))?;
    state.entries = state.entries.saturating_add(names.len());
    if state.entries > CATALOG_MAX_ENTRIES {
        return Err(invalid(
            &root.named_path().join(directory.relative_path()),
            "CodeBuddy source tree exceeds its entry bound",
        ));
    }
    for name in names {
        let relative_path = directory.relative_path().join(&name);
        validate_path(&root.named_path().join(&relative_path))?;
        match directory.open_child(&name)? {
            OpenedProviderSourcePath::Directory(child) => {
                discover_directory(root, child, depth.saturating_add(1), state)?;
            }
            OpenedProviderSourcePath::File(file) => {
                admit_file(root, relative_path, file, state)?;
            }
        }
    }
    directory.revalidate()
}

fn observe_directory(
    root: &ProviderSourceRoot,
    directory: &ProviderSourceDirectory,
    state: &mut DiscoveryState,
) -> Result<()> {
    state.routes.push(CatalogRoute {
        relative_path: directory.relative_path().to_path_buf(),
        display_path: root.named_path().join(directory.relative_path()),
        kind: RouteKind::Directory,
        authority_fingerprint: directory.authority_fingerprint(),
        frozen: None,
    });
    Ok(())
}

fn admit_file(
    root: &ProviderSourceRoot,
    relative_path: PathBuf,
    file: OpenedProviderSourceFile,
    state: &mut DiscoveryState,
) -> Result<()> {
    let display_path = root.named_path().join(&relative_path);
    let frozen = CodeBuddyFrozenFile::from_metadata(file.metadata())?;
    state.routes.push(CatalogRoute {
        relative_path,
        display_path,
        kind: RouteKind::File,
        authority_fingerprint: file.authority_fingerprint(),
        frozen: Some(frozen),
    });
    Ok(())
}

fn logical_leaves(
    selected_path: &Path,
    selected_relative_path: &Path,
    selection: CatalogSelection,
    routes: &[CatalogRoute],
) -> Result<Vec<ObservedDocumentLeaf<CodeBuddyDocumentLeaf>>> {
    let by_path = routes
        .iter()
        .map(|route| (route.relative_path.clone(), route))
        .collect::<BTreeMap<_, _>>();
    let mut extension_dirs =
        extension_session_dirs(selected_path, selected_relative_path, selection, &by_path);
    extension_dirs.sort();
    let mut leaves = Vec::new();
    for (index, session_relative) in extension_dirs.into_iter().enumerate() {
        leaves.push(extension_leaf(
            session_relative,
            index.saturating_add(1),
            routes,
            &by_path,
        )?);
    }

    let extension_count = leaves.len();
    let mut physical_cli = BTreeMap::<[u8; 32], Vec<&CatalogRoute>>::new();
    for route in cli_routes(
        selected_path,
        selected_relative_path,
        selection,
        routes,
        &by_path,
    ) {
        physical_cli
            .entry(route.authority_fingerprint)
            .or_default()
            .push(route);
    }
    for (index, mut aliases) in physical_cli.into_values().enumerate() {
        aliases.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        let selected = aliases[0]
            .observed_file()
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy CLI route lost its file observation",
            ))?;
        let source = source_backed::codebuddy_source_key_for_path(
            CodeBuddySourceShape::Cli,
            &selected.display_path,
        )?;
        let fingerprint = cli_fingerprint(&source, &selected);
        let aliases = aliases
            .into_iter()
            .map(|route| route.display_path.clone())
            .collect();
        leaves.push(ObservedDocumentLeaf::new(
            fingerprint,
            CodeBuddyDocumentLeaf {
                source,
                session_ordinal: extension_count.saturating_add(index).saturating_add(1),
                kind: DocumentLeafKind::Cli { selected, aliases },
            },
        ));
    }
    leaves.sort_by(|left, right| {
        left.provider_leaf
            .logical_path()
            .cmp(right.provider_leaf.logical_path())
    });
    Ok(leaves)
}

fn extension_leaf(
    session_relative: PathBuf,
    session_ordinal: usize,
    routes: &[CatalogRoute],
    by_path: &BTreeMap<PathBuf, &CatalogRoute>,
) -> Result<ObservedDocumentLeaf<CodeBuddyDocumentLeaf>> {
    let session_index = required_file(&session_relative.join("index.json"), by_path)?;
    let project_index_path = session_relative
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join("index.json");
    let project_index = by_path
        .get(&project_index_path)
        .filter(|route| route.kind == RouteKind::File)
        .and_then(|route| route.observed_file());
    let messages_relative = session_relative.join("messages");
    let mut messages = BTreeMap::new();
    for route in routes.iter().filter(|route| {
        route.kind == RouteKind::File
            && route.relative_path.parent() == Some(messages_relative.as_path())
    }) {
        if let Some(id) = route
            .relative_path
            .file_stem()
            .and_then(OsStr::to_str)
            .filter(|id| provider_safe_path_segment(id))
        {
            messages.insert(
                id.to_owned(),
                route.observed_file().ok_or(CaptureError::SystemInvariant(
                    "CodeBuddy message route lost its file observation",
                ))?,
            );
        }
    }
    let session_dir = session_index
        .display_path
        .parent()
        .ok_or_else(|| invalid(&session_index.display_path, "session index has no parent"))?
        .to_path_buf();
    let source = source_backed::codebuddy_source_key_for_path(
        CodeBuddySourceShape::Extension,
        &session_dir,
    )?;
    let fingerprint =
        extension_fingerprint(&source, &session_relative, &project_index_path, routes);
    Ok(ObservedDocumentLeaf::new(
        fingerprint,
        CodeBuddyDocumentLeaf {
            source,
            session_ordinal,
            kind: DocumentLeafKind::Extension {
                session_dir,
                session_index,
                project_index,
                messages,
            },
        },
    ))
}

fn extension_session_dirs(
    selected_path: &Path,
    selected_relative_path: &Path,
    selection: CatalogSelection,
    routes: &BTreeMap<PathBuf, &CatalogRoute>,
) -> Vec<PathBuf> {
    let mut sessions = BTreeSet::new();
    let root = PathBuf::new();
    let exact_index = matches!(
        selection,
        CatalogSelection::ExactFile {
            inventory_parent: true
        }
    ) && selected_relative_path.file_name().and_then(OsStr::to_str)
        == Some("index.json");
    if exact_index {
        if is_session(&root, routes) {
            sessions.insert(root);
        } else {
            insert_project_sessions(Path::new(""), routes, &mut sessions);
        }
        return sessions.into_iter().collect();
    }
    if selection != CatalogSelection::Directory {
        return Vec::new();
    }
    if is_session(&root, routes) {
        sessions.insert(root);
        return sessions.into_iter().collect();
    }
    insert_project_sessions(Path::new(""), routes, &mut sessions);
    if selected_path.file_name().and_then(OsStr::to_str) == Some("history") {
        insert_history_sessions(Path::new(""), routes, &mut sessions);
    } else {
        for route in routes.values() {
            if route.kind == RouteKind::Directory
                && route.relative_path.file_name().and_then(OsStr::to_str) == Some("history")
                && route.relative_path.components().count() <= 9
            {
                insert_history_sessions(&route.relative_path, routes, &mut sessions);
            }
        }
    }
    sessions.into_iter().collect()
}

fn insert_project_sessions(
    project: &Path,
    routes: &BTreeMap<PathBuf, &CatalogRoute>,
    sessions: &mut BTreeSet<PathBuf>,
) {
    for child in direct_child_directories(project, routes) {
        if is_session(&child, routes) {
            sessions.insert(child);
        }
    }
}

fn insert_history_sessions(
    history: &Path,
    routes: &BTreeMap<PathBuf, &CatalogRoute>,
    sessions: &mut BTreeSet<PathBuf>,
) {
    for project in direct_child_directories(history, routes) {
        insert_project_sessions(&project, routes, sessions);
    }
}

fn direct_child_directories(
    parent: &Path,
    routes: &BTreeMap<PathBuf, &CatalogRoute>,
) -> Vec<PathBuf> {
    routes
        .values()
        .filter(|route| {
            route.kind == RouteKind::Directory
                && !route.relative_path.as_os_str().is_empty()
                && route.relative_path.parent() == Some(parent)
        })
        .map(|route| route.relative_path.clone())
        .collect()
}

fn is_session(directory: &Path, routes: &BTreeMap<PathBuf, &CatalogRoute>) -> bool {
    route_is(&directory.join("index.json"), RouteKind::File, routes)
        && route_is(&directory.join("messages"), RouteKind::Directory, routes)
}

fn cli_routes<'a>(
    selected_path: &Path,
    selected_relative_path: &Path,
    selection: CatalogSelection,
    routes: &'a [CatalogRoute],
    by_path: &BTreeMap<PathBuf, &'a CatalogRoute>,
) -> Vec<&'a CatalogRoute> {
    if matches!(selection, CatalogSelection::ExactFile { .. }) {
        return by_path
            .get(selected_relative_path)
            .copied()
            .filter(|route| {
                route.kind == RouteKind::File
                    && route.relative_path.extension().and_then(OsStr::to_str) == Some("jsonl")
            })
            .into_iter()
            .collect();
    }
    let scan_root = if route_is(Path::new("projects"), RouteKind::Directory, by_path) {
        Some(PathBuf::from("projects"))
    } else if selected_path.file_name().and_then(OsStr::to_str) == Some("projects")
        || selected_path
            .parent()
            .and_then(Path::file_name)
            .and_then(OsStr::to_str)
            == Some("projects")
    {
        Some(PathBuf::new())
    } else {
        None
    };
    let Some(scan_root) = scan_root else {
        return Vec::new();
    };
    routes
        .iter()
        .filter(|route| {
            route.kind == RouteKind::File
                && route.relative_path.starts_with(&scan_root)
                && route.relative_path.extension().and_then(OsStr::to_str) == Some("jsonl")
        })
        .collect()
}

fn route_is(path: &Path, kind: RouteKind, routes: &BTreeMap<PathBuf, &CatalogRoute>) -> bool {
    routes.get(path).is_some_and(|route| route.kind == kind)
}

fn required_file(
    path: &Path,
    routes: &BTreeMap<PathBuf, &CatalogRoute>,
) -> Result<CodeBuddyObservedFile> {
    routes
        .get(path)
        .filter(|route| route.kind == RouteKind::File)
        .and_then(|route| route.observed_file())
        .ok_or(CaptureError::SourceChangedDuringCapture)
}

fn cli_fingerprint(source: &SourceKey, file: &CodeBuddyObservedFile) -> DocumentLeafFingerprint {
    let mut digest = Sha256::new();
    digest.update(LEAF_DOMAIN);
    digest.update(source.exact_descriptor_digest());
    hash_path(&mut digest, &file.relative_path);
    digest.update(file.authority_fingerprint);
    DocumentLeafFingerprint::new(digest.finalize().into())
}

fn extension_fingerprint(
    source: &SourceKey,
    session_relative: &Path,
    project_index_path: &Path,
    routes: &[CatalogRoute],
) -> DocumentLeafFingerprint {
    let session_index = session_relative.join("index.json");
    let messages = session_relative.join("messages");
    let mut digest = Sha256::new();
    digest.update(LEAF_DOMAIN);
    digest.update(source.exact_descriptor_digest());
    for route in routes.iter().filter(|route| {
        route.relative_path == session_relative
            || route.relative_path == session_index
            || route.relative_path == project_index_path
            || route.relative_path == messages
            || route.relative_path.starts_with(&messages)
    }) {
        digest.update([route.kind.tag()]);
        hash_path(&mut digest, &route.relative_path);
        digest.update(route.authority_fingerprint);
    }
    DocumentLeafFingerprint::new(digest.finalize().into())
}

fn tree_fingerprint(
    selection: CatalogSelection,
    selected_relative_path: &Path,
    routes: &[CatalogRoute],
    leaves: &[ObservedDocumentLeaf<CodeBuddyDocumentLeaf>],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(TREE_DOMAIN);
    digest.update([selection.tag()]);
    hash_path(&mut digest, selected_relative_path);
    for route in routes {
        digest.update([route.kind.tag()]);
        hash_path(&mut digest, &route.relative_path);
        digest.update(route.authority_fingerprint);
    }
    for leaf in leaves {
        digest.update(leaf.fingerprint.as_bytes());
        digest.update(leaf.provider_leaf.source.exact_descriptor_digest());
        for alias in leaf.provider_leaf.aliases() {
            hash_path(&mut digest, alias);
        }
    }
    digest.finalize().into()
}

pub(super) fn open_codebuddy_source(
    authority: &CodeBuddyTreeAuthority,
    leaf: &CodeBuddyDocumentLeaf,
) -> Result<CodeBuddySource> {
    match &leaf.kind {
        DocumentLeafKind::Cli { selected, .. } => {
            let primary = open_observed_file(&authority.root, selected)?;
            let revision = selected
                .frozen
                .source_revision_with_policy("cli-jsonl", CODEBUDDY_CLI_POLICY_REVISION);
            Ok(CodeBuddySource {
                shape: CodeBuddySourceShape::Cli,
                path: selected.display_path.clone(),
                canonical_path: selected.display_path.clone(),
                source_revision: revision,
                session_ordinal: leaf.session_ordinal,
                frozen: Some(selected.frozen.clone()),
                capability: Some(Arc::new(CodeBuddyCapabilitySource {
                    authority: authority.root.clone(),
                    primary: Some(primary),
                    extension: None,
                })),
            })
        }
        DocumentLeafKind::Extension {
            session_dir,
            session_index,
            project_index,
            messages,
        } => {
            let session_bytes = read_observed_file(
                &authority.root,
                session_index,
                MAX_PROVIDER_JSONL_LINE_BYTES,
            )?;
            let project_bytes = match project_index {
                Some(project) if project.relative_path == session_index.relative_path => {
                    Some(session_bytes.clone())
                }
                Some(project) => Some(read_observed_file(
                    &authority.root,
                    project,
                    MAX_PROVIDER_JSONL_LINE_BYTES,
                )?),
                None => None,
            };
            let metadata = codebuddy_extension_metadata_from_admitted(
                session_dir,
                &session_bytes,
                project_bytes.as_deref(),
            )?;
            let mut revision = CodeBuddyRevisionHasher::new();
            revision.update(b"codebuddy-extension-capability-v1");
            revision.update(&session_bytes);
            match project_bytes.as_deref() {
                Some(bytes) => {
                    revision.update(b"project-index");
                    revision.update(bytes);
                }
                None => revision.update(b"missing-project-index"),
            }
            let mut admitted = BTreeMap::new();
            for message_ref in metadata.messages() {
                serde_json::to_writer(&mut revision, message_ref)?;
                let Some(message_id) = message_ref
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| provider_safe_path_segment(id))
                else {
                    revision.update(b"rejected-message-id");
                    continue;
                };
                let file = messages
                    .get(message_id)
                    .ok_or(CaptureError::SourceChangedDuringCapture)?;
                file.frozen.update_revision(&mut revision);
                admitted.insert(message_id.to_owned(), file.clone());
            }
            Ok(CodeBuddySource {
                shape: CodeBuddySourceShape::Extension,
                path: session_dir.clone(),
                canonical_path: session_dir.clone(),
                source_revision: format!(
                    "codebuddy-extension-capability-v1:fnv1a64:{:016x}",
                    revision.finish()
                ),
                session_ordinal: leaf.session_ordinal,
                frozen: None,
                capability: Some(Arc::new(CodeBuddyCapabilitySource {
                    authority: authority.root.clone(),
                    primary: None,
                    extension: Some(CodeBuddyExtensionCapability {
                        metadata,
                        messages: admitted,
                    }),
                })),
            })
        }
    }
}

pub(super) fn read_observed_file(
    root: &ProviderSourceRoot,
    expected: &CodeBuddyObservedFile,
    maximum_bytes: usize,
) -> Result<Vec<u8>> {
    note_body_read();
    open_observed_file(root, expected)?.read_all_bounded(maximum_bytes)
}

fn open_observed_file(
    root: &ProviderSourceRoot,
    expected: &CodeBuddyObservedFile,
) -> Result<OpenedProviderSourceFile> {
    let opened = root.open_file(&expected.relative_path)?;
    let frozen = CodeBuddyFrozenFile::from_metadata(opened.metadata())?;
    if opened.authority_fingerprint() != expected.authority_fingerprint || frozen != expected.frozen
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(opened)
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if absolute
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(invalid(
            &absolute,
            "CodeBuddy selected paths must be traversal-free",
        ));
    }
    Ok(absolute)
}

fn validate_path(path: &Path) -> Result<()> {
    if path.as_os_str().as_encoded_bytes().len() > CATALOG_MAX_PATH_BYTES {
        return Err(invalid(
            path,
            "CodeBuddy source path exceeds its byte bound",
        ));
    }
    if path.to_str().is_none() {
        return Err(invalid(path, "CodeBuddy source paths must be valid UTF-8"));
    }
    Ok(())
}

fn hash_path(digest: &mut Sha256, path: &Path) {
    let bytes = path.as_os_str().as_encoded_bytes();
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

fn invalid(path: &Path, reason: &'static str) -> CaptureError {
    CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason,
    }
}
