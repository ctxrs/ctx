//! Bounded emission from the invocation-local Custom History spool.

use std::io::{BufReader, Read, Seek, SeekFrom, Write};

use ctx_history_core::{
    derive_event_id, CoreActivity, CoreRecord, EventIdentityInput, LiteralFactKind, NativeItemKey,
    ProviderDeclaredFact, ProviderNativeEventCopy, SourceKey, TypedKey, CORE_ACTIVITY_REVISION,
    MAX_CORE_CONTENT_BYTES,
};
use ctx_history_jsonl::{fit_jsonl_activity, JsonlActivityObservedBytes};

#[cfg(test)]
use super::source_backed::record_custom_history_work;
use super::source_backed::{
    custom_event_typed_key_parts, custom_session_identity, CustomHistorySourceBackedError,
    CustomHistorySourceBackedPage, CustomHistorySourceBackedResult, CustomSessionCatalogEntry,
    CustomSourceCatalogEntry, ParsedProjection, SpooledCustomEvent, ValidatedCopiedFrom,
    CUSTOM_EVENT_KEY_NAMESPACE, CUSTOM_LOGICAL_EVENT_KIND, CUSTOM_PAGE_MAX_DOCUMENTS,
    CUSTOM_PAGE_MAX_RETAINED_BYTES, CUSTOM_SOURCE_BACKED_PARSER_REVISION,
};
use crate::provider::custom_history_jsonl::CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES;
use crate::MAX_PROVIDER_JSONL_LINE_BYTES;

pub(super) const CUSTOM_HISTORY_CATALOG_ENTRY_OVERHEAD_BYTES: usize = 128;

#[derive(Debug, Clone, Copy)]
pub(super) struct CustomHistoryCatalogLimits {
    max_records: usize,
    max_metadata_bytes: usize,
}

impl CustomHistoryCatalogLimits {
    pub(super) const PRODUCTION: Self = Self {
        max_records: super::source_backed::CUSTOM_HISTORY_CATALOG_MAX_RECORDS,
        max_metadata_bytes: super::source_backed::CUSTOM_HISTORY_CATALOG_MAX_METADATA_BYTES,
    };
}

#[derive(Debug)]
pub(super) struct CatalogBudget {
    limits: CustomHistoryCatalogLimits,
    records: usize,
    metadata_bytes: usize,
}

impl CatalogBudget {
    pub(super) fn new(limits: CustomHistoryCatalogLimits) -> Self {
        Self {
            limits,
            records: 0,
            metadata_bytes: 0,
        }
    }

    pub(super) fn admit_record(&mut self) -> CustomHistorySourceBackedResult<()> {
        let observed = self.records.saturating_add(1);
        if observed > self.limits.max_records {
            return Err(CustomHistorySourceBackedError::Bounds {
                limit: super::source_backed::CustomHistorySourceBackedBound::CatalogRecords,
                maximum: self.limits.max_records,
                observed,
            });
        }
        self.records = observed;
        #[cfg(test)]
        record_custom_history_work(|work| {
            work.catalog_records = observed;
        });
        Ok(())
    }

    pub(super) fn admit_metadata(&mut self, bytes: usize) -> CustomHistorySourceBackedResult<()> {
        let observed = self.metadata_bytes.saturating_add(bytes);
        if observed > self.limits.max_metadata_bytes {
            return Err(CustomHistorySourceBackedError::Bounds {
                limit: super::source_backed::CustomHistorySourceBackedBound::CatalogMetadataBytes,
                maximum: self.limits.max_metadata_bytes,
                observed,
            });
        }
        self.metadata_bytes = observed;
        #[cfg(test)]
        record_custom_history_work(|work| {
            work.catalog_metadata_bytes = observed;
        });
        Ok(())
    }
}

pub(super) fn emit_projection_pages(
    source: &SourceKey,
    projection: &mut ParsedProjection,
    emit_from: u64,
    emit: &mut impl FnMut(CustomHistorySourceBackedPage) -> CustomHistorySourceBackedResult<()>,
) -> CustomHistorySourceBackedResult<()> {
    projection.event_spool.seek(SeekFrom::Start(0))?;
    let mut event_reader = BufReader::new(&mut projection.event_spool);
    let mut documents = Vec::new();
    let mut retained_bytes = 0_usize;
    let mut retained_body_bytes = 0_usize;
    while let Some(mut event) = read_spooled_event(&mut event_reader)? {
        let body_bytes = event.body.len();
        #[cfg(test)]
        record_resident_event_body_bytes(retained_body_bytes.saturating_add(body_bytes));
        let key = event.key();
        let Some(event_entry) = projection.events.get(&key) else {
            #[cfg(test)]
            record_resident_event_body_bytes(retained_body_bytes);
            continue;
        };
        if event_entry.line.byte_offset < emit_from {
            #[cfg(test)]
            record_resident_event_body_bytes(retained_body_bytes);
            continue;
        }
        let source_record = &projection
            .sources
            .get(&event.source_id)
            .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
        let session = &projection
            .sessions
            .get(&(event.source_id.clone(), event.provider_session_id.clone()))
            .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
        let record = core_record(
            source,
            source_record,
            session,
            projection.copied_origins.get(&key),
            projection.file_references.get(&key).map(Vec::as_slice),
            &mut event,
        )?;
        let record_bytes = record
            .encode_stored()
            .map_err(|error| {
                CustomHistorySourceBackedError::Capture(crate::CaptureError::InvalidPayload(
                    error.to_string(),
                ))
            })?
            .len();
        if !documents.is_empty()
            && (documents.len() == CUSTOM_PAGE_MAX_DOCUMENTS
                || retained_bytes.saturating_add(record_bytes) > CUSTOM_PAGE_MAX_RETAINED_BYTES)
        {
            emit(CustomHistorySourceBackedPage {
                records: std::mem::take(&mut documents),
            })?;
            retained_bytes = 0;
            retained_body_bytes = 0;
            #[cfg(test)]
            record_resident_event_body_bytes(body_bytes);
        }
        retained_bytes = retained_bytes.saturating_add(record_bytes);
        retained_body_bytes = retained_body_bytes.saturating_add(body_bytes);
        documents.push(record);
        #[cfg(test)]
        record_resident_event_body_bytes(retained_body_bytes);
    }
    if !documents.is_empty() {
        emit(CustomHistorySourceBackedPage { records: documents })?;
    }
    #[cfg(test)]
    record_resident_event_body_bytes(0);
    Ok(())
}

#[cfg(test)]
fn record_resident_event_body_bytes(bytes: usize) {
    record_custom_history_work(|work| {
        work.resident_event_body_bytes = bytes;
        work.peak_resident_event_body_bytes = work.peak_resident_event_body_bytes.max(bytes);
    });
}

#[allow(clippy::too_many_arguments)]
fn core_record(
    source: &SourceKey,
    source_record: &CustomSourceCatalogEntry,
    session: &CustomSessionCatalogEntry,
    copied_from: Option<&ValidatedCopiedFrom>,
    file_references: Option<&[ProviderDeclaredFact]>,
    event: &mut SpooledCustomEvent,
) -> CustomHistorySourceBackedResult<CoreRecord> {
    let session_id = custom_session_identity(
        source,
        &source_record.provider_key,
        &event.source_id,
        &session.provider_session_id,
    )
    .map_err(core_contract)?;
    let parent_session_id = session
        .parent_provider_session_id
        .as_deref()
        .map(|parent| {
            custom_session_identity(
                source,
                &source_record.provider_key,
                &event.source_id,
                parent,
            )
        })
        .transpose()?;
    let root_session_id = session
        .root_provider_session_id
        .as_deref()
        .map(|root| {
            custom_session_identity(source, &source_record.provider_key, &event.source_id, root)
        })
        .transpose()?;
    let event_key = custom_event_typed_key_parts(event.event_id.as_deref(), event.event_index)?;
    let native_item_key = NativeItemKey::native_id(CUSTOM_EVENT_KEY_NAMESPACE, event_key.clone())?;
    let event_selector = event.event_id.as_ref().map_or_else(
        || format!("event_index:{}", event.event_index),
        |id| format!("event_id:{id}"),
    );
    let native_event_id = TypedKey::composite(vec![
        TypedKey::utf8(source_record.provider_key.clone())?,
        TypedKey::utf8(event.source_id.clone())?,
        TypedKey::utf8(event_selector)?,
    ])?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: CUSTOM_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        event.event_index,
        event.event_type.clone(),
        CUSTOM_SOURCE_BACKED_PARSER_REVISION,
        std::mem::take(&mut event.body),
    )
    .map_err(core_contract)?;
    record.parent_session_id = parent_session_id;
    record.root_session_id = root_session_id;
    record.session_relationship = session.session_relationship;
    record.provider_session_id = Some(session.provider_session_id.clone());
    record.native_event_id = Some(native_event_id);
    if let Some(copied_from) = copied_from {
        let ancestor_session_id = custom_session_identity(
            source,
            &source_record.provider_key,
            &event.source_id,
            &copied_from.ancestor_provider_session_id,
        )?;
        let ancestor_native_item_key = NativeItemKey::native_id(
            CUSTOM_EVENT_KEY_NAMESPACE,
            // A copied claim always carries an exact native event selector, so
            // the positional fallback is intentionally irrelevant. This keeps
            // unresolved origin identity independent of target presence.
            custom_event_typed_key_parts(Some(&copied_from.ancestor_event_id), 0)?,
        )?;
        let ancestor_event_id = derive_event_id(EventIdentityInput {
            source,
            session_id: ancestor_session_id,
            logical_item_kind: CUSTOM_LOGICAL_EVENT_KIND,
            native_item_key: &ancestor_native_item_key,
            subrecord_selector: None,
        })?;
        record.event_copy = Some(ProviderNativeEventCopy {
            ancestor_session_id,
            ancestor_event_id,
            proof: copied_from.proof,
        });
    }
    record.occurred_at_unix_ms = Some(event.occurred_at_unix_ms);
    record.role = event.role.clone();
    record.agent_scope = session.agent_scope;
    record.content.structured_content = Some(std::mem::take(&mut event.payload));
    let mut activity = event.activity.take();
    let mut facts = Vec::new();
    if let Some(cwd) = &session.cwd {
        facts.push(ProviderDeclaredFact {
            kind: LiteralFactKind::SessionCwd,
            value: cwd.clone(),
        });
    }
    if let Some(activity) = activity.as_mut() {
        facts.append(&mut activity.facts);
    }
    if let Some(file_references) = file_references {
        facts.extend(file_references.iter().cloned());
    }
    if !facts.is_empty() {
        activity
            .get_or_insert_with(|| CoreActivity {
                revision: CORE_ACTIVITY_REVISION,
                provider_call_id: None,
                invocation: None,
                result: None,
                facts: Vec::new(),
            })
            .facts = facts;
    }
    record.content.activity = activity;
    fit_jsonl_activity(
        record.content.normalized_body.as_deref().unwrap_or(""),
        record.content.structured_content.as_ref(),
        &mut record.content.activity,
        JsonlActivityObservedBytes::infer_from_present(),
        MAX_CORE_CONTENT_BYTES,
    );
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()
        .map_err(core_contract)?;
    record.validate_contract().map_err(core_contract)?;
    Ok(record)
}

fn core_contract(error: impl std::fmt::Display) -> CustomHistorySourceBackedError {
    CustomHistorySourceBackedError::Capture(crate::CaptureError::InvalidPayload(error.to_string()))
}

pub(super) fn write_spooled_event(
    writer: &mut impl Write,
    event: &SpooledCustomEvent,
) -> CustomHistorySourceBackedResult<()> {
    writer.write_all(&[1])?;
    write_spool_string(writer, &event.source_id)?;
    write_spool_string(writer, &event.provider_session_id)?;
    writer.write_all(&event.event_index.to_be_bytes())?;
    write_optional_spool_string(writer, event.event_id.as_deref())?;
    write_spool_string(writer, &event.event_type)?;
    write_optional_spool_string(writer, event.role.as_deref())?;
    writer.write_all(&event.occurred_at_unix_ms.to_be_bytes())?;
    write_spool_string(writer, &event.body)?;
    write_spool_string(writer, &serde_json::to_string(&event.payload)?)?;
    let activity = event
        .activity
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    write_optional_spool_string(writer, activity.as_deref())?;
    Ok(())
}

pub(super) fn read_spooled_event(
    reader: &mut impl Read,
) -> CustomHistorySourceBackedResult<Option<SpooledCustomEvent>> {
    let mut marker = [0_u8; 1];
    match reader.read_exact(&mut marker) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    if marker != [1] {
        return Err(CustomHistorySourceBackedError::CountMismatch);
    }
    let source_id = read_spool_string(reader, CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES)?;
    let provider_session_id = read_spool_string(reader, CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES)?;
    let event_index = read_spool_u64(reader)?;
    let event_id = read_optional_spool_string(reader, MAX_PROVIDER_JSONL_LINE_BYTES)?;
    let event_type = read_spool_string(reader, 128)?;
    let role = read_optional_spool_string(reader, 128)?;
    let occurred_at_unix_ms = read_spool_i64(reader)?;
    let body = read_spool_string(reader, MAX_PROVIDER_JSONL_LINE_BYTES)?;
    let payload = serde_json::from_str(&read_spool_string(reader, MAX_PROVIDER_JSONL_LINE_BYTES)?)?;
    let activity = read_optional_spool_string(reader, MAX_PROVIDER_JSONL_LINE_BYTES)?
        .map(|activity| serde_json::from_str(&activity))
        .transpose()?;
    Ok(Some(SpooledCustomEvent {
        source_id,
        provider_session_id,
        event_index,
        event_id,
        event_type,
        role,
        occurred_at_unix_ms,
        body,
        payload,
        activity,
    }))
}

fn write_spool_string(writer: &mut impl Write, value: &str) -> CustomHistorySourceBackedResult<()> {
    let length =
        u64::try_from(value.len()).map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(value.as_bytes())?;
    Ok(())
}

fn write_optional_spool_string(
    writer: &mut impl Write,
    value: Option<&str>,
) -> CustomHistorySourceBackedResult<()> {
    match value {
        Some(value) => {
            writer.write_all(&[1])?;
            write_spool_string(writer, value)
        }
        None => {
            writer.write_all(&[0])?;
            Ok(())
        }
    }
}

fn read_spool_string(
    reader: &mut impl Read,
    maximum: usize,
) -> CustomHistorySourceBackedResult<String> {
    let length = usize::try_from(read_spool_u64(reader)?)
        .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    if length > maximum {
        return Err(CustomHistorySourceBackedError::CountMismatch);
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    String::from_utf8(bytes).map_err(|_| CustomHistorySourceBackedError::CountMismatch)
}

fn read_optional_spool_string(
    reader: &mut impl Read,
    maximum: usize,
) -> CustomHistorySourceBackedResult<Option<String>> {
    let mut marker = [0_u8; 1];
    reader.read_exact(&mut marker)?;
    match marker {
        [0] => Ok(None),
        [1] => read_spool_string(reader, maximum).map(Some),
        _ => Err(CustomHistorySourceBackedError::CountMismatch),
    }
}

fn read_spool_u64(reader: &mut impl Read) -> CustomHistorySourceBackedResult<u64> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_be_bytes(bytes))
}

fn read_spool_i64(reader: &mut impl Read) -> CustomHistorySourceBackedResult<i64> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(i64::from_be_bytes(bytes))
}
