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

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource,
    CertifiedSourceInventory, EventIdentityInput, EventRole, EventType, LocatorRevisionPolicy,
    NativeItemKey, NativeRecordCoordinate, NativeSessionKey, ProjectionContractError,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceFrontier,
    SourceInventoryObservation, SourceKey, SourceObservation, SourceRecordLocator,
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
        tool_input,
    },
    CaptureError, OutputObservationKind, OutputOutcome, ProviderAdapterContext,
    MAX_PROVIDER_JSONL_LINE_BYTES, ROVODEV_SOURCE_FORMAT,
};

const SOURCE_ANCHOR_NAMESPACE: &str = "rovodev.session";
const SESSION_KEY_NAMESPACE: &str = "rovodev.session";
const EVENT_KEY_NAMESPACE: &str = "rovodev.message";
const EVENT_POSITION_KIND: &str = "rovodev.message-object";
const LOGICAL_SESSION_KIND: &str = "rovodev-session";
const LOGICAL_EVENT_KIND: &str = "rovodev-event";
const SOURCE_SCHEMA_VARIANT: &str = "rovodev-session-json-tree-v1";
const SOURCE_REVISION_KIND: &str = "rovodev-session-tree-revision-v1";
const INVENTORY_AUTHORITY_NAMESPACE: &str = "rovodev.sessions-root";
const INVENTORY_REVISION_KIND: &str = "rovodev-sessions-inventory-v1";
const INVENTORY_DISCOVERY_REVISION: &str = "rovodev-sessions-discovery-v1";
const FRONTIER_KIND: &str = "rovodev-document-frontier-v1";
const PARSER_REVISION: &str = "rovodev-source-backed-v1";
const RELATIVE_CONTEXT_FILE: &str = "session_context.json";
const MESSAGE_OBJECT_KIND: &str = "message_history";
const FILE_HASH_BUFFER_BYTES: usize = 64 * 1024;
const SOURCE_BACKED_PAGE_MAX_RECORDS: usize = 64;
const SOURCE_BACKED_PAGE_MAX_BYTES: usize = 6 * 1024 * 1024;
const SOURCE_BACKED_MAX_JSON_DEPTH: usize = 128;
const SOURCE_BACKED_MAX_COLLECTION_ELEMENTS: usize = 65_536;
const SOURCE_BACKED_MAX_FAILURE_BYTES: usize = 4 * 1024;

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
    #[error("Rovo Dev source-backed scan was not drained to a terminal frontier")]
    IncompleteScan,
    #[error("Rovo Dev source-backed scan counts do not reconcile")]
    CountMismatch,
    #[error("Rovo Dev source-backed event coordinate exceeds its supported range")]
    CoordinateOverflow,
    #[error("locator is not a Rovo Dev session-tree record")]
    InvalidLocator,
    #[error("Rovo Dev locator source is absent from the authoritative sessions inventory")]
    LocatorSourceMissing,
    #[error("Rovo Dev locator source revision no longer matches provider bytes")]
    LocatorSourceChanged,
    #[error("Rovo Dev locator object coordinate no longer matches the provider document")]
    LocatorObjectChanged,
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

#[derive(Debug)]
struct ProjectedMessage {
    event_type: EventType,
    role: Option<EventRole>,
    occurred_at: chrono::DateTime<chrono::Utc>,
    touched_files: Vec<String>,
    touch_limit_exceeded: bool,
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

fn project_message(
    message: &serde_json::Value,
    index: usize,
    document: &PreparedDocument,
) -> std::result::Result<Option<ProjectedMessage>, String> {
    if !message.is_object() {
        return Err(bounded_failure(
            "Rovo Dev message_history member must be an object",
        ));
    }
    let role_text = message
        .get("role")
        .or_else(|| message.get("kind"))
        .or_else(|| message.get("type"))
        .and_then(serde_json::Value::as_str);
    let mut event_type = rovodev_event_type(message, role_text);
    if event_type == EventType::ToolOutput {
        let outcome = output_outcome(message);
        if !matches!(outcome, OutputOutcome::Failure | OutputOutcome::Timeout) {
            return Ok(None);
        }
        if output_kind(message) == OutputObservationKind::Command {
            event_type = EventType::CommandOutput;
        }
    }
    let occurred_at = message_timestamp(message).unwrap_or(document.started_at);
    let role = Some(provider_role_from_message(message, role_text));
    let mut touched_files = Vec::new();
    let include_structured = event_type_supports_structured_file_touches(event_type);
    let outcome = visit_provider_file_touch_drafts_with_limit(
        message,
        include_structured,
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        |(_, touch)| {
            touched_files.push(touch.path);
            Ok::<(), CaptureError>(())
        },
    )
    .map_err(|error| bounded_failure(error.to_string()))?;
    let _ = index;
    Ok(Some(ProjectedMessage {
        event_type,
        role,
        occurred_at,
        touched_files,
        touch_limit_exceeded: outcome.limit_exceeded(),
    }))
}

fn output_kind(value: &serde_json::Value) -> OutputObservationKind {
    let tool_name = recursive_string_field(value, &["tool_name", "toolName", "name", "tool"])
        .unwrap_or_else(|| "tool".to_owned());
    if tool_input::is_command_tool(&tool_name.to_ascii_lowercase()) {
        OutputObservationKind::Command
    } else {
        OutputObservationKind::Tool
    }
}

fn output_outcome(value: &serde_json::Value) -> OutputOutcome {
    if value_timed_out(value) {
        OutputOutcome::Timeout
    } else if provider_output_event_is_failure(value) {
        OutputOutcome::Failure
    } else if provider_result_outcome_evidence(EventType::ToolOutput, value).as_str()
        == Some("success")
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    }
}

fn recursive_string_field(value: &serde_json::Value, fields: &[&str]) -> Option<String> {
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .find_map(|value| recursive_string_field(value, fields)),
        serde_json::Value::Object(values) => fields
            .iter()
            .find_map(|field| values.get(*field).and_then(serde_json::Value::as_str))
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| {
                values
                    .values()
                    .find_map(|value| recursive_string_field(value, fields))
            }),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => None,
    }
}

fn value_timed_out(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(values) => values.iter().any(value_timed_out),
        serde_json::Value::Object(values) => {
            values.iter().any(|(key, value)| {
                matches!(key.as_str(), "timed_out" | "timedOut" | "timeout")
                    && value.as_bool().unwrap_or(false)
                    || matches!(key.as_str(), "status" | "state" | "outcome")
                        && value.as_str().is_some_and(|value| {
                            matches!(
                                value.trim().to_ascii_lowercase().as_str(),
                                "timeout" | "timed_out" | "timedout"
                            )
                        })
            }) || values.values().any(value_timed_out)
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => false,
    }
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

    fn revalidate(&self, authority: &ProviderSourceRoot) -> RovoDevSourceBackedResult<()> {
        self.context_file.revalidate()?;
        if let Some(metadata) = &self.metadata_file {
            metadata.revalidate()?;
        }
        authority.revalidate()?;
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

    fn message_count(&self) -> usize {
        self.document
            .as_ref()
            .map_or(0, |document| document.messages.len())
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

#[derive(Debug)]
pub(crate) struct RovoDevSourceBackedLeaf {
    authority: ProviderSourceRoot,
    source: RovoDevSessionSource,
    session_relative_path: PathBuf,
    context_relative_path: PathBuf,
    metadata_relative_path: Option<PathBuf>,
    source_key: SourceKey,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    unique_message_ids: HashSet<String>,
    snapshot: RovoDevSnapshot,
}

impl RovoDevSourceBackedLeaf {
    pub(crate) fn source_key(&self) -> &SourceKey {
        &self.source_key
    }

    pub(crate) fn provider_session_id(&self) -> &str {
        self.snapshot
            .document
            .as_ref()
            .map_or(self.source.provider_session_id.as_str(), |document| {
                document.provider_session_id.as_str()
            })
    }
}

#[derive(Debug)]
pub(crate) struct RovoDevSourceBackedInventory {
    authority: ProviderSourceRoot,
    context: ProviderAdapterContext,
    opening: SourceInventoryObservation,
    leaves: Vec<RovoDevSourceBackedLeaf>,
}

impl RovoDevSourceBackedInventory {
    pub(crate) fn leaves(&self) -> &[RovoDevSourceBackedLeaf] {
        &self.leaves
    }

    pub(crate) fn certify(&self) -> RovoDevSourceBackedResult<CertifiedSourceInventory> {
        for leaf in &self.leaves {
            leaf.snapshot.revalidate(&self.authority)?;
        }
        self.authority.revalidate()?;
        let closing = bind_inventory(&self.authority, &self.context)?;
        for leaf in &closing.leaves {
            leaf.snapshot.revalidate(&self.authority)?;
        }
        self.authority.revalidate()?;
        Ok(CertifiedSourceInventory::certify(
            self.opening.clone(),
            closing.observation,
            INVENTORY_DISCOVERY_REVISION,
            closing
                .leaves
                .into_iter()
                .map(|leaf| leaf.source_key)
                .collect(),
        )?)
    }
}

pub(crate) fn discover_rovodev_source_backed(
    sessions_root: &Path,
    context: ProviderAdapterContext,
) -> RovoDevSourceBackedResult<RovoDevSourceBackedInventory> {
    authoritative_discovery(sessions_root)?;
    let canonical_root = fs::canonicalize(sessions_root)?;
    let authority = ProviderSourceRoot::open(&canonical_root)?;
    let bound = bind_inventory(&authority, &context)?;
    Ok(RovoDevSourceBackedInventory {
        authority,
        context,
        opening: bound.observation,
        leaves: bound.leaves,
    })
}

struct BoundInventory {
    observation: SourceInventoryObservation,
    leaves: Vec<RovoDevSourceBackedLeaf>,
}

fn bind_inventory(
    authority: &ProviderSourceRoot,
    context: &ProviderAdapterContext,
) -> RovoDevSourceBackedResult<BoundInventory> {
    let discovery = authoritative_discovery(authority.named_path())?;
    let canonical_root = authority.named_path().to_path_buf();
    let mut source_ids = HashSet::with_capacity(discovery.sources().len());
    let mut leaves = Vec::with_capacity(discovery.sources().len());
    for source in discovery.sources() {
        let session_relative_path = relative_to_rovodev_authority(authority, &source.session_dir)?;
        let context_relative_path = relative_to_rovodev_authority(authority, &source.context_path)?;
        let metadata_relative_path = source
            .metadata_path
            .as_deref()
            .map(|path| relative_to_rovodev_authority(authority, path))
            .transpose()?;
        let snapshot = RovoDevSnapshot::read(
            source,
            context,
            authority,
            &session_relative_path,
            &context_relative_path,
            metadata_relative_path.as_deref(),
        )?;
        let provider_session_id = snapshot
            .document
            .as_ref()
            .map_or(source.provider_session_id.as_str(), |document| {
                document.provider_session_id.as_str()
            });
        let source_key = rovodev_source_key(provider_session_id)?;
        if !source_ids.insert(source_key.identity().digest()) {
            return Err(RovoDevSourceBackedError::DuplicateSession(
                provider_session_id.to_owned(),
            ));
        }
        let session_id = rovodev_session_identity(&source_key, provider_session_id)?;
        let unique_message_ids = unique_message_ids(&snapshot);
        leaves.push(RovoDevSourceBackedLeaf {
            authority: authority.clone(),
            source: source.clone(),
            session_relative_path,
            context_relative_path,
            metadata_relative_path,
            source_key,
            session_id,
            parent_session_id: None,
            root_session_id: session_id,
            unique_message_ids,
            snapshot,
        });
    }
    bind_session_lineage(&mut leaves)?;
    let observation = inventory_observation(&canonical_root, &leaves)?;
    Ok(BoundInventory {
        observation,
        leaves,
    })
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

fn bind_session_lineage(leaves: &mut [RovoDevSourceBackedLeaf]) -> RovoDevSourceBackedResult<()> {
    let parents = leaves
        .iter()
        .map(|leaf| {
            (
                leaf.provider_session_id().to_owned(),
                leaf.snapshot
                    .document
                    .as_ref()
                    .ok()
                    .and_then(|document| document.parent_provider_session_id.clone()),
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

fn inventory_observation(
    canonical_root: &Path,
    leaves: &[RovoDevSourceBackedLeaf],
) -> RovoDevSourceBackedResult<SourceInventoryObservation> {
    let mut digest = Sha256::new();
    digest.update(b"ctx.rovodev.source-backed.inventory-v1\0");
    digest.update(
        u64::try_from(leaves.len())
            .map_err(|_| RovoDevSourceBackedError::CountMismatch)?
            .to_be_bytes(),
    );
    for leaf in leaves {
        let relative = leaf
            .snapshot
            .frozen
            .canonical_path()
            .strip_prefix(canonical_root)
            .map_err(|_| RovoDevSourceBackedError::NonAuthoritativeRoot)?;
        let relative = relative.as_os_str().as_encoded_bytes();
        digest.update(
            u64::try_from(relative.len())
                .map_err(|_| RovoDevSourceBackedError::CountMismatch)?
                .to_be_bytes(),
        );
        digest.update(relative);
        digest.update(leaf.source_key.identity().digest());
        digest.update(leaf.snapshot.frozen.revision_authority());
        digest.update(leaf.snapshot.source_sha256);
    }
    let mut revision = Vec::with_capacity(40);
    revision.extend_from_slice(
        &u64::try_from(leaves.len())
            .map_err(|_| RovoDevSourceBackedError::CountMismatch)?
            .to_be_bytes(),
    );
    revision.extend_from_slice(&digest.finalize());
    Ok(SourceInventoryObservation::new(
        CaptureProvider::RovoDev.as_str(),
        INVENTORY_AUTHORITY_NAMESPACE,
        TypedKey::bytes(canonical_root.as_os_str().as_encoded_bytes().to_vec())?,
        INVENTORY_REVISION_KIND,
        revision,
    )?)
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

fn explicit_message_id(message: &serde_json::Value) -> Option<&str> {
    ["id", "message_id", "messageId", "request_id", "requestId"]
        .into_iter()
        .find_map(|field| message.get(field).and_then(serde_json::Value::as_str))
        .filter(|value| !value.trim().is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RovoDevSourceBackedDisposition {
    Cold,
    Replacement,
    Unchanged,
}

#[derive(Debug)]
pub(crate) struct RovoDevSourceBackedPage {
    pub(crate) documents: Vec<LexicalDocument>,
    pub(crate) complete_records: u64,
    pub(crate) retained_records: u64,
    pub(crate) rejected_records: u64,
    pub(crate) ignored_records: u64,
}

#[derive(Debug)]
pub(crate) struct RovoDevSourceBackedScan {
    pub(crate) disposition: RovoDevSourceBackedDisposition,
    pub(crate) source: CertifiedSource,
}

pub(crate) struct RovoDevSourceBackedReader<'a> {
    leaf: &'a RovoDevSourceBackedLeaf,
    context: ProviderAdapterContext,
    disposition: RovoDevSourceBackedDisposition,
    previous: Option<CertifiedSource>,
    next_message: usize,
    emitted_failure_document: bool,
    terminal: bool,
    counts: ScannedSourceCounts,
}

impl<'a> RovoDevSourceBackedReader<'a> {
    pub(crate) fn new(
        leaf: &'a RovoDevSourceBackedLeaf,
        context: ProviderAdapterContext,
        previous: Option<&CertifiedSource>,
    ) -> RovoDevSourceBackedResult<Self> {
        let disposition = match previous {
            None => RovoDevSourceBackedDisposition::Cold,
            Some(previous) => {
                previous.validate_contract()?;
                leaf.source_key
                    .validate_exact_descriptor(previous.observation().source())?;
                if previous.parser_revision() == PARSER_REVISION
                    && previous.observation()
                        == &leaf.snapshot.observation(leaf.source_key.clone())?
                    && previous.content_digest() == &leaf.snapshot.source_sha256
                {
                    RovoDevSourceBackedDisposition::Unchanged
                } else {
                    RovoDevSourceBackedDisposition::Replacement
                }
            }
        };
        Ok(Self {
            leaf,
            context,
            disposition,
            previous: previous.cloned(),
            next_message: 0,
            emitted_failure_document: false,
            terminal: disposition == RovoDevSourceBackedDisposition::Unchanged,
            counts: ScannedSourceCounts::default(),
        })
    }

    pub(crate) fn next_page(
        &mut self,
    ) -> RovoDevSourceBackedResult<Option<RovoDevSourceBackedPage>> {
        if self.terminal {
            return Ok(None);
        }
        let document = match self.leaf.snapshot.document.as_ref() {
            Ok(document) => document,
            Err(_) => {
                if self.emitted_failure_document {
                    self.terminal = true;
                    return Ok(None);
                }
                self.emitted_failure_document = true;
                self.terminal = true;
                let page = RovoDevSourceBackedPage {
                    documents: Vec::new(),
                    complete_records: 1,
                    retained_records: 0,
                    rejected_records: 1,
                    ignored_records: 0,
                };
                self.add_page_counts(&page)?;
                return Ok(Some(page));
            }
        };
        let start = self.next_message;
        if start > document.messages.len() {
            return Err(RovoDevSourceBackedError::CoordinateOverflow);
        }
        let mut documents = Vec::new();
        let mut rejected_records = if start == 0 {
            document.initial_failure_count
        } else {
            0
        };
        let mut ignored_records = 0_u64;
        let mut retained_bytes = 0_usize;
        let mut next = start;
        while next < document.messages.len()
            && next.saturating_sub(start) < SOURCE_BACKED_PAGE_MAX_RECORDS
        {
            let raw = &document.messages[next];
            let serialized_bytes = serde_json::to_vec(raw)
                .map_err(|error| RovoDevSourceBackedError::Capture(error.into()))?
                .len();
            if next > start
                && retained_bytes.saturating_add(serialized_bytes) > SOURCE_BACKED_PAGE_MAX_BYTES
            {
                break;
            }
            next = next
                .checked_add(1)
                .ok_or(RovoDevSourceBackedError::CoordinateOverflow)?;
            if serialized_bytes > SOURCE_BACKED_PAGE_MAX_BYTES {
                rejected_records = checked_add(rejected_records, 1)?;
                continue;
            }
            retained_bytes = retained_bytes.saturating_add(serialized_bytes);
            match project_message(raw, next.saturating_sub(1), document) {
                Err(_) => rejected_records = checked_add(rejected_records, 1)?,
                Ok(None) => ignored_records = checked_add(ignored_records, 1)?,
                Ok(Some(event)) => {
                    if event.touch_limit_exceeded {
                        rejected_records = checked_add(rejected_records, 1)?;
                    }
                    documents.push(lexical_document(
                        self.leaf,
                        document,
                        raw,
                        next.saturating_sub(1),
                        event,
                    )?);
                }
            }
        }
        let retained_records =
            u64::try_from(documents.len()).map_err(|_| RovoDevSourceBackedError::CountMismatch)?;
        let complete_records = retained_records
            .checked_add(rejected_records)
            .and_then(|count| count.checked_add(ignored_records))
            .ok_or(RovoDevSourceBackedError::CountMismatch)?;
        self.next_message = next;
        self.terminal = next == document.messages.len();
        let page = RovoDevSourceBackedPage {
            documents,
            complete_records,
            retained_records,
            rejected_records,
            ignored_records,
        };
        self.add_page_counts(&page)?;
        Ok(Some(page))
    }

    pub(crate) fn finish(mut self) -> RovoDevSourceBackedResult<RovoDevSourceBackedScan> {
        if !self.terminal {
            if self.disposition == RovoDevSourceBackedDisposition::Unchanged {
                self.terminal = true;
            } else {
                return Err(RovoDevSourceBackedError::IncompleteScan);
            }
        }
        self.leaf.snapshot.revalidate(&self.leaf.authority)?;
        let closing = RovoDevSnapshot::read(
            &self.leaf.source,
            &self.context,
            &self.leaf.authority,
            &self.leaf.session_relative_path,
            &self.leaf.context_relative_path,
            self.leaf.metadata_relative_path.as_deref(),
        )?;
        closing.revalidate(&self.leaf.authority)?;
        let opening_observation = self
            .leaf
            .snapshot
            .observation(self.leaf.source_key.clone())?;
        let closing_observation = closing.observation(self.leaf.source_key.clone())?;
        let counts = if self.disposition == RovoDevSourceBackedDisposition::Unchanged {
            self.previous
                .as_ref()
                .ok_or(RovoDevSourceBackedError::CountMismatch)?
                .counts()
        } else {
            self.counts.certified_bytes = self.leaf.snapshot.certified_bytes;
            self.counts
        };
        let frontier = final_frontier(&self.leaf.snapshot)?;
        let source = CertifiedSource::certify_with_frontier(
            opening_observation,
            closing_observation,
            PARSER_REVISION,
            self.leaf.snapshot.source_sha256,
            counts,
            Some(frontier),
        )?;
        Ok(RovoDevSourceBackedScan {
            disposition: self.disposition,
            source,
        })
    }

    fn add_page_counts(&mut self, page: &RovoDevSourceBackedPage) -> RovoDevSourceBackedResult<()> {
        self.counts.complete_records =
            checked_add(self.counts.complete_records, page.complete_records)?;
        self.counts.retained_records =
            checked_add(self.counts.retained_records, page.retained_records)?;
        self.counts.rejected_records =
            checked_add(self.counts.rejected_records, page.rejected_records)?;
        self.counts.ignored_records =
            checked_add(self.counts.ignored_records, page.ignored_records)?;
        self.counts.indexed_documents = checked_add(
            self.counts.indexed_documents,
            u64::try_from(page.documents.len())
                .map_err(|_| RovoDevSourceBackedError::CountMismatch)?,
        )?;
        Ok(())
    }
}

fn checked_add(left: u64, right: u64) -> RovoDevSourceBackedResult<u64> {
    left.checked_add(right)
        .ok_or(RovoDevSourceBackedError::CountMismatch)
}

fn final_frontier(snapshot: &RovoDevSnapshot) -> RovoDevSourceBackedResult<SourceFrontier> {
    Ok(SourceFrontier::new(
        FRONTIER_KIND,
        TypedKey::composite(vec![
            TypedKey::U64(
                u64::try_from(snapshot.message_count())
                    .map_err(|_| RovoDevSourceBackedError::CountMismatch)?,
            ),
            TypedKey::bytes(snapshot.source_sha256.to_vec())?,
        ])?,
        snapshot.certified_bytes,
        snapshot.source_sha256,
    )?)
}

fn lexical_document(
    leaf: &RovoDevSourceBackedLeaf,
    document: &PreparedDocument,
    raw_message: &serde_json::Value,
    index: usize,
    event: ProjectedMessage,
) -> RovoDevSourceBackedResult<LexicalDocument> {
    let native_item_key = native_item_key(leaf, raw_message, index)?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &leaf.source_key,
        session_id: leaf.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let message_index =
        u64::try_from(index).map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?;
    let native_record_id = provider_message_id(raw_message, message_index);
    let locator = SourceRecordLocator::new(
        leaf.source_key.clone(),
        NativeRecordCoordinate::TreeRecord {
            relative_file_key: TypedKey::utf8(RELATIVE_CONTEXT_FILE)?,
            record_coordinate: TypedKey::composite(vec![
                TypedKey::utf8(MESSAGE_OBJECT_KIND)?,
                TypedKey::U64(message_index),
                TypedKey::utf8(&native_record_id)?,
            ])?,
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(leaf.snapshot.source_sha256),
        leaf.snapshot.context_sha256,
    )?;
    let body = lexical_body(raw_message, event.event_type);
    Ok(LexicalDocument {
        event_id,
        session_id: leaf.session_id,
        parent_session_id: leaf.parent_session_id,
        root_session_id: leaf.root_session_id,
        source: leaf.source_key.clone(),
        locator,
        provider_session_id: Some(document.provider_session_id.clone()),
        branch: provider_string_field(
            &document.metadata,
            &[
                "branch",
                "git_branch",
                "gitBranch",
                "vcs_branch",
                "vcsBranch",
            ],
        )
        .or_else(|| document.context_branch.clone()),
        source_path: Some(leaf.source.context_path.display().to_string()),
        agent_type: if document.parent_provider_session_id.is_some() {
            AgentType::Subagent
        } else {
            AgentType::Primary
        }
        .as_str()
        .to_owned(),
        is_primary: document.parent_provider_session_id.is_none(),
        event_sequence: message_index,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body,
        workspace: document.cwd.clone(),
        cwd: document.cwd.clone(),
        touched_files: event.touched_files,
    })
}

fn native_item_key(
    leaf: &RovoDevSourceBackedLeaf,
    message: &serde_json::Value,
    index: usize,
) -> RovoDevSourceBackedResult<NativeItemKey> {
    if let Some(native_id) = explicit_message_id(message)
        .filter(|native_id| leaf.unique_message_ids.contains(*native_id))
    {
        return Ok(NativeItemKey::native_id(
            EVENT_KEY_NAMESPACE,
            TypedKey::utf8(native_id)?,
        )?);
    }
    let coordinate = TypedKey::composite(vec![
        explicit_message_id(message)
            .map(TypedKey::utf8)
            .transpose()?
            .unwrap_or(TypedKey::Null),
        TypedKey::U64(
            u64::try_from(index).map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?,
        ),
    ])?;
    Ok(NativeItemKey::revision_scoped_position(
        EVENT_POSITION_KIND,
        coordinate,
        TypedKey::bytes(leaf.snapshot.source_sha256.to_vec())?,
    )?)
}

fn lexical_body(
    raw_message: &serde_json::Value,
    event_type: ctx_history_core::EventType,
) -> String {
    let text = provider_block_text(raw_message).unwrap_or_default();
    if text.trim().is_empty() {
        event_type.as_str().to_owned()
    } else {
        text
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RovoDevHydratedSourceRecord {
    pub(crate) provider_bytes: Vec<u8>,
    pub(crate) decoded_display_text: Option<String>,
}

pub(crate) fn hydrate_rovodev_source_record(
    inventory: &RovoDevSourceBackedInventory,
    _event_id: StableEntityId,
    locator: &SourceRecordLocator,
) -> RovoDevSourceBackedResult<RovoDevHydratedSourceRecord> {
    locator.validate_contract()?;
    let leaf = inventory
        .leaves
        .iter()
        .find(|leaf| leaf.source_key.exact_descriptor_eq(locator.source()))
        .ok_or(RovoDevSourceBackedError::LocatorSourceMissing)?;
    if locator.source().provider() != CaptureProvider::RovoDev.as_str()
        || locator.source().source_format() != ROVODEV_SOURCE_FORMAT
        || locator.source().schema_variant() != SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
    {
        return Err(RovoDevSourceBackedError::LocatorSourceChanged);
    }
    let current = RovoDevSnapshot::read(
        &leaf.source,
        &inventory.context,
        &leaf.authority,
        &leaf.session_relative_path,
        &leaf.context_relative_path,
        leaf.metadata_relative_path.as_deref(),
    )?;
    if locator.certified_source_revision_digest() != Some(&current.source_sha256)
        || locator.record_digest() != &current.context_sha256
    {
        return Err(RovoDevSourceBackedError::LocatorSourceChanged);
    }
    let (message_index, expected_native_id) = decode_tree_coordinate(locator.coordinate())?;
    let document = current
        .document
        .as_ref()
        .map_err(|_| RovoDevSourceBackedError::LocatorObjectChanged)?;
    let message = document
        .messages
        .get(message_index)
        .ok_or(RovoDevSourceBackedError::LocatorObjectChanged)?;
    let observed_native_id = provider_message_id(
        message,
        u64::try_from(message_index).map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?,
    );
    if observed_native_id != expected_native_id {
        return Err(RovoDevSourceBackedError::LocatorObjectChanged);
    }
    let role_text = message
        .get("role")
        .or_else(|| message.get("kind"))
        .or_else(|| message.get("type"))
        .and_then(serde_json::Value::as_str);
    let decoded_display_text = lexical_body(message, rovodev_event_type(message, role_text));
    let provider_bytes = decoded_display_text.as_bytes().to_vec();
    current.revalidate(&leaf.authority)?;
    let closing = RovoDevSnapshot::read(
        &leaf.source,
        &inventory.context,
        &leaf.authority,
        &leaf.session_relative_path,
        &leaf.context_relative_path,
        leaf.metadata_relative_path.as_deref(),
    )?;
    closing.revalidate(&leaf.authority)?;
    if closing.source_sha256 != current.source_sha256
        || closing.context_sha256 != current.context_sha256
    {
        return Err(RovoDevSourceBackedError::LocatorSourceChanged);
    }
    Ok(RovoDevHydratedSourceRecord {
        provider_bytes,
        decoded_display_text: Some(decoded_display_text),
    })
}

fn decode_tree_coordinate(
    coordinate: &NativeRecordCoordinate,
) -> RovoDevSourceBackedResult<(usize, String)> {
    let NativeRecordCoordinate::TreeRecord {
        relative_file_key,
        record_coordinate,
    } = coordinate
    else {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    };
    let TypedKey::Utf8(relative_file) = relative_file_key else {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    };
    let TypedKey::Composite(parts) = record_coordinate else {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    };
    let [TypedKey::Utf8(object_kind), TypedKey::U64(message_index), TypedKey::Utf8(native_id)] =
        parts.as_slice()
    else {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    };
    if relative_file != RELATIVE_CONTEXT_FILE
        || object_kind != MESSAGE_OBJECT_KIND
        || native_id.is_empty()
    {
        return Err(RovoDevSourceBackedError::InvalidLocator);
    }
    Ok((
        usize::try_from(*message_index)
            .map_err(|_| RovoDevSourceBackedError::CoordinateOverflow)?,
        native_id.clone(),
    ))
}

#[cfg(test)]
#[path = "source_backed/tests.rs"]
mod tests;
