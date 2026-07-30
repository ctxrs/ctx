//! Thin Cursor adapter for the shared replacement-only JSONL family.

use std::{collections::BTreeSet, fs, io, path::Path, sync::Arc};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, EventHydrationRequest,
    EventIdentityInput, EventType, HydratedProviderRecord, HydrationFailure, HydrationFailureKind,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    PositionStability, SessionIdentityInput, SourceAnchor, SourceKey, SourceRecordLocator,
    StableEntityId, SubrecordSelector, TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    cursor_complete_content_message_record, discover_cursor_transcripts,
    parser::project_cursor_jsonl_record,
    projection::{CursorEventBody, CursorNativeEvent},
};
use crate::{
    common::io::OpenedProviderSourceFile,
    provider::source_backed::family::jsonl::{
        JsonlFamilyAdapter, JsonlFamilyHydrator, JsonlFamilyInventory, JsonlFamilyLeaf,
        JsonlFamilyProjector, JsonlFileObservation, JsonlRecordRef,
    },
    CaptureError, Result, CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT, MAX_PROVIDER_JSONL_LINE_BYTES,
};

const SOURCE_ANCHOR_NAMESPACE: &str = "cursor.session";
const NATIVE_SESSION_NAMESPACE: &str = "cursor.session";
const NATIVE_EVENT_POSITION_KIND: &str = "cursor.semantic-ordinal";
const NATIVE_SUBRECORD_POSITION_KIND: &str = "cursor.part-ordinal";
const LOGICAL_SESSION_KIND: &str = "cursor-session";
const LOGICAL_EVENT_KIND: &str = "cursor-event";
const SOURCE_SCHEMA_VARIANT: &str = "cursor-agent-transcript-jsonl-v1";
const PARSER_REVISION: &str = "cursor-shared-jsonl-v1";
const SOURCE_REVISION_DOMAIN: &[u8] = b"ctx.cursor.shared-jsonl.source-revision.v1\0";
const EVENT_SEQUENCE_PARTS: u64 = u16::MAX as u64 + 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorBinding {
    native_session_id: String,
    ordinary_file_token: [u8; 32],
}

#[derive(Debug, Clone, Copy)]
struct CursorJsonlAdapter;

pub(crate) fn cursor_jsonl_adapter() -> Arc<dyn JsonlFamilyAdapter> {
    Arc::new(CursorJsonlAdapter)
}

impl JsonlFamilyAdapter for CursorJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Cursor
    }

    fn source_format(&self) -> &'static str {
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        SOURCE_SCHEMA_VARIANT
    }

    fn parser_revision(&self) -> &'static str {
        PARSER_REVISION
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        match fs::symlink_metadata(root) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return JsonlFamilyInventory::missing(self.provider(), root);
            }
            Err(error) => return Err(error.into()),
        }
        let inventory = discover_cursor_transcripts(root);
        if !inventory.completed {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: root.to_path_buf(),
                reason: "Cursor transcript inventory could not be completed",
            });
        }
        let authority = Arc::new(
            inventory
                .authority()
                .ok_or(CaptureError::InvalidProviderTranscriptPath {
                    path: root.to_path_buf(),
                    reason: "Cursor discovery has no retained source authority",
                })?
                .clone(),
        );
        let mut native_sessions = BTreeSet::new();
        let mut leaves = Vec::with_capacity(inventory.transcripts.len());
        for transcript in inventory.transcripts {
            if transcript.authority().named_path() != authority.named_path()
                || transcript.authority().authority_fingerprint()
                    != authority.authority_fingerprint()
            {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let native_session_id = transcript.native_session_id().to_owned();
            if !native_sessions.insert(native_session_id.clone()) {
                return Err(CaptureError::InvalidPayload(format!(
                    "Cursor native session ID {native_session_id:?} resolves more than once"
                )));
            }
            let source = source_key(&native_session_id)?;
            let binding = CursorBinding {
                native_session_id,
                ordinary_file_token: transcript.ordinary_file_token(),
            };
            leaves.push(JsonlFamilyLeaf::observe(
                source,
                transcript.path().to_path_buf(),
                Arc::clone(&authority),
                transcript.authority_relative_path().to_path_buf(),
                TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract)?,
            )?);
        }
        JsonlFamilyInventory::present(self.provider(), root, authority, leaves)
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        let binding = decode_binding(leaf)?;
        validate_binding(leaf, &binding, source_file.as_ref())?;
        let session_id = session_id(leaf.source(), &binding.native_session_id)?;
        let source_path = leaf
            .source_path()
            .to_str()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: leaf.source_path().to_path_buf(),
                reason: "Cursor transcript path is not UTF-8",
            })?
            .to_owned();
        Ok(Box::new(CursorProjector {
            source: leaf.source().clone(),
            source_path,
            native_session_id: binding.native_session_id,
            session_id,
            source_revision_digest: source_revision_digest(leaf.observation())?,
            next_semantic_ordinal: 0,
        }))
    }

    fn hydrator(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
    ) -> std::result::Result<Box<dyn JsonlFamilyHydrator>, HydrationFailure> {
        let binding = decode_binding(leaf).map_err(unavailable)?;
        validate_binding(leaf, &binding, source_file.as_ref()).map_err(stale)?;
        Ok(Box::new(CursorHydrator {
            source: leaf.source().clone(),
            session_id: session_id(leaf.source(), &binding.native_session_id)
                .map_err(unavailable)?,
            native_session_id: binding.native_session_id,
            source_revision_digest: source_revision_digest(leaf.observation())
                .map_err(unavailable)?,
            source_file,
        }))
    }
}

struct CursorProjector {
    source: SourceKey,
    source_path: String,
    native_session_id: String,
    session_id: StableEntityId,
    source_revision_digest: [u8; 32],
    next_semantic_ordinal: u64,
}

impl JsonlFamilyProjector for CursorProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(LexicalDocument) -> Result<()>,
    ) -> Result<()> {
        let evidence = record.evidence();
        let Some(events) = project_cursor_jsonl_record(
            record.bytes(),
            self.next_semantic_ordinal,
            evidence.physical_ordinal(),
            evidence.byte_start(),
            evidence.byte_end_exclusive(),
        )?
        else {
            return Ok(());
        };
        self.next_semantic_ordinal =
            self.next_semantic_ordinal
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Cursor semantic ordinal overflowed",
                ))?;
        for event in events {
            if let Some(document) = lexical_document(
                &self.source,
                self.session_id,
                &self.native_session_id,
                &self.source_path,
                self.source_revision_digest,
                event,
            )? {
                emit(document)?;
            }
        }
        Ok(())
    }
}

struct CursorHydrator {
    source: SourceKey,
    session_id: StableEntityId,
    native_session_id: String,
    source_revision_digest: [u8; 32],
    source_file: Arc<OpenedProviderSourceFile>,
}

impl JsonlFamilyHydrator for CursorHydrator {
    fn hydrate(
        &mut self,
        request: &EventHydrationRequest,
    ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
        let coordinate = validate_locator(
            request.locator(),
            &self.source,
            &self.native_session_id,
            self.source_revision_digest,
        )?;
        let byte_length = usize::try_from(coordinate.byte_length)
            .map_err(|_| invalid("Cursor locator byte range exceeds platform limits"))?;
        if byte_length == 0 || byte_length > MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2) {
            return Err(invalid("Cursor locator byte range is invalid"));
        }
        let wire = self
            .source_file
            .read_exact_range(
                coordinate.byte_offset,
                byte_length,
                MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2),
            )
            .map_err(stale)?;
        if !wire.ends_with(b"\n") {
            return Err(stale("Cursor JSONL record boundary changed"));
        }
        let bytes = strip_jsonl_terminator(&wire);
        if Sha256::digest(bytes).as_slice() != request.locator().record_digest() {
            return Err(stale("Cursor JSONL record digest changed"));
        }
        let byte_end_exclusive = coordinate
            .byte_offset
            .checked_add(coordinate.byte_length)
            .ok_or_else(|| invalid("Cursor locator byte range overflows"))?;
        let events = project_cursor_jsonl_record(
            bytes,
            coordinate.semantic_ordinal,
            coordinate.physical_ordinal,
            coordinate.byte_offset,
            byte_end_exclusive,
        )
        .map_err(stale)?
        .ok_or_else(|| stale("Cursor locator record is no longer projectable"))?;
        let event = events
            .into_iter()
            .find(|event| event.native_order.part_ordinal == coordinate.part_ordinal)
            .ok_or_else(|| stale("Cursor locator subrecord no longer exists"))?;
        let expected_event_id = event_id(
            &self.source,
            self.session_id,
            coordinate.semantic_ordinal,
            coordinate.part_ordinal,
        )
        .map_err(unavailable)?;
        if expected_event_id != request.event_id()
            || event.event_type != EventType::Message
            || event.native_order.physical_ordinal != coordinate.physical_ordinal
        {
            return Err(invalid(
                "Cursor locator identity does not match the requested event",
            ));
        }
        let CursorEventBody::Text { text: indexed_text } = &event.body else {
            return Err(stale("Cursor locator no longer addresses a message"));
        };
        let content_ref = event
            .complete_content_ref
            .as_ref()
            .ok_or_else(|| stale("Cursor message has no exact content reference"))?;
        let value: Value =
            serde_json::from_slice(bytes).map_err(|_| stale("Cursor record JSON changed"))?;
        let (text, native_record_id, provider_event_hash) = cursor_complete_content_message_record(
            &value,
            coordinate.physical_ordinal,
            coordinate.part_ordinal,
            indexed_text,
        )
        .ok_or_else(|| stale("Cursor message display content changed"))?;
        let expected_native_record_id = format!(
            "cursor-line-v1:{}:{}",
            coordinate.physical_ordinal, coordinate.part_ordinal
        );
        if native_record_id != expected_native_record_id
            || provider_event_hash != event.provider_event_hash
            || !content_ref.verifies(text.as_bytes())
        {
            return Err(stale("Cursor message content evidence changed"));
        }
        Ok(HydratedProviderRecord {
            event_id: request.event_id(),
            provider_bytes: text.into_bytes(),
        })
    }
}

fn lexical_document(
    source: &SourceKey,
    session_id: StableEntityId,
    native_session_id: &str,
    source_path: &str,
    source_revision_digest: [u8; 32],
    event: CursorNativeEvent,
) -> Result<Option<LexicalDocument>> {
    if event.event_type != EventType::Message || event.complete_content_ref.is_none() {
        return Ok(None);
    }
    let CursorEventBody::Text { text } = event.body else {
        return Ok(None);
    };
    if text.is_empty() {
        return Ok(None);
    }
    let part_ordinal = event.native_order.part_ordinal;
    if part_ordinal > u32::from(u16::MAX) {
        return Err(CaptureError::InvalidPayload(
            "Cursor record exceeds the stable event-sequence part bound".to_owned(),
        ));
    }
    let event_id = event_id(
        source,
        session_id,
        event.native_order.semantic_ordinal,
        part_ordinal,
    )?;
    let byte_length = event
        .record_byte_end_exclusive
        .checked_sub(event.record_byte_start)
        .filter(|length| *length > 0)
        .ok_or_else(|| {
            CaptureError::InvalidPayload("Cursor JSONL byte range is invalid".to_owned())
        })?;
    let native_event_key = TypedKey::composite(vec![
        TypedKey::U64(event.native_order.semantic_ordinal),
        TypedKey::U64(u64::from(part_ordinal)),
    ])
    .map_err(contract)?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::Jsonl {
            byte_offset: event.record_byte_start,
            byte_length,
            physical_ordinal: event.native_order.physical_ordinal,
            native_session_key: Some(TypedKey::utf8(native_session_id).map_err(contract)?),
            native_event_key: Some(native_event_key),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(source_revision_digest),
        event.record_sha256,
    )
    .map_err(contract)?;
    let event_sequence = event
        .native_order
        .semantic_ordinal
        .checked_mul(EVENT_SEQUENCE_PARTS)
        .and_then(|base| base.checked_add(u64::from(part_ordinal)))
        .ok_or(CaptureError::SystemInvariant(
            "Cursor event sequence overflowed",
        ))?;
    Ok(Some(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(native_session_id.to_owned()),
        branch: None,
        source_path: Some(source_path.to_owned()),
        agent_type: AgentType::Primary.as_str().to_owned(),
        is_primary: true,
        event_sequence,
        occurred_at_unix_ms: event
            .occurred_at
            .map(|occurred_at| occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: Some(event.role.as_str().to_owned()),
        body: text,
        workspace: None,
        cwd: None,
        touched_files: Vec::new(),
    }))
}

#[derive(Debug, Clone, Copy)]
struct CursorCoordinate {
    byte_offset: u64,
    byte_length: u64,
    physical_ordinal: u64,
    semantic_ordinal: u64,
    part_ordinal: u32,
}

fn validate_locator(
    locator: &SourceRecordLocator,
    source: &SourceKey,
    native_session_id: &str,
    source_revision_digest: [u8; 32],
) -> std::result::Result<CursorCoordinate, HydrationFailure> {
    locator.validate_contract().map_err(invalid)?;
    if !locator.source().exact_descriptor_eq(source)
        || source.provider() != CaptureProvider::Cursor.as_str()
        || source.source_format() != CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT
        || source.schema_variant() != SOURCE_SCHEMA_VARIANT
        || source.provider_identity_version() != 1
        || locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
        || locator.certified_source_revision_digest() != Some(&source_revision_digest)
    {
        return Err(invalid("locator is not an exact Cursor JSONL record"));
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key,
        native_event_key,
    } = locator.coordinate()
    else {
        return Err(invalid("Cursor locator is not a JSONL byte range"));
    };
    if native_session_key.as_ref() != Some(&TypedKey::Utf8(native_session_id.to_owned())) {
        return Err(invalid("Cursor locator session key is invalid"));
    }
    let Some(TypedKey::Composite(event_key)) = native_event_key.as_ref() else {
        return Err(invalid("Cursor locator event key is malformed"));
    };
    let [TypedKey::U64(semantic_ordinal), TypedKey::U64(part_ordinal)] = event_key.as_slice()
    else {
        return Err(invalid("Cursor locator event key is malformed"));
    };
    let part_ordinal =
        u32::try_from(*part_ordinal).map_err(|_| invalid("Cursor locator part is invalid"))?;
    Ok(CursorCoordinate {
        byte_offset: *byte_offset,
        byte_length: *byte_length,
        physical_ordinal: *physical_ordinal,
        semantic_ordinal: *semantic_ordinal,
        part_ordinal,
    })
}

fn source_key(native_session_id: &str) -> Result<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(contract)?,
    )
    .map_err(contract)?;
    SourceKey::derive(
        CaptureProvider::Cursor.as_str(),
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )
    .map_err(contract)
}

fn session_id(source: &SourceKey, native_session_id: &str) -> Result<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(contract)?,
    )
    .map_err(contract)?;
    derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })
    .map_err(contract)
}

fn event_id(
    source: &SourceKey,
    session_id: StableEntityId,
    semantic_ordinal: u64,
    part_ordinal: u32,
) -> Result<StableEntityId> {
    let native_item_key = NativeItemKey::certified_position(
        NATIVE_EVENT_POSITION_KIND,
        TypedKey::U64(semantic_ordinal),
        PositionStability::AppendStable,
    )
    .map_err(contract)?;
    let subrecord = SubrecordSelector::certified_position(
        NATIVE_SUBRECORD_POSITION_KIND,
        TypedKey::U64(u64::from(part_ordinal)),
        PositionStability::StableSlot,
    )
    .map_err(contract)?;
    derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: Some(&subrecord),
    })
    .map_err(contract)
}

fn validate_binding(
    leaf: &JsonlFamilyLeaf,
    binding: &CursorBinding,
    source_file: &OpenedProviderSourceFile,
) -> Result<()> {
    if source_file.ordinary_file_token() != binding.ordinary_file_token
        || !source_key(&binding.native_session_id)?.exact_descriptor_eq(leaf.source())
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

fn decode_binding(leaf: &JsonlFamilyLeaf) -> Result<CursorBinding> {
    let TypedKey::Bytes(bytes) = leaf.binding() else {
        return Err(contract("Cursor family binding is malformed"));
    };
    Ok(serde_json::from_slice(bytes)?)
}

fn source_revision_digest(observation: &JsonlFileObservation) -> Result<[u8; 32]> {
    let encoded = serde_json::to_vec(observation)?;
    let mut digest = Sha256::new();
    digest.update(SOURCE_REVISION_DOMAIN);
    digest.update((encoded.len() as u64).to_be_bytes());
    digest.update(encoded);
    Ok(digest.finalize().into())
}

fn strip_jsonl_terminator(record: &[u8]) -> &[u8] {
    let record = record.strip_suffix(b"\n").unwrap_or(record);
    record.strip_suffix(b"\r").unwrap_or(record)
}

fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(format!("Cursor source-backed contract is invalid: {error}"))
}

fn invalid(error: impl std::fmt::Display) -> HydrationFailure {
    HydrationFailure::new(HydrationFailureKind::InvalidLocator, error.to_string())
}

fn stale(error: impl std::fmt::Display) -> HydrationFailure {
    HydrationFailure::new(HydrationFailureKind::StaleRecordEvidence, error.to_string())
}

fn unavailable(error: impl std::fmt::Display) -> HydrationFailure {
    HydrationFailure::new(
        HydrationFailureKind::TemporarilyUnavailable,
        error.to_string(),
    )
}
