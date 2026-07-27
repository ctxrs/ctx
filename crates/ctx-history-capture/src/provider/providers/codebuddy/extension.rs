use std::path::Path;

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole};
use ctx_history_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::captured_batch::whole_json::{
    WholeJsonBatchError, WholeJsonBatchProducer, WholeJsonItem,
};
use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, ProviderRecordKind,
    SourceObservation,
};
use crate::common::io::read_json_file_limited;
use crate::complete_content::structured::attach_structured_complete_content_locator;
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, emit_projected_normalization_units,
    provider_path_identity, provider_source_cursor_stream_for_path, BoundedParserCheckpoint,
    CapturedBatchCursorFinish, CapturedBatchCursorMode, CapturedBatchProjector,
    CapturedSourceAdmission, CertifiedProviderCursor, ProviderProjectionFatal,
    ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::normalization::provider_role;
use crate::provider::providers::task_json::task_json_time_field;
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    ProviderNormalizationResult, Result, CODEBUDDY_SOURCE_FORMAT, MAX_PROVIDER_JSONL_LINE_BYTES,
};

use super::normalization::{
    codebuddy_capture, codebuddy_captured_batch_error, codebuddy_checkpoint_time,
    codebuddy_decoded_message, codebuddy_mark_skipped_session, codebuddy_message_text,
    codebuddy_title_from_text, CodeBuddyCaptureDraft, CodeBuddyEventInput, CodeBuddyNativeShape,
    CodeBuddyProjectionCounts,
};
use super::{
    CODEBUDDY_CAPTURE_REVISION, CODEBUDDY_EXTENSION_RECORD_KIND, CODEBUDDY_POLICY_REVISION,
    CODEBUDDY_WHOLE_JSON_LOCATOR_KIND, CODEBUDDY_WHOLE_JSON_POSITION_KIND,
};

mod discovery;
mod source;

use source::{
    codebuddy_extension_line_number, codebuddy_extension_message_file,
    codebuddy_extension_metadata, codebuddy_extension_metadata_text, codebuddy_message_time,
    CodeBuddyExtensionMetadata, CodeBuddyExtensionObservation,
};

pub(super) fn visit_sessions(
    root: &Path,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<usize> {
    discovery::visit_codebuddy_extension_sessions(root, visit)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeBuddyExtensionParserCheckpoint {
    next_ordinal: u64,
    started_at: Option<String>,
    ended_at: Option<String>,
    generated_title_message_index: Option<u64>,
    counts: CodeBuddyProjectionCounts,
}

struct CodeBuddyExtensionCapturedBatchProjector<'a> {
    context: ProviderAdapterContext,
    metadata: &'a CodeBuddyExtensionMetadata,
    session_ordinal: usize,
    next_ordinal: u64,
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    native_title: Option<String>,
    cwd: Option<String>,
    generated_title: Option<String>,
    generated_title_message_index: Option<u64>,
    counts: CodeBuddyProjectionCounts,
}

impl<'a> CodeBuddyExtensionCapturedBatchProjector<'a> {
    fn fresh(
        context: ProviderAdapterContext,
        metadata: &'a CodeBuddyExtensionMetadata,
        session_ordinal: usize,
    ) -> Self {
        let started_at = metadata.conversation.as_ref().and_then(|value| {
            task_json_time_field(value, &["createdAt", "created_at", "timestamp"])
        });
        let ended_at = metadata.conversation.as_ref().and_then(|value| {
            task_json_time_field(
                value,
                &["lastMessageAt", "updatedAt", "completedAt", "last_modified"],
            )
        });
        let native_title = codebuddy_extension_metadata_text(metadata, &["name", "title"]);
        let cwd = codebuddy_extension_metadata_text(
            metadata,
            &["projectPath", "project_path", "cwd", "workspace"],
        );
        Self {
            context,
            metadata,
            session_ordinal,
            next_ordinal: 0,
            started_at,
            ended_at,
            native_title,
            cwd,
            generated_title: None,
            generated_title_message_index: None,
            counts: CodeBuddyProjectionCounts::default(),
        }
    }

    fn resume(
        context: ProviderAdapterContext,
        metadata: &'a CodeBuddyExtensionMetadata,
        session_ordinal: usize,
        cursor: &CertifiedProviderCursor,
    ) -> Result<Self> {
        let checkpoint: CodeBuddyExtensionParserCheckpoint =
            cursor.parser_checkpoint().deserialize()?;
        if checkpoint.next_ordinal
            != codebuddy_whole_json_position_ordinal(cursor.native_position())?
        {
            return Err(CaptureError::InvalidPayload(
                "CodeBuddy extension parser checkpoint does not match its native position"
                    .to_owned(),
            ));
        }
        let native_title = codebuddy_extension_metadata_text(metadata, &["name", "title"]);
        let cwd = codebuddy_extension_metadata_text(
            metadata,
            &["projectPath", "project_path", "cwd", "workspace"],
        );
        let generated_title = checkpoint
            .generated_title_message_index
            .map(|message_index| codebuddy_extension_generated_title(metadata, message_index))
            .transpose()?;
        let mut counts = checkpoint.counts;
        counts.rejected_records = counts.rejected_records.max(cursor.rejected_records());
        Ok(Self {
            context,
            metadata,
            session_ordinal,
            next_ordinal: checkpoint.next_ordinal,
            started_at: codebuddy_checkpoint_time(checkpoint.started_at, "extension start time")?,
            ended_at: codebuddy_checkpoint_time(checkpoint.ended_at, "extension end time")?,
            native_title,
            cwd,
            generated_title,
            generated_title_message_index: checkpoint.generated_title_message_index,
            counts,
        })
    }

    fn replay_summary(&self) -> Result<ProviderImportSummary> {
        let mut summary = self.counts.replay_summary()?;
        if self.is_empty_session() && self.next_ordinal != 0 {
            codebuddy_mark_skipped_session(&mut summary);
        }
        Ok(summary)
    }

    fn is_empty_session(&self) -> bool {
        self.counts.accepted_captures == 0 && self.counts.rejected_records == 0
    }

    fn title(&self) -> Option<&str> {
        self.native_title
            .as_deref()
            .or(self.generated_title.as_deref())
    }
}

impl CapturedBatchProjector for CodeBuddyExtensionCapturedBatchProjector<'_> {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() != CODEBUDDY_EXTENSION_RECORD_KIND {
            return Err(ProviderProjectionFatal::system_invariant(
                "CodeBuddy extension projector received an unexpected record kind",
            ));
        }
        if record.ordinal() != self.next_ordinal {
            return Err(ProviderProjectionFatal::system_invariant(
                "CodeBuddy extension captured record ordinal is not contiguous",
            ));
        }
        self.next_ordinal = self.next_ordinal.checked_add(1).ok_or_else(|| {
            ProviderProjectionFatal::system_invariant(
                "CodeBuddy extension record ordinal overflowed",
            )
        })?;
        let message_index =
            codebuddy_extension_message_index(record).map_err(ProviderProjectionFatal::new)?;
        let line_number = codebuddy_extension_line_number(self.session_ordinal, message_index);
        let message_ref = self.metadata.messages().get(message_index).ok_or_else(|| {
            ProviderProjectionFatal::system_invariant(
                "CodeBuddy extension locator exceeds its session manifest",
            )
        })?;
        let message_id = message_ref
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "CodeBuddy captured message lost its manifest id",
                )
            })?;
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "CodeBuddy extension projector requires whole-JSON native bytes",
            ));
        };
        let raw_message = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                return self.counts.reject(
                    output,
                    line_number,
                    format!("messages/{message_id}.json: json error: {error}"),
                );
            }
        };
        let decoded_message = codebuddy_decoded_message(&raw_message);
        let text = codebuddy_message_text(&decoded_message, &raw_message);
        if text.trim().is_empty() {
            return Ok(());
        }
        let message_path = self
            .metadata
            .session_dir
            .join("messages")
            .join(format!("{message_id}.json"));
        let occurred_at = codebuddy_message_time(
            &raw_message,
            &decoded_message,
            &message_path,
            self.context.imported_at,
        );
        if self.started_at.is_none() {
            self.started_at = Some(occurred_at);
        }
        if self
            .metadata
            .conversation
            .as_ref()
            .and_then(|value| {
                task_json_time_field(
                    value,
                    &["lastMessageAt", "updatedAt", "completedAt", "last_modified"],
                )
            })
            .is_none()
        {
            self.ended_at = Some(occurred_at);
        }
        let role = message_ref
            .get("role")
            .and_then(Value::as_str)
            .or_else(|| raw_message.get("role").and_then(Value::as_str))
            .map(str::to_owned);
        if self.title().is_none() && provider_role(role.as_deref()) == EventRole::User {
            self.generated_title = codebuddy_title_from_text(&text);
            if self.generated_title.is_some() {
                self.generated_title_message_index = Some(message_index as u64);
            }
        }
        let complete_text = text.clone();
        let event = CodeBuddyEventInput {
            provider_event_index: message_index as u64,
            native_message_id: message_id.to_owned(),
            role,
            ref_type: message_ref
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_owned),
            occurred_at,
            text,
            raw_message,
            decoded_message,
        };
        let file_names = ["index.json", "messages/*.json"];
        let started_at = self.started_at.ok_or_else(|| {
            ProviderProjectionFatal::system_invariant(
                "CodeBuddy extension projector lost its start time",
            )
        })?;
        let mut capture = codebuddy_capture(
            &CodeBuddyCaptureDraft {
                provider_session_id: &self.metadata.provider_session_id,
                native_session_id: &self.metadata.native_session_id,
                project_hash: &self.metadata.project_hash,
                raw_source_path: &self.metadata.source_path,
                context: &self.context,
                started_at,
                ended_at: self.ended_at,
                title: self.title(),
                cwd: self.cwd.as_deref(),
                project_index: self.metadata.project_index.as_ref(),
                conversation: self.metadata.conversation.as_ref(),
                session_index: &self.metadata.session_index,
                file_names: &file_names,
                shape: CodeBuddyNativeShape::Extension,
            },
            event,
        );
        let event = capture.event.as_mut().ok_or_else(|| {
            ProviderProjectionFatal::system_invariant("CodeBuddy structured capture lost its event")
        })?;
        let native_id = event.provider_event_hash.clone().unwrap_or_default();
        attach_structured_complete_content_locator(
            CaptureProvider::CodeBuddy,
            event,
            record.ordinal(),
            0,
            &native_id,
            bytes,
            &complete_text,
        )
        .map_err(ProviderProjectionFatal::new)?;
        emit_projected_normalization_units(
            output,
            ProviderNormalizationResult {
                captures: vec![(line_number, capture)],
                ..ProviderNormalizationResult::default()
            },
        )?;
        self.counts.accept()
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        let next_ordinal = codebuddy_whole_json_position_ordinal(position)?;
        if next_ordinal != 0 || self.next_ordinal != 0 {
            return Err(CaptureError::InvalidPayload(
                "CodeBuddy extension initial cursor candidate is not at the source start"
                    .to_owned(),
            ));
        }
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&CodeBuddyExtensionParserCheckpoint {
                next_ordinal,
                started_at: self.started_at.map(|value| value.to_rfc3339()),
                ended_at: self.ended_at.map(|value| value.to_rfc3339()),
                generated_title_message_index: None,
                counts: CodeBuddyProjectionCounts::default(),
            })?,
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        let next_ordinal = codebuddy_whole_json_position_ordinal(batch.range_end())?;
        if self.next_ordinal > next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "CodeBuddy extension projector advanced beyond the captured batch",
            ));
        }
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&CodeBuddyExtensionParserCheckpoint {
                    next_ordinal,
                    started_at: self.started_at.map(|value| value.to_rfc3339()),
                    ended_at: self.ended_at.map(|value| value.to_rfc3339()),
                    generated_title_message_index: self.generated_title_message_index,
                    counts: self.counts.clone(),
                })?,
            )?,
        ))
    }
}

fn codebuddy_extension_generated_title(
    metadata: &CodeBuddyExtensionMetadata,
    message_index: u64,
) -> Result<String> {
    let message_index = usize::try_from(message_index).map_err(|_| {
        CaptureError::InvalidPayload(
            "CodeBuddy extension title anchor exceeds platform limits".to_owned(),
        )
    })?;
    let message_ref = metadata.messages().get(message_index).ok_or_else(|| {
        CaptureError::InvalidPayload(
            "CodeBuddy extension title anchor exceeds its session manifest".to_owned(),
        )
    })?;
    let (message_path, _) = codebuddy_extension_message_file(&metadata.session_dir, message_ref)
        .map_err(CaptureError::InvalidPayload)?;
    let raw_message = read_json_file_limited(
        &message_path,
        MAX_PROVIDER_JSONL_LINE_BYTES,
        "CodeBuddy extension title anchor",
    )?;
    let role = message_ref
        .get("role")
        .and_then(Value::as_str)
        .or_else(|| raw_message.get("role").and_then(Value::as_str));
    let decoded_message = codebuddy_decoded_message(&raw_message);
    let text = codebuddy_message_text(&decoded_message, &raw_message);
    if provider_role(role) != EventRole::User {
        return Err(CaptureError::InvalidPayload(
            "CodeBuddy extension title anchor does not identify a user message".to_owned(),
        ));
    }
    codebuddy_title_from_text(&text).ok_or_else(|| {
        CaptureError::InvalidPayload(
            "CodeBuddy extension title anchor does not identify title text".to_owned(),
        )
    })
}

pub(super) fn import_session_batched(
    session_dir: &Path,
    session_ordinal: usize,
    store: &mut Store,
    context: &ProviderAdapterContext,
    import_options: &NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let (metadata, mut merged) = codebuddy_extension_metadata(session_dir, session_ordinal)?;
    let Some(metadata) = metadata else {
        return Ok(merged);
    };
    if metadata.messages().is_empty() {
        merged.skipped = merged.skipped.saturating_add(1);
        merged.skipped_sessions = merged.skipped_sessions.saturating_add(1);
        return Ok(merged);
    }
    let observation = CodeBuddyExtensionObservation::read(&metadata, session_ordinal, &mut merged)?;
    if observation.record_count == 0 {
        return Ok(merged);
    }
    let path_identity = provider_path_identity(&observation.canonical_session_dir)?;
    let file_context = ProviderAdapterContext {
        machine_id: context.machine_id.clone(),
        source_path: Some(session_dir.to_path_buf()),
        source_root: context
            .source_root
            .clone()
            .or_else(|| context.source_path.clone()),
        imported_at: context.imported_at,
    };
    let source = SourceObservation::new(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        format!("codebuddy-extension-session:{path_identity}"),
        observation.source_revision.clone(),
        provider_source_cursor_stream_for_path(
            CaptureProvider::CodeBuddy,
            CODEBUDDY_SOURCE_FORMAT,
            &path_identity,
        ),
        CODEBUDDY_CAPTURE_REVISION,
        CODEBUDDY_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(codebuddy_captured_batch_error)?;
    let record_kind = ProviderRecordKind::new(CODEBUDDY_EXTENSION_RECORD_KIND)
        .map_err(codebuddy_captured_batch_error)?;
    let initial_position = codebuddy_whole_json_position(0)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let had_expected_store_cursor = expected_store_cursor.is_some();
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    let mut start_ordinal = 0_u64;
    let mut resumed_projector = None;

    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.source_revision() == source.source_revision()
                    && certified.parser_revision() == source.capture_revision()
                    && certified.policy_revision() == source.policy_revision() =>
            {
                let ordinal = codebuddy_whole_json_position_ordinal(certified.native_position())?;
                if ordinal > observation.record_count {
                    return Err(CaptureError::InvalidPayload(
                        "CodeBuddy extension cursor exceeds its source".to_owned(),
                    ));
                }
                let projector = CodeBuddyExtensionCapturedBatchProjector::resume(
                    file_context.clone(),
                    &metadata,
                    session_ordinal,
                    &certified,
                )?;
                if ordinal == observation.record_count {
                    if !observation.revalidate(session_dir)? {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    merged.merge(projector.replay_summary()?);
                    return Ok(merged);
                }
                start_ordinal = ordinal;
                resumed_projector = Some(projector);
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }

    if !observation.revalidate(session_dir)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut projector = resumed_projector.unwrap_or_else(|| {
        CodeBuddyExtensionCapturedBatchProjector::fresh(
            file_context.clone(),
            &metadata,
            session_ordinal,
        )
    });
    let mut producer =
        codebuddy_extension_batch_producer(source.clone(), record_kind, &metadata, start_ordinal)?;
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &file_context)?;
    let mut imported_any = false;
    let summary = drain_captured_batches(
        store,
        &admission,
        import_options.clone(),
        &context.machine_id,
        context.imported_at,
        expected_store_cursor,
        &initial_position,
        cursor_mode,
        &stream,
        &mut projector,
        || {
            let batch = producer.next_batch().map_err(codebuddy_whole_json_error)?;
            imported_any |= batch.is_some();
            Ok(batch)
        },
        || observation.revalidate(session_dir),
    )?;
    if !imported_any && had_expected_store_cursor {
        merged.merge(projector.replay_summary()?);
    } else {
        merged.merge(summary);
        if imported_any && merged.failed == 0 && projector.is_empty_session() {
            codebuddy_mark_skipped_session(&mut merged);
        }
    }
    Ok(merged)
}

fn codebuddy_extension_batch_producer<'a>(
    source: SourceObservation,
    record_kind: ProviderRecordKind,
    metadata: &'a CodeBuddyExtensionMetadata,
    start_ordinal: u64,
) -> Result<WholeJsonBatchProducer<'a>> {
    let session_path = metadata.session_dir.clone();
    let messages = metadata.messages();
    let mut message_index = 0_usize;
    let mut captured_ordinal = 0_u64;
    WholeJsonBatchProducer::new(source, record_kind, move || loop {
        let Some(message_ref) = messages.get(message_index) else {
            return Ok(None);
        };
        let original_index = message_index;
        message_index = message_index.saturating_add(1);
        let Ok((message_path, file)) = codebuddy_extension_message_file(&session_path, message_ref)
        else {
            continue;
        };
        let ordinal = captured_ordinal;
        captured_ordinal = captured_ordinal
            .checked_add(1)
            .ok_or(WholeJsonBatchError::LengthOverflow)?;
        if ordinal < start_ordinal {
            continue;
        }
        let original_index =
            u64::try_from(original_index).map_err(|_| WholeJsonBatchError::LengthOverflow)?;
        return WholeJsonItem::new(
            ordinal,
            original_index.to_be_bytes().to_vec(),
            file.length,
            message_path,
        )
        .map(Some);
    })
    .map_err(codebuddy_whole_json_error)
}

fn codebuddy_extension_message_index(record: &CapturedRecord) -> Result<usize> {
    let locator = record.locator();
    let value = locator.value();
    if locator.kind() != CODEBUDDY_WHOLE_JSON_LOCATOR_KIND || value.len() != 12 {
        return Err(CaptureError::InvalidPayload(
            "CodeBuddy extension record has an invalid whole-JSON locator".to_owned(),
        ));
    }
    if value[..4] != 8_u32.to_be_bytes() {
        return Err(CaptureError::InvalidPayload(
            "CodeBuddy extension locator has an invalid source-item length".to_owned(),
        ));
    }
    let bytes: [u8; 8] = value[4..].try_into().map_err(|_| {
        CaptureError::InvalidPayload(
            "CodeBuddy extension locator has an invalid message index".to_owned(),
        )
    })?;
    usize::try_from(u64::from_be_bytes(bytes)).map_err(|_| {
        CaptureError::InvalidPayload(
            "CodeBuddy extension message index exceeds platform limits".to_owned(),
        )
    })
}

fn codebuddy_whole_json_position(ordinal: u64) -> Result<NativePosition> {
    NativePosition::new(
        CODEBUDDY_WHOLE_JSON_POSITION_KIND,
        ordinal.to_be_bytes().to_vec(),
    )
    .map_err(codebuddy_captured_batch_error)
}

fn codebuddy_whole_json_position_ordinal(position: &NativePosition) -> Result<u64> {
    if position.kind() != CODEBUDDY_WHOLE_JSON_POSITION_KIND || position.value().len() != 8 {
        return Err(CaptureError::InvalidPayload(
            "CodeBuddy cursor has an invalid whole-JSON position".to_owned(),
        ));
    }
    let bytes: [u8; 8] = position.value().try_into().map_err(|_| {
        CaptureError::InvalidPayload(
            "CodeBuddy cursor has an invalid whole-JSON ordinal".to_owned(),
        )
    })?;
    Ok(u64::from_be_bytes(bytes))
}

fn codebuddy_whole_json_error(error: WholeJsonBatchError) -> CaptureError {
    match error {
        WholeJsonBatchError::Io(error) => CaptureError::Io(error),
        WholeJsonBatchError::SourceSizeChanged { .. }
        | WholeJsonBatchError::SourceMetadataChangedDuringRead => {
            CaptureError::SourceChangedDuringCapture
        }
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

#[cfg(test)]
mod tests;
