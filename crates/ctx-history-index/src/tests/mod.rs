use std::sync::atomic::Ordering;

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSourceInventory, EventIdentityInput,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceFrontier,
    SourceInventoryObservation, SourceObservation, TypedKey,
};
use tantivy::{
    collector::DocSetCollector, indexer::NoMergePolicy, query::AllQuery,
    schema::Value as TantivyValue,
};
use tempfile::tempdir;

use super::*;

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
    deletion_evidence_with_retained(source, revision, Vec::new())
}

fn deletion_evidence_with_retained(
    source: &SourceKey,
    revision: u8,
    retained: Vec<SourceKey>,
) -> (CertifiedSourceDeletion, CertifiedSourceInventory) {
    let inventory = complete_inventory(source, revision, retained);
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
    let inventory =
        CertifiedSourceInventory::certify(inventory.clone(), inventory, "discovery-v1", sources)
            .unwrap();
    inventory
}

fn stage_exact_replay(writer: &mut GenerationWriter, source: &SourceKey) -> CertifiedSource {
    let base = writer.begin_source_append(source.clone()).unwrap().clone();
    let frontier = base.frontier().unwrap();
    let replay = CertifiedSourceAppend::certify(
        &base,
        base.clone(),
        frontier.certified_prefix_bytes(),
        *frontier.certified_prefix_digest(),
    )
    .unwrap();
    writer.certify_source_append(replay).unwrap();
    base
}

fn document(source: &SourceKey, sequence: u64, body: &str) -> LexicalDocument {
    document_for_session(source, "session", sequence, body)
}

fn document_for_session(
    source: &SourceKey,
    native_session_id: &str,
    sequence: u64,
    body: &str,
) -> LexicalDocument {
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
    LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        locator: SourceRecordLocator::new(
            source.clone(),
            NativeRecordCoordinate::Jsonl {
                byte_offset: sequence * 100,
                byte_length: 100,
                physical_ordinal: sequence,
                native_session_key: Some(native_session_coordinate),
                native_event_key: Some(TypedKey::U64(sequence)),
            },
            LocatorRevisionPolicy::StableRecordEvidence,
            None,
            [sequence as u8; 32],
        )
        .unwrap(),
        provider_session_id: Some(native_session_id.to_owned()),
        branch: Some("main".to_owned()),
        source_path: Some(format!("/history/{native_session_id}.jsonl")),
        agent_type: "primary".to_owned(),
        is_primary: true,
        event_sequence: sequence,
        occurred_at_unix_ms: Some(1_700_000_000_000 + sequence as i64),
        event_type: "message".to_owned(),
        role: Some("user".to_owned()),
        body: body.to_owned(),
        workspace: Some("ctx".to_owned()),
        cwd: Some("/work/ctx".to_owned()),
        touched_files: vec!["src/lib.rs".to_owned()],
    }
}

fn filtered_session_ids(index: &VerifiedIndex, filters: EventSearchFilters) -> Vec<Uuid> {
    sorted_uuids(
        index
            .search_event_candidates_with_filters("shared needle", &filters, 10)
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.event.session_id.as_uuid())
            .collect(),
    )
}

fn sorted_uuids(mut ids: Vec<Uuid>) -> Vec<Uuid> {
    ids.sort();
    ids
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
    let generation_id = manifest.generation_id().unwrap();
    write_manifest(root, &generation_id, &manifest).unwrap();
    let mut prepared = writer.prepare_commit().unwrap();
    prepared.set_payload(
        &serde_json::to_string(&CommitPayload {
            version: COMMIT_PAYLOAD_VERSION,
            generation_id,
        })
        .unwrap(),
    );
    prepared.commit().unwrap();
    writer.wait_merging_threads().unwrap();
    sync_directory(root).unwrap();
}

mod query;
mod recovery;
mod writer;
