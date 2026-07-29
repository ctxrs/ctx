//! Thin source-backed projection for Auggie's whole-session JSON documents.
//!
//! Auggie owns the complete content in `sessions/*.json`. This adapter emits
//! policy-selected lexical records plus exact document coordinates; lifecycle and
//! publication remain shared coordinator responsibilities.

use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use std::fs;

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, EventIdentityInput, LocatorRevisionPolicy,
    NativeItemKey, NativeRecordCoordinate, NativeSessionKey, ProjectionContractError,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation,
    SourceRecordLocator, SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    model::{
        ParsedAuggieEvent, ParsedAuggieSession, ParsedAuggieSource, AUGGIE_MAX_DISCOVERED_FILES,
        AUGGIE_PARSER_REVISION,
    },
    normalized_auggie_authority_path,
    parse::{parse_auggie_source, parse_opened_auggie_source},
    source::{invalid_source_path, AuggieFileStamp},
};
use crate::{
    common::io::{open_provider_source_path, OpenedProviderSourcePath, ProviderSourceRoot},
    provider::providers::auggie::{auggie_request_text, auggie_response_text},
    CaptureError, ProviderAdapterContext, AUGGIE_SESSION_JSON_SOURCE_FORMAT,
    MAX_PROVIDER_JSONL_LINE_BYTES,
};

const AUGGIE_SOURCE_ANCHOR_NAMESPACE: &str = "auggie.session";
const AUGGIE_NATIVE_SESSION_NAMESPACE: &str = "auggie.session";
const AUGGIE_NATIVE_EVENT_NAMESPACE: &str = "auggie.request-part";
const AUGGIE_EVENT_POSITION_KIND: &str = "auggie.chat-history-position";
const AUGGIE_LOGICAL_SESSION_KIND: &str = "auggie-session";
const AUGGIE_LOGICAL_EVENT_KIND: &str = "auggie-message";
const AUGGIE_SOURCE_SCHEMA_VARIANT: &str = "auggie-structured-session-v1";
const AUGGIE_SOURCE_REVISION_KIND: &str = "auggie-ordinary-file-observation-v1";

#[derive(Debug, Error)]
pub(crate) enum AuggieSourceBackedError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("Auggie source-backed inventory contains duplicate native session ID {0:?}")]
    DuplicateNativeSessionId(String),
    #[error("Auggie source contains duplicate stable event identity {0}")]
    DuplicateEventIdentity(StableEntityId),
    #[error("Auggie source-backed event has no meaningful lexical text")]
    MissingLexicalText,
    #[error("locator is not an Auggie structured-session document record")]
    InvalidLocator,
    #[error("Auggie locator source revision no longer matches provider bytes")]
    SourceRevisionChanged,
    #[error("Auggie locator document digest no longer matches provider bytes")]
    LocatorDigestMismatch,
    #[error("Auggie locator JSON pointer no longer resolves to its native message")]
    LocatorRecordMissing,
}

pub(crate) type AuggieSourceBackedResult<T> = Result<T, AuggieSourceBackedError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuggieSourceBackedRootKind {
    DefaultHome,
    Explicit,
}

/// One selected Auggie authority root.
///
/// Automatic discovery always resolves only `~/.augment/sessions`. A one-shot
/// `--augment-cache-dir` root is accepted only through [`Self::explicit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuggieSourceBackedRoot {
    path: PathBuf,
    kind: AuggieSourceBackedRootKind,
}

impl AuggieSourceBackedRoot {
    pub(crate) fn default_for_home(home: impl AsRef<Path>) -> Self {
        Self {
            path: home.as_ref().join(".augment").join("sessions"),
            kind: AuggieSourceBackedRootKind::DefaultHome,
        }
    }

    pub(crate) fn explicit(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            kind: AuggieSourceBackedRootKind::Explicit,
        }
    }

    pub(crate) fn configured_path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn is_explicit(&self) -> bool {
        self.kind == AuggieSourceBackedRootKind::Explicit
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuggieSourceBackedInventoryStatus {
    Complete,
    Unavailable,
}

/// Provider-local inventory only. Shared code owns deletion certification.
#[derive(Debug, Clone)]
pub(crate) struct AuggieSourceBackedInventory {
    pub(crate) authority_root: PathBuf,
    pub(crate) status: AuggieSourceBackedInventoryStatus,
    pub(crate) paths: Vec<PathBuf>,
    authority: Option<ProviderSourceRoot>,
}

impl AuggieSourceBackedInventory {
    pub(crate) fn is_complete(&self) -> bool {
        self.status == AuggieSourceBackedInventoryStatus::Complete
    }

    fn open_source(&self, path: &Path) -> AuggieSourceBackedResult<AuggieFileStamp> {
        let authority = self
            .authority
            .as_ref()
            .ok_or(CaptureError::SystemInvariant(
                "complete Auggie source-backed inventory has no retained authority",
            ))?;
        let relative = path.strip_prefix(authority.named_path()).map_err(|_| {
            invalid_source_path(path, "Auggie source escaped its retained authority root")
        })?;
        let opened = authority.open_file(relative)?;
        AuggieFileStamp::from_opened(path.to_path_buf(), opened).map_err(Into::into)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AuggieSourceBackedSource {
    pub(crate) path: PathBuf,
    pub(crate) certified_source: CertifiedSource,
    pub(crate) documents: Vec<LexicalDocument>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuggieHydratedSourceRecord {
    /// Exact provider-owned message bytes selected by the typed document locator.
    pub(crate) provider_bytes: Vec<u8>,
    pub(crate) decoded_display_text: String,
}

/// Enumerates only direct `*.json` children of the selected sessions root.
///
/// A missing authority is unavailable, not a complete empty inventory. An
/// existing empty directory is complete and may be used by shared lifecycle
/// code as one input to deletion certification.
pub(crate) fn discover_auggie_source_backed(
    root: &AuggieSourceBackedRoot,
) -> AuggieSourceBackedResult<AuggieSourceBackedInventory> {
    let selected = normalized_auggie_authority_path(&selected_sessions_path(root)?)?;
    let opened = match open_provider_source_path(&selected) {
        Ok(opened) => opened,
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(AuggieSourceBackedInventory {
                authority_root: selected,
                status: AuggieSourceBackedInventoryStatus::Unavailable,
                paths: Vec::new(),
                authority: None,
            });
        }
        Err(error) => return Err(error.into()),
    };

    if let OpenedProviderSourcePath::File(opened_file) = opened {
        let file_name = selected.file_name().ok_or_else(|| {
            invalid_source_path(&selected, "Auggie source file has no final component")
        })?;
        let parent = selected.parent().ok_or_else(|| {
            invalid_source_path(&selected, "Auggie source file has no authority parent")
        })?;
        let authority = ProviderSourceRoot::open(parent)?;
        let selected = authority.named_path().join(file_name);
        authority.open_file(Path::new(file_name))?.revalidate()?;
        opened_file.revalidate()?;
        let paths = if is_json_path(&selected) {
            vec![selected.clone()]
        } else {
            Vec::new()
        };
        return Ok(AuggieSourceBackedInventory {
            authority_root: selected,
            status: AuggieSourceBackedInventoryStatus::Complete,
            paths,
            authority: Some(authority),
        });
    }
    let OpenedProviderSourcePath::Directory(directory) = opened else {
        return Err(CaptureError::SystemInvariant(
            "Auggie source-backed root classification is incomplete",
        )
        .into());
    };
    let authority = directory.authority_root();
    let entries = directory.entries(AUGGIE_MAX_DISCOVERED_FILES.saturating_add(1))?;
    let mut paths = Vec::new();
    for name in entries {
        let path = authority.named_path().join(&name);
        match directory.open_child(&name)? {
            OpenedProviderSourcePath::File(opened) if is_json_path(&path) => {
                opened.revalidate()?;
                paths.push(path);
            }
            OpenedProviderSourcePath::File(_) | OpenedProviderSourcePath::Directory(_) => {}
        }
        if paths.len() > AUGGIE_MAX_DISCOVERED_FILES {
            return Err(invalid_source_path(
                authority.named_path(),
                "Auggie source-backed discovery exceeds the file bound",
            )
            .into());
        }
    }
    directory.revalidate()?;
    authority.revalidate()?;
    Ok(AuggieSourceBackedInventory {
        authority_root: authority.named_path().to_path_buf(),
        status: AuggieSourceBackedInventoryStatus::Complete,
        paths,
        authority: Some(authority),
    })
}

/// Parses and projects one certified Auggie session document.
pub(crate) fn project_auggie_source_backed(
    path: &Path,
    context: &ProviderAdapterContext,
) -> AuggieSourceBackedResult<AuggieSourceBackedSource> {
    let parsed = parse_auggie_source(path, context)?;
    project_parsed_auggie_source_backed(parsed)
}

fn project_opened_auggie_source_backed(
    stamp: AuggieFileStamp,
    context: &ProviderAdapterContext,
) -> AuggieSourceBackedResult<AuggieSourceBackedSource> {
    let parsed = parse_opened_auggie_source(stamp, context)?;
    project_parsed_auggie_source_backed(parsed)
}

fn project_parsed_auggie_source_backed(
    parsed: ParsedAuggieSource,
) -> AuggieSourceBackedResult<AuggieSourceBackedSource> {
    let source = auggie_source_key(&parsed.session.provider_session_id)?;
    let session_id = auggie_session_id(&source, &parsed.session.provider_session_id)?;
    let mut documents = Vec::with_capacity(parsed.events.len());
    let mut event_ids = HashSet::with_capacity(parsed.events.len());
    for event in parsed.events {
        let document = auggie_lexical_document(
            &source,
            session_id,
            &parsed.session,
            parsed.content_digest,
            event,
        )?;
        if !event_ids.insert(document.event_id) {
            return Err(AuggieSourceBackedError::DuplicateEventIdentity(
                document.event_id,
            ));
        }
        documents.push(document);
    }
    let indexed_documents = u64::try_from(documents.len())
        .map_err(|_| CaptureError::InvalidPayload("too many Auggie messages".to_owned()))?;
    let observation = auggie_source_observation(&source, &parsed.stamp)?;
    let certified_source = CertifiedSource::certify(
        observation.clone(),
        observation,
        AUGGIE_PARSER_REVISION,
        parsed.content_digest,
        ScannedSourceCounts {
            complete_records: indexed_documents,
            retained_records: indexed_documents,
            rejected_records: 0,
            ignored_records: 0,
            indexed_documents,
            certified_bytes: parsed.stamp.len,
        },
    )?;
    Ok(AuggieSourceBackedSource {
        path: parsed.stamp.canonical_path,
        certified_source,
        documents,
    })
}

/// Projects every source in a complete inventory and rejects duplicate native
/// session ownership before returning any provider rows.
pub(crate) fn project_auggie_source_backed_inventory(
    inventory: &AuggieSourceBackedInventory,
    context: &ProviderAdapterContext,
) -> AuggieSourceBackedResult<Vec<AuggieSourceBackedSource>> {
    if !inventory.is_complete() {
        return Ok(Vec::new());
    }
    let mut source_ids = HashSet::with_capacity(inventory.paths.len());
    let mut sources = Vec::with_capacity(inventory.paths.len());
    for path in &inventory.paths {
        let source = project_opened_auggie_source_backed(inventory.open_source(path)?, context)?;
        let provider_session_id = source
            .documents
            .first()
            .and_then(|document| document.provider_session_id.as_deref())
            .map(str::to_owned)
            .or_else(|| source_session_id_from_key(&source.certified_source).ok())
            .ok_or(AuggieSourceBackedError::InvalidLocator)?;
        if !source_ids.insert(provider_session_id.clone()) {
            return Err(AuggieSourceBackedError::DuplicateNativeSessionId(
                provider_session_id,
            ));
        }
        sources.push(source);
    }
    if let Some(authority) = inventory.authority.as_ref() {
        authority.revalidate()?;
    }
    Ok(sources)
}

/// Rehydrates one exact message from the provider-owned JSON document.
pub(crate) fn hydrate_auggie_source_backed(
    path: &Path,
    locator: &SourceRecordLocator,
) -> AuggieSourceBackedResult<AuggieHydratedSourceRecord> {
    locator.validate_contract()?;
    let (expected_session_id, expected_event_key, chat_index, message_kind, json_pointer) =
        validate_auggie_locator(locator)?;
    let before = AuggieFileStamp::observe(path)?;
    let maximum = u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES).unwrap_or(u64::MAX);
    if before.len > maximum {
        return Err(CaptureError::InvalidPayload(format!(
            "Auggie session JSON exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"
        ))
        .into());
    }
    let provider_bytes = before.read_all_bounded(MAX_PROVIDER_JSONL_LINE_BYTES)?;
    if u64::try_from(provider_bytes.len()).unwrap_or(u64::MAX) != before.len
        || !before.revalidate()?
    {
        return Err(CaptureError::SourceChangedDuringCapture.into());
    }
    let document_digest: [u8; 32] = Sha256::digest(&provider_bytes).into();
    if locator.certified_source_revision_digest() != Some(&document_digest) {
        return Err(AuggieSourceBackedError::SourceRevisionChanged);
    }
    if locator.record_digest() != &document_digest {
        return Err(AuggieSourceBackedError::LocatorDigestMismatch);
    }

    let root: Value = serde_json::from_slice(&provider_bytes)?;
    let actual_session_id = root
        .get("sessionId")
        .or_else(|| root.get("session_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(AuggieSourceBackedError::LocatorRecordMissing)?;
    if actual_session_id != expected_session_id {
        return Err(AuggieSourceBackedError::LocatorRecordMissing);
    }
    let expected_pointer = auggie_message_pointer(&root, chat_index)?;
    if json_pointer != expected_pointer {
        return Err(AuggieSourceBackedError::InvalidLocator);
    }
    let exchange = root
        .pointer(json_pointer)
        .ok_or(AuggieSourceBackedError::LocatorRecordMissing)?;
    let actual_event_key = auggie_native_event_key(exchange, chat_index, message_kind);
    if actual_event_key != expected_event_key {
        return Err(AuggieSourceBackedError::LocatorRecordMissing);
    }
    let decoded_display_text = match message_kind {
        "request" => auggie_request_text(exchange),
        "response" => auggie_response_text(exchange),
        _ => return Err(AuggieSourceBackedError::InvalidLocator),
    }
    .ok_or(AuggieSourceBackedError::LocatorRecordMissing)?;

    Ok(AuggieHydratedSourceRecord {
        provider_bytes: decoded_display_text.as_bytes().to_vec(),
        decoded_display_text,
    })
}

fn selected_sessions_path(root: &AuggieSourceBackedRoot) -> AuggieSourceBackedResult<PathBuf> {
    if !root.is_explicit() {
        return normalized_auggie_authority_path(&root.path).map_err(Into::into);
    }
    let root_path = normalized_auggie_authority_path(&root.path)?;
    let opened = match open_provider_source_path(&root_path) {
        Ok(opened) => opened,
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(root_path);
        }
        Err(error) => return Err(error.into()),
    };
    if let OpenedProviderSourcePath::Directory(directory) = opened {
        let sessions = root_path.join("sessions");
        match directory.open_child(std::ffi::OsStr::new("sessions")) {
            Ok(OpenedProviderSourcePath::Directory(child)) => {
                child.revalidate()?;
                directory.revalidate()?;
                return Ok(sessions);
            }
            Ok(OpenedProviderSourcePath::File(_)) => {}
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(root_path)
}

fn is_json_path(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("json")
}

fn auggie_source_key(native_session_id: &str) -> AuggieSourceBackedResult<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        AUGGIE_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(SourceKey::derive(
        ctx_history_core::CaptureProvider::Auggie.as_str(),
        AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        AUGGIE_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn auggie_session_id(
    source: &SourceKey,
    native_session_id: &str,
) -> AuggieSourceBackedResult<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        AUGGIE_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: AUGGIE_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

fn auggie_lexical_document(
    source: &SourceKey,
    session_id: StableEntityId,
    session: &ParsedAuggieSession,
    content_digest: [u8; 32],
    parsed: ParsedAuggieEvent,
) -> AuggieSourceBackedResult<LexicalDocument> {
    let parent_session_id = session
        .parent_provider_session_id
        .as_deref()
        .map(related_auggie_session_id)
        .transpose()?;
    let root_session_id = session
        .root_provider_session_id
        .as_deref()
        .map(related_auggie_session_id)
        .transpose()?
        .or(parent_session_id)
        .unwrap_or(session_id);
    let native_item_key = if let Some(native_event_id) = parsed.native_event_id.as_deref() {
        NativeItemKey::native_id(
            AUGGIE_NATIVE_EVENT_NAMESPACE,
            TypedKey::utf8(native_event_id)?,
        )?
    } else {
        NativeItemKey::revision_scoped_position(
            AUGGIE_EVENT_POSITION_KIND,
            TypedKey::composite(vec![
                TypedKey::U64(u64::try_from(parsed.chat_index).map_err(|_| {
                    CaptureError::InvalidPayload("Auggie chat history index exceeds u64".to_owned())
                })?),
                TypedKey::utf8(parsed.message_kind)?,
            ])?,
            TypedKey::bytes(content_digest.to_vec())?,
        )?
    };
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: AUGGIE_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let chat_index = u64::try_from(parsed.chat_index).map_err(|_| {
        CaptureError::InvalidPayload("Auggie chat history index exceeds u64".to_owned())
    })?;
    let object_key = TypedKey::composite(vec![
        TypedKey::utf8(&parsed.provider_event_hash)?,
        TypedKey::U64(chat_index),
        TypedKey::utf8(parsed.message_kind)?,
    ])?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::Document {
            object_key,
            json_pointer: Some(parsed.json_pointer),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(content_digest),
        content_digest,
    )?;
    let body = (!parsed.text.is_empty())
        .then_some(parsed.text)
        .ok_or(AuggieSourceBackedError::MissingLexicalText)?;
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id,
        root_session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(session.provider_session_id.clone()),
        branch: None,
        source_path: Some(session.raw_source_path.clone()),
        agent_type: ctx_history_core::AgentType::Primary.as_str().to_owned(),
        is_primary: true,
        event_sequence: parsed.provider_event_index,
        occurred_at_unix_ms: Some(parsed.occurred_at.timestamp_millis()),
        event_type: parsed.event_type.as_str().to_owned(),
        role: Some(parsed.role.as_str().to_owned()),
        body,
        workspace: session.cwd.clone(),
        cwd: session.cwd.clone(),
        touched_files: Vec::new(),
    })
}

fn related_auggie_session_id(native_session_id: &str) -> AuggieSourceBackedResult<StableEntityId> {
    let source = auggie_source_key(native_session_id)?;
    auggie_session_id(&source, native_session_id)
}

fn auggie_source_observation(
    source: &SourceKey,
    stamp: &AuggieFileStamp,
) -> AuggieSourceBackedResult<SourceObservation> {
    Ok(SourceObservation::new(
        source.clone(),
        AUGGIE_SOURCE_REVISION_KIND,
        auggie_stamp_revision(stamp),
    )?)
}

fn auggie_stamp_revision(stamp: &AuggieFileStamp) -> Vec<u8> {
    let mut revision = Vec::with_capacity(42);
    revision.extend_from_slice(&stamp.len.to_be_bytes());
    let (sign, seconds, nanos) = system_time_parts(stamp.modified);
    revision.push(sign);
    revision.extend_from_slice(&seconds.to_be_bytes());
    revision.extend_from_slice(&nanos.to_be_bytes());
    revision.push(u8::from(stamp.readonly));
    revision.extend_from_slice(&stamp.device.unwrap_or_default().to_be_bytes());
    revision.extend_from_slice(&stamp.inode.unwrap_or_default().to_be_bytes());
    revision
}

fn system_time_parts(time: SystemTime) -> (u8, u64, u32) {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => (1, duration.as_secs(), duration.subsec_nanos()),
        Err(error) => {
            let duration = error.duration();
            (0, duration.as_secs(), duration.subsec_nanos())
        }
    }
}

fn validate_auggie_locator(
    locator: &SourceRecordLocator,
) -> AuggieSourceBackedResult<(String, String, u64, &str, &str)> {
    let source = locator.source();
    if source.provider() != ctx_history_core::CaptureProvider::Auggie.as_str()
        || source.source_format() != AUGGIE_SESSION_JSON_SOURCE_FORMAT
        || source.schema_variant() != AUGGIE_SOURCE_SCHEMA_VARIANT
        || source.provider_identity_version() != 1
        || locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
    {
        return Err(AuggieSourceBackedError::InvalidLocator);
    }
    let SourceAnchor::ProviderNative { namespace, key } = source.anchor() else {
        return Err(AuggieSourceBackedError::InvalidLocator);
    };
    let TypedKey::Utf8(native_session_id) = key else {
        return Err(AuggieSourceBackedError::InvalidLocator);
    };
    if namespace != AUGGIE_SOURCE_ANCHOR_NAMESPACE {
        return Err(AuggieSourceBackedError::InvalidLocator);
    }
    let NativeRecordCoordinate::Document {
        object_key,
        json_pointer: Some(json_pointer),
    } = locator.coordinate()
    else {
        return Err(AuggieSourceBackedError::InvalidLocator);
    };
    let TypedKey::Composite(parts) = object_key else {
        return Err(AuggieSourceBackedError::InvalidLocator);
    };
    let [TypedKey::Utf8(event_key), TypedKey::U64(chat_index), TypedKey::Utf8(message_kind)] =
        parts.as_slice()
    else {
        return Err(AuggieSourceBackedError::InvalidLocator);
    };
    if !matches!(message_kind.as_str(), "request" | "response") {
        return Err(AuggieSourceBackedError::InvalidLocator);
    }
    Ok((
        native_session_id.clone(),
        event_key.clone(),
        *chat_index,
        message_kind,
        json_pointer,
    ))
}

fn auggie_message_pointer(root: &Value, chat_index: u64) -> AuggieSourceBackedResult<String> {
    let chat_index =
        usize::try_from(chat_index).map_err(|_| AuggieSourceBackedError::InvalidLocator)?;
    let (history_key, entries) =
        if let Some(entries) = root.get("chatHistory").and_then(Value::as_array) {
            ("chatHistory", entries)
        } else if let Some(entries) = root.get("chat_history").and_then(Value::as_array) {
            ("chat_history", entries)
        } else {
            return Err(AuggieSourceBackedError::LocatorRecordMissing);
        };
    let entry = entries
        .get(chat_index)
        .ok_or(AuggieSourceBackedError::LocatorRecordMissing)?;
    Ok(if entry.get("exchange").is_some() {
        format!("/{history_key}/{chat_index}/exchange")
    } else {
        format!("/{history_key}/{chat_index}")
    })
}

fn auggie_native_event_key(exchange: &Value, chat_index: u64, message_kind: &str) -> String {
    exchange
        .get("request_id")
        .or_else(|| exchange.get("requestId"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|request_id| format!("{request_id}:{message_kind}"))
        .unwrap_or_else(|| format!("chat-{chat_index}:{message_kind}"))
}

fn source_session_id_from_key(source: &CertifiedSource) -> AuggieSourceBackedResult<String> {
    let SourceAnchor::ProviderNative { namespace, key } = source.observation().source().anchor()
    else {
        return Err(AuggieSourceBackedError::InvalidLocator);
    };
    let TypedKey::Utf8(session_id) = key else {
        return Err(AuggieSourceBackedError::InvalidLocator);
    };
    if namespace != AUGGIE_SOURCE_ANCHOR_NAMESPACE {
        return Err(AuggieSourceBackedError::InvalidLocator);
    }
    Ok(session_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support_paths::tempdir;
    use serde_json::json;

    fn context(path: &Path) -> ProviderAdapterContext {
        ProviderAdapterContext {
            machine_id: "auggie-source-backed-test".to_owned(),
            source_path: Some(path.to_path_buf()),
            source_root: None,
            imported_at: "2026-07-28T12:00:00Z".parse().unwrap(),
        }
    }

    fn write_session(path: &Path, request_text: &str, response_text: &str) {
        write_history(path, &[("request-stable-id", request_text, response_text)]);
    }

    fn write_history(path: &Path, exchanges: &[(&str, &str, &str)]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let chat_history = exchanges
            .iter()
            .enumerate()
            .map(|(index, (request_id, request_text, response_text))| {
                json!({
                    "exchange": {
                        "request_id": request_id,
                        "request_message": request_text,
                        "response_text": response_text,
                    },
                    "finishedAt": format!("2026-07-28T11:{:02}:00Z", index + 1),
                })
            })
            .collect::<Vec<_>>();
        fs::write(
            path,
            serde_json::to_vec(&json!({
                "sessionId": "auggie-source-session",
                "created": "2026-07-28T11:00:00Z",
                "workspaceRoot": "/workspace/auggie",
                "chatHistory": chat_history,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn cold_projection_is_stable_full_body_and_document_located() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("home/.augment/sessions");
        let path = sessions.join("session.json");
        let request_text = format!("full-prefix-{}-auggie-tail", "x".repeat(3_000));
        write_session(&path, &request_text, "bounded response");
        let root = AuggieSourceBackedRoot::default_for_home(temp.path().join("home"));
        let inventory = discover_auggie_source_backed(&root).unwrap();
        assert_eq!(
            inventory.status,
            AuggieSourceBackedInventoryStatus::Complete
        );
        assert_eq!(inventory.paths.len(), 1);

        let first = project_auggie_source_backed(&inventory.paths[0], &context(&sessions)).unwrap();
        let second =
            project_auggie_source_backed(&inventory.paths[0], &context(&sessions)).unwrap();
        assert_eq!(first.certified_source, second.certified_source);
        assert_eq!(first.documents.len(), 2);
        assert_eq!(
            first
                .documents
                .iter()
                .map(|document| document.event_id)
                .collect::<Vec<_>>(),
            second
                .documents
                .iter()
                .map(|document| document.event_id)
                .collect::<Vec<_>>()
        );
        assert_eq!(first.documents[0].body, request_text);
        assert!(first.documents[0].body.ends_with("auggie-tail"));
        for document in &first.documents {
            assert_eq!(document.parent_session_id, None);
            assert_eq!(document.root_session_id, document.session_id);
            assert_eq!(
                document.provider_session_id.as_deref(),
                Some("auggie-source-session")
            );
            assert_eq!(
                document.source_path.as_deref(),
                Some(first.path.to_string_lossy().as_ref())
            );
            assert_eq!(document.agent_type, "primary");
            assert!(document.is_primary);
            assert_eq!(document.branch, None);
            assert_eq!(
                document.locator.revision_policy(),
                LocatorRevisionPolicy::ExactSourceRevision
            );
            assert_eq!(
                document.locator.certified_source_revision_digest(),
                Some(first.certified_source.content_digest())
            );
            assert!(matches!(
                document.locator.coordinate(),
                NativeRecordCoordinate::Document {
                    object_key: TypedKey::Composite(_),
                    json_pointer: Some(pointer),
                } if pointer.starts_with("/chatHistory/0")
            ));
        }
    }

    #[test]
    fn exact_hydration_fails_closed_and_replacement_keeps_native_ids() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let path = sessions.join("session.json");
        write_session(&path, "before replacement", "stable response");
        let before = project_auggie_source_backed(&path, &context(&sessions)).unwrap();
        let old_request = &before.documents[0];
        let hydrated = hydrate_auggie_source_backed(&before.path, &old_request.locator).unwrap();
        assert_eq!(hydrated.decoded_display_text, "before replacement");
        assert_eq!(hydrated.provider_bytes, b"before replacement");

        write_session(&path, "after replacement", "stable response");
        let after = project_auggie_source_backed(&path, &context(&sessions)).unwrap();
        assert_eq!(
            before.documents[0].session_id,
            after.documents[0].session_id
        );
        assert_eq!(before.documents[0].event_id, after.documents[0].event_id);
        assert_ne!(
            before.certified_source.content_digest(),
            after.certified_source.content_digest()
        );
        assert!(matches!(
            hydrate_auggie_source_backed(&after.path, &old_request.locator),
            Err(AuggieSourceBackedError::SourceRevisionChanged)
                | Err(AuggieSourceBackedError::LocatorDigestMismatch)
        ));
        assert_eq!(
            hydrate_auggie_source_backed(&after.path, &after.documents[0].locator)
                .unwrap()
                .decoded_display_text,
            "after replacement"
        );
    }

    #[test]
    fn provider_b_source_backed_body_architecture_has_no_preview_or_store_contract() {
        let forbidden_preview_cap = ["MAX_BODY_PREVIEW", "_CHARS"].concat();
        let forbidden_legacy_field = ["lexical_", "preview"].concat();
        let forbidden_store = ["ctx_history_", "store::Store"].concat();
        let sources = [
            ("auggie", include_str!("source_backed.rs")),
            (
                "codebuddy",
                include_str!("../../codebuddy/native_path/source_backed.rs"),
            ),
            (
                "continue_cli",
                include_str!("../../continue_cli/native_path/source_backed.rs"),
            ),
            (
                "crush",
                include_str!("../../crush/native_path/source_backed.rs"),
            ),
            ("cursor", include_str!("../../cursor/source_backed.rs")),
            (
                "deepagents",
                include_str!("../../deepagents/native_path/source_backed.rs"),
            ),
            (
                "firebender",
                include_str!("../../firebender/native_path/source_backed.rs"),
            ),
            ("goose", include_str!("../../goose/source_backed.rs")),
            ("hermes", include_str!("../../hermes/source_backed.rs")),
            (
                "kimi",
                include_str!("../../kimi/native_path/source_backed.rs"),
            ),
            ("kiro", include_str!("../../kiro/source_backed.rs")),
        ];
        for (provider, source) in sources {
            assert!(
                !source.contains(&forbidden_preview_cap),
                "{provider} restored the index preview cap"
            );
            assert!(
                !source.contains(&forbidden_legacy_field),
                "{provider} restored lexical-preview construction"
            );
            assert!(
                !source.contains(&forbidden_store),
                "{provider} restored the legacy Store path"
            );
        }
    }

    #[test]
    fn default_inventory_excludes_one_shot_cache_roots_until_explicit() {
        let temp = tempdir().unwrap();
        let home = temp.path().join("home");
        let default_sessions = home.join(".augment/sessions");
        let cache_root = home.join("one-shot-augment-cache");
        let cache_sessions = cache_root.join("sessions");
        write_session(
            &default_sessions.join("default.json"),
            "default request",
            "default response",
        );
        write_session(
            &default_sessions.join("nested/ignored.json"),
            "nested request",
            "nested response",
        );
        write_session(
            &cache_sessions.join("explicit.json"),
            "explicit request",
            "explicit response",
        );

        let automatic =
            discover_auggie_source_backed(&AuggieSourceBackedRoot::default_for_home(&home))
                .unwrap();
        assert_eq!(automatic.paths.len(), 1);
        assert_eq!(
            automatic.paths[0],
            fs::canonicalize(default_sessions.join("default.json")).unwrap()
        );
        assert!(!automatic
            .paths
            .iter()
            .any(|path| path.starts_with(&cache_root)));

        let explicit =
            discover_auggie_source_backed(&AuggieSourceBackedRoot::explicit(&cache_root)).unwrap();
        assert_eq!(explicit.paths.len(), 1);
        assert_eq!(
            explicit.paths[0],
            fs::canonicalize(cache_sessions.join("explicit.json")).unwrap()
        );
    }

    #[test]
    fn inventory_and_projection_signal_append_rewrite_truncate_delete_and_unavailable() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let root = AuggieSourceBackedRoot::explicit(&sessions);

        let missing = discover_auggie_source_backed(&root).unwrap();
        assert_eq!(
            missing.status,
            AuggieSourceBackedInventoryStatus::Unavailable
        );
        assert!(missing.paths.is_empty());

        fs::create_dir_all(&sessions).unwrap();
        let empty = discover_auggie_source_backed(&root).unwrap();
        assert_eq!(empty.status, AuggieSourceBackedInventoryStatus::Complete);
        assert!(empty.paths.is_empty());

        let path = sessions.join("session.json");
        write_history(
            &path,
            &[("stable-request-1", "initial request", "initial response")],
        );
        let initial_inventory = discover_auggie_source_backed(&root).unwrap();
        let initial =
            project_auggie_source_backed_inventory(&initial_inventory, &context(&sessions))
                .unwrap();
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].documents.len(), 2);
        let initial_ids = initial[0]
            .documents
            .iter()
            .map(|document| document.event_id)
            .collect::<Vec<_>>();

        write_history(
            &path,
            &[
                (
                    "stable-request-1",
                    "rewritten request with a longer body",
                    "rewritten response",
                ),
                ("stable-request-2", "appended request", "appended response"),
            ],
        );
        let appended = project_auggie_source_backed_inventory(
            &discover_auggie_source_backed(&root).unwrap(),
            &context(&sessions),
        )
        .unwrap();
        assert_eq!(appended[0].documents.len(), 4);
        assert_eq!(appended[0].documents[0].event_id, initial_ids[0]);
        assert_eq!(appended[0].documents[1].event_id, initial_ids[1]);
        assert_eq!(
            appended[0].documents[0].body,
            "rewritten request with a longer body"
        );

        write_history(
            &path,
            &[(
                "stable-request-1",
                "truncated generation request",
                "truncated generation response",
            )],
        );
        let truncated = project_auggie_source_backed_inventory(
            &discover_auggie_source_backed(&root).unwrap(),
            &context(&sessions),
        )
        .unwrap();
        assert_eq!(truncated[0].documents.len(), 2);
        assert_eq!(truncated[0].documents[0].event_id, initial_ids[0]);
        assert_eq!(truncated[0].documents[1].event_id, initial_ids[1]);

        let stale_inventory = discover_auggie_source_backed(&root).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(
            project_auggie_source_backed_inventory(&stale_inventory, &context(&sessions)).is_err()
        );
        let deleted = discover_auggie_source_backed(&root).unwrap();
        assert_eq!(deleted.status, AuggieSourceBackedInventoryStatus::Complete);
        assert!(deleted.paths.is_empty());

        fs::remove_dir(&sessions).unwrap();
        let unavailable = discover_auggie_source_backed(&root).unwrap();
        assert_eq!(
            unavailable.status,
            AuggieSourceBackedInventoryStatus::Unavailable
        );
        assert!(unavailable.paths.is_empty());
    }
}
