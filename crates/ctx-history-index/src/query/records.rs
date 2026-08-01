use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QueryMetadataChunkHeader {
    chunk_index: usize,
    chunk_count: usize,
    total_bytes: usize,
    payload_bytes: usize,
    encoded_digest: [u8; QUERY_METADATA_CHUNK_DIGEST_BYTES],
}

fn query_metadata_chunk_header(chunk: &[u8]) -> Result<QueryMetadataChunkHeader> {
    const HEADER_PREFIX_BYTES: usize = 12;
    if chunk.len() < QUERY_METADATA_CHUNK_HEADER_BYTES
        || chunk.len() > QUERY_METADATA_CHUNK_BYTES
        || chunk[..4] != QUERY_METADATA_CHUNK_MAGIC
    {
        return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
    }
    let chunk_index = usize::from(u16::from_be_bytes([chunk[4], chunk[5]]));
    let chunk_count = usize::from(u16::from_be_bytes([chunk[6], chunk[7]]));
    let total_bytes = u32::from_be_bytes([chunk[8], chunk[9], chunk[10], chunk[11]]) as usize;
    let calculated_chunk_count = total_bytes.div_ceil(QUERY_METADATA_CHUNK_PAYLOAD_BYTES);
    if total_bytes == 0
        || total_bytes > MAX_QUERY_METADATA_BYTES
        || chunk_count == 0
        || chunk_count != calculated_chunk_count
        || chunk_index >= chunk_count
    {
        return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
    }
    let start = chunk_index
        .checked_mul(QUERY_METADATA_CHUNK_PAYLOAD_BYTES)
        .ok_or(IndexError::CountOverflow)?;
    let end = start
        .checked_add(QUERY_METADATA_CHUNK_PAYLOAD_BYTES)
        .ok_or(IndexError::CountOverflow)?
        .min(total_bytes);
    let payload_bytes = chunk.len() - QUERY_METADATA_CHUNK_HEADER_BYTES;
    if payload_bytes != end.saturating_sub(start) {
        return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
    }
    let encoded_digest: [u8; QUERY_METADATA_CHUNK_DIGEST_BYTES] = chunk
        [HEADER_PREFIX_BYTES..QUERY_METADATA_CHUNK_HEADER_BYTES]
        .try_into()
        .map_err(|_| IndexError::InvalidStoredDocumentField("query_metadata"))?;
    Ok(QueryMetadataChunkHeader {
        chunk_index,
        chunk_count,
        total_bytes,
        payload_bytes,
        encoded_digest,
    })
}

fn note_query_metadata_chunk_read() {
    #[cfg(test)]
    QUERY_METADATA_CHUNK_READS.set(QUERY_METADATA_CHUNK_READS.get().saturating_add(1));
}

fn note_query_metadata_exact_allocation(bytes: usize) {
    #[cfg(test)]
    QUERY_METADATA_EXACT_ALLOCATED_BYTES.set(
        QUERY_METADATA_EXACT_ALLOCATED_BYTES
            .get()
            .saturating_add(bytes),
    );
    #[cfg(not(test))]
    let _ = bytes;
}

pub(crate) fn stored_event_record(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    _fields: Fields,
) -> Result<EventRecord> {
    #[cfg(test)]
    STORED_EVENT_RECORD_MATERIALIZATIONS
        .set(STORED_EVENT_RECORD_MATERIALIZATIONS.get().saturating_add(1));
    let segment = searcher
        .segment_readers()
        .get(address.segment_ord as usize)
        .ok_or(IndexError::InvalidStoredDocumentField("query_metadata"))?;
    let column = segment
        .fast_fields()
        .bytes(QUERY_METADATA_FIELD)?
        .ok_or(IndexError::InvalidStoredDocumentField("query_metadata"))?;
    let maximum_chunks = MAX_QUERY_METADATA_BYTES.div_ceil(QUERY_METADATA_CHUNK_PAYLOAD_BYTES);
    let mut chunks_by_index = BTreeMap::new();
    let mut expected_layout = None;
    let mut observed_payload_bytes = 0_usize;
    let mut chunk = Vec::new();
    for (observed_chunks, term_ord) in column.term_ords(address.doc_id).enumerate() {
        if observed_chunks >= maximum_chunks {
            return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
        }
        chunk.clear();
        note_query_metadata_chunk_read();
        if !column.ord_to_bytes(term_ord, &mut chunk)? {
            return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
        }
        let header = query_metadata_chunk_header(&chunk)?;
        match expected_layout {
            None => {
                expected_layout = Some((
                    header.chunk_count,
                    header.total_bytes,
                    header.encoded_digest,
                ));
            }
            Some(expected)
                if expected
                    != (
                        header.chunk_count,
                        header.total_bytes,
                        header.encoded_digest,
                    ) =>
            {
                return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
            }
            Some(_) => {}
        }
        if chunks_by_index
            .insert(header.chunk_index, term_ord)
            .is_some()
        {
            return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
        }
        observed_payload_bytes = observed_payload_bytes
            .checked_add(header.payload_bytes)
            .ok_or(IndexError::CountOverflow)?;
    }
    let (chunk_count, total_bytes, expected_digest) =
        expected_layout.ok_or(IndexError::InvalidStoredDocumentField("query_metadata"))?;
    if chunks_by_index.len() != chunk_count || observed_payload_bytes != total_bytes {
        return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
    }

    // Authenticate the complete ordered payload before trusting the declared
    // total for its one exact allocation.
    let mut payload_digest = Sha256::new();
    payload_digest.update(QUERY_METADATA_DIGEST_DOMAIN);
    for (expected_index, (chunk_index, term_ord)) in chunks_by_index.iter().enumerate() {
        if *chunk_index != expected_index {
            return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
        }
        chunk.clear();
        note_query_metadata_chunk_read();
        if !column.ord_to_bytes(*term_ord, &mut chunk)? {
            return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
        }
        let header = query_metadata_chunk_header(&chunk)?;
        if header.chunk_index != expected_index
            || header.chunk_count != chunk_count
            || header.total_bytes != total_bytes
            || header.encoded_digest != expected_digest
        {
            return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
        }
        payload_digest.update(&chunk[QUERY_METADATA_CHUNK_HEADER_BYTES..]);
    }
    let actual_digest: [u8; QUERY_METADATA_CHUNK_DIGEST_BYTES] = payload_digest.finalize().into();
    if actual_digest != expected_digest {
        return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
    }

    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(total_bytes)
        .map_err(|_| IndexError::InvalidStoredDocumentField("query_metadata"))?;
    note_query_metadata_exact_allocation(total_bytes);
    for (expected_index, (chunk_index, term_ord)) in chunks_by_index.into_iter().enumerate() {
        if chunk_index != expected_index {
            return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
        }
        chunk.clear();
        note_query_metadata_chunk_read();
        if !column.ord_to_bytes(term_ord, &mut chunk)? {
            return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
        }
        let header = query_metadata_chunk_header(&chunk)?;
        if header.chunk_index != expected_index
            || header.chunk_count != chunk_count
            || header.total_bytes != total_bytes
            || header.encoded_digest != expected_digest
        {
            return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
        }
        encoded.extend_from_slice(&chunk[QUERY_METADATA_CHUNK_HEADER_BYTES..]);
    }
    if encoded.len() != total_bytes {
        return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
    }
    let event = query_metadata_event_record(&encoded)?;
    if fast_uuid(
        segment,
        address.doc_id,
        EVENT_ID_HIGH_FIELD,
        EVENT_ID_LOW_FIELD,
    )? != event.event_id.as_uuid()
        || fast_uuid(
            segment,
            address.doc_id,
            SESSION_ID_HIGH_FIELD,
            SESSION_ID_LOW_FIELD,
        )? != event.session_id.as_uuid()
        || fast_string(segment, address.doc_id, EVENT_IDENTITY_DIGEST_FIELD)?
            != hex(&event.event_id.digest())
        || fast_string(segment, address.doc_id, SOURCE_KEY_FIELD)? != source_token(&event.source)
    {
        return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
    }
    Ok(event)
}

fn fast_uuid(
    segment: &SegmentReader,
    doc: DocId,
    high_field: &'static str,
    low_field: &'static str,
) -> Result<Uuid> {
    let high = segment
        .fast_fields()
        .u64(high_field)?
        .first(doc)
        .ok_or(IndexError::InvalidStoredDocumentField(high_field))?;
    let low = segment
        .fast_fields()
        .u64(low_field)?
        .first(doc)
        .ok_or(IndexError::InvalidStoredDocumentField(low_field))?;
    Ok(Uuid::from_u128((u128::from(high) << 64) | u128::from(low)))
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
    let event = stored_event_record(searcher, address, fields)?;
    let document: TantivyDocument = searcher.doc(address)?;
    stored_core_event_record_from_document(&document, fields, event)
}

pub(super) fn stored_core_verification_record(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<(CoreEventRecord, [u8; 32])> {
    let event = stored_event_record(searcher, address, fields)?;
    let document: TantivyDocument = searcher.doc(address)?;
    let encoded_core_record = unique_required_bytes(&document, fields.core_record, "core_record")?;
    let leaf = crate::staging::core_record_leaf(event.event_id, encoded_core_record)?;
    let (record, _) = stored_core_event_record_from_document(&document, fields, event)?;
    verify_identity_field(
        &document,
        fields.event_identity,
        Some(record.event_id),
        "event_identity",
    )?;
    verify_identity_field(
        &document,
        fields.session_identity,
        Some(record.session_id),
        "session_identity",
    )?;
    verify_identity_field(
        &document,
        fields.parent_session_identity,
        record.parent_session_id,
        "parent_session_identity",
    )?;
    verify_identity_field(
        &document,
        fields.root_session_identity,
        Some(record.root_session_id),
        "root_session_identity",
    )?;
    Ok((record, leaf))
}

fn unique_required_bytes<'a>(
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

fn verify_identity_field(
    document: &TantivyDocument,
    field: tantivy::schema::Field,
    expected: Option<StableEntityId>,
    field_name: &'static str,
) -> Result<()> {
    let mut values = document.get_all(field);
    let actual = values
        .next()
        .map(|value| {
            value
                .as_bytes()
                .ok_or(IndexError::InvalidStoredDocumentField(field_name))
                .and_then(|encoded| StableEntityId::decode_canonical(encoded).map_err(Into::into))
        })
        .transpose()?;
    if values.next().is_some() || actual != expected {
        return Err(IndexError::InvalidStoredDocumentField(field_name));
    }
    Ok(())
}

fn note_stored_core_event_record_materialization() {
    #[cfg(test)]
    STORED_CORE_EVENT_RECORD_MATERIALIZATIONS.set(
        STORED_CORE_EVENT_RECORD_MATERIALIZATIONS
            .get()
            .saturating_add(1),
    );
}

fn query_metadata_event_record(encoded: &[u8]) -> Result<EventRecord> {
    if encoded.is_empty() || encoded.len() > MAX_QUERY_METADATA_BYTES {
        return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
    }
    let metadata: StoredQueryMetadata = serde_json::from_slice(encoded)?;
    metadata.source.validate_contract()?;
    metadata.event_id.validate_contract()?;
    metadata.session_id.validate_contract()?;
    metadata.root_session_id.validate_contract()?;
    if let Some(parent_session_id) = metadata.parent_session_id {
        parent_session_id.validate_contract()?;
        if parent_session_id.entity_kind() != StableEntityKind::Session {
            return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
        }
    }
    if let Some(native_event_id) = metadata.native_event_id.as_ref() {
        native_event_id.validate_contract()?;
    }
    let invalid_text = metadata.agent_type.is_empty()
        || metadata.event_type.is_empty()
        || metadata.agent_type.len() > super::MAX_DOCUMENT_METADATA_BYTES
        || metadata.event_type.len() > super::MAX_DOCUMENT_METADATA_BYTES
        || [
            metadata.provider_session_id.as_deref(),
            metadata.branch.as_deref(),
            metadata.role.as_deref(),
            metadata.workspace.as_deref(),
            metadata.cwd.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|value| value.is_empty() || value.len() > super::MAX_DOCUMENT_METADATA_BYTES);
    if metadata.event_id.entity_kind() != StableEntityKind::Event
        || metadata.session_id.entity_kind() != StableEntityKind::Session
        || metadata.root_session_id.entity_kind() != StableEntityKind::Session
        || metadata.event_id.source_digest() != metadata.source.identity().digest()
        || metadata.event_id.source_descriptor_digest() != metadata.source.exact_descriptor_digest()
        || metadata.session_id.source_digest() != metadata.source.identity().digest()
        || metadata.session_id.source_descriptor_digest()
            != metadata.source.exact_descriptor_digest()
        || invalid_text
    {
        return Err(IndexError::InvalidStoredDocumentField("query_metadata"));
    }
    let provider = metadata.source.provider().to_owned();
    let source_format = metadata.source.source_format().to_owned();
    Ok(EventRecord {
        event_id: metadata.event_id,
        session_id: metadata.session_id,
        parent_session_id: metadata.parent_session_id,
        root_session_id: metadata.root_session_id,
        source: metadata.source,
        provider,
        source_format,
        provider_session_id: metadata.provider_session_id,
        native_event_id: metadata.native_event_id,
        branch: metadata.branch,
        agent_type: metadata.agent_type,
        is_primary: metadata.is_primary,
        event_sequence: metadata.event_sequence,
        occurred_at_unix_ms: metadata.occurred_at_unix_ms,
        event_type: metadata.event_type,
        role: metadata.role,
        workspace: metadata.workspace,
        cwd: metadata.cwd,
        touched_files: Vec::new(),
    })
}

fn stored_core_event_record_from_document(
    document: &TantivyDocument,
    fields: Fields,
    mut event: EventRecord,
) -> Result<(CoreEventRecord, usize)> {
    note_stored_core_event_record_materialization();
    let encoded_core_record = required_bytes(document, fields.core_record, "core_record")?;
    let stored_core_bytes = encoded_core_record.len();
    let core_record = CoreRecord::decode_stored(encoded_core_record)?;
    if event.event_id != core_record.event_id
        || event.session_id != core_record.session_id
        || event.parent_session_id != core_record.parent_session_id
        || event.root_session_id != core_record.root_session_id
        || event.source != core_record.source
        || event.native_event_id != core_record.native_event_id
        || event.provider_session_id != core_record.provider_session_id
        || event.branch != core_record.branch
        || event.agent_type != core_record.agent_type
        || event.is_primary != core_record.is_primary
        || event.event_sequence != core_record.event_sequence
        || event.occurred_at_unix_ms != core_record.occurred_at_unix_ms
        || event.event_type != core_record.event_type
        || event.role != core_record.role
        || event.workspace != core_record.workspace
        || event.cwd != core_record.cwd
    {
        return Err(IndexError::InvalidStoredDocumentField("core_record"));
    }

    let mut touched_files = BTreeSet::new();
    for observation in &core_record.repository_file_observations {
        touched_files.insert(observation.relative_path.clone());
        if let Some(prior_relative_path) = &observation.prior_relative_path {
            touched_files.insert(prior_relative_path.clone());
        }
    }
    event.touched_files = touched_files.into_iter().collect();

    Ok((CoreEventRecord { event, core_record }, stored_core_bytes))
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
