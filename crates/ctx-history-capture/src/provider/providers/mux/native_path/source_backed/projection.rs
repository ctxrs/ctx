use std::sync::Arc;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, EventIdentityInput, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, SourceKey, SourceRecordLocator, TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    common::io::ProviderSourceRoot,
    provider::{
        file_touches::{
            event_type_supports_structured_file_touches,
            visit_provider_file_touch_drafts_with_limit,
        },
        providers::mux::normalization::{
            apply_mux_core_output_diagnostic, mux_core_event, mux_event_id,
            mux_message_timestamp_opt, mux_output_projection, mux_partial_event_index,
            MuxMessageRow, MuxOutputOutcome,
        },
        source_backed::family::jsonl::{
            JsonlFamilyProjector, JsonlReader, JsonlRecordRef, JsonlSourceIdentity,
        },
    },
    CaptureError, Result,
};

use super::{
    bound_stream, open_verified, resolver::mux_exact_logical_content, MuxBinding, MuxStreamKind,
    LOGICAL_EVENT_KIND, PARSER_REVISION,
};

const NATIVE_ITEM_NAMESPACE: &str = "mux.record";
const PROVIDER_NATIVE_LOCATOR_NAMESPACE: &str = "mux.logical-record.v2";
const PARTIAL_NATIVE_ORDINAL: u64 = 1_u64 << 63;
const MAX_ORDINAL: u64 = (1_u64 << 47) - 1;
const MAX_FILE_TOUCHES: usize = 448;

pub(super) struct MuxProjector {
    source: SourceKey,
    authority: Arc<ProviderSourceRoot>,
    binding: MuxBinding,
}

impl MuxProjector {
    pub(super) fn new(
        source: SourceKey,
        authority: Arc<ProviderSourceRoot>,
        binding: MuxBinding,
    ) -> Self {
        Self {
            source,
            authority,
            binding,
        }
    }

    fn project_record(
        &self,
        stream: MuxStreamKind,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(LexicalDocument) -> Result<()>,
    ) -> Result<()> {
        let bytes = record.bytes();
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
            return Ok(());
        };
        if !value.is_object() {
            return Ok(());
        }
        if value
            .get("workspaceId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .is_some_and(|owner| owner != self.binding.metadata.provider_session_id)
        {
            return Err(CaptureError::InvalidPayload(
                "Mux record changed its native session owner".to_owned(),
            ));
        }
        let output = mux_output_projection(&value);
        if output.as_ref().is_some_and(|output| {
            !output.body_available
                || !matches!(
                    output.outcome,
                    MuxOutputOutcome::Failure | MuxOutputOutcome::Timeout
                )
        }) {
            return Ok(());
        }
        let evidence = record.evidence();
        let ordinal = evidence.physical_ordinal();
        if !stream.is_partial() && ordinal > MAX_ORDINAL {
            return Err(CaptureError::InvalidPayload(
                "Mux source ordinal exceeds event identity capacity".to_owned(),
            ));
        }
        let event_sequence = if stream.is_partial() {
            PARTIAL_NATIVE_ORDINAL | (mux_partial_event_index(bytes) & MAX_ORDINAL)
        } else {
            ordinal
        };
        let line_number = usize::try_from(ordinal)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Mux source ordinal exceeds platform limits",
            ))?;
        let role = value
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let native_record_id = mux_event_id(&value, line_number, role, stream.is_partial());
        let native_item_key = NativeItemKey::native_id(
            NATIVE_ITEM_NAMESPACE,
            TypedKey::utf8(&native_record_id).map_err(contract)?,
        )
        .map_err(contract)?;
        let event_id = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id: self.binding.session_id,
            logical_item_kind: LOGICAL_EVENT_KIND,
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .map_err(contract)?;
        let stream_path = self
            .authority
            .named_path()
            .join(&bound_stream(&self.binding, stream)?.relative_path);
        let row = MuxMessageRow { value };
        let occurred_at = mux_message_timestamp_opt(&row.value).unwrap_or_else(|| {
            self.binding
                .metadata
                .started_at
                .parse::<DateTime<Utc>>()
                .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
        });
        let mut event = mux_core_event(&row, occurred_at);
        if let Some(output) = output.as_ref() {
            apply_mux_core_output_diagnostic(&mut event, &row.value, output);
        }
        let body = mux_exact_logical_content(&row.value).map_err(|failure| {
            CaptureError::InvalidPayload(format!("{:?}: {}", failure.kind, failure.detail))
        })?;
        if body.is_empty() {
            return Err(CaptureError::InvalidPayload(
                "Mux source-backed event has no exact lexical body".to_owned(),
            ));
        }
        let mut touched_files = Vec::new();
        if event_type_supports_structured_file_touches(event.event_type) {
            let _ = visit_provider_file_touch_drafts_with_limit(
                &row.value,
                true,
                MAX_FILE_TOUCHES,
                |(_, touch)| {
                    touched_files.push(touch.path);
                    Ok::<(), std::convert::Infallible>(())
                },
            );
        }
        let locator = SourceRecordLocator::new(
            self.source.clone(),
            NativeRecordCoordinate::ProviderNative {
                namespace: PROVIDER_NATIVE_LOCATOR_NAMESPACE.to_owned(),
                coordinate: encode_mux_coordinate(
                    stream,
                    evidence.byte_start(),
                    evidence.byte_end_exclusive(),
                    ordinal,
                    event_sequence,
                    &native_record_id,
                )?,
            },
            if stream.is_partial() {
                LocatorRevisionPolicy::ExactSourceRevision
            } else {
                LocatorRevisionPolicy::StableRecordEvidence
            },
            Some(self.binding.source_revision_digest),
            Sha256::digest(bytes).into(),
        )
        .map_err(contract)?;
        emit(LexicalDocument {
            event_id,
            session_id: self.binding.session_id,
            parent_session_id: self.binding.parent_session_id,
            root_session_id: self.binding.root_session_id,
            source: self.source.clone(),
            locator,
            provider_session_id: Some(self.binding.metadata.provider_session_id.clone()),
            branch: None,
            source_path: Some(stream_path.display().to_string()),
            agent_type: if self.binding.parent_session_id.is_some() {
                "subagent".to_owned()
            } else {
                "primary".to_owned()
            },
            is_primary: self.binding.parent_session_id.is_none(),
            event_sequence,
            occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
            event_type: event.event_type.as_str().to_owned(),
            role: event.role.map(|role| role.as_str().to_owned()),
            body,
            workspace: None,
            cwd: self.binding.metadata.cwd.clone(),
            touched_files,
        })
    }
}

impl JsonlFamilyProjector for MuxProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(LexicalDocument) -> Result<()>,
    ) -> Result<()> {
        self.project_record(self.binding.primary_stream, record, emit)
    }

    fn finish_projecting(
        &mut self,
        emit: &mut dyn FnMut(LexicalDocument) -> Result<()>,
    ) -> Result<()> {
        if self.binding.primary_stream.is_partial() {
            return Ok(());
        }
        let Some(partial) = self.binding.partial.as_ref() else {
            return Ok(());
        };
        let source_file = open_verified(&self.authority, partial)?;
        let path = self.authority.named_path().join(&partial.relative_path);
        let mut reader = JsonlReader::open_whole_record(
            JsonlSourceIdentity::new(
                "mux",
                PARSER_REVISION,
                "mux-bounded-partial-snapshot-v1",
                self.source.exact_descriptor_digest(),
                path,
            ),
            source_file,
            None,
        )?;
        while reader
            .visit_page(&mut |record| self.project_record(MuxStreamKind::Partial, record, emit))?
            .is_some()
        {}
        if reader.outcome().is_none() {
            return Err(CaptureError::SystemInvariant(
                "Mux partial snapshot scan has no terminal evidence",
            ));
        }
        Ok(())
    }
}

pub(super) fn encode_mux_coordinate(
    stream: MuxStreamKind,
    byte_start: u64,
    byte_end_exclusive: u64,
    source_record_ordinal: u64,
    event_sequence: u64,
    native_record_id: &str,
) -> Result<TypedKey> {
    if byte_start >= byte_end_exclusive
        || native_record_id.is_empty()
        || (stream.is_partial() && (byte_start != 0 || source_record_ordinal != 0))
        || (!stream.is_partial() && event_sequence != source_record_ordinal)
    {
        return Err(CaptureError::InvalidPayload(
            "Mux native coordinate is internally inconsistent".to_owned(),
        ));
    }
    TypedKey::composite(vec![
        TypedKey::U64(2),
        TypedKey::U64(if stream.is_partial() { 2 } else { 1 }),
        TypedKey::U64(byte_start),
        TypedKey::U64(byte_end_exclusive),
        TypedKey::U64(source_record_ordinal),
        TypedKey::U64(event_sequence),
        TypedKey::utf8(native_record_id).map_err(contract)?,
    ])
    .map_err(contract)
}

fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}
