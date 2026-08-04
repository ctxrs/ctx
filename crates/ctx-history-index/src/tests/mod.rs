use std::{collections::HashSet, sync::atomic::Ordering};

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSourceInventory, CoreRecord, CoreRecordAnnotation,
    EventIdentityInput, GitObjectFormat, GitObjectId, NativeItemKey, NativeSessionKey,
    RepositoryBinding, RepositoryEvidence, RepositoryEvidenceConfidence, RepositoryEvidenceKind,
    RepositoryFileObservation, RepositoryFileObservationKind, RepositoryOutcomeKind,
    RepositoryOutcomeLinkage, RepositoryOutcomeObservation, RepositoryVcsObservation,
    RepositoryVcsObservationKind, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceFrontier, SourceInventoryObservation, SourceObservation, TypedKey,
    CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION, CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
};
use tantivy::{
    collector::DocSetCollector, indexer::NoMergePolicy, query::AllQuery,
    schema::Value as TantivyValue,
};
use tempfile::{tempdir, TempDir};

use super::*;

pub(crate) fn source(name: &str) -> SourceKey {
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
    CertifiedSourceInventory::certify(inventory.clone(), inventory, "discovery-v1", sources)
        .unwrap()
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

pub(crate) fn document(source: &SourceKey, sequence: u64, body: &str) -> CoreRecord {
    document_for_session(source, "session", sequence, body)
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
        session_id,
        source.clone(),
        sequence,
        "message",
        "primary",
        true,
        "index-test-core-record-v1",
        body,
    )
    .unwrap();
    record.provider_session_id = Some(native_session_id.to_owned());
    record.native_event_id = Some(TypedKey::U64(sequence));
    record.branch = Some("main".to_owned());
    record.occurred_at_unix_ms = Some(1_700_000_000_000 + sequence as i64);
    record.role = Some("user".to_owned());
    record.workspace = Some("ctx".to_owned());
    record.cwd = Some("/work/ctx".to_owned());
    record
}

fn with_annotation(mut record: CoreRecord, annotation: CoreRecordAnnotation) -> CoreRecord {
    record.content.structured_content = annotation.structured_content;
    record.metadata = annotation.metadata;
    record.repository_candidate_evidence = annotation.repository_candidate_evidence;
    record.repository_bindings = annotation.repository_bindings;
    record.repository_abstentions = annotation.repository_abstentions;
    record.repository_file_observations = annotation.repository_file_observations;
    record.repository_vcs_observations = annotation.repository_vcs_observations;
    record
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

fn indexed_document(record: CoreRecord) -> TantivyDocument {
    let schema = lexical_schema();
    let fields = fields_from_schema(&schema).unwrap();
    let encoded = record.encode_stored().unwrap();
    let content_bytes = core_content_bytes(&record.content).unwrap();
    let source_fields = IndexSourceFields::new(&record.source, &source_token(&record.source));
    IndexDocument::from_core(fields, record, encoded.into(), content_bytes, source_fields)
        .unwrap()
        .into_tantivy_document()
}

fn decoded_stored_core(searcher: &Searcher, address: tantivy::DocAddress) -> CoreRecord {
    let fields = fields_from_schema(searcher.schema()).unwrap();
    let document: TantivyDocument = searcher.doc(address).unwrap();
    let encoded = document
        .get_first(fields.core_record)
        .and_then(|value| value.as_bytes())
        .unwrap();
    CoreRecord::decode_stored(encoded).unwrap()
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
    let physical_integrity_digest = physical_integrity_digest(index, &generation_path).unwrap();
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
    let manifest = load_publication_for_metas(root, &metas).unwrap().manifest;
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .unwrap();
    (reader.searcher(), manifest)
}

fn active_generation_path(root: &Path) -> PathBuf {
    let pointer = load_active_generation_pointer(root).unwrap().unwrap();
    root.join(INDEX_GENERATIONS_DIRECTORY)
        .join(pointer.active().directory())
}

fn omit_managed_and_corrupt_body_projection(generation_path: &Path) -> PathBuf {
    use std::{
        collections::HashSet,
        io::{Read, Seek, Write},
    };

    let managed_path = generation_path.join(".managed.json");
    let mut managed = serde_json::from_slice::<HashSet<PathBuf>>(
        &fs::read(&managed_path).expect("managed topology must be readable"),
    )
    .expect("managed topology must be valid");
    let projection_path = fs::read_dir(generation_path)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|extension| extension == "pos")
        })
        .or_else(|| {
            fs::read_dir(generation_path)
                .unwrap()
                .filter_map(std::result::Result::ok)
                .map(|entry| entry.path())
                .find(|path| {
                    path.extension()
                        .and_then(std::ffi::OsStr::to_str)
                        .is_some_and(|extension| extension == "idx")
                })
        })
        .expect("generation must contain a body-search projection file");
    let relative = projection_path
        .strip_prefix(generation_path)
        .unwrap()
        .to_path_buf();
    assert!(
        managed.remove(&relative),
        "active body-search projection must begin in the managed topology"
    );
    fs::write(&managed_path, serde_json::to_vec(&managed).unwrap()).unwrap();

    let mut projection = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&projection_path)
        .unwrap();
    let offset = projection.metadata().unwrap().len() / 2;
    projection.seek(std::io::SeekFrom::Start(offset)).unwrap();
    let mut byte = [0_u8; 1];
    projection.read_exact(&mut byte).unwrap();
    byte[0] ^= 0x5a;
    projection.seek(std::io::SeekFrom::Start(offset)).unwrap();
    projection.write_all(&byte).unwrap();
    projection.sync_all().unwrap();
    projection_path
}

fn corrupt_candidate_segment_store(
    generation_path: &Path,
    base_segment_ids: &HashSet<String>,
    corrupt_retained_segment: bool,
) -> PathBuf {
    use std::io::{Read, Seek, Write};

    let directory = DurableMmapDirectory::open(generation_path).unwrap();
    let index = Index::open(directory).unwrap();
    let metas = index.load_metas().unwrap();
    let segment = metas
        .segments
        .iter()
        .find(|segment| {
            base_segment_ids.contains(&segment.id().uuid_string()) == corrupt_retained_segment
        })
        .unwrap_or_else(|| {
            panic!(
                "candidate must contain a {} segment",
                if corrupt_retained_segment {
                    "retained"
                } else {
                    "changed"
                }
            )
        });
    let store_path =
        generation_path.join(segment.relative_path(tantivy::index::SegmentComponent::Store));
    drop(index);

    if corrupt_retained_segment {
        let private_copy = store_path.with_extension("store.ctx-corruption-copy");
        fs::copy(&store_path, &private_copy).unwrap();
        fs::remove_file(&store_path).unwrap();
        fs::rename(private_copy, &store_path).unwrap();
    }

    let mut store = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&store_path)
        .unwrap();
    let length = store.metadata().unwrap().len();
    assert!(
        length > 0,
        "segment store must contain a CRC-protected file"
    );
    let offset = length / 2;
    store.seek(std::io::SeekFrom::Start(offset)).unwrap();
    let mut byte = [0_u8; 1];
    store.read_exact(&mut byte).unwrap();
    byte[0] ^= 0x5a;
    store.seek(std::io::SeekFrom::Start(offset)).unwrap();
    store.write_all(&byte).unwrap();
    store.sync_all().unwrap();
    store_path
}

fn multisegment_fixture(
    source_count: usize,
    documents_per_source: u64,
) -> (TempDir, Vec<SourceKey>) {
    assert!(source_count < LEXICAL_SEGMENT_MERGE_FAN_IN);
    let temp = tempdir().unwrap();
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: INDEX_MEMORY_MIN_PER_THREAD,
    };
    let mut sources = Vec::with_capacity(source_count);
    for source_index in 0..source_count {
        let current = source(&format!("verification-{source_index}.jsonl"));
        let mut writer = GenerationWriter::open(temp.path(), options.clone())
            .unwrap()
            .into_writer()
            .unwrap();
        writer.begin_source(current.clone()).unwrap();
        for sequence in 1..=documents_per_source {
            writer
                .add_core_record(document_for_session(
                    &current,
                    &format!("session-{source_index}"),
                    sequence,
                    "representative verifier fixture",
                ))
                .unwrap();
        }
        writer
            .certify_source(certificate(
                &current,
                (source_index + 1) as u8,
                documents_per_source,
            ))
            .unwrap();
        writer.commit(|_| true).unwrap();
        sources.push(current);
    }
    (temp, sources)
}

mod integrity_certification;
mod migration;
#[cfg(target_os = "linux")]
mod migration_qualification;
mod pinned_generation;
mod publication_metadata;
mod query;
mod recovery;
mod writer;
