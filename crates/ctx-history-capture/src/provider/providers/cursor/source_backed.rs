//! Thin Cursor adapter for the shared certified-append JSONL family.

use std::{collections::BTreeSet, fs, io, path::Path, sync::Arc};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CoreRecord, EventIdentityInput,
    EventType, NativeItemKey, NativeSessionKey, PositionStability, SessionIdentityInput,
    SourceAnchor, SourceKey, StableEntityId, SubrecordSelector, TypedKey,
};
use serde::{Deserialize, Serialize};

use super::{
    discover_cursor_transcripts,
    parser::project_cursor_jsonl_record,
    projection::{CursorEventBody, CursorNativeEvent},
};
use crate::{
    common::io::OpenedProviderSourceFile,
    provider::source_backed::family::jsonl::{
        JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyInventory, JsonlFamilyLeaf,
        JsonlFamilyProjector, JsonlRecordRef,
    },
    CaptureError, Result, CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
};

const SOURCE_ANCHOR_NAMESPACE: &str = "cursor.session";
const NATIVE_SESSION_NAMESPACE: &str = "cursor.session";
const NATIVE_EVENT_POSITION_KIND: &str = "cursor.physical-ordinal";
const NATIVE_SUBRECORD_POSITION_KIND: &str = "cursor.part-ordinal";
const LOGICAL_SESSION_KIND: &str = "cursor-session";
const LOGICAL_EVENT_KIND: &str = "cursor-event";
const SOURCE_SCHEMA_VARIANT: &str = "cursor-agent-transcript-jsonl-v1";
const PARSER_REVISION: &str = "cursor-shared-jsonl-v2-physical-record-evidence";
const EVENT_SEQUENCE_PARTS: u64 = u16::MAX as u64 + 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorBinding {
    native_session_id: String,
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

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
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
            let binding = CursorBinding { native_session_id };
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
        Ok(Box::new(CursorProjector {
            source: leaf.source().clone(),
            native_session_id: binding.native_session_id,
            session_id,
        }))
    }
}

struct CursorProjector {
    source: SourceKey,
    native_session_id: String,
    session_id: StableEntityId,
}

impl JsonlFamilyProjector for CursorProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let evidence = record.evidence();
        let Some(events) = project_cursor_jsonl_record(
            record.bytes(),
            evidence.physical_ordinal(),
            evidence.physical_ordinal(),
            evidence.byte_start(),
            evidence.byte_end_exclusive(),
        )?
        else {
            return Ok(());
        };
        for event in events {
            if let Some(document) = core_record(
                &self.source,
                self.session_id,
                &self.native_session_id,
                event,
            )? {
                emit(document)?;
            }
        }
        Ok(())
    }
}

fn core_record(
    source: &SourceKey,
    session_id: StableEntityId,
    native_session_id: &str,
    event: CursorNativeEvent,
) -> Result<Option<CoreRecord>> {
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
    let native_event_key = TypedKey::composite(vec![
        TypedKey::U64(event.native_order.semantic_ordinal),
        TypedKey::U64(u64::from(part_ordinal)),
    ])
    .map_err(contract)?;
    let event_sequence = event
        .native_order
        .semantic_ordinal
        .checked_mul(EVENT_SEQUENCE_PARTS)
        .and_then(|base| base.checked_add(u64::from(part_ordinal)))
        .ok_or(CaptureError::SystemInvariant(
            "Cursor event sequence overflowed",
        ))?;
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        event_sequence,
        event.event_type.as_str(),
        AgentType::Primary.as_str(),
        true,
        PARSER_REVISION,
        text,
    )
    .map_err(contract)?;
    record.provider_session_id = Some(native_session_id.to_owned());
    record.native_event_id = Some(native_event_key);
    record.occurred_at_unix_ms = event
        .occurred_at
        .map(|occurred_at| occurred_at.timestamp_millis());
    record.role = Some(event.role.as_str().to_owned());
    record.validate_contract().map_err(contract)?;
    Ok(Some(record))
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
    _source_file: &OpenedProviderSourceFile,
) -> Result<()> {
    if !source_key(&binding.native_session_id)?.exact_descriptor_eq(leaf.source()) {
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

fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(format!("Cursor source-backed contract is invalid: {error}"))
}
