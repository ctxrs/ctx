use super::*;

pub(crate) fn stored_event_record(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<EventRecord> {
    #[cfg(test)]
    STORED_EVENT_RECORD_MATERIALIZATIONS
        .set(STORED_EVENT_RECORD_MATERIALIZATIONS.get().saturating_add(1));
    let document: TantivyDocument = searcher.doc(address)?;
    let (core_record, _) = decode_core_document(searcher, address, &document, fields)?;
    Ok(event_record_from_owned_core(core_record))
}

fn fast_uuid(
    segment: &SegmentReader,
    doc: DocId,
    high_field: &'static str,
    low_field: &'static str,
) -> Result<Uuid> {
    let high = unique_fast_u64(segment, doc, high_field)?;
    let low = unique_fast_u64(segment, doc, low_field)?;
    Ok(Uuid::from_u128((u128::from(high) << 64) | u128::from(low)))
}

fn unique_fast_u64(segment: &SegmentReader, doc: DocId, field_name: &'static str) -> Result<u64> {
    let column = segment.fast_fields().u64(field_name)?;
    let mut values = column.values_for_doc(doc);
    let value = values
        .next()
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))?;
    if values.next().is_some() {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(value)
}

fn fast_string(segment: &SegmentReader, doc: DocId, field_name: &'static str) -> Result<String> {
    let column = segment
        .fast_fields()
        .str(field_name)?
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))?;
    let mut term_ords = column.term_ords(doc);
    let term_ord = term_ords
        .next()
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))?;
    if term_ords.next().is_some() {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    let mut value = String::new();
    if !column.ord_to_str(term_ord, &mut value)? {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(value)
}

pub(super) fn stored_core_event_record(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<CoreEventRecord> {
    stored_core_event_record_with_size(searcher, address, fields).map(|(record, _)| record)
}

pub(super) fn stored_core_event_record_with_size(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<(CoreEventRecord, usize)> {
    note_stored_core_event_record_materialization();
    let document: TantivyDocument = searcher.doc(address)?;
    let (core_record, stored_core_bytes) =
        decode_core_document(searcher, address, &document, fields)?;
    let event = event_record_from_core(&core_record);
    Ok((CoreEventRecord { event, core_record }, stored_core_bytes))
}

/// Returns exact indexed identity and size metadata without loading a stored
/// document.
pub(super) fn core_event_fast_preflight(
    searcher: &tantivy::Searcher,
    address: DocAddress,
) -> Result<(Uuid, usize, usize)> {
    let segment = searcher
        .segment_readers()
        .get(address.segment_ord as usize)
        .ok_or(IndexError::InvalidStoredDocumentField("event_id"))?;
    let event_id = fast_uuid(
        segment,
        address.doc_id,
        EVENT_ID_HIGH_FIELD,
        EVENT_ID_LOW_FIELD,
    )?;
    let encoded_core_bytes = core_record_encoded_bytes(searcher, address)?;
    let content_bytes = unique_fast_u64(segment, address.doc_id, CORE_CONTENT_BYTES_FIELD)?;
    let content_bytes = usize::try_from(content_bytes).map_err(|_| IndexError::CountOverflow)?;
    Ok((event_id, encoded_core_bytes, content_bytes))
}

fn core_record_encoded_bytes(searcher: &tantivy::Searcher, address: DocAddress) -> Result<usize> {
    let segment = searcher
        .segment_readers()
        .get(address.segment_ord as usize)
        .ok_or(IndexError::InvalidStoredDocumentField(
            CORE_RECORD_ENCODED_BYTES_FIELD,
        ))?;
    let encoded_core_bytes =
        unique_fast_u64(segment, address.doc_id, CORE_RECORD_ENCODED_BYTES_FIELD)?;
    let encoded_core_bytes =
        usize::try_from(encoded_core_bytes).map_err(|_| IndexError::CountOverflow)?;
    if encoded_core_bytes == 0 || encoded_core_bytes > MAX_ENCODED_CORE_RECORD_BYTES {
        return Err(IndexError::InvalidStoredDocumentField(
            CORE_RECORD_ENCODED_BYTES_FIELD,
        ));
    }
    Ok(encoded_core_bytes)
}

pub(crate) fn validate_core_record_encoded_bytes(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    actual_encoded_core_bytes: usize,
) -> Result<()> {
    if core_record_encoded_bytes(searcher, address)? != actual_encoded_core_bytes {
        return Err(IndexError::InvalidStoredDocumentField(
            CORE_RECORD_ENCODED_BYTES_FIELD,
        ));
    }
    Ok(())
}

pub(super) fn stored_core_event_record_with_source_json(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<StoredCoreEventRecord> {
    note_stored_core_event_record_materialization();
    let document: TantivyDocument = searcher.doc(address)?;
    let encoded_core_record = unique_required_bytes(&document, fields.core_record, "core_record")?;
    let core_record = decode_core_bytes(searcher, address, encoded_core_record)?;
    let content_bytes = core_content_bytes(&core_record.content)?;
    Ok(StoredCoreEventRecord {
        core_record,
        stored_json: StoredCoreRecordJson {
            content_bytes,
            document,
            core_record_field: fields.core_record,
        },
    })
}

pub(super) fn stored_core_verification_record(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<(CoreEventRecord, [u8; 32], usize)> {
    note_stored_core_event_record_materialization();
    let document: TantivyDocument = searcher.doc(address)?;
    let encoded_core_record = unique_required_bytes(&document, fields.core_record, "core_record")?;
    let core_record = decode_core_bytes(searcher, address, encoded_core_record)?;
    let leaf = crate::staging::core_record_leaf(core_record.event_id, encoded_core_record)?;
    let event = event_record_from_core(&core_record);
    Ok((
        CoreEventRecord { event, core_record },
        leaf,
        encoded_core_record.len(),
    ))
}

pub(super) fn unique_required_bytes<'a>(
    document: &'a TantivyDocument,
    field: tantivy::schema::Field,
    field_name: &'static str,
) -> Result<&'a [u8]> {
    let mut values = document.get_all(field);
    let value = values
        .next()
        .and_then(|value| value.as_bytes())
        .ok_or(IndexError::InvalidStoredDocumentField(field_name))?;
    if values.next().is_some() {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(value)
}

fn note_stored_core_event_record_materialization() {
    #[cfg(test)]
    STORED_CORE_EVENT_RECORD_MATERIALIZATIONS.set(
        STORED_CORE_EVENT_RECORD_MATERIALIZATIONS
            .get()
            .saturating_add(1),
    );
}

fn decode_core_document(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    document: &TantivyDocument,
    fields: Fields,
) -> Result<(CoreRecord, usize)> {
    let encoded_core_record = unique_required_bytes(document, fields.core_record, "core_record")?;
    let stored_core_bytes = encoded_core_record.len();
    let core_record = decode_core_bytes(searcher, address, encoded_core_record)?;
    Ok((core_record, stored_core_bytes))
}

fn decode_core_bytes(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    encoded_core_record: &[u8],
) -> Result<CoreRecord> {
    validate_core_record_encoded_bytes(searcher, address, encoded_core_record.len())?;
    #[cfg(test)]
    CORE_RECORD_DECODES.set(CORE_RECORD_DECODES.get().saturating_add(1));
    let core_record = CoreRecord::decode_stored(encoded_core_record)?;
    let segment = searcher
        .segment_readers()
        .get(address.segment_ord as usize)
        .ok_or(IndexError::InvalidStoredDocumentField("core_record"))?;
    if fast_uuid(
        segment,
        address.doc_id,
        EVENT_ID_HIGH_FIELD,
        EVENT_ID_LOW_FIELD,
    )? != core_record.event_id.as_uuid()
        || fast_uuid(
            segment,
            address.doc_id,
            SESSION_ID_HIGH_FIELD,
            SESSION_ID_LOW_FIELD,
        )? != core_record.session_id.as_uuid()
        || fast_string(segment, address.doc_id, EVENT_IDENTITY_DIGEST_FIELD)?
            != hex(&core_record.event_id.digest())
        || fast_string(segment, address.doc_id, SOURCE_KEY_FIELD)?
            != source_token(&core_record.source)
    {
        return Err(IndexError::InvalidStoredDocumentField("core_record"));
    }
    Ok(core_record)
}

fn touched_files(core_record: &CoreRecord) -> Vec<String> {
    let mut touched_files = BTreeSet::new();
    for observation in &core_record.repository_file_observations {
        touched_files.insert(observation.relative_path.clone());
        if let Some(prior_relative_path) = &observation.prior_relative_path {
            touched_files.insert(prior_relative_path.clone());
        }
    }
    touched_files.into_iter().collect()
}

fn event_record_from_core(core_record: &CoreRecord) -> EventRecord {
    EventRecord {
        event_id: core_record.event_id,
        session_id: core_record.session_id,
        parent_session_id: core_record.parent_session_id,
        root_session_id: core_record.root_session_id,
        source: core_record.source.clone(),
        provider: core_record.source.provider().to_owned(),
        source_format: core_record.source.source_format().to_owned(),
        provider_session_id: core_record.provider_session_id.clone(),
        native_event_id: core_record.native_event_id.clone(),
        branch: core_record.branch.clone(),
        agent_type: core_record.agent_type.clone(),
        is_primary: core_record.is_primary,
        event_sequence: core_record.event_sequence,
        occurred_at_unix_ms: core_record.occurred_at_unix_ms,
        event_type: core_record.event_type.clone(),
        role: core_record.role.clone(),
        workspace: core_record.workspace.clone(),
        cwd: core_record.cwd.clone(),
        touched_files: touched_files(core_record),
    }
}

fn event_record_from_owned_core(core_record: CoreRecord) -> EventRecord {
    let provider = core_record.source.provider().to_owned();
    let source_format = core_record.source.source_format().to_owned();
    let touched_files = touched_files(&core_record);

    EventRecord {
        event_id: core_record.event_id,
        session_id: core_record.session_id,
        parent_session_id: core_record.parent_session_id,
        root_session_id: core_record.root_session_id,
        source: core_record.source,
        provider,
        source_format,
        provider_session_id: core_record.provider_session_id,
        native_event_id: core_record.native_event_id,
        branch: core_record.branch,
        agent_type: core_record.agent_type,
        is_primary: core_record.is_primary,
        event_sequence: core_record.event_sequence,
        occurred_at_unix_ms: core_record.occurred_at_unix_ms,
        event_type: core_record.event_type,
        role: core_record.role,
        workspace: core_record.workspace,
        cwd: core_record.cwd,
        touched_files,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EventAddressCandidate {
    pub(super) identity_digest: [u8; 32],
    pub(super) address: DocAddress,
    pub(super) source_order: Option<SourceEventOrderKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SessionEventAddressCandidate {
    pub(super) order: SessionEventOrderKey,
    pub(super) address: DocAddress,
}

impl From<&EventRecord> for SessionRecord {
    fn from(event: &EventRecord) -> Self {
        Self {
            session_id: event.session_id,
            parent_session_id: event.parent_session_id,
            root_session_id: event.root_session_id,
            provider: event.provider.clone(),
            source_format: event.source_format.clone(),
            provider_session_id: event.provider_session_id.clone(),
            branch: event.branch.clone(),
            agent_type: event.agent_type.clone(),
            is_primary: event.is_primary,
            workspace: event.workspace.clone(),
            cwd: event.cwd.clone(),
            first_event_sequence: event.event_sequence,
            first_occurred_at_unix_ms: event.occurred_at_unix_ms,
        }
    }
}
