//! Bounded emission from the invocation-local Custom History spool.

use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom, Write},
};

use ctx_history_core::{
    derive_event_id, EventIdentityInput, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, SourceKey, SourceRecordLocator,
};
use ctx_history_index::LexicalDocument;

#[cfg(test)]
use super::source_backed::record_custom_history_work;
use super::source_backed::{
    bounded_metadata, custom_event_typed_key_parts, custom_session_identity,
    custom_session_typed_key, CustomHistorySourceBackedError, CustomHistorySourceBackedInput,
    CustomHistorySourceBackedPage, CustomHistorySourceBackedResult, CustomSessionCatalogEntry,
    CustomSessionKey, CustomSourceCatalogEntry, ParsedProjection, SpooledCustomEvent,
    TouchSpoolRef, CUSTOM_EVENT_KEY_NAMESPACE, CUSTOM_LOGICAL_EVENT_KIND,
    CUSTOM_PAGE_MAX_DOCUMENTS, CUSTOM_PAGE_MAX_RETAINED_BYTES,
};
use crate::provider::custom_history_jsonl::{
    custom_history_internal_session_id, CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES,
};
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

    #[cfg(test)]
    pub(super) const fn new(max_records: usize, max_metadata_bytes: usize) -> Self {
        Self {
            max_records,
            max_metadata_bytes,
        }
    }
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
    input: &CustomHistorySourceBackedInput,
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
            .get(&(event.source_id.clone(), event.session_id.clone()))
            .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
        let touched_files = read_touched_files(
            &mut projection.touch_spool,
            projection.event_touches.get(&key).map(Vec::as_slice),
        )?;
        let document = lexical_document(
            source,
            input,
            &projection.session_roots,
            source_record,
            session,
            &mut event,
            &event_entry.line,
            touched_files,
        )?;
        let document_bytes = retained_document_bytes(&document);
        if !documents.is_empty()
            && (documents.len() == CUSTOM_PAGE_MAX_DOCUMENTS
                || retained_bytes.saturating_add(document_bytes) > CUSTOM_PAGE_MAX_RETAINED_BYTES)
        {
            emit(CustomHistorySourceBackedPage {
                documents: std::mem::take(&mut documents),
            })?;
            retained_bytes = 0;
            retained_body_bytes = 0;
            #[cfg(test)]
            record_resident_event_body_bytes(body_bytes);
        }
        retained_bytes = retained_bytes.saturating_add(document_bytes);
        retained_body_bytes = retained_body_bytes.saturating_add(body_bytes);
        documents.push(document);
        #[cfg(test)]
        record_resident_event_body_bytes(retained_body_bytes);
    }
    if !documents.is_empty() {
        emit(CustomHistorySourceBackedPage { documents })?;
    }
    #[cfg(test)]
    record_resident_event_body_bytes(0);
    Ok(())
}

fn read_touched_files(
    spool: &mut File,
    references: Option<&[TouchSpoolRef]>,
) -> CustomHistorySourceBackedResult<Vec<String>> {
    let Some(references) = references else {
        return Ok(Vec::new());
    };
    let mut paths = Vec::with_capacity(references.len());
    for reference in references {
        spool.seek(SeekFrom::Start(reference.byte_offset))?;
        let mut bytes = vec![0; reference.byte_length];
        spool.read_exact(&mut bytes)?;
        paths.push(
            String::from_utf8(bytes).map_err(|_| CustomHistorySourceBackedError::CountMismatch)?,
        );
    }
    Ok(paths)
}

#[cfg(test)]
fn record_resident_event_body_bytes(bytes: usize) {
    record_custom_history_work(|work| {
        work.resident_event_body_bytes = bytes;
        work.peak_resident_event_body_bytes = work.peak_resident_event_body_bytes.max(bytes);
    });
}

#[allow(clippy::too_many_arguments)]
fn lexical_document(
    source: &SourceKey,
    input: &CustomHistorySourceBackedInput,
    session_roots: &BTreeMap<CustomSessionKey, String>,
    source_record: &CustomSourceCatalogEntry,
    session: &CustomSessionCatalogEntry,
    event: &mut SpooledCustomEvent,
    line: &super::source_backed::CompleteLine,
    touched_files: Vec<String>,
) -> CustomHistorySourceBackedResult<LexicalDocument> {
    let session_id = custom_session_identity(
        source,
        &source_record.provider_key,
        &event.source_id,
        &session.session_id,
    )?;
    let parent_session_id = session
        .parent_session_id
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
    #[cfg(test)]
    record_custom_history_work(|work| {
        work.event_root_lookups = work.event_root_lookups.saturating_add(1);
    });
    let root_native_session_id = session_roots
        .get(&(session.source_id.clone(), session.session_id.clone()))
        .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
    let root_session_id = if root_native_session_id == &session.session_id {
        session_id
    } else {
        custom_session_identity(
            source,
            &source_record.provider_key,
            &event.source_id,
            root_native_session_id,
        )?
    };
    let event_key = custom_event_typed_key_parts(event.event_id.as_deref(), event.event_index)?;
    let native_item_key = NativeItemKey::native_id(CUSTOM_EVENT_KEY_NAMESPACE, event_key.clone())?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: CUSTOM_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::Jsonl {
            byte_offset: line.byte_offset,
            byte_length: line.byte_length,
            physical_ordinal: line.physical_ordinal,
            native_session_key: Some(custom_session_typed_key(
                &source_record.provider_key,
                &event.source_id,
                &session.session_id,
            )?),
            native_event_key: Some(event_key),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        line.record_digest,
    )?;
    let source_path = source_record
        .raw_source_path
        .clone()
        .or_else(|| input.path().to_str().and_then(bounded_metadata));
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id,
        root_session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(custom_history_internal_session_id(
            &source_record.provider_key,
            &event.source_id,
            &session.session_id,
        )),
        // The v1 interchange schema has no branch or workspace field.
        branch: None,
        source_path,
        agent_type: session.agent_type.clone(),
        is_primary: session.is_primary,
        event_sequence: event.event_index,
        occurred_at_unix_ms: Some(event.occurred_at_unix_ms),
        event_type: event.event_type.clone(),
        role: event.role.clone(),
        body: std::mem::take(&mut event.body),
        workspace: None,
        cwd: session.cwd.clone(),
        touched_files,
    })
}

fn retained_document_bytes(document: &LexicalDocument) -> usize {
    document
        .body
        .len()
        .saturating_add(document.provider_session_id.as_ref().map_or(0, String::len))
        .saturating_add(document.source_path.as_ref().map_or(0, String::len))
        .saturating_add(document.cwd.as_ref().map_or(0, String::len))
        .saturating_add(
            document
                .touched_files
                .iter()
                .map(String::len)
                .sum::<usize>(),
        )
        .saturating_add(512)
}

pub(super) fn write_spooled_event(
    writer: &mut impl Write,
    event: &SpooledCustomEvent,
) -> CustomHistorySourceBackedResult<()> {
    writer.write_all(&[1])?;
    write_spool_string(writer, &event.source_id)?;
    write_spool_string(writer, &event.session_id)?;
    writer.write_all(&event.event_index.to_be_bytes())?;
    write_optional_spool_string(writer, event.event_id.as_deref())?;
    write_spool_string(writer, &event.event_type)?;
    write_optional_spool_string(writer, event.role.as_deref())?;
    writer.write_all(&event.occurred_at_unix_ms.to_be_bytes())?;
    write_spool_string(writer, &event.body)?;
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
    let session_id = read_spool_string(reader, CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES)?;
    let event_index = read_spool_u64(reader)?;
    let event_id = read_optional_spool_string(reader, MAX_PROVIDER_JSONL_LINE_BYTES)?;
    let event_type = read_spool_string(reader, 128)?;
    let role = read_optional_spool_string(reader, 128)?;
    let occurred_at_unix_ms = read_spool_i64(reader)?;
    let body = read_spool_string(reader, MAX_PROVIDER_JSONL_LINE_BYTES)?;
    Ok(Some(SpooledCustomEvent {
        source_id,
        session_id,
        event_index,
        event_id,
        event_type,
        role,
        occurred_at_unix_ms,
        body,
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
