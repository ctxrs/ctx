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

use ctx_history_capture_model::normalization::{
    provider_block_text, provider_explicit_result_value_text, provider_role_from_message,
    provider_timestamp_from_fields,
};
use ctx_history_core::{
    admit_optional_metadata_text, admit_optional_provider_call_id, admit_provider_declared_fact,
    derive_event_id, derive_session_id, ActivityInvocation, ActivityJsonCapture, ActivityResult,
    ActivityTextCapture, AgentScope, CaptureProvider, CoreActivity, CoreRecord, CoreRecordError,
    EventIdentityInput, EventRole, EventType, LiteralFactKind, NativeItemKey, NativeSessionKey,
    ProjectionContractError, ScannedSourceCounts, SessionIdentityInput, SourceAnchorScope,
    SourceKey, SourceObservation, StableEntityId, TypedKey, CORE_ACTIVITY_REVISION,
};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
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
    CaptureError, CaptureLifecycleSink, ChangedDocumentSink, CompleteDocumentTree,
    DocumentLeafExecutionPolicy, DocumentLeafFingerprint, DocumentRecordSpool,
    DocumentSourceTerminal, ObservedDocumentLeaf, ProviderAdapterContext, ReplacementDocumentTree,
    SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
    MAX_PROVIDER_JSONL_LINE_BYTES, ROVODEV_SOURCE_FORMAT,
};

#[path = "source_backed/document.rs"]
mod document;

const SOURCE_ANCHOR_NAMESPACE: &str = "rovodev.session";
const SESSION_KEY_NAMESPACE: &str = "rovodev.session";
const EVENT_KEY_NAMESPACE: &str = "rovodev.message";
const EVENT_POSITION_KIND: &str = "rovodev.message-object";
const LOGICAL_SESSION_KIND: &str = "rovodev-session";
const LOGICAL_EVENT_KIND: &str = "rovodev-event";
const SOURCE_SCHEMA_VARIANT: &str = "rovodev-session-json-tree-v1";
const SOURCE_REVISION_KIND: &str = "rovodev-session-tree-revision-v1";
const PARSER_REVISION: &str = "rovodev-source-backed-v6-core-admission";
const RELATIVE_CONTEXT_FILE: &str = "session_context.json";
const MESSAGE_OBJECT_KIND: &str = "message_history";
const FILE_HASH_BUFFER_BYTES: usize = 64 * 1024;
const SOURCE_BACKED_MAX_JSON_DEPTH: usize = 128;
const SOURCE_BACKED_MAX_COLLECTION_ELEMENTS: usize = 65_536;
const SOURCE_BACKED_MAX_FAILURE_BYTES: usize = 4 * 1024;
const LEAF_FINGERPRINT_DOMAIN: &[u8] = b"ctx.rovodev.document-leaf.v4.direct-lineage\0";
const TREE_FINGERPRINT_DOMAIN: &[u8] = b"ctx.rovodev.document-tree.v1\0";

#[derive(Debug, Error)]
pub(crate) enum RovoDevSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Rovo Dev source-backed discovery requires an authoritative sessions directory")]
    NonAuthoritativeRoot,
    #[error("Rovo Dev authoritative inventory contains duplicate session identity {0:?}")]
    DuplicateSession(String),
    #[error("Rovo Dev source-backed scan counts do not reconcile")]
    CountMismatch,
    #[error("Rovo Dev source-backed event coordinate exceeds its supported range")]
    CoordinateOverflow,
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
            let read_len =
                usize::try_from(byte_len).map_err(|_| RovoDevSourceBackedError::CountMismatch)?;
            let bytes = source.read_exact_range(0, read_len, MAX_PROVIDER_JSONL_LINE_BYTES)?;
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
    if json_has_duplicate_key(context_bytes).map_err(|error| {
        bounded_failure(format!("invalid Rovo Dev session_context.json: {error}"))
    })? {
        return Err(bounded_failure(
            "Rovo Dev session_context.json has duplicate JSON fields",
        ));
    }
    let context_json =
        serde_json::from_slice::<serde_json::Value>(context_bytes).map_err(|error| {
            bounded_failure(format!("invalid Rovo Dev session_context.json: {error}"))
        })?;
    validate_json_bounds(&context_json)
        .map_err(|error| bounded_failure(format!("Rovo Dev session_context.json {error}")))?;
    let messages = message_history(&context_json).cloned().ok_or_else(|| {
        bounded_failure("Rovo Dev session_context.json is missing message_history array")
    })?;
    let context_branch = exact_provider_string_field(
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
        Some(bytes) if json_has_duplicate_key(bytes).unwrap_or(true) => {
            initial_failure_count = initial_failure_count.saturating_add(1);
            serde_json::Value::Null
        }
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
    let provider_session_id = exact_provider_string_field(&metadata, &["session_id", "sessionId"])
        .or_else(|| exact_provider_string_field(&context_json, &["session_id", "sessionId"]))
        .unwrap_or_else(|| source.provider_session_id.clone());
    let parent_provider_session_id = exact_provider_string_field(
        &metadata,
        &[
            "parent_session_id",
            "parentSessionId",
            "forked_from_session_id",
            "forkedFromSessionId",
            "fork_parent_id",
        ],
    );
    let started_at = exact_provider_timestamp_from_fields(
        &metadata,
        &["created_at", "createdAt", "started_at", "startedAt"],
    )
    .or_else(|| messages.iter().find_map(message_timestamp))
    .unwrap_or(context.imported_at);
    let cwd = exact_provider_string_field(
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
    let mut selected = None;
    for candidate in [
        value.get("message_history"),
        value.pointer("/session_context/message_history"),
        value.get("messages"),
        value.pointer("/conversation/messages"),
    ]
    .into_iter()
    .flatten()
    {
        let candidate = candidate.as_array()?;
        if selected.is_some_and(|selected| selected != candidate) {
            return None;
        }
        selected = Some(candidate);
    }
    selected
}

fn message_timestamp(value: &serde_json::Value) -> Option<chrono::DateTime<chrono::Utc>> {
    exact_provider_timestamp_from_fields(
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

fn exact_provider_string_field(value: &serde_json::Value, fields: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    let mut selected = None;
    for field in fields {
        let Some(candidate) = object.get(*field).and_then(serde_json::Value::as_str) else {
            continue;
        };
        if selected.is_some_and(|selected| selected != candidate) {
            return None;
        }
        selected = Some(candidate);
    }
    selected.map(str::to_owned)
}

fn exact_provider_timestamp_from_fields(
    value: &serde_json::Value,
    fields: &[&str],
) -> Option<chrono::DateTime<chrono::Utc>> {
    let object = value.as_object()?;
    let mut selected = None;
    let mut observed = false;
    for field in fields {
        if !object.contains_key(*field) {
            continue;
        }
        let candidate = provider_timestamp_from_fields(value, &[*field]);
        if observed && selected != candidate {
            return None;
        }
        selected = candidate;
        observed = true;
    }
    selected
}

#[path = "source_backed/json_validation.rs"]
mod json_validation;

use json_validation::{bounded_failure, json_has_duplicate_key, validate_json_bounds};

#[derive(Debug)]
struct RovoDevSnapshot {
    frozen: RovoDevSessionObservation,
    source_sha256: [u8; 32],
    certified_bytes: u64,
    document: std::result::Result<PreparedDocument, String>,
}

impl RovoDevSnapshot {
    fn read(
        source: &RovoDevOpenedSource,
        context: &ProviderAdapterContext,
    ) -> RovoDevSourceBackedResult<Self> {
        let files = source.open_files()?;
        let context_oversized =
            source.opening.context_length() > MAX_PROVIDER_JSONL_LINE_BYTES as u64;
        let context_file = FileSnapshot::read(
            &files.context,
            source.opening.context_length(),
            !context_oversized,
        )?;
        let metadata_oversized = source
            .opening
            .metadata_length()
            .is_some_and(|length| length > MAX_PROVIDER_JSONL_LINE_BYTES as u64);
        let metadata_file = match (files.metadata.as_ref(), source.opening.metadata_length()) {
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
                &source.source,
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
        files.revalidate()?;
        Ok(Self {
            frozen: source.opening.clone(),
            source_sha256,
            certified_bytes,
            document,
        })
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
    opening: RovoDevSessionObservation,
    certified_bytes: u64,
}

#[derive(Debug)]
struct RovoDevOpenedSource {
    source: RovoDevSessionSource,
    authority: ProviderSourceRoot,
    context_relative_path: PathBuf,
    metadata_candidate_relative_path: PathBuf,
    metadata_relative_path: Option<PathBuf>,
    opening: RovoDevSessionObservation,
}

#[derive(Debug)]
struct RovoDevOpenedFiles {
    context: OpenedProviderSourceFile,
    metadata: Option<OpenedProviderSourceFile>,
}

impl RovoDevOpenedSource {
    fn open_files(&self) -> RovoDevSourceBackedResult<RovoDevOpenedFiles> {
        let context = self.authority.open_file(&self.context_relative_path)?;
        let metadata = match self
            .authority
            .open_file(&self.metadata_candidate_relative_path)
        {
            Ok(file) => Some(file),
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if metadata.is_some() != self.metadata_relative_path.is_some() {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let current = RovoDevSessionObservation::from_opened(
            self.authority
                .named_path()
                .join(&self.context_relative_path),
            self.source.context_path.clone(),
            &context,
            metadata
                .as_ref()
                .map(|metadata| (self.source.session_dir.join("metadata.json"), metadata)),
        );
        if current != self.opening {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        Ok(RovoDevOpenedFiles { context, metadata })
    }

    fn revalidate_current(&self) -> RovoDevSourceBackedResult<()> {
        self.open_files()?.revalidate()
    }

    fn proof(&self) -> RovoDevSourceBackedResult<RovoDevDocumentProof> {
        let certified_bytes = self
            .opening
            .metadata_length()
            .map_or(Some(self.opening.context_length()), |metadata| {
                self.opening.context_length().checked_add(metadata)
            })
            .ok_or(RovoDevSourceBackedError::CountMismatch)?;
        Ok(RovoDevDocumentProof {
            opening: self.opening.clone(),
            certified_bytes,
        })
    }
}

impl RovoDevOpenedFiles {
    fn revalidate(&self) -> RovoDevSourceBackedResult<()> {
        self.context.revalidate_leaf()?;
        if let Some(metadata) = &self.metadata {
            metadata.revalidate_leaf()?;
        }
        Ok(())
    }
}

impl RovoDevTreeAuthority {
    fn revalidate_terminal_inventory(&self) -> RovoDevSourceBackedResult<()> {
        self.authority.revalidate()?;
        let discovery = authoritative_discovery(self.authority.named_path())?;
        if discovery.sources().len() != self.sources.len() {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        for (current, opened) in discovery.sources().iter().zip(&self.sources) {
            if current.session_dir != opened.source.session_dir
                || current.context_path != opened.source.context_path
                || current.metadata_path != opened.source.metadata_path
                || current.provider_session_id != opened.source.provider_session_id
            {
                return Err(CaptureError::SourceChangedDuringCapture.into());
            }
            opened.revalidate_current()?;
        }
        self.authority.revalidate()?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct RovoDevDocumentLeaf {
    source_index: usize,
    proof: RovoDevDocumentProof,
    header: RovoDevDocumentHeader,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RovoDevDocumentHeader {
    source_key: SourceKey,
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    session_id: StableEntityId,
}

#[derive(Debug)]
struct RovoDevBoundDocument {
    source_key: SourceKey,
    provider_session_id: String,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    unique_message_ids: HashSet<String>,
}

#[derive(Debug)]
pub struct RovoDevTreeAuthority {
    authority: ProviderSourceRoot,
    sources: Vec<RovoDevOpenedSource>,
}

type RovoDevDocumentTree = CompleteDocumentTree<RovoDevDocumentLeaf, RovoDevTreeAuthority>;

enum RovoDevSourceBackedDisposition {
    Complete(Box<RovoDevDocumentTree>),
    Unavailable,
}

#[cfg(test)]
fn discover_rovodev_source_backed(
    sessions_root: &Path,
) -> RovoDevSourceBackedResult<RovoDevSourceBackedDisposition> {
    discover_rovodev_source_backed_scoped(sessions_root, SourceAnchorScope::Unqualified)
}

fn discover_rovodev_source_backed_scoped(
    sessions_root: &Path,
    source_anchor_scope: SourceAnchorScope,
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
    bind_document_tree(authority, source_anchor_scope)
        .map(Box::new)
        .map(RovoDevSourceBackedDisposition::Complete)
}

fn bind_document_tree(
    authority: ProviderSourceRoot,
    source_anchor_scope: SourceAnchorScope,
) -> RovoDevSourceBackedResult<RovoDevDocumentTree> {
    let discovery = authoritative_discovery(authority.named_path())?;
    let mut sources = Vec::with_capacity(discovery.sources().len());
    for source in discovery.sources() {
        let session_relative_path = relative_to_rovodev_authority(&authority, &source.session_dir)?;
        let context_relative_path =
            relative_to_rovodev_authority(&authority, &source.context_path)?;
        let metadata_relative_path = source
            .metadata_path
            .as_deref()
            .map(|path| relative_to_rovodev_authority(&authority, path))
            .transpose()?;
        let context_file = authority.open_file(&context_relative_path)?;
        let discovered_metadata_relative_path = session_relative_path.join("metadata.json");
        let metadata_file = match authority.open_file(&discovered_metadata_relative_path) {
            Ok(file) => Some(file),
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if metadata_file.is_some() != metadata_relative_path.is_some() {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        let opening = RovoDevSessionObservation::from_opened(
            authority.named_path().join(&context_relative_path),
            source.context_path.clone(),
            &context_file,
            metadata_file
                .as_ref()
                .map(|metadata| (source.session_dir.join("metadata.json"), metadata)),
        );
        let opened = RovoDevOpenedSource {
            source: source.clone(),
            authority: authority.clone(),
            context_relative_path,
            metadata_candidate_relative_path: discovered_metadata_relative_path,
            metadata_relative_path,
            opening,
        };
        context_file.revalidate_leaf()?;
        if let Some(metadata) = &metadata_file {
            metadata.revalidate_leaf()?;
        }
        sources.push(opened);
    }
    authority.revalidate()?;
    // Each leaf owns only the relationship claim carried by that session's
    // files. Parent presence and transitive ancestry are deliberately not
    // replay inputs for the child logical source.
    let mut observed = Vec::with_capacity(sources.len());
    let mut source_owners = HashSet::with_capacity(sources.len());
    for (source_index, source) in sources.iter().enumerate() {
        let header = document::probe_document_header(source, source_anchor_scope)?;
        if !source_owners.insert(header.source_key.identity().digest()) {
            return Err(RovoDevSourceBackedError::DuplicateSession(
                header.provider_session_id,
            ));
        }
        observed.push(observed_rovodev_leaf(source_index, source, header)?);
    }
    let tree_fingerprint = rovodev_tree_fingerprint(&authority, &observed);
    Ok(CompleteDocumentTree::new(
        tree_fingerprint,
        observed,
        RovoDevTreeAuthority { authority, sources },
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

#[cfg(test)]
fn rovodev_source_key(provider_session_id: &str) -> RovoDevSourceBackedResult<SourceKey> {
    rovodev_source_key_scoped(provider_session_id, SourceAnchorScope::Unqualified)
}

fn rovodev_source_key_scoped(
    provider_session_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> RovoDevSourceBackedResult<SourceKey> {
    Ok(SourceKey::derive_provider_native_scoped(
        CaptureProvider::RovoDev.as_str(),
        ROVODEV_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(provider_session_id)?,
        source_anchor_scope,
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

#[cfg(test)]
fn provider_thread_session_identity(
    provider_session_id: &str,
) -> RovoDevSourceBackedResult<StableEntityId> {
    provider_thread_session_identity_scoped(provider_session_id, SourceAnchorScope::Unqualified)
}

fn provider_thread_session_identity_scoped(
    provider_session_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> RovoDevSourceBackedResult<StableEntityId> {
    let source_key = rovodev_source_key_scoped(provider_session_id, source_anchor_scope)?;
    rovodev_session_identity(&source_key, provider_session_id)
}

fn observed_rovodev_leaf(
    source_index: usize,
    source: &RovoDevOpenedSource,
    header: RovoDevDocumentHeader,
) -> RovoDevSourceBackedResult<ObservedDocumentLeaf<RovoDevDocumentLeaf>> {
    let proof = source.proof()?;
    let mut digest = Sha256::new();
    digest.update(LEAF_FINGERPRINT_DOMAIN);
    hash_path(&mut digest, &source.context_relative_path);
    match &source.metadata_relative_path {
        Some(path) => {
            digest.update([1]);
            hash_path(&mut digest, path);
        }
        None => digest.update([0]),
    }
    digest.update(proof.opening.revision_authority());
    digest.update(proof.certified_bytes.to_be_bytes());
    digest.update(header.source_key.identity().digest());
    match &header.parent_provider_session_id {
        Some(parent) => {
            digest.update([1]);
            digest.update((parent.len() as u64).to_be_bytes());
            digest.update(parent.as_bytes());
        }
        None => digest.update([0]),
    }
    Ok(ObservedDocumentLeaf::new(
        DocumentLeafFingerprint::new(digest.finalize().into()),
        RovoDevDocumentLeaf {
            source_index,
            proof,
            header,
        },
    ))
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

fn apply_direct_session_relationship(
    record: &mut CoreRecord,
    parent_session_id: Option<StableEntityId>,
) -> RovoDevSourceBackedResult<()> {
    if let Some(parent_session_id) = parent_session_id {
        record.parent_session_id = Some(parent_session_id);
        record.agent_scope = Some(AgentScope::Subagent);
    } else {
        record.agent_scope = Some(AgentScope::Primary);
    }
    Ok(())
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

pub struct RovoDevDocumentTreeAdapter<L, S, C> {
    root: PathBuf,
    context: ProviderAdapterContext,
    source_anchor_scope: SourceAnchorScope,
    _lifecycle: crate::ProviderLifecycleMarker<L, S, C>,
}

impl<L, S, C> RovoDevDocumentTreeAdapter<L, S, C> {
    pub fn new(root: PathBuf, context: ProviderAdapterContext) -> Self {
        Self::new_scoped(root, context, SourceAnchorScope::Unqualified)
    }

    pub fn new_scoped(
        root: PathBuf,
        context: ProviderAdapterContext,
        source_anchor_scope: SourceAnchorScope,
    ) -> Self {
        Self {
            root,
            context,
            source_anchor_scope,
            _lifecycle: std::marker::PhantomData,
        }
    }
}

impl<L, S, C> ReplacementDocumentTree for RovoDevDocumentTreeAdapter<L, S, C>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
    C: Send + Sync + 'static,
{
    type Lifecycle = L;
    type Spool = S;
    type RouteControl = C;
    type Leaf = RovoDevDocumentLeaf;
    type TreeAuthority = RovoDevTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        owns_rovodev_source(source)
    }

    fn leaf_execution_policy(&self) -> DocumentLeafExecutionPolicy {
        DocumentLeafExecutionPolicy::Independent
    }

    fn independent_leaf_source(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
    ) -> SourceBackedRouteResult<SourceKey> {
        authority.source(leaf).map_err(rovodev_route_error)?;
        Ok(leaf.header.source_key.clone())
    }

    fn discover_complete(&self) -> SourceBackedRouteResult<RovoDevDocumentTree> {
        match discover_rovodev_source_backed_scoped(&self.root, self.source_anchor_scope)
            .map_err(rovodev_route_error)?
        {
            RovoDevSourceBackedDisposition::Complete(tree) => Ok(*tree),
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
        sink: &mut ChangedDocumentSink<'_, '_, L, S>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        document::scan_rovodev_document(
            authority,
            leaf,
            &self.context,
            self.source_anchor_scope,
            sink,
        )
    }

    fn revalidate_complete(&self, tree: &RovoDevDocumentTree) -> SourceBackedRouteResult<[u8; 32]> {
        tree.authority
            .revalidate_terminal_inventory()
            .map_err(rovodev_route_error)?;
        Ok(tree.tree_fingerprint)
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
    let object = message.as_object()?;
    let mut selected = None;
    for field in ["id", "message_id", "messageId", "request_id", "requestId"] {
        let Some(candidate) = object
            .get(field)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        if selected.is_some_and(|selected| selected != candidate) {
            return None;
        }
        selected = Some(candidate);
    }
    selected
}

#[cfg(test)]
#[path = "source_backed/tests.rs"]
mod tests;
