use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CertifiedSourceDeletion,
    CertifiedSourceInventory, EventIdentityInput, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceInventoryObservation, SourceObservation, SourceRecordLocator, TypedKey,
};
use ctx_history_index::{GenerationWriter, LexicalDocument, WriterOptions};
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

fn document(source: &SourceKey, sequence: u64, provider_file: &Path) -> LexicalDocument {
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
                native_session_key: Some(native_session),
                native_event_key: Some(TypedKey::U64(sequence)),
            },
            LocatorRevisionPolicy::StableRecordEvidence,
            None,
            [sequence as u8; 32],
        )
        .unwrap(),
        provider_session_id: Some("provider-session".to_owned()),
        branch: Some("main".to_owned()),
        source_path: Some(provider_file.to_string_lossy().into_owned()),
        agent_type: "primary".to_owned(),
        is_primary: true,
        event_sequence: sequence,
        occurred_at_unix_ms: Some(1_700_000_000_000 + sequence as i64),
        event_type: "message".to_owned(),
        role: Some("user".to_owned()),
        body: format!("{BODY_SENTINEL}-{sequence}"),
        workspace: Some("ctx".to_owned()),
        cwd: Some("/work/ctx".to_owned()),
        touched_files: vec!["unscoped/legacy-path-must-not-project.rs".to_owned()],
    }
}

fn replace_generation(
    data_root: &Path,
    source: &SourceKey,
    revision: u8,
    documents: Vec<LexicalDocument>,
) -> String {
    let count = documents.len() as u64;
    let mut writer = GenerationWriter::open(
        source_backed_index_root(data_root),
        WriterOptions::default(),
    )
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for document in documents {
        writer.add_document(document).unwrap();
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
            .map(|sequence| document(&source, sequence, &provider_file))
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
        vec![document(&source, 1, &provider_file)],
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
        vec![vec![RawSqlValue::Integer(1)]]
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
        vec![document(&source, 2, &provider_file)],
    );
    assert!(
        run_after_core_publication(temp.path(), &replacement_generation)
            .unwrap()
            .did_work
    );
    assert_eq!(
        query(temp.path(), "SELECT event_seq FROM ctx_events"),
        vec![vec![RawSqlValue::Integer(2)]]
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
        vec![document(&source, 1, &provider_file)],
    );
    run_after_core_publication(temp.path(), &generation).unwrap();
    let path = sql_compatibility_path(temp.path());
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE core_relational_state SET active_materializer_revision = 0",
            [],
        )
        .unwrap();
    drop(connection);

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
        vec![document(&source, 1, &provider_file)],
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
