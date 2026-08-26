use super::*;

const SEARCH_REF_EVENT_RANGE_ORDER_FIELD: &str = "event_range_order";
const SEARCH_REF_SOURCE_KEY_FIELD: &str = "source_key";

pub(crate) fn stored_event_record(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<EventRecord> {
    stored_event_record_with_size(searcher, address, fields).map(|(record, _)| record)
}

pub(crate) fn stored_event_record_with_size(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<(EventRecord, usize)> {
    #[cfg(any(test, feature = "test-support"))]
    STORED_EVENT_RECORD_MATERIALIZATIONS
        .set(STORED_EVENT_RECORD_MATERIALIZATIONS.get().saturating_add(1));
    let document: TantivyDocument = searcher.doc(address)?;
    let (core_record, encoded_core_bytes) =
        ctx_history_index_format::decode_core_document(searcher, address, &document, fields)?;
    note_core_record_decode();
    Ok((
        event_record_from_owned_core(core_record),
        encoded_core_bytes,
    ))
}

/// Loads only the bounded metadata needed to rank, group, and later verify a
/// Search candidate. The stored Core JSON is never decoded here; one complete
/// Core decode is reserved for final-result hydration.
pub(crate) fn ranked_event_ref_at_address(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<(RankedEventRef, usize)> {
    let order = event_range_order_at_address(searcher, address)?;
    ranked_event_ref_at_address_with_order(searcher, address, fields, order)
}

pub(crate) fn ranked_event_ref_at_address_with_order(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
    order: ctx_history_index_format::EventRangeOrderKey,
) -> Result<(RankedEventRef, usize)> {
    let facts = ctx_history_index_format::core_document_fast_facts(searcher, address)?;
    let event_identity_digest = order.event_identity_digest();
    let event_id = ctx_history_index_format::CompactIdentity {
        digest: event_identity_digest,
    }
    .as_uuid();
    if event_id != facts.event_id
        || order.encoded_core_bytes() != facts.encoded_core_bytes
        || order.content_bytes() != facts.content_bytes
    {
        return Err(IndexError::InvalidStoredDocumentField(
            SEARCH_REF_EVENT_RANGE_ORDER_FIELD,
        ));
    }

    let segment = searcher
        .segment_readers()
        .get(address.segment_ord as usize)
        .ok_or(IndexError::InvalidStoredDocumentField(
            SESSION_ID_HIGH_FIELD,
        ))?;
    let compact_session = Uuid::from_u128(
        (u128::from(unique_fast_u64_for_search_ref(
            segment,
            address.doc_id,
            SESSION_ID_HIGH_FIELD,
        )?) << 64)
            | u128::from(unique_fast_u64_for_search_ref(
                segment,
                address.doc_id,
                SESSION_ID_LOW_FIELD,
            )?),
    );
    let source_owner_digest = indexed_source_owner_digest(segment, address.doc_id)?;
    let has_event_copy = indexed_event_copy_presence(segment, address.doc_id, fields)?;
    Ok((
        RankedEventRef {
            event_id,
            event_identity_digest,
            session_id: compact_session,
            source_owner_digest,
            event_sequence: order.event_sequence(),
            occurred_at_unix_ms: order.occurred_at_unix_ms(),
            has_event_copy,
        },
        facts.encoded_core_bytes,
    ))
}

fn indexed_source_owner_digest(segment: &tantivy::SegmentReader, doc: DocId) -> Result<[u8; 32]> {
    let column = segment
        .fast_fields()
        .str(SEARCH_REF_SOURCE_KEY_FIELD)?
        .ok_or(IndexError::InvalidStoredDocumentField(
            SEARCH_REF_SOURCE_KEY_FIELD,
        ))?;
    let mut term_ords = column.term_ords(doc);
    let term_ord = term_ords
        .next()
        .ok_or(IndexError::InvalidStoredDocumentField(
            SEARCH_REF_SOURCE_KEY_FIELD,
        ))?;
    if term_ords.next().is_some() {
        return Err(IndexError::InvalidStoredDocumentField(
            SEARCH_REF_SOURCE_KEY_FIELD,
        ));
    }
    let mut token = String::with_capacity(64);
    if !column.ord_to_str(term_ord, &mut token)? || token.len() != 64 {
        return Err(IndexError::InvalidStoredDocumentField(
            SEARCH_REF_SOURCE_KEY_FIELD,
        ));
    }
    let mut digest = [0_u8; 32];
    for (target, pair) in digest.iter_mut().zip(token.as_bytes().chunks_exact(2)) {
        let high = lowercase_hex_nibble(pair[0]).ok_or(IndexError::InvalidStoredDocumentField(
            SEARCH_REF_SOURCE_KEY_FIELD,
        ))?;
        let low = lowercase_hex_nibble(pair[1]).ok_or(IndexError::InvalidStoredDocumentField(
            SEARCH_REF_SOURCE_KEY_FIELD,
        ))?;
        *target = (high << 4) | low;
    }
    Ok(digest)
}

const fn lowercase_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn event_range_order_at_address(
    searcher: &tantivy::Searcher,
    address: DocAddress,
) -> Result<ctx_history_index_format::EventRangeOrderKey> {
    let segment = searcher
        .segment_readers()
        .get(address.segment_ord as usize)
        .ok_or(IndexError::InvalidStoredDocumentField(
            SEARCH_REF_EVENT_RANGE_ORDER_FIELD,
        ))?;
    let column = segment
        .fast_fields()
        .bytes(SEARCH_REF_EVENT_RANGE_ORDER_FIELD)?
        .ok_or(IndexError::InvalidStoredDocumentField(
            SEARCH_REF_EVENT_RANGE_ORDER_FIELD,
        ))?;
    let mut ordinals = column.term_ords(address.doc_id);
    let ordinal = ordinals
        .next()
        .ok_or(IndexError::InvalidStoredDocumentField(
            SEARCH_REF_EVENT_RANGE_ORDER_FIELD,
        ))?;
    if ordinals.next().is_some() {
        return Err(IndexError::InvalidStoredDocumentField(
            SEARCH_REF_EVENT_RANGE_ORDER_FIELD,
        ));
    }
    let mut encoded = Vec::with_capacity(ctx_history_index_format::EVENT_RANGE_ORDER_KEY_LEN);
    if !column.ord_to_bytes(ordinal, &mut encoded)? {
        return Err(IndexError::InvalidStoredDocumentField(
            SEARCH_REF_EVENT_RANGE_ORDER_FIELD,
        ));
    }
    ctx_history_index_format::EventRangeOrderKey::decode(&encoded)
}

fn unique_fast_u64_for_search_ref(
    segment: &tantivy::SegmentReader,
    doc: DocId,
    field: &'static str,
) -> Result<u64> {
    let column = segment.fast_fields().u64(field)?;
    let mut values = column.values_for_doc(doc);
    let value = values
        .next()
        .ok_or(IndexError::InvalidStoredDocumentField(field))?;
    if values.next().is_some() {
        return Err(IndexError::InvalidStoredDocumentField(field));
    }
    Ok(value)
}

fn indexed_event_copy_presence(
    segment: &tantivy::SegmentReader,
    doc: DocId,
    fields: Fields,
) -> Result<bool> {
    let inverted = segment.inverted_index(fields.event_copy_proof)?;
    for proof in [
        "native_event_identity",
        "native_copied_from_field",
        "native_call_result_identity",
    ] {
        let term = Term::from_field_text(fields.event_copy_proof, proof);
        let Some(term_info) = inverted.get_term_info(&term)? else {
            continue;
        };
        let mut postings =
            inverted.read_postings_from_terminfo(&term_info, IndexRecordOption::Basic)?;
        let posting_doc = postings.doc();
        if posting_doc == doc || (posting_doc < doc && postings.seek(doc) == doc) {
            return Ok(true);
        }
    }
    Ok(false)
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
        ctx_history_index_format::decode_core_document(searcher, address, &document, fields)?;
    note_core_record_decode();
    let event = event_record_from_core(&core_record);
    Ok((CoreEventRecord { event, core_record }, stored_core_bytes))
}

/// Returns exact indexed identity and size metadata without loading a stored
/// document.
pub(super) fn core_event_fast_preflight(
    searcher: &tantivy::Searcher,
    address: DocAddress,
) -> Result<(Uuid, usize, usize)> {
    let facts = ctx_history_index_format::core_document_fast_facts(searcher, address)?;
    Ok((
        facts.event_id,
        facts.encoded_core_bytes,
        facts.content_bytes,
    ))
}

pub(super) fn stored_core_event_record_with_source_json(
    searcher: &tantivy::Searcher,
    address: DocAddress,
    fields: Fields,
) -> Result<StoredCoreEventRecord> {
    note_stored_core_event_record_materialization();
    let document: TantivyDocument = searcher.doc(address)?;
    let (core_record, _, accepted_document) =
        ctx_history_index_format::decode_owned_core_document(searcher, address, document, fields)?;
    note_core_record_decode();
    let content_bytes = core_content_bytes(&core_record.content)?;
    Ok(StoredCoreEventRecord {
        core_record,
        stored_json: StoredCoreRecordJson {
            content_bytes,
            accepted_document,
        },
    })
}

fn note_stored_core_event_record_materialization() {
    #[cfg(any(test, feature = "test-support"))]
    STORED_CORE_EVENT_RECORD_MATERIALIZATIONS.set(
        STORED_CORE_EVENT_RECORD_MATERIALIZATIONS
            .get()
            .saturating_add(1),
    );
}

fn note_core_record_decode() {
    #[cfg(any(test, feature = "test-support"))]
    CORE_RECORD_DECODES.set(CORE_RECORD_DECODES.get().saturating_add(1));
}

fn event_record_from_core(core_record: &CoreRecord) -> EventRecord {
    EventRecord {
        event_id: core_record.event_id,
        session_id: core_record.session_id,
        parent_session_id: core_record.parent_session_id,
        root_session_id: core_record.root_session_id,
        session_relationship: core_record.session_relationship,
        event_copy: core_record.event_copy.clone(),
        source: core_record.source.clone(),
        provider: core_record.source.provider().to_owned(),
        source_format: core_record.source.source_format().to_owned(),
        provider_session_id: core_record.provider_session_id.clone(),
        native_event_id: core_record.native_event_id.clone(),
        agent_scope: core_record.agent_scope,
        event_sequence: core_record.event_sequence,
        occurred_at_unix_ms: core_record.occurred_at_unix_ms,
        event_type: core_record.event_type.clone(),
        role: core_record.role.clone(),
    }
}

fn event_record_from_owned_core(core_record: CoreRecord) -> EventRecord {
    let provider = core_record.source.provider().to_owned();
    let source_format = core_record.source.source_format().to_owned();

    EventRecord {
        event_id: core_record.event_id,
        session_id: core_record.session_id,
        parent_session_id: core_record.parent_session_id,
        root_session_id: core_record.root_session_id,
        session_relationship: core_record.session_relationship,
        event_copy: core_record.event_copy,
        source: core_record.source,
        provider,
        source_format,
        provider_session_id: core_record.provider_session_id,
        native_event_id: core_record.native_event_id,
        agent_scope: core_record.agent_scope,
        event_sequence: core_record.event_sequence,
        occurred_at_unix_ms: core_record.occurred_at_unix_ms,
        event_type: core_record.event_type,
        role: core_record.role,
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
        let (provider_key, source_id) = event
            .custom_source_identity()
            .map_or((None, None), |(provider_key, source_id)| {
                (Some(provider_key.to_owned()), Some(source_id.to_owned()))
            });
        Self {
            session_id: event.session_id,
            parent_session_id: event.parent_session_id,
            root_session_id: event.root_session_id,
            session_relationship: event.session_relationship,
            provider: event.provider.clone(),
            provider_key,
            source_id,
            source_format: event.source_format.clone(),
            provider_session_id: event.provider_session_id.clone(),
            agent_scope: event.agent_scope,
            first_event_sequence: event.event_sequence,
            first_occurred_at_unix_ms: event.occurred_at_unix_ms,
        }
    }
}
