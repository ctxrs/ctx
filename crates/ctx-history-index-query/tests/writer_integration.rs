use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentScope as CoreAgentScope, CertifiedSource,
    CertifiedSourceAppend, CertifiedSourceDeletion, CertifiedSourceInventory, CoreActivity,
    CoreDiscoveryExclusion, CoreRecord, CoreRecordAnnotation, EventIdentityInput, LiteralFactKind,
    NativeItemKey, NativeSessionKey, ProviderDeclaredFact, ProviderNativeCopyProof,
    ProviderNativeEventCopy, ProviderNativeSessionRelationship, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceFrontier, SourceInventoryObservation, SourceKey,
    SourceObservation, TypedKey, CORE_ACTIVITY_REVISION,
};
use ctx_history_index::*;
use ctx_history_index_format::{
    core_content_bytes, fields_from_schema, lexical_schema, load_publication_for_metas,
    required_field, source_token, write_manifest, CommitPayload, IndexDocument,
    COMMIT_PAYLOAD_VERSION, INDEX_MEMORY_MIN_PER_THREAD,
};
use ctx_history_index_generation::{
    load_active_generation_pointer as load_generation_pointer, manifest_path,
    physical_integrity_digest, publish_active_generation_pointer, ActiveGenerationPointer,
    DurableMmapDirectory, GenerationSlot, INDEX_GENERATIONS_DIRECTORY,
};
use tantivy::{
    collector::{Count, DocSetCollector},
    indexer::NoMergePolicy,
    query::{AllQuery, TermQuery},
    schema::{Document as TantivyDocumentTrait, IndexRecordOption, Value as TantivyValue},
    Index, ReloadPolicy, Searcher, TantivyDocument, Term,
};
use tempfile::{tempdir, TempDir};
use uuid::Uuid;

fn load_active_generation_pointer(root: &Path) -> Result<Option<ActiveGenerationPointer>> {
    Ok(load_generation_pointer(root)?)
}

fn source(name: &str) -> SourceKey {
    source_for_provider("codex", "codex_session_jsonl", name)
}

fn source_for_provider(provider: &str, source_format: &str, name: &str) -> SourceKey {
    SourceKey::derive(
        provider,
        source_format,
        "session",
        1,
        SourceAnchor::provider_native("session-file", TypedKey::utf8(name).unwrap()).unwrap(),
    )
    .unwrap()
}

fn certificate(source: &SourceKey, revision: u8, documents: u64) -> CertifiedSource {
    let opening =
        SourceObservation::new(source.clone(), "regular-file-v1", vec![revision]).unwrap();
    CertifiedSource::certify(
        opening.clone(),
        opening,
        "codex-parser-v1",
        [revision; 32],
        ScannedSourceCounts {
            complete_records: documents,
            retained_records: documents,
            indexed_documents: documents,
            certified_bytes: documents * 10,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn appendable_certificate(
    source: &SourceKey,
    revision: u8,
    documents: u64,
    bytes: u64,
) -> CertifiedSource {
    let observation =
        SourceObservation::new(source.clone(), "regular-file-v1", vec![revision]).unwrap();
    CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        "codex-parser-v1",
        [revision; 32],
        ScannedSourceCounts {
            complete_records: documents,
            retained_records: documents,
            indexed_documents: documents,
            certified_bytes: bytes,
            ..ScannedSourceCounts::default()
        },
        Some(
            SourceFrontier::new(
                "jsonl-byte-offset",
                TypedKey::U64(bytes),
                bytes,
                [revision; 32],
            )
            .unwrap(),
        ),
    )
    .unwrap()
}

fn deletion_evidence(
    source: &SourceKey,
    revision: u8,
) -> (CertifiedSourceDeletion, CertifiedSourceInventory) {
    let inventory = complete_inventory(source, revision, Vec::new());
    let deletion = CertifiedSourceDeletion::from_inventory(source.clone(), &inventory).unwrap();
    (deletion, inventory)
}

fn complete_inventory(
    authority_source: &SourceKey,
    revision: u8,
    sources: Vec<SourceKey>,
) -> CertifiedSourceInventory {
    let inventory = SourceInventoryObservation::new(
        authority_source.provider(),
        "provider-root",
        TypedKey::utf8("root-lineage").unwrap(),
        "tree-inventory-v1",
        vec![revision],
    )
    .unwrap();
    CertifiedSourceInventory::certify(inventory.clone(), inventory, "discovery-v1", sources)
        .unwrap()
}

fn document(source: &SourceKey, sequence: u64, body: &str) -> CoreRecord {
    document_for_session(source, "session", sequence, body)
}

fn retrieval_excluded(mut record: CoreRecord) -> CoreRecord {
    record.content.discovery_exclusion = Some(CoreDiscoveryExclusion::CtxRetrievalDerived);
    record.validate_contract().unwrap();
    record
}

fn document_for_session(
    source: &SourceKey,
    native_session_id: &str,
    sequence: u64,
    body: &str,
) -> CoreRecord {
    let native_session_coordinate = TypedKey::utf8(native_session_id).unwrap();
    let session_key =
        NativeSessionKey::native_id("session", native_session_coordinate.clone()).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let native_item_key = NativeItemKey::native_id(
        "message",
        TypedKey::utf8(format!("event-{sequence}")).unwrap(),
    )
    .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        sequence,
        "message",
        "index-test-core-record-v1",
        body,
    )
    .unwrap();
    record.provider_session_id = Some(native_session_id.to_owned());
    record.native_event_id = Some(TypedKey::U64(sequence));
    record.occurred_at_unix_ms = Some(1_700_000_000_000 + sequence as i64);
    record.role = Some("user".to_owned());
    record.agent_scope = Some(CoreAgentScope::Primary);
    record
}

fn with_annotation(mut record: CoreRecord, annotation: CoreRecordAnnotation) -> CoreRecord {
    record.content.structured_content = annotation.structured_content;
    if annotation.activity.is_some() {
        record.content.activity = annotation.activity;
    }
    record
}

fn replace_literal_fact(record: &mut CoreRecord, kind: LiteralFactKind, value: impl Into<String>) {
    let activity = record.content.activity.get_or_insert_with(|| CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: None,
        invocation: None,
        result: None,
        facts: Vec::new(),
    });
    activity.facts.retain(|fact| fact.kind != kind);
    activity.facts.push(ProviderDeclaredFact {
        kind,
        value: value.into(),
    });
}

fn add_literal_fact(record: &mut CoreRecord, kind: LiteralFactKind, value: impl Into<String>) {
    let activity = record.content.activity.get_or_insert_with(|| CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: None,
        invocation: None,
        result: None,
        facts: Vec::new(),
    });
    activity.facts.push(ProviderDeclaredFact {
        kind,
        value: value.into(),
    });
}

fn filtered_session_ids(index: &VerifiedIndex, filters: EventSearchFilters) -> Vec<Uuid> {
    sorted_uuids(
        index
            .search_event_candidates_with_filters("shared needle", &filters, 10)
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.event.session_id)
            .collect(),
    )
}

fn lexical_search_batch(
    index: &VerifiedIndex,
    natural_texts: &[&str],
    filters: &EventSearchFilters,
    limit: usize,
) -> ctx_history_index_query::LexicalSearchResult<ctx_history_index_query::LexicalSearchBatch> {
    execute_lexical_batch(
        index,
        ctx_history_index_query::LexicalMode::Search(natural_texts),
        filters,
        limit,
        None,
    )
}

fn lexical_search_batch_with_budget(
    index: &VerifiedIndex,
    natural_texts: &[&str],
    filters: &EventSearchFilters,
    limit: usize,
    budget: ctx_history_index_query::LexicalWorkBudget,
) -> ctx_history_index_query::LexicalSearchResult<ctx_history_index_query::LexicalSearchBatch> {
    execute_lexical_batch(
        index,
        ctx_history_index_query::LexicalMode::Search(natural_texts),
        filters,
        limit,
        Some(budget),
    )
}

fn lexical_list_batch(
    index: &VerifiedIndex,
    filters: &EventSearchFilters,
    limit: usize,
) -> ctx_history_index_query::LexicalSearchResult<ctx_history_index_query::LexicalSearchBatch> {
    execute_lexical_batch(
        index,
        ctx_history_index_query::LexicalMode::List,
        filters,
        limit,
        None,
    )
}

fn lexical_list_batch_with_budget(
    index: &VerifiedIndex,
    filters: &EventSearchFilters,
    limit: usize,
    budget: ctx_history_index_query::LexicalWorkBudget,
) -> ctx_history_index_query::LexicalSearchResult<ctx_history_index_query::LexicalSearchBatch> {
    execute_lexical_batch(
        index,
        ctx_history_index_query::LexicalMode::List,
        filters,
        limit,
        Some(budget),
    )
}

fn execute_lexical_batch(
    index: &VerifiedIndex,
    mode: ctx_history_index_query::LexicalMode<'_>,
    filters: &EventSearchFilters,
    limit: usize,
    budget: Option<ctx_history_index_query::LexicalWorkBudget>,
) -> ctx_history_index_query::LexicalSearchResult<ctx_history_index_query::LexicalSearchBatch> {
    let filter = CompiledSearchFilter::compile(filters.clone())?;
    let execution = ctx_history_index_query::LexicalExecution::new(mode, &filter, limit);
    let execution = match budget {
        Some(budget) => execution.with_budget_for_test(budget),
        None => execution,
    };
    index
        .execute_lexical(execution)
        .map(|observed| observed.batch)
        .map_err(|failure| ctx_history_index_query::LexicalSearchError::Index(failure.error))
}

fn sorted_uuids(mut ids: Vec<Uuid>) -> Vec<Uuid> {
    ids.sort();
    ids
}

fn indexed_document(record: CoreRecord) -> TantivyDocument {
    let schema = lexical_schema();
    let fields = fields_from_schema(&schema).unwrap();
    let encoded = record.encode_stored().unwrap();
    let content_bytes = core_content_bytes(&record.content).unwrap();
    let projected = IndexDocument::from_core(fields, record, encoded, content_bytes).unwrap();
    let mut document = TantivyDocument::default();
    for (field, value) in projected.iter_fields_and_values() {
        if let Some(value) = value.as_str() {
            document.add_text(field, value);
        } else if let Some(value) = value.as_bytes() {
            document.add_bytes(field, value);
        } else if let Some(value) = value.as_u64() {
            document.add_u64(field, value);
        } else if let Some(value) = value.as_i64() {
            document.add_i64(field, value);
        } else {
            panic!("canonical test projection contained an unsupported Tantivy value");
        }
    }
    document
}

fn decoded_stored_core(searcher: &Searcher, address: tantivy::DocAddress) -> CoreRecord {
    let fields = fields_from_schema(searcher.schema()).unwrap();
    let document: TantivyDocument = searcher.doc(address).unwrap();
    ctx_history_index_format::decode_core_document(searcher, address, &document, fields)
        .unwrap()
        .0
}

fn collect_source_pages(
    index: &VerifiedIndex,
    source: &SourceKey,
    limit: usize,
) -> Vec<EventRecord> {
    let mut cursor = None;
    let mut records = Vec::new();
    loop {
        let page = index
            .source_event_page(source, cursor.as_ref(), limit)
            .unwrap();
        records.extend(page.items);
        if page.terminal {
            assert!(page.next_cursor.is_none());
            return records;
        }
        cursor = Some(page.next_cursor.unwrap());
    }
}

fn publish_unchecked_generation(
    root: &Path,
    index: &Index,
    manifest: GenerationManifest,
    delete_sources: &[SourceKey],
    documents: Vec<TantivyDocument>,
) {
    let mut writer = index
        .writer_with_num_threads::<TantivyDocument>(1, INDEX_MEMORY_MIN_PER_THREAD)
        .unwrap();
    let source_key = required_field(&index.schema(), "source_key").unwrap();
    for source in delete_sources {
        writer.delete_term(Term::from_field_text(source_key, &source_token(source)));
    }
    for document in documents {
        writer.add_document(document).unwrap();
    }
    writer.commit().unwrap();
    writer.wait_merging_threads().unwrap();
    let pointer = load_active_generation_pointer(root).unwrap().unwrap();
    let generation_path = active_generation_path(root);
    let generation_id = manifest.generation_id().unwrap();
    write_manifest(root, &generation_id, &manifest).unwrap();
    let mut payload_writer = index
        .writer_with_num_threads::<TantivyDocument>(1, INDEX_MEMORY_MIN_PER_THREAD)
        .unwrap();
    payload_writer.set_merge_policy(Box::<NoMergePolicy>::default());
    let mut prepared = payload_writer.prepare_commit().unwrap();
    prepared.set_payload(
        &serde_json::to_string(&CommitPayload {
            version: COMMIT_PAYLOAD_VERSION,
            generation_id: generation_id.clone(),
            publication_metadata: None,
        })
        .unwrap(),
    );
    prepared.commit().unwrap();
    payload_writer.wait_merging_threads().unwrap();
    let physical_integrity_digest =
        physical_integrity_digest(index, &generation_path, Some(&pointer)).unwrap();
    let active = GenerationSlot::new(
        generation_id,
        pointer.active().directory().to_owned(),
        physical_integrity_digest,
    )
    .unwrap();
    publish_active_generation_pointer(root, &ActiveGenerationPointer::new(active, None).unwrap())
        .unwrap();
}

fn open_unverified_generation(root: &Path) -> (Searcher, GenerationManifest) {
    let directory = DurableMmapDirectory::open(active_generation_path(root)).unwrap();
    let index = Index::open(directory).unwrap();
    let metas = index.load_metas().unwrap();
    let manifest = load_publication_for_metas(root, &metas)
        .unwrap()
        .into_parts()
        .1;
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .unwrap();
    (reader.searcher(), Arc::unwrap_or_clone(manifest))
}

fn active_generation_path(root: &Path) -> PathBuf {
    let pointer = load_active_generation_pointer(root).unwrap().unwrap();
    root.join(INDEX_GENERATIONS_DIRECTORY)
        .join(pointer.active().directory())
}

#[path = "writer_integration/event_range.rs"]
mod event_range;
#[path = "writer_integration/pinned_generation.rs"]
mod pinned_generation;
#[path = "writer_integration/query.rs"]
mod query;
