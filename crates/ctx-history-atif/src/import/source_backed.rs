//! Replacement-document-tree source-backed adapter for ATIF v1.0-v1.8 files and directories.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet, VecDeque},
    io,
    marker::PhantomData,
    path::{Component, Path, PathBuf},
};

use ctx_history_capture_runtime::{
    ChangedDocumentSink, CompleteDocumentTree, DocumentLeafExecutionPolicy,
    DocumentLeafFingerprint, DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
    SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
};
use ctx_history_core::{
    AgentScope, CaptureProvider, ProviderNativeSessionRelationship, ScannedSourceCounts,
    SourceAnchor, SourceKey, SourceObservation, StableEntityId,
};
use ctx_history_provider_runtime::ProviderRuntimeBinding;
use ctx_history_source_io::{
    is_non_regular_source_rejection, is_symlink_source_rejection, open_provider_source_path,
    OpenedProviderSourceFile, OpenedProviderSourcePath, ProviderSourceDirectory,
    ProviderSourceRoot, SourceIoError,
};
use sha2::{Digest, Sha256};

use crate::{
    schema::{SubagentTrajectoryRef, Trajectory},
    ATIF_SOURCE_FORMAT,
};

use super::{
    has_schema_version_field, parse_atif_bytes,
    projection::{
        derive_atif_document_session_id, project_trajectory_to_core_records, AtifLineageLink,
        AtifProjectionInput,
    },
    AtifImportError,
};

pub const ATIF_SOURCE_SCHEMA_VARIANT: &str = "atif-v1-source-backed-v1";
pub const ATIF_PARSER_REVISION: &str = "atif-v1-source-backed-v1";
const ATIF_SOURCE_IDENTITY_VERSION: u32 = 1;
const ATIF_SOURCE_REVISION_KIND: &str = "atif-ordinary-tree-observation-v1";

pub const ATIF_MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
pub const ATIF_MAX_DISCOVERED_FILES: usize = 10_000;
pub const ATIF_MAX_DIRECTORY_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedAtifFile {
    relative_path: PathBuf,
    relative_key: String,
    len: u64,
    ordinary_token: [u8; 32],
    authority_fingerprint: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtifDocumentLeaf {
    files: Vec<ObservedAtifFile>,
    tree_fingerprint: [u8; 32],
}

#[derive(Debug)]
pub struct AtifTreeAuthority {
    root: ProviderSourceRoot,
    is_single_file: bool,
    primary_file: Option<PathBuf>,
    files: Vec<ObservedAtifFile>,
}

impl AtifTreeAuthority {
    fn open_and_verify(&self, file: &ObservedAtifFile) -> SourceBackedRouteResult<Vec<u8>> {
        let opened = self
            .root
            .open_file(&file.relative_path)
            .map_err(io_route_error)?;
        let token = opened.ordinary_file_token();
        let auth = opened.authority_fingerprint();
        if opened.len() != file.len
            || token != file.ordinary_token
            || auth != file.authority_fingerprint
        {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::SourceChanged,
                format!("ATIF file `{}` changed during capture", file.relative_key),
            ));
        }
        let bytes = opened
            .read_all_bounded(ATIF_MAX_FILE_BYTES)
            .map_err(io_route_error)?;
        if self.is_single_file {
            opened
                .revalidate_leaf_with_same_object_root()
                .map_err(io_route_error)?;
        } else {
            opened.revalidate().map_err(io_route_error)?;
        }
        if u64::try_from(bytes.len()).ok() != Some(file.len) {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::SourceChanged,
                format!(
                    "ATIF file `{}` length changed during read",
                    file.relative_key
                ),
            ));
        }
        Ok(bytes)
    }

    fn revalidate_all(&self) -> SourceBackedRouteResult<[u8; 32]> {
        if self.is_single_file {
            self.root.revalidate_same_object().map_err(io_route_error)?;
        } else {
            self.root.revalidate().map_err(io_route_error)?;
        }
        for file in &self.files {
            let opened = self
                .root
                .open_file(&file.relative_path)
                .map_err(io_route_error)?;
            if opened.len() != file.len
                || opened.ordinary_file_token() != file.ordinary_token
                || opened.authority_fingerprint() != file.authority_fingerprint
            {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    format!(
                        "ATIF file `{}` changed before publication commit",
                        file.relative_key
                    ),
                ));
            }
            if self.is_single_file {
                opened
                    .revalidate_leaf_with_same_object_root()
                    .map_err(io_route_error)?;
            } else {
                opened.revalidate().map_err(io_route_error)?;
            }
        }
        Ok(compute_tree_fingerprint(
            self.is_single_file,
            self.root.authority_fingerprint(),
            &self.files,
        ))
    }
}

type AtifDocumentTree = CompleteDocumentTree<AtifDocumentLeaf, AtifTreeAuthority>;

/// Explicit source-backed document-tree adapter for one ATIF file or directory.
#[derive(Debug, Clone)]
pub struct AtifDocumentTreeAdapter<B = ()> {
    path: PathBuf,
    source: SourceKey,
    _binding: PhantomData<fn() -> B>,
}

impl<B> AtifDocumentTreeAdapter<B> {
    pub fn new(
        path: impl Into<PathBuf>,
        catalog_lineage: [u8; 32],
    ) -> Result<Self, ctx_history_core::ProjectionContractError> {
        let source = SourceKey::derive(
            CaptureProvider::Custom.as_str(),
            ATIF_SOURCE_FORMAT,
            ATIF_SOURCE_SCHEMA_VARIANT,
            ATIF_SOURCE_IDENTITY_VERSION,
            SourceAnchor::CatalogLineage(catalog_lineage),
        )?;
        Ok(Self {
            path: path.into(),
            source,
            _binding: PhantomData,
        })
    }

    pub fn source_key(&self) -> &SourceKey {
        &self.source
    }
}

impl<B> ReplacementDocumentTree for AtifDocumentTreeAdapter<B>
where
    B: ProviderRuntimeBinding,
{
    type Lifecycle = B::CaptureLifecycleSink;
    type Spool = B::DocumentRecordSpool;
    type RouteControl = ctx_history_provider_runtime::ProviderRouteControlExpectation;
    type Leaf = AtifDocumentLeaf;
    type TreeAuthority = AtifTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        ATIF_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        self.source.exact_descriptor_eq(source)
    }

    fn leaf_execution_policy(&self) -> DocumentLeafExecutionPolicy {
        DocumentLeafExecutionPolicy::Serial
    }

    fn durable_replay_source(
        &self,
        _authority: &Self::TreeAuthority,
        _leaf: &Self::Leaf,
    ) -> SourceBackedRouteResult<Option<SourceKey>> {
        Ok(Some(self.source.clone()))
    }

    fn discover_complete(&self) -> SourceBackedRouteResult<AtifDocumentTree> {
        discover_atif_tree(&self.path)
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_, Self::Lifecycle, Self::Spool>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        scan_atif_tree(&self.source, authority, leaf, sink)
    }

    fn revalidate_complete(&self, tree: &AtifDocumentTree) -> SourceBackedRouteResult<[u8; 32]> {
        tree.authority.revalidate_all()
    }
}

fn discover_atif_tree(path: &Path) -> SourceBackedRouteResult<AtifDocumentTree> {
    let absolute = std::path::absolute(path).map_err(|error| {
        SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::InvalidSource,
            format!("resolve ATIF path {}: {error}", path.display()),
        )
    })?;
    let opened = open_provider_source_path(&absolute).map_err(io_route_error)?;
    match opened {
        OpenedProviderSourcePath::File(file) => {
            drop(file);
            discover_single_atif_file(&absolute)
        }
        OpenedProviderSourcePath::Directory(directory) => {
            drop(directory);
            discover_atif_directory(&absolute)
        }
    }
}

fn discover_single_atif_file(path: &Path) -> SourceBackedRouteResult<AtifDocumentTree> {
    if !is_json_extension(path) {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::InvalidSource,
            format!(
                "ATIF source file must have a .json extension: {}",
                path.display()
            ),
        ));
    }
    let file_name = path.file_name().ok_or_else(|| {
        SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::InvalidSource,
            format!("ATIF source file has no filename: {}", path.display()),
        )
    })?;
    let parent = path.parent().ok_or_else(|| {
        SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::InvalidSource,
            format!(
                "ATIF source file has no parent directory: {}",
                path.display()
            ),
        )
    })?;
    let root = ProviderSourceRoot::open(parent).map_err(io_route_error)?;
    let primary_rel = PathBuf::from(file_name);

    let mut visited = BTreeSet::new();
    let mut queue = VecDeque::new();
    let mut files = Vec::new();
    queue.push_back((primary_rel.clone(), true));

    while let Some((rel, required)) = queue.pop_front() {
        let key = path_to_posix_key(&rel);
        if !visited.insert(key.clone()) {
            continue;
        }
        if files.len() >= ATIF_MAX_DISCOVERED_FILES {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::InvalidSource,
                format!("ATIF source exceeds maximum of {ATIF_MAX_DISCOVERED_FILES} files"),
            ));
        }
        let opened = match root.open_file(&rel) {
            Ok(opened) => opened,
            Err(SourceIoError::Io(source))
                if !required && source.kind() == io::ErrorKind::NotFound =>
            {
                continue;
            }
            Err(SourceIoError::SystemIo { source, .. })
                if !required && source.kind() == io::ErrorKind::NotFound =>
            {
                continue;
            }
            Err(error) => return Err(io_route_error(error)),
        };
        let observed = observe_opened_file(rel.clone(), key, &opened);
        let bytes = opened
            .read_all_bounded(ATIF_MAX_FILE_BYTES)
            .map_err(io_route_error)?;
        opened
            .revalidate_leaf_with_same_object_root()
            .map_err(io_route_error)?;
        // Discover local relative trajectory_path / continued_trajectory_ref files
        // within the same parent root so commit-time revalidation and replay cover them.
        if let Ok(trajectory) = parse_atif_bytes(&bytes) {
            for target in collect_local_relative_refs(&rel, &trajectory) {
                queue.push_back((target, false));
            }
        }
        files.push(observed);
    }

    files.sort_by(|left, right| left.relative_key.cmp(&right.relative_key));
    let tree_fingerprint = compute_tree_fingerprint(true, root.authority_fingerprint(), &files);
    let leaf = AtifDocumentLeaf {
        files: files.clone(),
        tree_fingerprint,
    };
    let authority = AtifTreeAuthority {
        root,
        is_single_file: true,
        primary_file: Some(primary_rel),
        files,
    };
    Ok(CompleteDocumentTree::new(
        tree_fingerprint,
        vec![ObservedDocumentLeaf::new(
            DocumentLeafFingerprint::new(tree_fingerprint),
            leaf,
        )],
        authority,
    ))
}

fn discover_atif_directory(path: &Path) -> SourceBackedRouteResult<AtifDocumentTree> {
    let root = ProviderSourceRoot::open(path).map_err(io_route_error)?;
    let root_dir = root.directory().map_err(io_route_error)?;
    let mut files = Vec::new();
    collect_directory_json_files(&root_dir, Path::new(""), 0, &mut files)?;
    files.sort_by(|left, right| left.relative_key.cmp(&right.relative_key));

    let tree_fingerprint = compute_tree_fingerprint(false, root.authority_fingerprint(), &files);
    let leaf = AtifDocumentLeaf {
        files: files.clone(),
        tree_fingerprint,
    };
    let authority = AtifTreeAuthority {
        root,
        is_single_file: false,
        primary_file: None,
        files,
    };
    Ok(CompleteDocumentTree::new(
        tree_fingerprint,
        vec![ObservedDocumentLeaf::new(
            DocumentLeafFingerprint::new(tree_fingerprint),
            leaf,
        )],
        authority,
    ))
}

fn collect_directory_json_files(
    directory: &ProviderSourceDirectory,
    prefix: &Path,
    depth: usize,
    out: &mut Vec<ObservedAtifFile>,
) -> SourceBackedRouteResult<()> {
    if depth > ATIF_MAX_DIRECTORY_DEPTH {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::InvalidSource,
            format!("ATIF directory nesting depth exceeds maximum {ATIF_MAX_DIRECTORY_DEPTH}"),
        ));
    }
    let remaining = ATIF_MAX_DISCOVERED_FILES.saturating_sub(out.len()) + 1;
    let entries = directory
        .entries_snapshot(remaining)
        .map_err(io_route_error)?;
    for name in entries {
        let rel = if prefix.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            prefix.join(&name)
        };
        let opened = match directory.open_child(&name) {
            Ok(opened) => opened,
            Err(error)
                if !is_json_extension(&rel)
                    && (is_symlink_source_rejection(&error)
                        || is_non_regular_source_rejection(&error)) =>
            {
                continue;
            }
            Err(error) => return Err(io_route_error(error)),
        };
        match opened {
            OpenedProviderSourcePath::Directory(child_dir) => {
                collect_directory_json_files(&child_dir, &rel, depth + 1, out)?;
            }
            OpenedProviderSourcePath::File(opened) if is_json_extension(&rel) => {
                if out.len() >= ATIF_MAX_DISCOVERED_FILES {
                    return Err(SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::InvalidSource,
                        format!(
                            "ATIF directory exceeds maximum of {ATIF_MAX_DISCOVERED_FILES} JSON files"
                        ),
                    ));
                }
                let key = path_to_posix_key(&rel);
                out.push(observe_opened_file(rel, key, &opened));
            }
            OpenedProviderSourcePath::File(_) => {}
        }
    }
    Ok(())
}

struct LoadedAtifDocument {
    relative_path: PathBuf,
    relative_key: String,
    trajectory: Trajectory,
    session_id: StableEntityId,
}

#[derive(Debug, Clone)]
struct ExternalParentEdge {
    parent_key: String,
    relationship: ProviderNativeSessionRelationship,
    agent_scope: AgentScope,
}

fn scan_atif_tree<L, S>(
    source: &SourceKey,
    authority: &AtifTreeAuthority,
    leaf: &AtifDocumentLeaf,
    sink: &mut ChangedDocumentSink<'_, '_, L, S>,
) -> SourceBackedRouteResult<DocumentSourceTerminal>
where
    L: ctx_history_capture_runtime::CaptureLifecycleSink,
    S: ctx_history_capture_runtime::DocumentRecordSpool,
{
    let mut content_hasher = Sha256::new();
    content_hasher.update(b"ctx.atif-v1-tree-content.v1\0");

    let mut raw_bytes_by_key: BTreeMap<String, (PathBuf, Vec<u8>)> = BTreeMap::new();
    let mut certified_bytes = 0_u64;
    for file in &leaf.files {
        let bytes = authority.open_and_verify(file)?;
        certified_bytes = certified_bytes.saturating_add(file.len);
        content_hasher.update((file.relative_key.len() as u64).to_be_bytes());
        content_hasher.update(file.relative_key.as_bytes());
        content_hasher.update((bytes.len() as u64).to_be_bytes());
        content_hasher.update(&bytes);
        raw_bytes_by_key.insert(
            file.relative_key.clone(),
            (file.relative_path.clone(), bytes),
        );
    }

    let primary_key = authority.primary_file.as_deref().map(path_to_posix_key);
    let mut parsed_docs: BTreeMap<String, LoadedAtifDocument> = BTreeMap::new();
    let mut ignored_files = 0_u64;

    // First pass: parse all files that declare schema_version (or are the explicit single file).
    for (key, (rel, bytes)) in &raw_bytes_by_key {
        let must_be_atif = authority.is_single_file && primary_key.as_deref() == Some(key.as_str());
        if !must_be_atif {
            let is_candidate = has_schema_version_field(bytes).map_err(|error| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::InvalidSource,
                    format!("invalid JSON in `{key}`: {error}"),
                )
            })?;
            if !is_candidate {
                ignored_files = ignored_files.saturating_add(1);
                continue;
            }
        }
        let trajectory =
            parse_atif_bytes(bytes).map_err(|error| atif_parse_route_error(key, error))?;
        let (_, session_id) = derive_atif_document_session_id(source, key, &trajectory)
            .map_err(|error| internal_error(error.to_string()))?;
        parsed_docs.insert(
            key.clone(),
            LoadedAtifDocument {
                relative_path: rel.clone(),
                relative_key: key.clone(),
                trajectory,
                session_id,
            },
        );
    }

    // Second pass: if a parsed ATIF file references a local relative file that
    // had been skipped (e.g. missing schema_version), force strict ATIF parsing on it.
    let mut forced_targets = Vec::new();
    for doc in parsed_docs.values() {
        for target in collect_local_relative_refs(&doc.relative_path, &doc.trajectory) {
            let target_key = path_to_posix_key(&target);
            if raw_bytes_by_key.contains_key(&target_key) && !parsed_docs.contains_key(&target_key)
            {
                forced_targets.push(target_key);
            }
        }
    }
    for target_key in forced_targets {
        if let Some((_, bytes)) = raw_bytes_by_key.get(&target_key) {
            parse_atif_bytes(bytes).map_err(|error| atif_parse_route_error(&target_key, error))?;
        }
    }

    if parsed_docs.is_empty() {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::InvalidSource,
            "no ATIF trajectory files found in selected path",
        ));
    }

    // Build parent/continuation edges across discovered ATIF documents.
    let mut parent_edges: BTreeMap<String, ExternalParentEdge> = BTreeMap::new();
    for doc in parsed_docs.values() {
        let mut subagent_targets = Vec::new();
        collect_external_subagent_paths(&doc.trajectory, &mut subagent_targets);
        for raw_target in subagent_targets {
            if let Some(target_rel) = resolve_local_relative_path(&doc.relative_path, &raw_target) {
                let target_key = path_to_posix_key(&target_rel);
                if target_key != doc.relative_key && parsed_docs.contains_key(&target_key) {
                    parent_edges
                        .entry(target_key)
                        .or_insert(ExternalParentEdge {
                            parent_key: doc.relative_key.clone(),
                            relationship: ProviderNativeSessionRelationship::Delegated,
                            agent_scope: AgentScope::Subagent,
                        });
                }
            }
        }
        if let Some(cont_ref) = doc.trajectory.continued_trajectory_ref.as_deref() {
            if let Some(target_rel) = resolve_local_relative_path(&doc.relative_path, cont_ref) {
                let target_key = path_to_posix_key(&target_rel);
                if target_key != doc.relative_key && parsed_docs.contains_key(&target_key) {
                    parent_edges
                        .entry(target_key)
                        .or_insert(ExternalParentEdge {
                            parent_key: doc.relative_key.clone(),
                            relationship: ProviderNativeSessionRelationship::ResumedFrom,
                            agent_scope: AgentScope::Primary,
                        });
                }
            }
        }
    }

    sink.begin_source(source.clone())?;
    let mut emitted_event_ids = HashSet::new();
    let mut indexed_documents = 0_u64;
    let mut complete_records = 0_u64;

    for (key, doc) in &parsed_docs {
        let lineage = resolve_lineage_link(key, &parsed_docs, &parent_edges);
        let records = project_trajectory_to_core_records(&AtifProjectionInput {
            source,
            document_key: key,
            trajectory: &doc.trajectory,
            lineage: lineage.as_ref(),
        })
        .map_err(|error| {
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::InvalidSource,
                format!("project ATIF document `{key}`: {error}"),
            )
        })?;
        for record in records {
            if !emitted_event_ids.insert(record.event_id) {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::InvalidSource,
                    format!(
                        "ATIF source produced duplicate event identity {}",
                        record.event_id
                    ),
                ));
            }
            sink.emit_core_record(record)?;
            indexed_documents = indexed_documents.checked_add(1).ok_or_else(|| {
                internal_error("ATIF indexed document count overflowed".to_owned())
            })?;
            complete_records = complete_records.checked_add(1).ok_or_else(|| {
                internal_error("ATIF complete record count overflowed".to_owned())
            })?;
        }
    }

    let observation = SourceObservation::new(
        source.clone(),
        ATIF_SOURCE_REVISION_KIND,
        leaf.tree_fingerprint.to_vec(),
    )
    .map_err(|error| internal_error(error.to_string()))?;
    let content_digest: [u8; 32] = content_hasher.finalize().into();
    let complete_records = complete_records
        .checked_add(ignored_files)
        .ok_or_else(|| internal_error("ATIF complete record count overflowed".to_owned()))?;

    Ok(DocumentSourceTerminal {
        source: source.clone(),
        opening: observation.clone(),
        closing: observation,
        parser_revision: ATIF_PARSER_REVISION,
        content_digest,
        counts: ScannedSourceCounts {
            complete_records,
            retained_records: indexed_documents,
            rejected_records: 0,
            ignored_records: ignored_files,
            indexed_documents,
            certified_bytes,
        },
    })
}

fn resolve_lineage_link(
    key: &str,
    docs: &BTreeMap<String, LoadedAtifDocument>,
    edges: &BTreeMap<String, ExternalParentEdge>,
) -> Option<AtifLineageLink> {
    let edge = edges.get(key)?;
    let parent_doc = docs.get(&edge.parent_key)?;
    // Walk to root with cycle guard.
    let mut visited = HashSet::new();
    visited.insert(key);
    let mut current_key = edge.parent_key.as_str();
    let mut root_doc = parent_doc;
    let mut inherited_session_id = parent_doc.trajectory.session_id.clone();
    while visited.insert(current_key) {
        if let Some(next_edge) = edges.get(current_key) {
            if let Some(next_doc) = docs.get(&next_edge.parent_key) {
                root_doc = next_doc;
                if inherited_session_id.is_none() {
                    inherited_session_id = next_doc.trajectory.session_id.clone();
                }
                current_key = next_edge.parent_key.as_str();
                continue;
            }
        }
        break;
    }
    if parent_doc.session_id == docs.get(key)?.session_id {
        return None;
    }
    Some(AtifLineageLink {
        parent_session_id: parent_doc.session_id,
        root_session_id: root_doc.session_id,
        relationship: edge.relationship,
        agent_scope: edge.agent_scope,
        inherited_session_id,
    })
}

fn collect_local_relative_refs(current_rel: &Path, trajectory: &Trajectory) -> Vec<PathBuf> {
    let mut raw_paths = Vec::new();
    collect_external_subagent_paths(trajectory, &mut raw_paths);
    if let Some(cont) = trajectory.continued_trajectory_ref.as_deref() {
        raw_paths.push(cont.to_owned());
    }
    raw_paths
        .into_iter()
        .filter_map(|raw| resolve_local_relative_path(current_rel, &raw))
        .collect()
}

fn collect_external_subagent_paths(trajectory: &Trajectory, out: &mut Vec<String>) {
    for step in &trajectory.steps {
        if let Some(obs) = step.observation.as_ref() {
            for result in &obs.results {
                if let Some(refs) = result.subagent_trajectory_ref.as_ref() {
                    for sub_ref in refs {
                        collect_ref_path(sub_ref, out);
                    }
                }
            }
        }
    }
    if let Some(subagents) = trajectory.subagent_trajectories.as_ref() {
        for subagent in subagents {
            collect_external_subagent_paths(subagent, out);
        }
    }
}

fn collect_ref_path(sub_ref: &SubagentTrajectoryRef, out: &mut Vec<String>) {
    if let Some(path) = sub_ref.trajectory_path.as_deref() {
        if !path.trim().is_empty() {
            out.push(path.to_owned());
        }
    }
}

/// Resolves a local relative path reference against `current_rel` strictly
/// inside the approved root directory. Rejects URLs, absolute paths, Windows
/// drive prefixes, and any `..` traversal escaping the root.
pub(crate) fn resolve_local_relative_path(current_rel: &Path, raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.contains("://")
        || trimmed.starts_with('/')
        || trimmed.starts_with('\\')
    {
        return None;
    }
    // Reject Windows drive-letter prefixes (e.g. `C:foo`).
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return None;
    }
    let normalized_input = trimmed.replace('\\', "/");
    let mut stack: Vec<String> = Vec::new();
    if let Some(parent) = current_rel.parent() {
        for component in parent.components() {
            if let Component::Normal(segment) = component {
                stack.push(segment.to_string_lossy().into_owned());
            }
        }
    }
    for segment in normalized_input.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                stack.pop()?;
            }
            other => {
                if other.chars().any(char::is_control) {
                    return None;
                }
                stack.push(other.to_owned());
            }
        }
    }
    if stack.is_empty() {
        return None;
    }
    let mut path = PathBuf::new();
    for segment in stack {
        path.push(segment);
    }
    Some(path)
}

fn observe_opened_file(
    relative_path: PathBuf,
    relative_key: String,
    opened: &OpenedProviderSourceFile,
) -> ObservedAtifFile {
    ObservedAtifFile {
        relative_path,
        relative_key,
        len: opened.len(),
        ordinary_token: opened.ordinary_file_token(),
        authority_fingerprint: opened.authority_fingerprint(),
    }
}

fn compute_tree_fingerprint(
    is_single_file: bool,
    root_fingerprint: [u8; 32],
    files: &[ObservedAtifFile],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx.atif-v1-document-tree.v1\0");
    hasher.update([u8::from(is_single_file)]);
    hasher.update(root_fingerprint);
    hasher.update((files.len() as u64).to_be_bytes());
    for file in files {
        hasher.update((file.relative_key.len() as u64).to_be_bytes());
        hasher.update(file.relative_key.as_bytes());
        hasher.update(file.len.to_be_bytes());
        hasher.update(file.ordinary_token);
        hasher.update(file.authority_fingerprint);
    }
    hasher.finalize().into()
}

fn is_json_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
}

fn path_to_posix_key(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(segment) => Some(segment.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn atif_parse_route_error(key: &str, error: AtifImportError) -> SourceBackedRouteError {
    let kind = match &error {
        AtifImportError::UnsupportedSchemaVersion(_) => SourceBackedRouteErrorKind::Unsupported,
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, format!("ATIF file `{key}`: {error}"))
}

fn io_route_error(error: SourceIoError) -> SourceBackedRouteError {
    match error {
        SourceIoError::Io(ref source) if source.kind() == io::ErrorKind::NotFound => {
            SourceBackedRouteError::new(SourceBackedRouteErrorKind::Unavailable, error.to_string())
        }
        SourceIoError::SystemIo { ref source, .. } if source.kind() == io::ErrorKind::NotFound => {
            SourceBackedRouteError::new(SourceBackedRouteErrorKind::Unavailable, error.to_string())
        }
        other => SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::InvalidSource,
            other.to_string(),
        ),
    }
}

fn internal_error(detail: String) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}
