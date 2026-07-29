use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CertifiedSourceAppend,
    CertifiedSourceDeletion, CertifiedSourceInventory, EventIdentityInput, LocatorRevisionPolicy,
    NativeItemKey, NativeRecordCoordinate, NativeSessionKey, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceFrontier, SourceInventoryObservation,
    SourceObservation, SourceRecordLocator, TypedKey,
};
use ctx_history_index::{GenerationWriter, LexicalDocument, WriterOptions};
use ctx_history_relational::{RawSqlValue, RelationalProjectionStatus};

use super::*;
use crate::source_sql::SqlCompatibility;

const PROVIDER_TEXT: &str = "provider-body-sentinel-must-not-enter-relational";
const PREVIEW_TEXT: &str = "provider-preview-sentinel-must-not-enter-relational";

#[test]
fn durable_state_path_is_purpose_based() {
    assert_eq!(
        status_path(Path::new("ctx-data")),
        Path::new("ctx-data/daemon/jobs/relational-catch-up.json")
    );
}

fn source() -> ctx_history_core::SourceKey {
    ctx_history_core::SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("relational-production-writer.jsonl").unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn appendable_certificate(
    source: &ctx_history_core::SourceKey,
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

fn document(
    source: &ctx_history_core::SourceKey,
    sequence: u64,
    role: &str,
    touched_files: &[&str],
) -> LexicalDocument {
    let native_session = TypedKey::utf8("provider-session").unwrap();
    let session_key = NativeSessionKey::native_id("session", native_session.clone()).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let native_item = NativeItemKey::native_id(
        "message",
        TypedKey::utf8(format!("event-{sequence}")).unwrap(),
    )
    .unwrap();
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
        source_path: Some("/provider/codex/session.jsonl".to_owned()),
        agent_type: "primary".to_owned(),
        is_primary: true,
        event_sequence: sequence,
        occurred_at_unix_ms: Some(1_700_000_000_000 + sequence as i64),
        event_type: "message".to_owned(),
        role: Some(role.to_owned()),
        body: format!("{PROVIDER_TEXT} {PREVIEW_TEXT} sequence-{sequence}"),
        workspace: Some("ctx".to_owned()),
        cwd: Some("/work/ctx".to_owned()),
        touched_files: touched_files
            .iter()
            .map(|path| (*path).to_owned())
            .collect(),
    }
}

fn replace_generation(
    data_root: &Path,
    source: &ctx_history_core::SourceKey,
    revision: u8,
    documents: Vec<LexicalDocument>,
) -> String {
    let document_count = documents.len() as u64;
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
        .certify_source(appendable_certificate(
            source,
            revision,
            document_count,
            document_count * 100,
        ))
        .unwrap();
    writer.commit(|_| true).unwrap().generation_id
}

fn initial_generation(data_root: &Path, source: &ctx_history_core::SourceKey) -> String {
    replace_generation(
        data_root,
        source,
        1,
        vec![document(source, 1, "user", &["src/lib.rs"])],
    )
}

fn append_generation(data_root: &Path, source: &ctx_history_core::SourceKey) -> String {
    let mut writer = GenerationWriter::open(
        source_backed_index_root(data_root),
        WriterOptions::default(),
    )
    .unwrap();
    let base = writer.begin_source_append(source.clone()).unwrap().clone();
    writer
        .add_document(document(
            source,
            2,
            "assistant",
            &["src/lib.rs", "src/main.rs"],
        ))
        .unwrap();
    let current = appendable_certificate(source, 2, 2, 200);
    writer
        .certify_source_append(
            CertifiedSourceAppend::certify(&base, current, 100, [1; 32]).unwrap(),
        )
        .unwrap();
    writer.commit(|_| true).unwrap().generation_id
}

fn delete_generation(data_root: &Path, source: &ctx_history_core::SourceKey) -> String {
    let observation = SourceInventoryObservation::new(
        source.provider(),
        "provider-root",
        TypedKey::utf8("root-lineage").unwrap(),
        "tree-inventory-v1",
        vec![4],
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

fn projection_bytes(data_root: &Path) -> Vec<u8> {
    let path = sql_compatibility_path(data_root);
    let mut output = fs::read(&path).unwrap();
    for suffix in ["-wal", "-shm"] {
        if let Ok(bytes) = fs::read(format!("{}{suffix}", path.display())) {
            output.extend(bytes);
        }
    }
    output
}

fn contains_bytes(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|candidate| candidate == needle.as_bytes())
}

#[test]
fn cold_append_rewrite_delete_and_noop_preserve_only_relational_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let source = source();
    let first_generation = initial_generation(temp.path(), &source);

    let cold = run_after_core_publication(temp.path(), &first_generation).unwrap();
    assert!(cold.did_work);
    assert_eq!(cold.status["status"], "completed");
    assert_eq!(cold.status["core_generation_id"], first_generation);
    assert_eq!(cold.status["receipt_core_generation_id"], first_generation);

    let metadata = SqlCompatibility::open_for_data_root(temp.path())
        .unwrap()
        .metadata()
        .unwrap();
    let index = VerifiedIndex::open(source_backed_index_root(temp.path())).unwrap();
    assert_eq!(metadata.status, RelationalProjectionStatus::Ready);
    assert_eq!(
        metadata.active_core_generation_id.as_deref(),
        Some(first_generation.as_str())
    );
    assert_eq!(metadata.active_manifest_version, Some(3));
    assert_eq!(metadata.active_lexical_schema_version, Some(5));
    assert_eq!(
        metadata.active_policy_schema_hash.as_deref(),
        Some(index.manifest().policy_schema_hash.as_str())
    );
    assert_eq!((metadata.source_count, metadata.session_count), (1, 1));
    assert_eq!((metadata.event_count, metadata.file_touch_count), (1, 1));

    let rows = query(
        temp.path(),
        "SELECT e.provider, e.provider_session_id, s.agent_type, e.branch, e.workspace, e.cwd,
                e.source_path, e.event_type, e.role, e.event_seq, e.payload_json
         FROM ctx_events e
         JOIN ctx_sessions s ON s.ctx_session_id = e.ctx_session_id",
    );
    assert_eq!(rows.len(), 1);
    assert!(matches!(
        &rows[0][0],
        RawSqlValue::Text { value, .. } if value == "codex"
    ));
    assert!(matches!(
        &rows[0][2],
        RawSqlValue::Text { value, .. } if value == "primary"
    ));
    assert!(matches!(
        &rows[0][8],
        RawSqlValue::Text { value, .. } if value == "user"
    ));
    assert!(matches!(rows[0][9], RawSqlValue::Integer(1)));
    assert!(matches!(
        &rows[0][10],
        RawSqlValue::Text { value, .. } if value == r#"{"content_authority":"provider_source"}"#
    ));
    let locator_rows = query(
        temp.path(),
        "SELECT native_locator_json FROM source_backed_events",
    );
    assert_eq!(locator_rows.len(), 1);
    assert!(matches!(
        &locator_rows[0][0],
        RawSqlValue::Blob { bytes, .. } if *bytes > 0
    ));
    let bytes = projection_bytes(temp.path());
    assert!(!contains_bytes(&bytes, PROVIDER_TEXT));
    assert!(!contains_bytes(&bytes, PREVIEW_TEXT));

    let build_generation = metadata.build_generation;
    let noop = run_after_core_publication(temp.path(), &first_generation).unwrap();
    assert!(!noop.did_work);
    assert_eq!(
        SqlCompatibility::open_for_data_root(temp.path())
            .unwrap()
            .metadata()
            .unwrap()
            .build_generation,
        build_generation
    );

    let appended_generation = append_generation(temp.path(), &source);
    let appended = run_after_core_publication(temp.path(), &appended_generation).unwrap();
    assert!(appended.did_work);
    assert_eq!(
        query(temp.path(), "SELECT COUNT(*) FROM ctx_events")[0][0],
        RawSqlValue::Integer(2)
    );
    assert_eq!(
        query(temp.path(), "SELECT COUNT(*) FROM ctx_files_touched")[0][0],
        RawSqlValue::Integer(3)
    );

    let rewritten_generation = replace_generation(
        temp.path(),
        &source,
        3,
        vec![document(&source, 3, "tool", &["README.md"])],
    );
    let rewritten = run_after_core_publication(temp.path(), &rewritten_generation).unwrap();
    assert!(rewritten.did_work);
    assert_eq!(
        query(temp.path(), "SELECT event_seq FROM ctx_events"),
        vec![vec![RawSqlValue::Integer(3)]]
    );
    assert_eq!(
        query(temp.path(), "SELECT path FROM ctx_files_touched"),
        vec![vec![RawSqlValue::Text {
            value: "README.md".to_owned(),
            bytes: "README.md".len(),
            truncated: false,
        }]]
    );
    let bytes = projection_bytes(temp.path());
    assert!(!contains_bytes(&bytes, PROVIDER_TEXT));
    assert!(!contains_bytes(&bytes, PREVIEW_TEXT));

    let deleted_generation = delete_generation(temp.path(), &source);
    let deleted = run_after_core_publication(temp.path(), &deleted_generation).unwrap();
    assert!(deleted.did_work);
    let metadata = SqlCompatibility::open_for_data_root(temp.path())
        .unwrap()
        .metadata()
        .unwrap();
    assert_eq!(
        metadata.active_core_generation_id.as_deref(),
        Some(deleted_generation.as_str())
    );
    assert_eq!(
        (
            metadata.source_count,
            metadata.session_count,
            metadata.event_count,
            metadata.file_touch_count,
        ),
        (0, 0, 0, 0)
    );
}

#[test]
fn failed_catch_up_keeps_prior_generation_and_a_later_tick_retries() {
    let temp = tempfile::tempdir().unwrap();
    let source = source();
    let first_generation = initial_generation(temp.path(), &source);
    run_after_core_publication(temp.path(), &first_generation).unwrap();
    let appended_generation = append_generation(temp.path(), &source);

    let interrupted = SourceBackedRelationalCatchUpStatus::pending(
        &appended_generation,
        1,
        projection_metadata(temp.path()).as_ref(),
    );
    persist_status(temp.path(), &interrupted).unwrap();

    let failed = run_with(
        temp.path(),
        &appended_generation,
        |data_root, generation_id| {
            let index =
                VerifiedIndex::open(source_backed_index_root(data_root)).map_err(|error| {
                    SourceBackedRelationalCatchUpError::IndexUnavailable(error.to_string())
                })?;
            let generation = committed_generation(&index)?;
            let mut projection =
                SourceBackedRelationalProjection::open(sql_compatibility_path(data_root))
                    .map_err(SourceBackedRelationalCatchUpError::projection)?;
            let error = projection
                .catch_up(&generation, Vec::<RelationalProjectionRecord>::new())
                .expect_err("changed source must be present");
            assert_eq!(generation_id, generation.generation_id);
            Err(SourceBackedRelationalCatchUpError::projection(error))
        },
    )
    .unwrap();
    assert!(!failed.did_work);
    assert_eq!(failed.status["status"], "error");
    assert_eq!(failed.status["pending"], true);
    assert_eq!(failed.status["retryable"], true);
    assert_eq!(failed.status["attempts"], 2);
    assert_eq!(failed.status["active_core_generation_id"], first_generation);
    assert_eq!(failed.status["core_generation_id"], appended_generation);
    assert_eq!(failed.status["projection_status"], "behind");

    let compatibility_error = SqlCompatibility::open_for_data_root(temp.path())
        .err()
        .expect("SQL compatibility must fail closed while projection is behind");
    assert!(
        compatibility_error
            .to_string()
            .contains("wait for daemon catch-up"),
        "{compatibility_error}"
    );
    let prior_projection =
        SourceBackedRelationalProjection::open_read_only(sql_compatibility_path(temp.path()))
            .unwrap();
    let prior = prior_projection.metadata().unwrap();
    assert_eq!(
        prior.active_core_generation_id.as_deref(),
        Some(first_generation.as_str())
    );
    assert_eq!(
        prior.target_core_generation_id,
        Some(appended_generation.clone())
    );
    assert_eq!(prior.status, RelationalProjectionStatus::Behind);
    assert_eq!(
        prior_projection
            .raw_sql_query("SELECT COUNT(*) FROM ctx_events", RawSqlOptions::default())
            .unwrap()
            .rows[0][0],
        RawSqlValue::Integer(1)
    );

    let retried = run_after_core_publication(temp.path(), &appended_generation).unwrap();
    assert!(retried.did_work);
    assert_eq!(retried.status["status"], "completed");
    assert_eq!(retried.status["attempts"], 3);
    assert_eq!(
        retried.status["active_core_generation_id"],
        appended_generation
    );
    let ready = SqlCompatibility::open_for_data_root(temp.path())
        .unwrap()
        .metadata()
        .unwrap();
    assert_eq!(ready.status, RelationalProjectionStatus::Ready);
    assert_eq!(ready.target_core_generation_id, None);
    assert_eq!(
        query(temp.path(), "SELECT COUNT(*) FROM ctx_events")[0][0],
        RawSqlValue::Integer(2)
    );
}

#[test]
fn arbitrary_relational_bytes_fail_closed_during_daemon_catch_up() {
    let temp = tempfile::tempdir().unwrap();
    let source = source();
    let first_generation = initial_generation(temp.path(), &source);
    run_after_core_publication(temp.path(), &first_generation).unwrap();
    let appended_generation = append_generation(temp.path(), &source);
    let projection_path = sql_compatibility_path(temp.path());

    fs::write(&projection_path, vec![0xa5; 4096]).unwrap();
    let run = run_after_core_publication(temp.path(), &appended_generation).unwrap();

    assert!(!run.did_work);
    assert_eq!(run.status["status"], "error");
    assert_eq!(run.status["pending"], true);
    assert_eq!(run.status["retryable"], true);
    assert_eq!(
        run.status["error_code"],
        "source_relational_projection_unavailable"
    );
    assert!(
        run.status["last_error"]
            .as_str()
            .is_some_and(|error| error.contains("file is not a database")),
        "{:#}",
        run.status
    );
    let compatibility_error = SqlCompatibility::open_for_data_root(temp.path())
        .err()
        .expect("arbitrarily corrupted relational bytes must fail closed");
    assert!(
        compatibility_error
            .to_string()
            .contains("file is not a database"),
        "{compatibility_error}"
    );
    let legacy_path = temp.path().join(["work", ".sqlite"].concat());
    assert!(!legacy_path.exists());
}

#[test]
fn generation_mismatch_is_persistent_and_never_creates_a_fallback_database() {
    let temp = tempfile::tempdir().unwrap();
    let source = source();
    let generation = initial_generation(temp.path(), &source);
    let wrong_generation = "f".repeat(64);
    assert_ne!(wrong_generation, generation);

    let run = run_after_core_publication(temp.path(), &wrong_generation).unwrap();

    assert!(!run.did_work);
    assert_eq!(run.status["status"], "error");
    assert_eq!(
        run.status["error_code"],
        "source_relational_generation_mismatch"
    );
    assert_eq!(run.status["core_generation_id"], wrong_generation);
    assert!(!sql_compatibility_path(temp.path()).exists());
}

#[test]
fn materializer_has_no_provider_hydration_or_legacy_store_authority() {
    let source = include_str!("source_backed_relational_catch_up.rs");
    for forbidden in [
        ["database_", "path"].concat(),
        ["work", ".sqlite"].concat(),
        ["SourceBacked", "ResolverRegistry"].concat(),
        ["provider_", "bytes"].concat(),
        ["bounded_", "preview"].concat(),
    ] {
        assert!(
            !source.contains(&forbidden),
            "relational catch-up contains forbidden architecture term {forbidden}"
        );
    }
    assert!(source.contains("VerifiedIndex::open"));
    assert!(source.contains(".source_event_page("));
    assert!(source.contains("SourceBackedRelationalProjection::open"));
}
