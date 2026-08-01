use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CertifiedSourceDeletion,
    CertifiedSourceInventory, CoreRecord, EventIdentityInput, NativeItemKey, NativeSessionKey,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceInventoryObservation,
    SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationWriter, WriterOptions};
use ctx_history_relational::{
    RawSqlOptions, RawSqlValue, RelationalProjectionStatus, RELATIONAL_MATERIALIZER_REVISION,
};

use super::*;
use crate::source_sql::SqlCompatibility;

const BODY_SENTINEL: &str = "complete-core-body-must-not-enter-relational";

fn source() -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("provider-session.jsonl").unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn certificate(source: &SourceKey, revision: u8, documents: u64) -> CertifiedSource {
    let observation =
        SourceObservation::new(source.clone(), "regular-file-v1", vec![revision]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "codex-parser-v1",
        [revision; 32],
        ScannedSourceCounts {
            complete_records: documents,
            retained_records: documents,
            indexed_documents: documents,
            certified_bytes: documents * 100,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn record(source: &SourceKey, sequence: u64, _provider_file: &Path) -> CoreRecord {
    let native_session = TypedKey::utf8("provider-session").unwrap();
    let session_key = NativeSessionKey::native_id("session", native_session.clone()).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let native_item = NativeItemKey::native_id("message", TypedKey::U64(sequence)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item,
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
        "codex-parser-v1",
        format!("{BODY_SENTINEL}-{sequence}"),
    )
    .unwrap();
    record.provider_session_id = Some("provider-session".to_owned());
    record.native_event_id = Some(TypedKey::U64(sequence));
    record.branch = Some("main".to_owned());
    record.occurred_at_unix_ms = Some(1_700_000_000_000 + sequence as i64);
    record.role = Some("user".to_owned());
    record.workspace = Some("ctx".to_owned());
    record.cwd = Some("/work/ctx".to_owned());
    record.validate_contract().unwrap();
    record
}

fn replace_generation(
    data_root: &Path,
    source: &SourceKey,
    revision: u8,
    records: Vec<CoreRecord>,
) -> String {
    let count = records.len() as u64;
    let mut writer = GenerationWriter::open(
        source_backed_index_root(data_root),
        WriterOptions::default(),
    )
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in records {
        writer.add_core_record(record).unwrap();
    }
    writer
        .certify_source(certificate(source, revision, count))
        .unwrap();
    writer.commit(|_| true).unwrap().generation_id
}

fn delete_generation(data_root: &Path, source: &SourceKey) -> String {
    let observation = SourceInventoryObservation::new(
        source.provider(),
        "provider-root",
        TypedKey::utf8("root-lineage").unwrap(),
        "tree-inventory-v1",
        vec![2],
    )
    .unwrap();
    let inventory =
        CertifiedSourceInventory::certify(observation.clone(), observation, "discovery-v1", vec![])
            .unwrap();
    let deletion = CertifiedSourceDeletion::from_inventory(source.clone(), &inventory).unwrap();
    let mut writer = GenerationWriter::open(
        source_backed_index_root(data_root),
        WriterOptions::default(),
    )
    .unwrap();
    writer.delete_source(deletion, inventory).unwrap();
    writer.commit(|_| true).unwrap().generation_id
}

fn query(data_root: &Path, sql: &str) -> Vec<Vec<RawSqlValue>> {
    SqlCompatibility::open_for_data_root(data_root)
        .unwrap()
        .query(sql, RawSqlOptions::default())
        .unwrap()
        .rows
}

fn query_projection(data_root: &Path, sql: &str) -> Vec<Vec<RawSqlValue>> {
    SourceBackedRelationalProjection::open_read_only(sql_compatibility_path(data_root))
        .unwrap()
        .raw_sql_query(sql, RawSqlOptions::default())
        .unwrap()
        .rows
}

fn sequence_value(value: u64) -> RawSqlValue {
    RawSqlValue::Text {
        value: format!("{value:020}"),
        bytes: 20,
        truncated: false,
    }
}

fn relational_bytes(data_root: &Path) -> Vec<u8> {
    let path = sql_compatibility_path(data_root);
    let mut bytes = fs::read(&path).unwrap();
    for suffix in ["-wal", "-shm"] {
        if let Ok(sidecar) = fs::read(format!("{}{suffix}", path.display())) {
            bytes.extend(sidecar);
        }
    }
    bytes
}

fn projection_receipt(
    generation: &CommittedCoreGeneration,
    metadata: &RelationalProjectionMetadata,
) -> RelationalProjectionReceipt {
    RelationalProjectionReceipt {
        core_generation_id: generation.generation_id.clone(),
        relational_schema_version: RELATIONAL_PROJECTION_SCHEMA_VERSION,
        materializer_revision: RELATIONAL_MATERIALIZER_REVISION,
        build_generation: metadata.build_generation,
        source_count: metadata.source_count,
        session_count: metadata.session_count,
        event_count: metadata.event_count,
        repository_binding_count: metadata.repository_binding_count,
        file_touch_count: metadata.file_touch_count,
        vcs_observation_count: metadata.vcs_observation_count,
    }
}

fn committed_generation_for(data_root: &Path, generation_id: &str) -> CommittedCoreGeneration {
    let index = VerifiedIndex::open(source_backed_index_root(data_root)).unwrap();
    assert_eq!(index.generation_id(), generation_id);
    committed_generation(&index).unwrap()
}

fn force_materializer_rebuild(data_root: &Path) {
    let connection = rusqlite::Connection::open(sql_compatibility_path(data_root)).unwrap();
    connection
        .execute(
            "UPDATE core_relational_state SET active_materializer_revision = 0",
            [],
        )
        .unwrap();
}

fn build_rebuild_candidate(
    data_root: &Path,
    generation_id: &str,
) -> (
    SourceBackedRelationalProjection,
    PathBuf,
    CommittedCoreGeneration,
    RelationalProjectionReceipt,
) {
    let index = VerifiedIndex::open(source_backed_index_root(data_root)).unwrap();
    assert_eq!(index.generation_id(), generation_id);
    let generation = committed_generation(&index).unwrap();
    let destination = sql_compatibility_path(data_root);
    let PreparedProjection::RebuildCandidate {
        mut projection,
        path,
    } = prepare_projection(&destination, &generation).unwrap()
    else {
        panic!("test generation unexpectedly did not require a relational rebuild");
    };
    let plan = projection.plan_generation(&generation).unwrap();
    assert_eq!(plan, RelationalProjectionPlan::Rebuild);
    let records = relational_record_stream(
        &index,
        RelationalSourceSelection::All,
        &generation,
        MAX_SOURCE_EVENT_PAGE_ITEMS,
    );
    let receipt = projection.rebuild_stream(&generation, records).unwrap();
    (projection, path, generation, receipt)
}

fn assert_no_sqlite_sidecars(path: &Path) {
    for suffix in ["-wal", "-shm"] {
        let sidecar = sqlite_sidecar_path(path, suffix);
        assert!(!sidecar.exists(), "stale sidecar {}", sidecar.display());
    }
}

#[test]
fn first_publish_seals_syncs_and_reopens_exact_generation() {
    let temp = tempfile::tempdir().unwrap();
    let source = source();
    let generation_id = replace_generation(
        temp.path(),
        &source,
        1,
        vec![record(&source, 1, &temp.path().join("first-publish.jsonl"))],
    );
    let destination = sql_compatibility_path(temp.path());
    let candidate = candidate_projection_path(&destination);
    assert!(!destination.exists());

    let run = run_after_core_publication(temp.path(), &generation_id).unwrap();

    assert!(run.did_work, "unexpected catch-up status: {}", run.status);
    assert!(destination.is_file());
    assert!(!candidate.exists());
    assert_no_sqlite_sidecars(&destination);
    assert_no_sqlite_sidecars(&candidate);
    let generation = committed_generation_for(temp.path(), &generation_id);
    let projection = SourceBackedRelationalProjection::open_read_only(&destination).unwrap();
    let metadata = projection.metadata().unwrap();
    let receipt = projection_receipt(&generation, &metadata);
    verify_projection_identity(&destination, &generation, &receipt).unwrap();
}

#[test]
fn live_catch_up_appends_rewrites_and_deletes_without_touching_a_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let source = source();
    let provider = temp.path().join("live-catch-up.jsonl");
    let first_generation =
        replace_generation(temp.path(), &source, 1, vec![record(&source, 1, &provider)]);
    run_after_core_publication(temp.path(), &first_generation).unwrap();
    let destination = sql_compatibility_path(temp.path());
    let candidate = candidate_projection_path(&destination);
    let candidate_sentinel = b"candidate-must-remain-untouched-on-live-catch-up";
    fs::write(&candidate, candidate_sentinel).unwrap();
    assert!(fs::metadata(&destination).unwrap().len() > candidate_sentinel.len() as u64);

    let appended_generation = replace_generation(
        temp.path(),
        &source,
        2,
        vec![record(&source, 1, &provider), record(&source, 2, &provider)],
    );
    let append = run_after_core_publication(temp.path(), &appended_generation).unwrap();

    assert!(append.did_work);
    assert_eq!(fs::read(&candidate).unwrap(), candidate_sentinel);
    let metadata = projection_metadata(temp.path()).unwrap();
    assert_eq!(
        metadata.active_core_generation_id.as_deref(),
        Some(appended_generation.as_str())
    );
    assert_eq!(metadata.build_generation, 2);
    assert_eq!(
        query(
            temp.path(),
            "SELECT event_seq FROM ctx_events ORDER BY event_seq",
        ),
        vec![vec![sequence_value(1)], vec![sequence_value(2)]]
    );

    let rewritten_generation =
        replace_generation(temp.path(), &source, 3, vec![record(&source, 3, &provider)]);
    let rewrite = run_after_core_publication(temp.path(), &rewritten_generation).unwrap();

    assert!(rewrite.did_work);
    assert_eq!(fs::read(&candidate).unwrap(), candidate_sentinel);
    let metadata = projection_metadata(temp.path()).unwrap();
    assert_eq!(
        metadata.active_core_generation_id.as_deref(),
        Some(rewritten_generation.as_str())
    );
    assert_eq!(metadata.build_generation, 3);
    assert_eq!(
        query(
            temp.path(),
            "SELECT event_seq FROM ctx_events ORDER BY event_seq",
        ),
        vec![vec![sequence_value(3)]]
    );

    let deleted_generation = delete_generation(temp.path(), &source);
    let delete = run_after_core_publication(temp.path(), &deleted_generation).unwrap();

    assert!(delete.did_work);
    assert_eq!(fs::read(&candidate).unwrap(), candidate_sentinel);
    let metadata = projection_metadata(temp.path()).unwrap();
    assert_eq!(
        metadata.active_core_generation_id.as_deref(),
        Some(deleted_generation.as_str())
    );
    assert_eq!(metadata.build_generation, 4);
    assert_eq!(metadata.event_count, 0);

    let noop = run_after_core_publication(temp.path(), &deleted_generation).unwrap();

    assert!(!noop.did_work);
    assert_eq!(
        projection_metadata(temp.path()).unwrap().build_generation,
        4
    );
    assert_eq!(fs::read(&candidate).unwrap(), candidate_sentinel);
}

#[test]
fn live_catch_up_record_error_rolls_back_partial_rows_and_generation() {
    let temp = tempfile::tempdir().unwrap();
    let source = source();
    let provider = temp.path().join("live-catch-up-error.jsonl");
    let first_generation =
        replace_generation(temp.path(), &source, 1, vec![record(&source, 1, &provider)]);
    run_after_core_publication(temp.path(), &first_generation).unwrap();
    let destination = sql_compatibility_path(temp.path());
    let candidate = candidate_projection_path(&destination);
    let target_generation = replace_generation(
        temp.path(),
        &source,
        2,
        vec![record(&source, 1, &provider), record(&source, 2, &provider)],
    );
    let index = VerifiedIndex::open(source_backed_index_root(temp.path())).unwrap();
    let generation = committed_generation(&index).unwrap();
    assert_eq!(generation.generation_id, target_generation);
    let PreparedProjection::LiveCatchUp(mut projection) =
        prepare_projection(&destination, &generation).unwrap()
    else {
        panic!("incremental generation did not select live catch-up");
    };
    let RelationalProjectionPlan::CatchUp { changed_source_ids } =
        projection.plan_generation(&generation).unwrap()
    else {
        panic!("live projection did not retain its catch-up plan");
    };
    let mut core_records = 0;
    let records = relational_record_stream(
        &index,
        RelationalSourceSelection::Changed(&changed_source_ids),
        &generation,
        MAX_SOURCE_EVENT_PAGE_ITEMS,
    )
    .map(|record| {
        let record = record?;
        if matches!(&record, RelationalProjectionRecord::CoreRecord(_)) {
            core_records += 1;
            if core_records == 2 {
                return Err(RelationalProjectionError::InvalidRecord(
                    "injected Core stream interruption before commit".to_owned(),
                ));
            }
        }
        Ok(record)
    });

    let error = projection
        .catch_up_stream(&generation, records)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("injected Core stream interruption"));
    drop(projection);

    let metadata = projection_metadata(temp.path()).unwrap();
    assert_eq!(metadata.status, RelationalProjectionStatus::Behind);
    assert_eq!(
        metadata.active_core_generation_id.as_deref(),
        Some(first_generation.as_str())
    );
    assert_eq!(
        metadata.target_core_generation_id.as_deref(),
        Some(target_generation.as_str())
    );
    assert_eq!(metadata.build_generation, 1);
    assert_eq!(
        query_projection(temp.path(), "SELECT event_seq FROM ctx_events"),
        vec![vec![sequence_value(1)]]
    );
    assert!(!candidate.exists());

    let retry = run_after_core_publication(temp.path(), &target_generation).unwrap();
    assert!(retry.did_work);
    assert_eq!(
        query(
            temp.path(),
            "SELECT event_seq FROM ctx_events ORDER BY event_seq",
        ),
        vec![vec![sequence_value(1)], vec![sequence_value(2)]]
    );
}

#[test]
fn mismatched_receipt_cannot_publish_completed_status_or_advance_live_generation() {
    let temp = tempfile::tempdir().unwrap();
    let source = source();
    let provider = temp.path().join("receipt-mismatch.jsonl");
    let first_generation =
        replace_generation(temp.path(), &source, 1, vec![record(&source, 1, &provider)]);
    run_after_core_publication(temp.path(), &first_generation).unwrap();
    let first_metadata = projection_metadata(temp.path()).unwrap();
    let first_committed = committed_generation_for(temp.path(), &first_generation);
    let stale_receipt = projection_receipt(&first_committed, &first_metadata);
    let target_generation =
        replace_generation(temp.path(), &source, 2, vec![record(&source, 2, &provider)]);

    let run = run_with(temp.path(), &target_generation, move |_, _| {
        Ok(ProjectionOutcome {
            receipt: stale_receipt,
            did_work: true,
        })
    })
    .unwrap();

    assert!(!run.did_work);
    assert_eq!(run.status["status"], "error");
    assert_eq!(
        run.status["error_code"],
        "source_relational_receipt_mismatch"
    );
    let metadata = projection_metadata(temp.path()).unwrap();
    assert_eq!(
        metadata.active_core_generation_id.as_deref(),
        Some(first_generation.as_str())
    );
    assert_eq!(metadata.build_generation, first_metadata.build_generation);
    assert_eq!(
        query_projection(temp.path(), "SELECT event_seq FROM ctx_events"),
        vec![vec![sequence_value(1)]]
    );
}

#[test]
fn equal_count_core_aggregate_change_replays_the_source() {
    let temp = tempfile::tempdir().unwrap();
    let source = source();
    let provider = temp.path().join("aggregate-only-change.jsonl");
    let first_generation =
        replace_generation(temp.path(), &source, 1, vec![record(&source, 1, &provider)]);
    run_after_core_publication(temp.path(), &first_generation).unwrap();
    let first_index = VerifiedIndex::open(source_backed_index_root(temp.path())).unwrap();
    let first_certificate = first_index.manifest().sources[0].clone();
    let first_aggregate = first_index.manifest().core_record_aggregates[0].clone();
    let first_revision = committed_generation(&first_index).unwrap().sources[0].revision_digest;
    drop(first_index);

    let replacement_generation =
        replace_generation(temp.path(), &source, 1, vec![record(&source, 2, &provider)]);
    let replacement_index = VerifiedIndex::open(source_backed_index_root(temp.path())).unwrap();
    assert_eq!(replacement_index.manifest().sources[0], first_certificate);
    assert_ne!(
        replacement_index.manifest().core_record_aggregates[0],
        first_aggregate
    );
    let replacement_revision =
        committed_generation(&replacement_index).unwrap().sources[0].revision_digest;
    assert_ne!(replacement_revision, first_revision);
    drop(replacement_index);

    let run = run_after_core_publication(temp.path(), &replacement_generation).unwrap();

    assert!(run.did_work);
    assert_eq!(
        query(temp.path(), "SELECT event_seq FROM ctx_events"),
        vec![vec![sequence_value(2)]]
    );
}

#[test]
fn replacement_removes_stale_destination_and_candidate_sidecars() {
    let temp = tempfile::tempdir().unwrap();
    let source = source();
    let provider = temp.path().join("stale-sidecars.jsonl");
    let first_generation =
        replace_generation(temp.path(), &source, 1, vec![record(&source, 1, &provider)]);
    run_after_core_publication(temp.path(), &first_generation).unwrap();
    force_materializer_rebuild(temp.path());
    let destination = sql_compatibility_path(temp.path());
    let candidate = candidate_projection_path(&destination);
    fs::write(sqlite_sidecar_path(&destination, "-wal"), b"stale wal").unwrap();
    fs::write(sqlite_sidecar_path(&destination, "-shm"), b"stale shm").unwrap();
    fs::write(&candidate, b"abandoned candidate").unwrap();
    fs::write(
        sqlite_sidecar_path(&candidate, "-wal"),
        b"stale candidate wal",
    )
    .unwrap();
    fs::write(
        sqlite_sidecar_path(&candidate, "-shm"),
        b"stale candidate shm",
    )
    .unwrap();

    let replacement_generation =
        replace_generation(temp.path(), &source, 2, vec![record(&source, 2, &provider)]);
    let run = run_after_core_publication(temp.path(), &replacement_generation).unwrap();

    assert!(run.did_work);
    assert!(!candidate.exists());
    assert_no_sqlite_sidecars(&candidate);
    assert_no_sqlite_sidecars(&destination);
    assert_eq!(
        SourceBackedRelationalProjection::open_read_only(&destination)
            .unwrap()
            .metadata()
            .unwrap()
            .active_core_generation_id
            .as_deref(),
        Some(replacement_generation.as_str())
    );
}

#[test]
fn prepublication_replacement_failure_keeps_prior_projection_visible() {
    let temp = tempfile::tempdir().unwrap();
    let source = source();
    let provider = temp.path().join("injected-replace-failure.jsonl");
    let first_generation =
        replace_generation(temp.path(), &source, 1, vec![record(&source, 1, &provider)]);
    run_after_core_publication(temp.path(), &first_generation).unwrap();
    let destination = sql_compatibility_path(temp.path());
    let replacement_generation =
        replace_generation(temp.path(), &source, 2, vec![record(&source, 2, &provider)]);
    force_materializer_rebuild(temp.path());
    let (projection, candidate, generation, receipt) =
        build_rebuild_candidate(temp.path(), &replacement_generation);

    let error = finish_candidate_publication_with(
        projection,
        &candidate,
        &destination,
        &generation,
        &receipt,
        |_, _| Err(io::Error::other("injected atomic replacement failure")),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        SourceBackedRelationalCatchUpError::Publication(
            RelationalPublicationError::AtomicReplace { .. }
        )
    ));
    assert_eq!(error.code(), "source_relational_publication_failed");
    assert!(error
        .to_string()
        .contains("prior projection remains visible"));
    assert!(candidate.is_file());
    assert_no_sqlite_sidecars(&candidate);
    assert_no_sqlite_sidecars(&destination);
    assert_eq!(
        SourceBackedRelationalProjection::open_read_only(&destination)
            .unwrap()
            .metadata()
            .unwrap()
            .active_core_generation_id
            .as_deref(),
        Some(first_generation.as_str())
    );
}

#[test]
fn published_projection_is_reopened_and_wrong_generation_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let source = source();
    let provider = temp.path().join("reopen-generation.jsonl");
    let first_generation =
        replace_generation(temp.path(), &source, 1, vec![record(&source, 1, &provider)]);
    run_after_core_publication(temp.path(), &first_generation).unwrap();
    let expected_generation =
        replace_generation(temp.path(), &source, 2, vec![record(&source, 2, &provider)]);
    force_materializer_rebuild(temp.path());
    let (projection, candidate, generation, receipt) =
        build_rebuild_candidate(temp.path(), &expected_generation);
    let destination = sql_compatibility_path(temp.path());

    let impostor_root = tempfile::tempdir().unwrap();
    let impostor_generation = replace_generation(
        impostor_root.path(),
        &source,
        3,
        vec![record(
            &source,
            3,
            &impostor_root.path().join("impostor.jsonl"),
        )],
    );
    run_after_core_publication(impostor_root.path(), &impostor_generation).unwrap();
    let impostor = sql_compatibility_path(impostor_root.path());

    let error = finish_candidate_publication_with(
        projection,
        &candidate,
        &destination,
        &generation,
        &receipt,
        |candidate, destination| {
            fs::copy(&impostor, candidate)?;
            fs::File::open(candidate)?.sync_all()?;
            durable_atomic_replace_file(candidate, destination)
        },
    )
    .unwrap_err();

    assert!(matches!(
        error,
        SourceBackedRelationalCatchUpError::Publication(
            RelationalPublicationError::PublishedVerification { .. }
        )
    ));
    assert!(error
        .to_string()
        .contains("after replacement became visible"));
    assert!(error.to_string().contains(&expected_generation));
    assert_eq!(
        SourceBackedRelationalProjection::open_read_only(&destination)
            .unwrap()
            .metadata()
            .unwrap()
            .active_core_generation_id
            .as_deref(),
        Some(impostor_generation.as_str())
    );
}

#[test]
fn relational_stream_reads_bounded_complete_core_pages() {
    let temp = tempfile::tempdir().unwrap();
    let source = source();
    let provider_file = temp.path().join("bounded-provider-session.jsonl");
    replace_generation(
        temp.path(),
        &source,
        1,
        (1..=7)
            .map(|sequence| record(&source, sequence, &provider_file))
            .collect(),
    );
    let index = VerifiedIndex::open(source_backed_index_root(temp.path())).unwrap();
    let mut stream = RelationalRecordStream::new(&index, RelationalSourceSelection::All, 2);
    let mut records = 0;
    for item in stream.by_ref() {
        if let RelationalProjectionRecord::CoreRecord(record) = item.unwrap() {
            records += 1;
            assert!(record.content.meaningful_text().contains(BODY_SENTINEL));
        }
    }

    assert_eq!(records, 7);
    assert_eq!(stream.pages_loaded, 4);
    assert_eq!(stream.page_items_loaded, 7);
    assert!(stream.max_page_items <= 2);
}

#[test]
fn initial_noop_replacement_and_deletion_need_no_provider_file_and_store_no_body() {
    let temp = tempfile::tempdir().unwrap();
    let source = source();
    let provider_file = temp.path().join("provider-session.jsonl");
    fs::write(&provider_file, BODY_SENTINEL).unwrap();
    let provider_path = provider_file.to_string_lossy().into_owned();
    let initial_generation = replace_generation(
        temp.path(),
        &source,
        1,
        vec![record(&source, 1, &provider_file)],
    );
    fs::remove_file(&provider_file).unwrap();

    let initial = run_after_core_publication(temp.path(), &initial_generation).unwrap();
    assert!(initial.did_work);
    let metadata = SqlCompatibility::open_for_data_root(temp.path())
        .unwrap()
        .metadata()
        .unwrap();
    assert_eq!(metadata.status, RelationalProjectionStatus::Ready);
    assert_eq!(metadata.event_count, 1);
    assert_eq!(
        metadata.file_touch_count, 0,
        "unscoped legacy paths are not authority"
    );
    assert_eq!(
        query(temp.path(), "SELECT event_seq FROM ctx_events"),
        vec![vec![sequence_value(1)]]
    );
    let bytes = relational_bytes(temp.path());
    assert!(!bytes
        .windows(BODY_SENTINEL.len())
        .any(|candidate| candidate == BODY_SENTINEL.as_bytes()));
    assert!(!bytes
        .windows(provider_path.len())
        .any(|candidate| candidate == provider_path.as_bytes()));

    let noop = run_after_core_publication(temp.path(), &initial_generation).unwrap();
    assert!(!noop.did_work);
    assert_eq!(
        SqlCompatibility::open_for_data_root(temp.path())
            .unwrap()
            .metadata()
            .unwrap()
            .build_generation,
        metadata.build_generation
    );

    let replacement_generation = replace_generation(
        temp.path(),
        &source,
        2,
        vec![record(&source, 2, &provider_file)],
    );
    assert!(
        run_after_core_publication(temp.path(), &replacement_generation)
            .unwrap()
            .did_work
    );
    assert_eq!(
        query(temp.path(), "SELECT event_seq FROM ctx_events"),
        vec![vec![sequence_value(2)]]
    );

    let deleted_generation = delete_generation(temp.path(), &source);
    assert!(
        run_after_core_publication(temp.path(), &deleted_generation)
            .unwrap()
            .did_work
    );
    assert_eq!(
        query(temp.path(), "SELECT COUNT(*) FROM ctx_events")[0][0],
        RawSqlValue::Integer(0)
    );

    let bytes = relational_bytes(temp.path());
    assert!(!bytes
        .windows(BODY_SENTINEL.len())
        .any(|candidate| candidate == BODY_SENTINEL.as_bytes()));
    assert!(!provider_file.exists());
}

#[test]
fn materializer_revision_mismatch_rebuilds_from_the_same_pinned_core_generation() {
    let temp = tempfile::tempdir().unwrap();
    let source = source();
    let provider_file = temp.path().join("revision-provider-session.jsonl");
    let generation = replace_generation(
        temp.path(),
        &source,
        1,
        vec![record(&source, 1, &provider_file)],
    );
    run_after_core_publication(temp.path(), &generation).unwrap();
    force_materializer_rebuild(temp.path());

    assert!(generation_needs_catch_up(temp.path(), &generation));
    let rebuilt = run_after_core_publication(temp.path(), &generation).unwrap();

    assert!(rebuilt.did_work);
    let metadata = SqlCompatibility::open_for_data_root(temp.path())
        .unwrap()
        .metadata()
        .unwrap();
    assert_eq!(
        metadata.active_materializer_revision,
        Some(RELATIONAL_MATERIALIZER_REVISION)
    );
    assert_eq!(metadata.build_generation, 2);
    assert_eq!(metadata.event_count, 1);
}

#[test]
fn generation_mismatch_reports_lag_without_creating_a_projection() {
    let temp = tempfile::tempdir().unwrap();
    let source = source();
    let provider_file = temp.path().join("mismatch-provider-session.jsonl");
    let generation = replace_generation(
        temp.path(),
        &source,
        1,
        vec![record(&source, 1, &provider_file)],
    );
    let wrong_generation = "f".repeat(64);
    assert_ne!(wrong_generation, generation);

    let run = run_after_core_publication(temp.path(), &wrong_generation).unwrap();

    assert!(!run.did_work);
    assert_eq!(run.status["status"], "error");
    assert_eq!(
        run.status["error_code"],
        "source_relational_generation_mismatch"
    );
    assert!(!sql_compatibility_path(temp.path()).exists());
}
