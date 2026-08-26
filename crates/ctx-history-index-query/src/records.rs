use super::*;

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
