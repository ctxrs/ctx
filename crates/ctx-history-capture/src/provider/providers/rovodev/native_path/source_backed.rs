//! Thin source-backed projection adapter for Rovo Dev session trees.
//!
//! The existing provider discovery, bounded document parser, and verified
//! structured-content route remain authoritative. Shared code owns lifecycle
//! admission, projection emission, and deletion.

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

#[cfg(test)]
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, BatchHydrationRequest, BatchHydrationResult,
    CaptureProvider, EventIdentityInput, EventRole, EventType, HydratedProviderRecord,
    HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, NativeSessionKey, ProjectionContractError, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::super::{
    event::rovodev_event_type,
    source::{
        discover_rovodev_session_sources, RovoDevDiscovery, RovoDevSessionObservation,
        RovoDevSessionSource,
    },
};
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::{
        file_touches::{
            event_type_supports_structured_file_touches,
            visit_provider_file_touch_drafts_with_limit, MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        },
        normalization::{
            provider_block_text, provider_message_id, provider_output_event_is_failure,
            provider_result_outcome_evidence, provider_role_from_message, provider_string_field,
            provider_timestamp_from_fields,
        },
        source_backed::{
            family::document::{
                ChangedDocumentSink, CompleteDocumentTree, DocumentLeafFingerprint,
                DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
            },
            SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
        },
        tool_input,
    },
    CaptureError, OutputObservationKind, OutputOutcome, ProviderAdapterContext,
    MAX_PROVIDER_JSONL_LINE_BYTES, ROVODEV_SOURCE_FORMAT,
};

mod document;

const SOURCE_ANCHOR_NAMESPACE: &str = "rovodev.session";
const SESSION_KEY_NAMESPACE: &str = "rovodev.session";
const EVENT_KEY_NAMESPACE: &str = "rovodev.message";
const EVENT_POSITION_KIND: &str = "rovodev.message-object";
const LOGICAL_SESSION_KIND: &str = "rovodev-session";
const LOGICAL_EVENT_KIND: &str = "rovodev-event";
const SOURCE_SCHEMA_VARIANT: &str = "rovodev-session-json-tree-v1";
const SOURCE_REVISION_KIND: &str = "rovodev-session-tree-revision-v1";
const PARSER_REVISION: &str = "rovodev-source-backed-v1";
const RELATIVE_CONTEXT_FILE: &str = "session_context.json";
const MESSAGE_OBJECT_KIND: &str = "message_history";
const FILE_HASH_BUFFER_BYTES: usize = 64 * 1024;
const SOURCE_BACKED_MAX_RECORD_BYTES: usize = 6 * 1024 * 1024;
const SOURCE_BACKED_MAX_JSON_DEPTH: usize = 128;
const SOURCE_BACKED_MAX_COLLECTION_ELEMENTS: usize = 65_536;
const SOURCE_BACKED_MAX_FAILURE_BYTES: usize = 4 * 1024;
const LEAF_FINGERPRINT_DOMAIN: &[u8] = b"ctx.rovodev.document-leaf.v1\0";
const TREE_FINGERPRINT_DOMAIN: &[u8] = b"ctx.rovodev.document-tree.v1\0";

#[derive(Debug, Error)]
pub(crate) enum RovoDevSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Rovo Dev source-backed discovery requires an authoritative sessions directory")]
    NonAuthoritativeRoot,
    #[error("Rovo Dev authoritative inventory contains duplicate session identity {0:?}")]
    DuplicateSession(String),
    #[error("Rovo Dev session lineage contains a cycle at provider thread {0:?}")]
    LineageCycle(String),
    #[error("Rovo Dev source-backed scan counts do not reconcile")]
    CountMismatch,
    #[error("Rovo Dev source-backed event coordinate exceeds its supported range")]
    CoordinateOverflow,
    #[error("locator is not a Rovo Dev session-tree record")]
    InvalidLocator,
}

pub(crate) type RovoDevSourceBackedResult<T> = Result<T, RovoDevSourceBackedError>;

#[derive(Debug)]
struct FileSnapshot {
    bytes: Option<Vec<u8>>,
    byte_len: u64,
    sha256: [u8; 32],
}

impl FileSnapshot {
    fn read(
        source: &OpenedProviderSourceFile,
        byte_len: u64,
        retain_bytes: bool,
    ) -> RovoDevSourceBackedResult<Self> {
        if retain_bytes {
            let bytes = source.read_all_bounded(MAX_PROVIDER_JSONL_LINE_BYTES)?;
            if u64::try_from(bytes.len()).ok() != Some(byte_len) {
                return Err(CaptureError::SourceChangedDuringCapture.into());
            }
            let sha256 = Sha256::digest(&bytes).into();
            return Ok(Self {
                bytes: Some(bytes),
                byte_len,
                sha256,
            });
        }

        let mut file = source.file().try_clone()?;
        let mut digest = Sha256::new();
        let mut observed = 0_u64;
        let mut buffer = [0_u8; FILE_HASH_BUFFER_BYTES];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            observed = observed
                .checked_add(
                    u64::try_from(read).map_err(|_| RovoDevSourceBackedError::CountMismatch)?,
                )
                .ok_or(RovoDevSourceBackedError::CountMismatch)?;
            digest.update(&buffer[..read]);
        }
        if observed != byte_len {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        Ok(Self {
            bytes: None,
            byte_len,
            sha256: digest.finalize().into(),
        })
    }
}

#[derive(Debug)]
struct PreparedDocument {
    metadata: serde_json::Value,
    context_branch: Option<String>,
    messages: Vec<serde_json::Value>,
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    started_at: chrono::DateTime<chrono::Utc>,
    cwd: Option<String>,
    initial_failure_count: u64,
}

fn prepare_document(
    source: &RovoDevSessionSource,
    context: &ProviderAdapterContext,
    context_bytes: &[u8],
    metadata_bytes: Option<&[u8]>,
    metadata_acquisition_failure: Option<String>,
) -> std::result::Result<PreparedDocument, String> {
    let context_json =
        serde_json::from_slice::<serde_json::Value>(context_bytes).map_err(|error| {
            bounded_failure(format!("invalid Rovo Dev session_context.json: {error}"))
        })?;
    validate_json_bounds(&context_json)
        .map_err(|error| bounded_failure(format!("Rovo Dev session_context.json {error}")))?;
    let messages = message_history(&context_json).cloned().ok_or_else(|| {
        bounded_failure("Rovo Dev session_context.json is missing message_history array")
    })?;
    let context_branch = provider_string_field(
        &context_json,
        &[
            "branch",
            "git_branch",
            "gitBranch",
            "vcs_branch",
            "vcsBranch",
        ],
    );

    let mut initial_failure_count = u64::from(metadata_acquisition_failure.is_some());
    let metadata = match metadata_bytes {
        Some(bytes) => match serde_json::from_slice::<serde_json::Value>(bytes) {
            Ok(value) => match validate_json_bounds(&value) {
                Ok(()) => value,
                Err(_) => {
                    initial_failure_count = initial_failure_count.saturating_add(1);
                    serde_json::Value::Null
                }
            },
            Err(_) => {
                initial_failure_count = initial_failure_count.saturating_add(1);
                serde_json::Value::Null
            }
        },
        None => serde_json::Value::Null,
    };
    let provider_session_id = provider_string_field(&metadata, &["session_id", "sessionId"])
        .or_else(|| provider_string_field(&context_json, &["session_id", "sessionId"]))
        .unwrap_or_else(|| source.provider_session_id.clone());
    let parent_provider_session_id = provider_string_field(
        &metadata,
        &[
            "parent_session_id",
            "parentSessionId",
            "forked_from_session_id",
            "forkedFromSessionId",
            "fork_parent_id",
        ],
    );
    let started_at = provider_timestamp_from_fields(
        &metadata,
        &["created_at", "createdAt", "started_at", "startedAt"],
    )
    .or_else(|| messages.iter().find_map(message_timestamp))
    .unwrap_or(context.imported_at);
    let cwd = provider_string_field(
        &metadata,
        &[
            "workspace_path",
            "workspacePath",
            "working_directory",
            "workingDirectory",
            "cwd",
        ],
    );
    Ok(PreparedDocument {
        metadata,
        context_branch,
        messages,
        provider_session_id,
        parent_provider_session_id,
        started_at,
        cwd,
        initial_failure_count,
    })
}

fn message_history(value: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    value
        .get("message_history")
        .or_else(|| value.pointer("/session_context/message_history"))
        .or_else(|| value.get("messages"))
        .or_else(|| value.pointer("/conversation/messages"))
        .and_then(serde_json::Value::as_array)
}

fn message_timestamp(value: &serde_json::Value) -> Option<chrono::DateTime<chrono::Utc>> {
    provider_timestamp_from_fields(
        value,
        &[
            "timestamp",
            "created_at",
            "createdAt",
            "updated_at",
            "updatedAt",
            "user_sent_time",
        ],
    )
}

fn validate_json_bounds(value: &serde_json::Value) -> std::result::Result<(), &'static str> {
    let mut stack = vec![(value, 0_usize)];
    let mut collection_elements = 0_usize;
    while let Some((value, depth)) = stack.pop() {
        if depth > SOURCE_BACKED_MAX_JSON_DEPTH {
            return Err("exceeds maximum JSON depth");
        }
        match value {
            serde_json::Value::Array(values) => {
                collection_elements = collection_elements.saturating_add(values.len());
                if collection_elements > SOURCE_BACKED_MAX_COLLECTION_ELEMENTS {
                    return Err("exceeds JSON collection element budget");
                }
                stack.extend(values.iter().map(|value| (value, depth.saturating_add(1))));
            }
            serde_json::Value::Object(values) => {
                collection_elements = collection_elements.saturating_add(values.len());
                if collection_elements > SOURCE_BACKED_MAX_COLLECTION_ELEMENTS {
                    return Err("exceeds JSON collection element budget");
                }
                stack.extend(
                    values
                        .values()
                        .map(|value| (value, depth.saturating_add(1))),
                );
            }
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => {}
        }
    }
    Ok(())
}

fn bounded_failure(error: impl Into<String>) -> String {
    let mut error = error.into();
    if error.len() > SOURCE_BACKED_MAX_FAILURE_BYTES {
        let mut boundary = SOURCE_BACKED_MAX_FAILURE_BYTES;
        while !error.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }
        error.truncate(boundary);
    }
    error
}

#[derive(Debug)]
struct RovoDevSnapshot {
    frozen: RovoDevSessionObservation,
    context_sha256: [u8; 32],
    source_sha256: [u8; 32],
    certified_bytes: u64,
    document: std::result::Result<PreparedDocument, String>,
    context_file: OpenedProviderSourceFile,
    metadata_file: Option<OpenedProviderSourceFile>,
}

impl RovoDevSnapshot {
    fn read(
        source: &RovoDevSessionSource,
        context: &ProviderAdapterContext,
        authority: &ProviderSourceRoot,
        session_relative_path: &Path,
        context_relative_path: &Path,
        expected_metadata: Option<&Path>,
    ) -> RovoDevSourceBackedResult<Self> {
        let context_handle = authority.open_file(context_relative_path)?;
        let metadata_relative_path = session_relative_path.join("metadata.json");
        let metadata_handle = match authority.open_file(&metadata_relative_path) {
            Ok(file) => Some(file),
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if metadata_handle.is_some() != expected_metadata.is_some() {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let frozen = RovoDevSessionObservation::from_admitted(
            authority.named_path().join(context_relative_path),
            source.context_path.clone(),
            context_handle.metadata(),
            metadata_handle.as_ref().map(|metadata| {
                (
                    source.session_dir.join("metadata.json"),
                    metadata.metadata(),
                )
            }),
        )?;
        let context_oversized = frozen.context_length() > MAX_PROVIDER_JSONL_LINE_BYTES as u64;
        let context_file =
            FileSnapshot::read(&context_handle, frozen.context_length(), !context_oversized)?;
        let metadata_oversized = frozen
            .metadata_length()
            .is_some_and(|length| length > MAX_PROVIDER_JSONL_LINE_BYTES as u64);
        let metadata_file = match (metadata_handle.as_ref(), frozen.metadata_length()) {
            (Some(file), Some(length)) => {
                Some(FileSnapshot::read(file, length, !metadata_oversized)?)
            }
            (None, None) => None,
            _ => return Err(CaptureError::SourceChangedDuringCapture.into()),
        };
        let certified_bytes = metadata_file
            .as_ref()
            .map_or(Some(context_file.byte_len), |metadata| {
                context_file.byte_len.checked_add(metadata.byte_len)
            })
            .ok_or(RovoDevSourceBackedError::CountMismatch)?;
        let source_sha256 = compound_source_digest(&context_file, metadata_file.as_ref());
        let document = if context_oversized {
            Err(bounded_failure(format!(
                    "Rovo Dev session_context.json exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"
                )))
        } else {
            prepare_document(
                source,
                context,
                context_file.bytes.as_deref().unwrap_or_default(),
                metadata_file.as_ref().and_then(|file| file.bytes.as_deref()),
                metadata_oversized.then(|| {
                    bounded_failure(format!(
                            "Rovo Dev metadata.json exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"
                        ))
                }),
            )
        };
        Ok(Self {
            frozen,
            context_sha256: context_file.sha256,
            source_sha256,
            certified_bytes,
            document,
            context_file: context_handle,
            metadata_file: metadata_handle,
        })
    }

    fn revalidate_files(&self) -> RovoDevSourceBackedResult<()> {
        self.context_file.revalidate()?;
        if let Some(metadata) = &self.metadata_file {
            metadata.revalidate()?;
        }
        Ok(())
    }

    fn observation(&self, source_key: SourceKey) -> RovoDevSourceBackedResult<SourceObservation> {
        let mut revision = Vec::with_capacity(64);
        revision.extend_from_slice(&self.frozen.revision_authority());
        revision.extend_from_slice(&self.source_sha256);
        Ok(SourceObservation::new(
            source_key,
            SOURCE_REVISION_KIND,
            revision,
        )?)
    }

    fn proof(&self) -> RovoDevDocumentProof {
        RovoDevDocumentProof {
            frozen: self.frozen.clone(),
            context_sha256: self.context_sha256,
            source_sha256: self.source_sha256,
            certified_bytes: self.certified_bytes,
        }
    }

    fn matches(&self, proof: &RovoDevDocumentProof) -> bool {
        self.proof() == *proof
    }
}

fn compound_source_digest(context: &FileSnapshot, metadata: Option<&FileSnapshot>) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx.rovodev.source-backed.compound-v1\0");
    digest.update(context.byte_len.to_be_bytes());
    digest.update(context.sha256);
    match metadata {
        Some(metadata) => {
            digest.update([1]);
            digest.update(metadata.byte_len.to_be_bytes());
            digest.update(metadata.sha256);
        }
        None => digest.update([0]),
    }
    digest.finalize().into()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RovoDevDocumentProof {
    frozen: RovoDevSessionObservation,
    context_sha256: [u8; 32],
    source_sha256: [u8; 32],
    certified_bytes: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct RovoDevDocumentLeaf {
    source: RovoDevSessionSource,
    session_relative_path: PathBuf,
    context_relative_path: PathBuf,
    metadata_relative_path: Option<PathBuf>,
    source_key: SourceKey,
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    unique_message_ids: HashSet<String>,
    proof: RovoDevDocumentProof,
}

impl RovoDevDocumentLeaf {
    fn provider_session_id(&self) -> &str {
        &self.provider_session_id
    }
}

#[derive(Debug)]
pub(crate) struct RovoDevTreeAuthority {
    authority: ProviderSourceRoot,
}

type RovoDevDocumentTree = CompleteDocumentTree<RovoDevDocumentLeaf, RovoDevTreeAuthority>;

enum RovoDevSourceBackedDisposition {
    Complete(RovoDevDocumentTree),
    Unavailable,
}

fn discover_rovodev_source_backed(
    sessions_root: &Path,
    context: ProviderAdapterContext,
) -> RovoDevSourceBackedResult<RovoDevSourceBackedDisposition> {
    match fs::symlink_metadata(sessions_root) {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) =>
        {
            return Ok(RovoDevSourceBackedDisposition::Unavailable);
        }
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let canonical_root = fs::canonicalize(sessions_root)?;
    let authority = ProviderSourceRoot::open(&canonical_root)?;
    bind_document_tree(authority, &context).map(RovoDevSourceBackedDisposition::Complete)
}

fn bind_document_tree(
    authority: ProviderSourceRoot,
    context: &ProviderAdapterContext,
) -> RovoDevSourceBackedResult<RovoDevDocumentTree> {
    let discovery = authoritative_discovery(authority.named_path())?;
    let mut source_ids = HashSet::with_capacity(discovery.sources().len());
    let mut leaves = Vec::with_capacity(discovery.sources().len());
    for source in discovery.sources() {
        let session_relative_path = relative_to_rovodev_authority(&authority, &source.session_dir)?;
        let context_relative_path =
            relative_to_rovodev_authority(&authority, &source.context_path)?;
        let metadata_relative_path = source
            .metadata_path
            .as_deref()
            .map(|path| relative_to_rovodev_authority(&authority, path))
            .transpose()?;
        let snapshot = RovoDevSnapshot::read(
            source,
            context,
            &authority,
            &session_relative_path,
            &context_relative_path,
            metadata_relative_path.as_deref(),
        )?;
        let (provider_session_id, parent_provider_session_id) = snapshot
            .document
            .as_ref()
            .map(|document| {
                (
                    document.provider_session_id.clone(),
                    document.parent_provider_session_id.clone(),
                )
            })
            .unwrap_or_else(|_| (source.provider_session_id.clone(), None));
        let source_key = rovodev_source_key(&provider_session_id)?;
        if !source_ids.insert(source_key.identity().digest()) {
            return Err(RovoDevSourceBackedError::DuplicateSession(
                provider_session_id,
            ));
        }
        let session_id = rovodev_session_identity(&source_key, &provider_session_id)?;
        let unique_message_ids = unique_message_ids(&snapshot);
        let proof = snapshot.proof();
        snapshot.revalidate_files()?;
        leaves.push(RovoDevDocumentLeaf {
            source: source.clone(),
            session_relative_path,
            context_relative_path,
            metadata_relative_path,
            source_key,
            provider_session_id,
            parent_provider_session_id,
            session_id,
            parent_session_id: None,
            root_session_id: session_id,
            unique_message_ids,
            proof,
        });
    }
    authority.revalidate()?;
    bind_session_lineage(&mut leaves)?;
    let observed = leaves
        .into_iter()
        .map(observed_rovodev_leaf)
        .collect::<Vec<_>>();
    let tree_fingerprint = rovodev_tree_fingerprint(&authority, &observed);
    Ok(CompleteDocumentTree::new(
        tree_fingerprint,
        observed,
        RovoDevTreeAuthority { authority },
    ))
}

fn authoritative_discovery(root: &Path) -> RovoDevSourceBackedResult<RovoDevDiscovery> {
    let metadata = fs::symlink_metadata(root).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            RovoDevSourceBackedError::NonAuthoritativeRoot
        } else {
            RovoDevSourceBackedError::Io(error)
        }
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(RovoDevSourceBackedError::NonAuthoritativeRoot);
    }
    if fs::symlink_metadata(root.join(RELATIVE_CONTEXT_FILE)).is_ok() {
        return Err(RovoDevSourceBackedError::NonAuthoritativeRoot);
    }
    let discovery = discover_rovodev_session_sources(root)?;
    if !discovery.root_exists() {
        return Err(RovoDevSourceBackedError::NonAuthoritativeRoot);
    }
    Ok(discovery)
}

fn relative_to_rovodev_authority(
    authority: &ProviderSourceRoot,
    path: &Path,
) -> RovoDevSourceBackedResult<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    canonical
        .strip_prefix(authority.named_path())
        .map(Path::to_path_buf)
        .map_err(|_| RovoDevSourceBackedError::NonAuthoritativeRoot)
}

fn rovodev_source_key(provider_session_id: &str) -> RovoDevSourceBackedResult<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(provider_session_id)?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::RovoDev.as_str(),
        ROVODEV_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn rovodev_session_identity(
    source_key: &SourceKey,
    provider_session_id: &str,
) -> RovoDevSourceBackedResult<StableEntityId> {
    let session_key =
        NativeSessionKey::native_id(SESSION_KEY_NAMESPACE, TypedKey::utf8(provider_session_id)?)?;
    Ok(derive_session_id(SessionIdentityInput {
        source: source_key,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &session_key,
    })?)
}

fn provider_thread_session_identity(
    provider_session_id: &str,
) -> RovoDevSourceBackedResult<StableEntityId> {
    let source_key = rovodev_source_key(provider_session_id)?;
    rovodev_session_identity(&source_key, provider_session_id)
}

fn bind_session_lineage(leaves: &mut [RovoDevDocumentLeaf]) -> RovoDevSourceBackedResult<()> {
    let parents = leaves
        .iter()
        .map(|leaf| {
            (
                leaf.provider_session_id().to_owned(),
                leaf.parent_provider_session_id.clone(),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut bound = Vec::with_capacity(leaves.len());
    for leaf in leaves.iter() {
        let provider_session_id = leaf.provider_session_id();
        let parent_provider_session_id =
            parents.get(provider_session_id).and_then(Option::as_deref);
        let parent_session_id = parent_provider_session_id
            .map(provider_thread_session_identity)
            .transpose()?;
        let mut root_session_id = leaf.session_id;
        let mut cursor = parent_provider_session_id;
        let mut visited = HashSet::new();
        visited.insert(provider_session_id.to_owned());
        while let Some(ancestor_provider_session_id) = cursor {
            if !visited.insert(ancestor_provider_session_id.to_owned()) {
                return Err(RovoDevSourceBackedError::LineageCycle(
                    ancestor_provider_session_id.to_owned(),
                ));
            }
            root_session_id = provider_thread_session_identity(ancestor_provider_session_id)?;
            cursor = parents
                .get(ancestor_provider_session_id)
                .and_then(Option::as_deref);
        }
        bound.push((parent_session_id, root_session_id));
    }
    for (leaf, (parent_session_id, root_session_id)) in leaves.iter_mut().zip(bound) {
        leaf.parent_session_id = parent_session_id;
        leaf.root_session_id = root_session_id;
    }
    Ok(())
}

fn observed_rovodev_leaf(leaf: RovoDevDocumentLeaf) -> ObservedDocumentLeaf<RovoDevDocumentLeaf> {
    let mut digest = Sha256::new();
    digest.update(LEAF_FINGERPRINT_DOMAIN);
    digest.update(leaf.source_key.exact_descriptor_digest());
    hash_path(&mut digest, &leaf.context_relative_path);
    match &leaf.metadata_relative_path {
        Some(path) => {
            digest.update([1]);
            hash_path(&mut digest, path);
        }
        None => digest.update([0]),
    }
    digest.update(leaf.proof.context_sha256);
    digest.update(leaf.proof.source_sha256);
    digest.update(leaf.proof.certified_bytes.to_be_bytes());
    ObservedDocumentLeaf::new(DocumentLeafFingerprint::new(digest.finalize().into()), leaf)
}

fn rovodev_tree_fingerprint(
    authority: &ProviderSourceRoot,
    leaves: &[ObservedDocumentLeaf<RovoDevDocumentLeaf>],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(TREE_FINGERPRINT_DOMAIN);
    digest.update(authority.authority_fingerprint());
    digest.update((leaves.len() as u64).to_be_bytes());
    for leaf in leaves {
        digest.update(leaf.fingerprint.as_bytes());
    }
    digest.finalize().into()
}

fn hash_path(digest: &mut Sha256, path: &Path) {
    let bytes = path.as_os_str().as_encoded_bytes();
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

fn unique_message_ids(snapshot: &RovoDevSnapshot) -> HashSet<String> {
    let mut counts = HashMap::<String, usize>::new();
    if let Ok(document) = snapshot.document.as_ref() {
        for message in &document.messages {
            if let Some(native_id) = explicit_message_id(message) {
                let count = counts.entry(native_id.to_owned()).or_default();
                *count = count.saturating_add(1);
            }
        }
    }
    counts
        .into_iter()
        .filter_map(|(native_id, count)| (count == 1).then_some(native_id))
        .collect()
}

pub(crate) struct RovoDevDocumentTreeAdapter {
    root: PathBuf,
    context: ProviderAdapterContext,
    #[cfg(test)]
    projection_scans: Option<Arc<AtomicUsize>>,
    #[cfg(test)]
    hydration_scans: Option<Arc<AtomicUsize>>,
    #[cfg(test)]
    terminal_revalidation_hook: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl RovoDevDocumentTreeAdapter {
    pub(crate) fn new(root: PathBuf, context: ProviderAdapterContext) -> Self {
        Self {
            root,
            context,
            #[cfg(test)]
            projection_scans: None,
            #[cfg(test)]
            hydration_scans: None,
            #[cfg(test)]
            terminal_revalidation_hook: None,
        }
    }

    #[cfg(test)]
    fn with_projection_scans(mut self, scans: Arc<AtomicUsize>) -> Self {
        self.projection_scans = Some(scans);
        self
    }

    #[cfg(test)]
    fn with_hydration_scans(mut self, scans: Arc<AtomicUsize>) -> Self {
        self.hydration_scans = Some(scans);
        self
    }

    #[cfg(test)]
    fn with_terminal_revalidation_hook(mut self, hook: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.terminal_revalidation_hook = Some(hook);
        self
    }
}

impl ReplacementDocumentTree for RovoDevDocumentTreeAdapter {
    type Leaf = RovoDevDocumentLeaf;
    type TreeAuthority = RovoDevTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        owns_rovodev_source(source)
    }

    fn discover_complete(&self) -> SourceBackedRouteResult<RovoDevDocumentTree> {
        match discover_rovodev_source_backed(&self.root, self.context.clone())
            .map_err(rovodev_route_error)?
        {
            RovoDevSourceBackedDisposition::Complete(tree) => Ok(tree),
            RovoDevSourceBackedDisposition::Unavailable => Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Unavailable,
                "Rovo Dev selected sessions root is temporarily unavailable",
            )),
        }
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        #[cfg(test)]
        if let Some(scans) = self.projection_scans.as_ref() {
            scans.fetch_add(1, Ordering::Relaxed);
        }
        document::scan_rovodev_document(authority, leaf, &self.context, sink)
    }

    fn revalidate_complete(
        &self,
        _tree: &RovoDevDocumentTree,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        #[cfg(test)]
        if let Some(hook) = self.terminal_revalidation_hook.as_ref() {
            hook();
        }
        match discover_rovodev_source_backed(&self.root, self.context.clone())
            .map_err(rovodev_route_error)?
        {
            RovoDevSourceBackedDisposition::Complete(tree) => Ok(tree.tree_fingerprint),
            RovoDevSourceBackedDisposition::Unavailable => Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::SourceChanged,
                "Rovo Dev sessions root disappeared before terminal revalidation",
            )),
        }
    }

    fn hydrate_group(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        #[cfg(test)]
        if let Some(scans) = self.hydration_scans.as_ref() {
            scans.fetch_add(1, Ordering::Relaxed);
        }
        document::hydrate_rovodev_group(&self.root, &self.context, request)
    }
}

fn owns_rovodev_source(source: &SourceKey) -> bool {
    source.provider() == CaptureProvider::RovoDev.as_str()
        && source.source_format() == ROVODEV_SOURCE_FORMAT
        && source.schema_variant() == SOURCE_SCHEMA_VARIANT
        && source.provider_identity_version() == 1
}

fn rovodev_route_error(error: RovoDevSourceBackedError) -> SourceBackedRouteError {
    let kind = match &error {
        RovoDevSourceBackedError::Capture(CaptureError::SourceChangedDuringCapture) => {
            SourceBackedRouteErrorKind::SourceChanged
        }
        RovoDevSourceBackedError::Io(error)
        | RovoDevSourceBackedError::Capture(CaptureError::Io(error))
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) =>
        {
            SourceBackedRouteErrorKind::SourceChanged
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

fn explicit_message_id(message: &serde_json::Value) -> Option<&str> {
    ["id", "message_id", "messageId", "request_id", "requestId"]
        .into_iter()
        .find_map(|field| message.get(field).and_then(serde_json::Value::as_str))
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
#[path = "source_backed/tests.rs"]
mod tests;
