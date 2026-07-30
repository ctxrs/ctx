use std::{
    collections::BTreeMap,
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

use crate::provider::source_backed::family::document::{
    CompleteDocumentTree, DocumentLeafFingerprint, ObservedDocumentLeaf,
};

use super::*;

mod catalog;
mod index;
mod inspection;

use catalog::{catalog_routes, CatalogRoute, CatalogSelection};
use index::DiscoveryIndex;
use inspection::{logical_leaves, tree_fingerprint};

const CATALOG_MAX_DEPTH: usize = 16;
const CATALOG_MAX_ENTRIES: usize = 16_384;
const CATALOG_MAX_PATH_BYTES: usize = 4 * 1024;
const LEAF_DOMAIN: &[u8] = b"ctx.codebuddy.document-leaf.v1\0";
const TREE_DOMAIN: &[u8] = b"ctx.codebuddy.document-tree.v1\0";

#[cfg(test)]
std::thread_local! {
    static BODY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static DISCOVERY_OPERATIONS: std::cell::Cell<[usize; 3]> =
        const { std::cell::Cell::new([0; 3]) };
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DiscoveryOperationCounts {
    pub(super) route_inspections: usize,
    pub(super) indexed_path_lookups: usize,
    pub(super) indexed_parent_lookups: usize,
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

#[cfg(test)]
pub(super) fn reset_discovery_operations() {
    DISCOVERY_OPERATIONS.with(|operations| operations.set([0; 3]));
}

#[cfg(test)]
pub(super) fn discovery_operations() -> DiscoveryOperationCounts {
    let [route_inspections, indexed_path_lookups, indexed_parent_lookups] =
        DISCOVERY_OPERATIONS.with(std::cell::Cell::get);
    DiscoveryOperationCounts {
        route_inspections,
        indexed_path_lookups,
        indexed_parent_lookups,
    }
}

#[inline(always)]
fn note_discovery_operation(operation: usize) {
    #[cfg(test)]
    DISCOVERY_OPERATIONS.with(|operations| {
        let mut current = operations.get();
        current[operation] = current[operation].saturating_add(1);
        operations.set(current);
    });
    #[cfg(not(test))]
    let _ = operation;
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
    index: DiscoveryIndex,
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

    #[cfg(test)]
    pub(super) fn indexed_extension_fingerprints_match_full_scan(
        &self,
        leaves: &[ObservedDocumentLeaf<CodeBuddyDocumentLeaf>],
    ) -> bool {
        leaves.iter().all(|leaf| {
            let DocumentLeafKind::Extension { session_index, .. } = &leaf.provider_leaf.kind else {
                return true;
            };
            let session = session_index
                .relative_path
                .parent()
                .unwrap_or_else(|| Path::new(""));
            let project_index = session
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .join("index.json");
            inspection::full_scan_extension_fingerprint(
                &leaf.provider_leaf.source,
                session,
                &project_index,
                &self.routes,
            ) == leaf.fingerprint
        })
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
        .index
        .route(&relative_path, &tree.authority.routes)
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
    let routes = catalog_routes(&root, &selected_relative_path, selection)?;
    let index = DiscoveryIndex::new(&routes)?;
    let leaves = logical_leaves(
        &selected_path,
        &selected_relative_path,
        selection,
        &routes,
        &index,
    )?;
    let tree_fingerprint = tree_fingerprint(selection, &selected_relative_path, &routes, &leaves);
    Ok(CompleteDocumentTree::new(
        tree_fingerprint,
        leaves,
        CodeBuddyTreeAuthority {
            root,
            selected_path,
            selected_relative_path,
            selection,
            routes,
            index,
        },
    ))
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
