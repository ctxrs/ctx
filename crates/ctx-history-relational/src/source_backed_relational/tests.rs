use std::{
    fs,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CertifiedSourceDeletion,
    CertifiedSourceInventory, Confidence, EventIdentityInput, EventType, Fidelity,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    ScannedSourceCounts, SessionIdentityInput, SessionStatus, SourceAnchor,
    SourceInventoryObservation, SourceObservation, SourceRecordLocator, TypedKey,
};
use rusqlite::{
    ffi::{self, ErrorCode},
    types::ValueRef,
};
use tempfile::TempDir;

use super::manifest::ValidatedManifest;
use super::*;
use crate::{RawSqlValue, RAW_SQL_DEFAULT_MAX_ROWS};

fn source(lineage: u8) -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "rollout",
        1,
        SourceAnchor::CatalogLineage([lineage; 32]),
    )
    .unwrap()
}

fn corpus_source(index: u64) -> SourceKey {
    let mut lineage = [0_u8; 32];
    lineage[24..].copy_from_slice(&index.to_be_bytes());
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "rollout",
        1,
        SourceAnchor::CatalogLineage(lineage),
    )
    .unwrap()
}

fn certificate(source: SourceKey, revision: u8, events: u64) -> CertifiedSource {
    let observation =
        SourceObservation::new(source, "ordinary_file_v1", vec![revision, 1]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        format!("codex-parser-{revision}"),
        [revision; 32],
        ScannedSourceCounts {
            complete_records: events,
            retained_records: events,
            indexed_documents: events,
            certified_bytes: 100 + u64::from(revision),
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn large_manifest_certificate(index: u64) -> CertifiedSource {
    let observation =
        SourceObservation::new(corpus_source(index), "ordinary_file_v1", vec![u8::MAX; 512])
            .unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "codex-parser-large-manifest-v1",
        Sha256::digest(index.to_be_bytes()).into(),
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 1,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn generation(sources: Vec<CertifiedSource>) -> CommittedCoreGeneration {
    generation_with_removals(sources, Vec::new())
}

fn generation_with_removals(
    mut sources: Vec<CertifiedSource>,
    mut removals: Vec<GenerationRemoval>,
) -> CommittedCoreGeneration {
    sources.sort_by_key(|source| source.observation().source().identity().digest());
    removals.sort_by_key(|removal| removal.deletion.source().identity().digest());
    let indexed_documents = sources
        .iter()
        .map(|source| source.counts().indexed_documents)
        .sum();
    let certified_source_bytes = sources
        .iter()
        .map(|source| source.counts().certified_bytes)
        .sum();
    let manifest = GenerationManifest {
        manifest_version: GENERATION_MANIFEST_VERSION,
        identity_version: IDENTITY_VERSION,
        lexical_schema_version: REQUIRED_LEXICAL_SCHEMA_VERSION,
        lexical_analyzer_version: REQUIRED_LEXICAL_ANALYZER_VERSION,
        policy_schema_hash: REQUIRED_SOURCE_GENERATION_POLICY_HASH.to_owned(),
        indexed_documents,
        certified_source_bytes,
        sources,
        removals,
    };
    let manifest_json = serde_json::to_vec(&manifest).unwrap();
    CommittedCoreGeneration {
        generation_id: hex(&Sha256::digest(&manifest_json)),
        certified_sources: manifest.sources.len(),
        manifest_json,
        indexed_documents,
        certified_source_bytes,
    }
}

fn replace_manifest(committed: &mut CommittedCoreGeneration, manifest: &GenerationManifest) {
    committed.manifest_json = serde_json::to_vec(manifest).unwrap();
    committed.generation_id = hex(&Sha256::digest(&committed.manifest_json));
}

fn removal(source: &SourceKey, revision: u8) -> GenerationRemoval {
    let observation = SourceInventoryObservation::new(
        source.provider(),
        "provider-root",
        TypedKey::utf8("root-lineage").unwrap(),
        "tree-inventory-v1",
        vec![revision],
    )
    .unwrap();
    let inventory = CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        "discovery-v1",
        Vec::new(),
    )
    .unwrap();
    let deletion = CertifiedSourceDeletion::from_inventory(source.clone(), &inventory).unwrap();
    GenerationRemoval {
        deletion,
        inventory,
    }
}

fn identities(source: &SourceKey, event_index: u64) -> (StableEntityId, StableEntityId) {
    let native_session = NativeSessionKey::native_id(
        "thread_id",
        TypedKey::utf8(format!("thread-{}", source.identity())).unwrap(),
    )
    .unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &native_session,
    })
    .unwrap();
    let native_event = NativeItemKey::native_id("event_id", TypedKey::U64(event_index)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_event,
        subrecord_selector: None,
    })
    .unwrap();
    (session_id, event_id)
}

fn records(source: SourceKey, revision: u8, event_count: u64) -> Vec<RelationalProjectionRecord> {
    let (session_id, _) = identities(&source, 0);
    let mut records = vec![
        RelationalProjectionRecord::BeginSource(RelationalSourceMetadata {
            source: source.clone(),
            source_root: Some("/provider/codex".to_owned()),
            source_path: Some(format!("/provider/codex/{revision}.jsonl")),
            cwd: Some("/workspace/ctx".to_owned()),
        }),
        RelationalProjectionRecord::Session(RelationalSessionMetadata {
            session_id,
            parent_session_id: None,
            root_session_id: session_id,
            provider_session_id: Some("provider-thread".to_owned()),
            external_agent_id: Some("agent-1".to_owned()),
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            branch: Some("main".to_owned()),
            workspace: Some("/workspace/ctx".to_owned()),
            cwd: Some("/workspace/ctx".to_owned()),
            source_path: Some(format!("/provider/codex/{revision}.jsonl")),
            status: SessionStatus::Imported,
            fidelity: Fidelity::Imported,
            started_at_unix_ms: Some(1_700_000_000_000),
            ended_at_unix_ms: None,
        }),
    ];
    for event_index in 0..event_count {
        let (_, event_id) = identities(&source, event_index);
        let locator = SourceRecordLocator::new(
            source.clone(),
            NativeRecordCoordinate::Jsonl {
                byte_offset: event_index * 100,
                byte_length: 50,
                physical_ordinal: event_index,
                native_session_key: None,
                native_event_key: Some(TypedKey::U64(event_index)),
            },
            LocatorRevisionPolicy::StableRecordEvidence,
            None,
            [(event_index as u8).wrapping_add(1); 32],
        )
        .unwrap();
        records.push(RelationalProjectionRecord::Event(RelationalEventMetadata {
            event_id,
            session_id,
            event_sequence: event_index,
            event_type: EventType::Message,
            role: Some(EventRole::Assistant),
            occurred_at_unix_ms: Some(1_700_000_000_000 + event_index as i64),
            fidelity: Fidelity::Imported,
            locator,
        }));
        records.push(RelationalProjectionRecord::FileTouch(
            RelationalFileTouchMetadata {
                file_touch_id: event_id.as_uuid(),
                event_id: Some(event_id),
                session_id: Some(session_id),
                path: format!("src/event_{event_index}.rs"),
                old_path: None,
                change_kind: Some(FileChangeKind::Modified),
                line_count_delta: Some(1),
                confidence: Confidence::Explicit,
                created_at_unix_ms: Some(1_700_000_000_000),
                updated_at_unix_ms: Some(1_700_000_000_001),
            },
        ));
    }
    records.push(RelationalProjectionRecord::EndSource {
        source_id: source.identity().as_uuid(),
    });
    records
}

fn projection() -> (TempDir, SourceBackedRelationalProjection) {
    let temp = tempfile::tempdir().unwrap();
    let projection =
        SourceBackedRelationalProjection::open(temp.path().join("relational.sqlite")).unwrap();
    (temp, projection)
}

fn query_rows(projection: &SourceBackedRelationalProjection, sql: &str) -> Vec<Vec<RawSqlValue>> {
    projection
        .raw_sql_query(
            sql,
            RawSqlOptions {
                max_rows: RAW_SQL_DEFAULT_MAX_ROWS,
                ..RawSqlOptions::default()
            },
        )
        .unwrap()
        .rows
}

fn schema_columns(projection: &SourceBackedRelationalProjection, object: &str) -> Vec<String> {
    let mut statement = projection
        .conn
        .prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
        .unwrap();
    statement
        .query_map([object], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

#[derive(Debug)]
struct ProjectionWork {
    vm_steps: u64,
    page_cache_misses: u64,
}

fn measured_projection_work(
    projection: &mut SourceBackedRelationalProjection,
    operation: impl FnOnce(&mut SourceBackedRelationalProjection),
) -> ProjectionWork {
    const PROGRESS_GRANULARITY: u64 = 1;

    projection
        .conn
        .execute_batch("PRAGMA cache_size = -64; PRAGMA shrink_memory;")
        .unwrap();
    sqlite_cache_misses(&projection.conn, true);
    let progress_calls = Arc::new(AtomicU64::new(0));
    let measured_calls = Arc::clone(&progress_calls);
    projection.conn.progress_handler(
        PROGRESS_GRANULARITY as i32,
        Some(move || {
            measured_calls.fetch_add(1, Ordering::Relaxed);
            false
        }),
    );

    operation(projection);

    projection.conn.progress_handler(0, None::<fn() -> bool>);
    ProjectionWork {
        vm_steps: progress_calls.load(Ordering::Relaxed) * PROGRESS_GRANULARITY,
        page_cache_misses: sqlite_cache_misses(&projection.conn, false),
    }
}

fn sqlite_cache_misses(conn: &Connection, reset: bool) -> u64 {
    let mut current = 0;
    let mut highwater = 0;
    // SAFETY: sqlite3_db_status only reads and optionally resets a counter on
    // this live connection; both output pointers remain valid for the call.
    let result = unsafe {
        ffi::sqlite3_db_status(
            conn.handle(),
            ffi::SQLITE_DBSTATUS_CACHE_MISS,
            &mut current,
            &mut highwater,
            i32::from(reset),
        )
    };
    assert_eq!(result, ffi::SQLITE_OK);
    u64::try_from(current).unwrap()
}

fn incremental_work_with_unchanged_events(
    unchanged_event_count: u64,
) -> (ProjectionWork, ProjectionWork) {
    let (_temp, mut projection) = projection();
    let unchanged = source(31);
    let changing = source(32);
    let initial_generation = generation(vec![
        certificate(unchanged.clone(), 1, unchanged_event_count),
        certificate(changing.clone(), 1, 1),
    ]);
    let mut initial_records = records(unchanged.clone(), 1, unchanged_event_count);
    initial_records.extend(records(changing.clone(), 1, 1));
    projection
        .rebuild(&initial_generation, initial_records)
        .unwrap();

    let append_generation = generation(vec![
        certificate(unchanged.clone(), 1, unchanged_event_count),
        certificate(changing.clone(), 2, 2),
    ]);
    let append = measured_projection_work(&mut projection, |projection| {
        let receipt = projection
            .catch_up(&append_generation, records(changing.clone(), 2, 2))
            .unwrap();
        assert_eq!(receipt.event_count, unchanged_event_count + 2);
    });

    let deletion_generation = generation_with_removals(
        vec![certificate(unchanged, 1, unchanged_event_count)],
        vec![removal(&changing, 3)],
    );
    let deletion = measured_projection_work(&mut projection, |projection| {
        let receipt = projection
            .catch_up(&deletion_generation, Vec::new())
            .unwrap();
        assert_eq!(receipt.event_count, unchanged_event_count);
    });
    (append, deletion)
}

fn secondary_index_definitions(
    projection: &SourceBackedRelationalProjection,
) -> Vec<(String, String)> {
    let mut statement = projection
        .conn
        .prepare(
            "SELECT name, sql
             FROM sqlite_schema
             WHERE type = 'index' AND sql IS NOT NULL
             ORDER BY name",
        )
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn logical_projection_digest(projection: &SourceBackedRelationalProjection) -> String {
    let queries = [
        "SELECT * FROM source_backed_relational_state ORDER BY singleton",
        "SELECT * FROM source_backed_sources ORDER BY source_id",
        "SELECT * FROM source_backed_sessions ORDER BY ctx_session_id",
        "SELECT * FROM source_backed_events ORDER BY ctx_event_id",
        "SELECT * FROM source_backed_files_touched ORDER BY ctx_file_touch_id",
    ];
    let mut digest = Sha256::new();
    for sql in queries {
        digest.update((sql.len() as u64).to_be_bytes());
        digest.update(sql.as_bytes());
        let mut statement = projection.conn.prepare(sql).unwrap();
        let column_count = statement.column_count();
        let mut rows = statement.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            digest.update([0xff]);
            for column in 0..column_count {
                match row.get_ref(column).unwrap() {
                    ValueRef::Null => digest.update([0]),
                    ValueRef::Integer(value) => {
                        digest.update([1]);
                        digest.update(value.to_be_bytes());
                    }
                    ValueRef::Real(value) => {
                        digest.update([2]);
                        digest.update(value.to_bits().to_be_bytes());
                    }
                    ValueRef::Text(value) => {
                        digest.update([3]);
                        digest.update((value.len() as u64).to_be_bytes());
                        digest.update(value);
                    }
                    ValueRef::Blob(value) => {
                        digest.update([4]);
                        digest.update((value.len() as u64).to_be_bytes());
                        digest.update(value);
                    }
                }
            }
        }
    }
    hex(&digest.finalize())
}

#[test]
#[ignore = "manual relational materialization wall-time fixture"]
fn relational_materialization_profile_fixture() {
    const SOURCE_COUNT: u64 = 200;
    const EVENTS_PER_SOURCE: u64 = 200;

    let sources = (0..SOURCE_COUNT).map(corpus_source).collect::<Vec<_>>();
    let committed = generation(
        sources
            .iter()
            .cloned()
            .map(|source| certificate(source, 1, EVENTS_PER_SOURCE))
            .collect(),
    );
    let records = sources
        .into_iter()
        .flat_map(|source| records(source, 1, EVENTS_PER_SOURCE))
        .collect::<Vec<_>>();
    let expected_events = SOURCE_COUNT * EVENTS_PER_SOURCE;
    let expected_records = SOURCE_COUNT * 3 + expected_events * 2;
    assert_eq!(records.len() as u64, expected_records);

    let (_temp, mut projection) = projection();
    let started = Instant::now();
    let receipt = projection.rebuild(&committed, records).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(receipt.source_count, SOURCE_COUNT);
    assert_eq!(receipt.session_count, SOURCE_COUNT);
    assert_eq!(receipt.event_count, expected_events);
    assert_eq!(receipt.file_touch_count, expected_events);
    let foreign_key_errors: i64 = projection
        .conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(foreign_key_errors, 0);

    let database_bytes = fs::metadata(projection.path()).unwrap().len();
    let wal_path = projection.path().with_extension("sqlite-wal");
    let wal_bytes = fs::metadata(wal_path).map_or(0, |metadata| metadata.len());
    let logical_digest = logical_projection_digest(&projection);
    assert_eq!(
        logical_digest,
        "81804b81731f87f2eed19355e6301833f4c0e8dfc9fd2ded7517e15c82f5f241"
    );
    let cache_size: i64 = projection
        .conn
        .query_row("PRAGMA cache_size", [], |row| row.get(0))
        .unwrap();
    let temp_store: i64 = projection
        .conn
        .query_row("PRAGMA temp_store", [], |row| row.get(0))
        .unwrap();
    assert_eq!(cache_size, -65_536);
    assert_eq!(temp_store, 2);
    eprintln!(
        "relational_materialization_profile \
         sources={SOURCE_COUNT} sessions={SOURCE_COUNT} events={expected_events} \
         file_touches={expected_events} input_records={expected_records} \
         logical_digest={} database_bytes={database_bytes} wal_bytes={wal_bytes} \
         wall_ms={}",
        logical_digest,
        elapsed.as_millis()
    );
}

#[test]
fn source_backed_projection_preserves_metadata_sql_and_exact_generation_evidence() {
    let (_temp, mut projection) = projection();
    let source = source(1);
    let committed = generation(vec![certificate(source.clone(), 1, 1)]);

    let receipt = projection
        .rebuild(&committed, records(source.clone(), 1, 1))
        .unwrap();
    assert_eq!(receipt.core_generation_id, committed.generation_id);
    assert_eq!((receipt.source_count, receipt.session_count), (1, 1));
    assert_eq!((receipt.event_count, receipt.file_touch_count), (1, 1));

    let sessions = query_rows(
        &projection,
        "SELECT provider, provider_session_id, parent_ctx_session_id,
                root_ctx_session_id, agent_type, is_primary, branch, workspace,
                source_path
         FROM ctx_sessions",
    );
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0][0],
        RawSqlValue::Text {
            value: "codex".to_owned(),
            bytes: 5,
            truncated: false,
        }
    );
    assert_eq!(
        sessions[0][6],
        RawSqlValue::Text {
            value: "main".to_owned(),
            bytes: 4,
            truncated: false,
        }
    );

    let events = query_rows(
        &projection,
        "SELECT e.ctx_event_id, e.ctx_session_id, e.event_seq, e.event_type, e.role,
                e.occurred_at_ms, e.fidelity, s.agent_type, e.branch, e.workspace
         FROM ctx_events e
         JOIN ctx_sessions s ON s.ctx_session_id = e.ctx_session_id",
    );
    assert_eq!(events.len(), 1);
    assert_eq!(events[0][2], RawSqlValue::Integer(0));
    assert_eq!(
        events[0][3],
        RawSqlValue::Text {
            value: "message".to_owned(),
            bytes: 7,
            truncated: false,
        }
    );
    assert_eq!(
        events[0][4],
        RawSqlValue::Text {
            value: "assistant".to_owned(),
            bytes: 9,
            truncated: false,
        }
    );
    assert_eq!(
        events[0][7],
        RawSqlValue::Text {
            value: "primary".to_owned(),
            bytes: 7,
            truncated: false,
        }
    );
    // Agent scope is session metadata; native locator evidence remains internal.
    let misplaced_event_view_columns: i64 = projection
        .conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('ctx_events')
             WHERE name IN ('agent_type', 'native_locator_json')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(misplaced_event_view_columns, 0);

    let files = query_rows(
        &projection,
        "SELECT path, provider, provider_session_id, ctx_session_id
         FROM ctx_files_touched",
    );
    assert_eq!(files.len(), 1);
    let sources = query_rows(
        &projection,
        "SELECT provider, source_format, indexed_status, indexed_event_count,
                last_imported_file_sha256, branch, workspace
         FROM ctx_sources",
    );
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0][3], RawSqlValue::Integer(1));

    let forbidden_columns: i64 = projection
        .conn
        .query_row(
            "SELECT COUNT(*) FROM (
                SELECT name FROM pragma_table_info('source_backed_events')
                UNION ALL
                SELECT name FROM pragma_table_info('ctx_events')
             )
             WHERE lower(name) IN (
                'payload', 'payload_json', 'body', 'content', 'provider_body',
                'raw_json', 'body_preview', 'body_search', 'search_text', 'preview', 'text'
             )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(forbidden_columns, 0);
    let locator_bytes: Vec<u8> = projection
        .conn
        .query_row(
            "SELECT native_locator_json FROM source_backed_events",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let locator: SourceRecordLocator = serde_json::from_slice(&locator_bytes).unwrap();
    assert_eq!(locator.source(), &source);

    let metadata = projection.metadata().unwrap();
    assert_eq!(
        metadata.active_core_generation_id.as_deref(),
        Some(committed.generation_id.as_str())
    );
    assert_eq!(
        metadata.active_manifest_version,
        Some(GENERATION_MANIFEST_VERSION)
    );
    assert_eq!(
        metadata.active_lexical_schema_version,
        Some(REQUIRED_LEXICAL_SCHEMA_VERSION)
    );
    assert_eq!(
        metadata.active_policy_schema_hash.as_deref(),
        Some(REQUIRED_SOURCE_GENERATION_POLICY_HASH)
    );
    let manifest_digest: Vec<u8> = projection
        .conn
        .query_row(
            "SELECT core_manifest_sha256 FROM ctx_projection_metadata",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        manifest_digest,
        Sha256::digest(&committed.manifest_json).as_slice()
    );
    let policy_rows = query_rows(
        &projection,
        "SELECT schema_version, contract_version, core_manifest_version,
                core_lexical_schema_version, core_policy_schema_hash
         FROM ctx_projection_metadata",
    );
    assert_eq!(
        policy_rows[0][0],
        RawSqlValue::Integer(i64::from(RELATIONAL_PROJECTION_SCHEMA_VERSION))
    );
    assert_eq!(
        policy_rows[0][1],
        RawSqlValue::Integer(i64::from(RELATIONAL_PROJECTION_CONTRACT_VERSION))
    );
    assert_eq!(
        policy_rows[0][2],
        RawSqlValue::Integer(i64::from(GENERATION_MANIFEST_VERSION))
    );
    assert_eq!(
        policy_rows[0][3],
        RawSqlValue::Integer(i64::from(REQUIRED_LEXICAL_SCHEMA_VERSION))
    );
    assert_eq!(
        policy_rows[0][4],
        RawSqlValue::Text {
            value: REQUIRED_SOURCE_GENERATION_POLICY_HASH.to_owned(),
            bytes: REQUIRED_SOURCE_GENERATION_POLICY_HASH.len(),
            truncated: false,
        }
    );
}

#[test]
fn append_rewrite_deletion_and_rebuild_are_atomic_and_source_scoped() {
    let (_temp, mut projection) = projection();
    let first = source(1);
    let second = source(2);
    let generation_one = generation(vec![
        certificate(first.clone(), 1, 1),
        certificate(second.clone(), 1, 1),
    ]);
    let mut initial = records(first.clone(), 1, 1);
    initial.extend(records(second.clone(), 1, 1));
    projection.rebuild(&generation_one, initial).unwrap();
    let second_event_before_append = query_rows(
        &projection,
        &format!(
            "SELECT ctx_event_id FROM source_backed_events WHERE source_id = '{}'",
            second.identity().as_uuid()
        ),
    );

    // The first source appends one event. Its stable first event survives while
    // the unchanged second source is not part of the catch-up stream.
    let generation_two = generation(vec![
        certificate(first.clone(), 2, 2),
        certificate(second.clone(), 1, 1),
    ]);
    projection
        .catch_up(&generation_two, records(first.clone(), 2, 2))
        .unwrap();
    assert_eq!(
        query_rows(
            &projection,
            &format!(
                "SELECT ctx_event_id FROM source_backed_events WHERE source_id = '{}'",
                second.identity().as_uuid()
            ),
        ),
        second_event_before_append
    );
    assert_eq!(
        query_rows(&projection, "SELECT COUNT(*) FROM ctx_events")[0][0],
        RawSqlValue::Integer(3)
    );

    // The second source is then rewritten independently.
    let generation_three = generation(vec![
        certificate(first.clone(), 2, 2),
        certificate(second.clone(), 2, 2),
    ]);
    projection
        .catch_up(&generation_three, records(second.clone(), 2, 2))
        .unwrap();
    assert_eq!(
        query_rows(&projection, "SELECT COUNT(*) FROM ctx_events")[0][0],
        RawSqlValue::Integer(4)
    );
    let baseline = query_rows(
        &projection,
        "SELECT ctx_event_id, ctx_session_id, event_seq, event_type, role,
                occurred_at_ms, fidelity
         FROM ctx_events ORDER BY ctx_event_id",
    );
    let mut complete = records(first.clone(), 2, 2);
    complete.extend(records(second.clone(), 2, 2));
    projection.rebuild(&generation_three, complete).unwrap();
    assert_eq!(
        baseline,
        query_rows(
            &projection,
            "SELECT ctx_event_id, ctx_session_id, event_seq, event_type, role,
                    occurred_at_ms, fidelity
             FROM ctx_events ORDER BY ctx_event_id",
        )
    );

    // Omission alone is not deletion authority and cannot retire SQL rows.
    let uncertified_omission = generation(vec![certificate(first.clone(), 2, 2)]);
    assert!(matches!(
        projection.catch_up(&uncertified_omission, Vec::new()),
        Err(RelationalProjectionError::InvalidCoreGeneration(_))
    ));
    assert_eq!(
        query_rows(&projection, "SELECT COUNT(*) FROM ctx_events")[0][0],
        RawSqlValue::Integer(4)
    );
    assert_eq!(
        projection
            .metadata()
            .unwrap()
            .active_core_generation_id
            .as_deref(),
        Some(generation_three.generation_id.as_str())
    );

    // Durable certified removal makes omission authoritative and cascades all
    // session, event, and file rows for that source in one transaction.
    let generation_four =
        generation_with_removals(vec![certificate(first, 2, 2)], vec![removal(&second, 3)]);
    projection.catch_up(&generation_four, Vec::new()).unwrap();
    assert_eq!(
        query_rows(&projection, "SELECT COUNT(*) FROM ctx_events")[0][0],
        RawSqlValue::Integer(2)
    );
    assert_eq!(
        query_rows(
            &projection,
            "SELECT
                (SELECT COUNT(*) FROM source_backed_sources),
                (SELECT COUNT(*) FROM source_backed_sessions),
                (SELECT COUNT(*) FROM source_backed_files_touched)"
        )[0],
        vec![
            RawSqlValue::Integer(1),
            RawSqlValue::Integer(1),
            RawSqlValue::Integer(2),
        ]
    );
}

#[test]
fn incremental_validation_work_does_not_scale_with_unchanged_rows() {
    let (small_append, small_deletion) = incremental_work_with_unchanged_events(8);
    let (large_append, large_deletion) = incremental_work_with_unchanged_events(2_048);

    eprintln!(
        concat!(
            "incremental validation work: small_append={:?} ",
            "large_append={:?} small_deletion={:?} ",
            "large_deletion={:?}"
        ),
        small_append, large_append, small_deletion, large_deletion
    );
    assert!(
        large_append.vm_steps <= small_append.vm_steps + 500,
        "tiny append VM work grew with unchanged rows: {small_append:?} -> {large_append:?}"
    );
    assert!(
        large_deletion.vm_steps <= small_deletion.vm_steps + 500,
        "deletion VM work grew with unchanged rows: {small_deletion:?} -> {large_deletion:?}"
    );
    for (operation, work) in [
        ("small append", &small_append),
        ("large append", &large_append),
        ("small deletion", &small_deletion),
        ("large deletion", &large_deletion),
    ] {
        assert!(
            work.page_cache_misses <= 512,
            "{operation} exceeded the indexed incremental page budget: {work:?}"
        );
    }
}

#[test]
fn cross_source_lineage_survives_rewrite_and_deletion_fails_closed() {
    let (_temp, mut projection) = projection();
    let parent_source = source(41);
    let child_source = source(42);
    let (parent_session_id, _) = identities(&parent_source, 0);
    let mut child_records = records(child_source.clone(), 1, 1);
    let RelationalProjectionRecord::Session(child_session) = &mut child_records[1] else {
        panic!("fixture session ordering changed");
    };
    child_session.parent_session_id = Some(parent_session_id);
    child_session.root_session_id = parent_session_id;

    let initial_generation = generation(vec![
        certificate(parent_source.clone(), 1, 1),
        certificate(child_source.clone(), 1, 1),
    ]);
    let mut initial_records = records(parent_source.clone(), 1, 1);
    initial_records.extend(child_records);
    projection
        .rebuild(&initial_generation, initial_records)
        .unwrap();

    let rewrite_generation = generation(vec![
        certificate(parent_source.clone(), 2, 2),
        certificate(child_source.clone(), 1, 1),
    ]);
    let rewrite_receipt = projection
        .catch_up(&rewrite_generation, records(parent_source.clone(), 2, 2))
        .unwrap();
    assert_eq!(
        (rewrite_receipt.source_count, rewrite_receipt.event_count),
        (2, 3)
    );

    let deletion_generation = generation_with_removals(
        vec![certificate(child_source, 1, 1)],
        vec![removal(&parent_source, 3)],
    );
    let error = projection
        .catch_up(&deletion_generation, Vec::new())
        .unwrap_err();
    assert!(matches!(
        error,
        RelationalProjectionError::InvalidRecord(ref detail)
            if detail == "session relationships reference absent sessions"
    ));
    let metadata = projection.metadata().unwrap();
    assert_eq!(
        metadata.active_core_generation_id.as_deref(),
        Some(rewrite_generation.generation_id.as_str())
    );
    assert_eq!(
        metadata.target_core_generation_id.as_deref(),
        Some(deletion_generation.generation_id.as_str())
    );
    assert_eq!(metadata.status, RelationalProjectionStatus::Behind);
    assert_eq!((metadata.source_count, metadata.session_count), (2, 2));
}

#[test]
fn committed_core_remains_current_when_sql_projection_fails_then_catches_up() {
    let (_temp, mut projection) = projection();
    let source = source(7);
    let generation_one = generation(vec![certificate(source.clone(), 1, 1)]);
    projection
        .rebuild(&generation_one, records(source.clone(), 1, 1))
        .unwrap();
    let old_event_id = query_rows(&projection, "SELECT ctx_event_id FROM ctx_events");

    // This receipt represents an already successful Core commit. SQL receives
    // an incomplete source stream and must fail without rolling Core or the
    // previously active SQL generation back.
    let generation_two = generation(vec![certificate(source.clone(), 2, 2)]);
    let error = projection
        .catch_up(&generation_two, records(source.clone(), 2, 1))
        .unwrap_err();
    assert!(matches!(
        error,
        RelationalProjectionError::SourceEventCountMismatch { .. }
    ));
    assert_eq!(
        old_event_id,
        query_rows(&projection, "SELECT ctx_event_id FROM ctx_events")
    );
    let failed = projection.metadata().unwrap();
    assert_eq!(
        failed.active_core_generation_id.as_deref(),
        Some(generation_one.generation_id.as_str())
    );
    assert_eq!(
        failed.target_core_generation_id.as_deref(),
        Some(generation_two.generation_id.as_str())
    );
    assert_eq!(failed.status, RelationalProjectionStatus::Behind);

    projection
        .catch_up(&generation_two, records(source, 2, 2))
        .unwrap();
    let caught_up = projection.metadata().unwrap();
    assert_eq!(
        caught_up.active_core_generation_id.as_deref(),
        Some(generation_two.generation_id.as_str())
    );
    assert_eq!(caught_up.target_core_generation_id, None);
    assert_eq!(caught_up.status, RelationalProjectionStatus::Ready);
    assert_eq!(caught_up.event_count, 2);
}

#[test]
fn provisional_and_final_sessions_preserve_parity_and_missing_sessions_fail_closed() {
    let source = source(14);
    let committed = generation(vec![certificate(source.clone(), 1, 2)]);
    let ordered_records = records(source.clone(), 1, 2);
    let session_index = ordered_records
        .iter()
        .position(|record| matches!(record, RelationalProjectionRecord::Session(_)))
        .unwrap();
    let session = ordered_records[session_index].clone();
    let mut finalized_records = ordered_records.clone();
    finalized_records.insert(finalized_records.len() - 1, session);

    let (_ordered_temp, mut ordered) = projection();
    ordered.rebuild(&committed, ordered_records).unwrap();
    let ordered_digest = logical_projection_digest(&ordered);

    let (_finalized_temp, mut finalized) = projection();
    finalized
        .rebuild(&committed, finalized_records.clone())
        .unwrap();
    assert_eq!(logical_projection_digest(&finalized), ordered_digest);

    let (_failed_temp, mut failed) = projection();
    finalized_records.retain(|record| !matches!(record, RelationalProjectionRecord::Session(_)));
    failed
        .rebuild(&committed, finalized_records)
        .expect_err("events without a provisional session must fail");
    assert_eq!(
        query_rows(
            &failed,
            "SELECT
                (SELECT COUNT(*) FROM source_backed_sources),
                (SELECT COUNT(*) FROM source_backed_sessions),
                (SELECT COUNT(*) FROM source_backed_events),
                (SELECT COUNT(*) FROM source_backed_files_touched)"
        )[0],
        vec![
            RawSqlValue::Integer(0),
            RawSqlValue::Integer(0),
            RawSqlValue::Integer(0),
            RawSqlValue::Integer(0),
        ]
    );
}

#[test]
fn fallible_record_stream_error_rolls_back_then_replays_with_exact_indexes() {
    let (_temp, mut projection) = projection();
    let source = source(8);
    let generation_one = generation(vec![certificate(source.clone(), 1, 1)]);
    projection
        .rebuild(&generation_one, records(source.clone(), 1, 1))
        .unwrap();
    let old_event_id = query_rows(&projection, "SELECT ctx_event_id FROM ctx_events");
    let old_indexes = secondary_index_definitions(&projection);

    let generation_two = generation(vec![certificate(source.clone(), 2, 2)]);
    let mut interrupted_records = records(source, 2, 2);
    let finalized_session = interrupted_records
        .iter()
        .find(|record| matches!(record, RelationalProjectionRecord::Session(_)))
        .unwrap()
        .clone();
    let finalization_index = interrupted_records.len() - 1;
    interrupted_records.insert(finalization_index, finalized_session);
    let replay_records = interrupted_records.clone();
    let stream = interrupted_records
        .into_iter()
        .enumerate()
        .map(|(index, record)| {
            if index == finalization_index {
                Err(RelationalProjectionError::InvalidRecord(
                    "injected session finalization failure".to_owned(),
                ))
            } else {
                Ok(record)
            }
        });
    let error = projection
        .catch_up_stream(&generation_two, stream)
        .unwrap_err();

    assert!(matches!(
        error,
        RelationalProjectionError::InvalidRecord(ref detail)
            if detail == "injected session finalization failure"
    ));
    assert_eq!(
        old_event_id,
        query_rows(&projection, "SELECT ctx_event_id FROM ctx_events")
    );
    assert_eq!(secondary_index_definitions(&projection), old_indexes);
    let failed = projection.metadata().unwrap();
    assert_eq!(
        failed.active_core_generation_id.as_deref(),
        Some(generation_one.generation_id.as_str())
    );
    assert_eq!(
        failed.target_core_generation_id.as_deref(),
        Some(generation_two.generation_id.as_str())
    );
    assert_eq!(failed.status, RelationalProjectionStatus::Behind);

    projection
        .catch_up(&generation_two, replay_records)
        .unwrap();
    assert_eq!(secondary_index_definitions(&projection), old_indexes);
    let replayed = projection.metadata().unwrap();
    assert_eq!(replayed.status, RelationalProjectionStatus::Ready);
    assert_eq!(
        replayed.active_core_generation_id.as_deref(),
        Some(generation_two.generation_id.as_str())
    );
    assert_eq!((replayed.event_count, replayed.file_touch_count), (2, 2));
}

#[test]
fn valid_5566_source_manifest_larger_than_8_mib_is_accepted() {
    const SOURCE_COUNT: usize = 5_566;
    const PREVIOUS_RELATIONAL_LIMIT_BYTES: usize = 8 * 1024 * 1024;

    let certificates = (0..SOURCE_COUNT as u64)
        .map(large_manifest_certificate)
        .collect::<Vec<_>>();
    let mut expected_source_ids = certificates
        .iter()
        .map(|certificate| {
            certificate
                .observation()
                .source()
                .identity()
                .as_uuid()
                .to_string()
        })
        .collect::<Vec<_>>();
    expected_source_ids.sort();
    let committed = generation(certificates);

    assert_eq!(committed.certified_sources, SOURCE_COUNT);
    assert!(
        committed.manifest_json.len() > PREVIOUS_RELATIONAL_LIMIT_BYTES,
        "fixture manifest was only {} bytes",
        committed.manifest_json.len()
    );

    let validated = ValidatedManifest::from_commit(&committed).unwrap();
    assert_eq!(validated.sources.len(), SOURCE_COUNT);
    assert_eq!(
        validated.sources.keys().cloned().collect::<Vec<_>>(),
        expected_source_ids
    );
}

#[test]
fn manifest_v3_schema_v5_and_exact_policy_hash_fail_closed() {
    let (_temp, mut projection) = projection();
    let source = source(9);
    let current = generation(vec![certificate(source, 1, 1)]);
    let manifest: GenerationManifest = serde_json::from_slice(&current.manifest_json).unwrap();
    assert_eq!(manifest.manifest_version, 3);
    assert_eq!(manifest.lexical_schema_version, 5);
    assert_eq!(manifest.lexical_analyzer_version, 2);
    assert_eq!(
        manifest.policy_schema_hash,
        "a17e860b6d719dfde065256ec070970b3d12e4d76ff0e59f16aabbc1666b71b9"
    );
    assert_eq!(
        manifest.policy_schema_hash,
        REQUIRED_SOURCE_GENERATION_POLICY_HASH
    );

    let mut old_manifest = current.clone();
    let mut old_manifest_contract = manifest.clone();
    old_manifest_contract.manifest_version = 2;
    replace_manifest(&mut old_manifest, &old_manifest_contract);
    let error = projection.rebuild(&old_manifest, Vec::new()).unwrap_err();
    assert!(matches!(
        error,
        RelationalProjectionError::InvalidCoreGeneration(_)
    ));
    assert!(error
        .to_string()
        .contains("rebuild the disposable Core generation"));

    let mut old_schema = current.clone();
    let mut old_schema_contract = manifest.clone();
    old_schema_contract.lexical_schema_version = 4;
    replace_manifest(&mut old_schema, &old_schema_contract);
    assert!(matches!(
        projection.rebuild(&old_schema, Vec::new()),
        Err(RelationalProjectionError::InvalidCoreGeneration(_))
    ));

    let mut old_analyzer = current.clone();
    let mut old_analyzer_contract = manifest.clone();
    old_analyzer_contract.lexical_analyzer_version = 1;
    replace_manifest(&mut old_analyzer, &old_analyzer_contract);
    assert!(matches!(
        projection.rebuild(&old_analyzer, Vec::new()),
        Err(RelationalProjectionError::InvalidCoreGeneration(_))
    ));

    let mut wrong_policy = current.clone();
    let mut wrong_policy_contract = manifest.clone();
    wrong_policy_contract.policy_schema_hash = "0".repeat(64);
    replace_manifest(&mut wrong_policy, &wrong_policy_contract);
    assert!(matches!(
        projection.rebuild(&wrong_policy, Vec::new()),
        Err(RelationalProjectionError::InvalidCoreGeneration(_))
    ));
}

#[test]
fn malformed_manifest_identity_version_and_digest_fail_closed() {
    let (_temp, mut projection) = projection();
    let current = generation(vec![certificate(source(10), 1, 1)]);

    let mut malformed = current.clone();
    malformed.manifest_json = b"{".to_vec();
    malformed.generation_id = hex(&Sha256::digest(&malformed.manifest_json));
    assert!(matches!(
        projection.rebuild(&malformed, Vec::new()),
        Err(RelationalProjectionError::InvalidCoreGeneration(_))
    ));

    let mut wrong_identity = current.clone();
    let mut wrong_identity_contract: GenerationManifest =
        serde_json::from_slice(&current.manifest_json).unwrap();
    wrong_identity_contract.identity_version += 1;
    replace_manifest(&mut wrong_identity, &wrong_identity_contract);
    assert!(matches!(
        projection.rebuild(&wrong_identity, Vec::new()),
        Err(RelationalProjectionError::InvalidCoreGeneration(_))
    ));

    let mut wrong_generation_id = current;
    wrong_generation_id.generation_id = "f".repeat(64);
    assert!(matches!(
        projection.rebuild(&wrong_generation_id, Vec::new()),
        Err(RelationalProjectionError::InvalidCoreGeneration(_))
    ));
}

#[test]
fn previous_relational_schema_requires_disposable_rebuild() {
    let (temp, projection) = projection();
    let path = projection.path().to_path_buf();
    drop(projection);
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE source_backed_relational_state
         SET schema_version = 2, contract_version = 2
         WHERE singleton = 1",
        [],
    )
    .unwrap();
    drop(conn);

    let error = match SourceBackedRelationalProjection::open(&path) {
        Ok(_) => panic!("old relational schema unexpectedly opened"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        RelationalProjectionError::UnsupportedSchema {
            schema_version: 2,
            contract_version: 2,
        }
    ));
    assert!(error
        .to_string()
        .contains("delete and rebuild the disposable relational projection"));
    assert!(temp.path().exists());
}

#[test]
fn sqlite_schema_bytes_and_views_exclude_event_payloads() {
    let temp = tempfile::tempdir().unwrap();
    let provider_dir = temp.path().join("provider");
    fs::create_dir_all(&provider_dir).unwrap();
    let provider_path = provider_dir.join("session.jsonl");
    let forbidden = [
        "provider-body-sentinel-7d64ac",
        "lexical-search-text-sentinel-621ee0",
        "legacy-preview-sentinel-49f8b1",
    ];
    fs::write(&provider_path, forbidden.join(" ")).unwrap();

    let database_path = temp.path().join("sql/relational.sqlite");
    let mut projection = SourceBackedRelationalProjection::open(&database_path).unwrap();
    let source = source(11);
    let committed = generation(vec![certificate(source.clone(), 1, 1)]);
    let mut projected_records = records(source, 1, 1);
    let RelationalProjectionRecord::BeginSource(source_metadata) = &mut projected_records[0] else {
        panic!("fixture source ordering changed");
    };
    source_metadata.source_path = Some(provider_path.to_string_lossy().into_owned());
    let RelationalProjectionRecord::Session(session_metadata) = &mut projected_records[1] else {
        panic!("fixture session ordering changed");
    };
    session_metadata.source_path = Some(provider_path.to_string_lossy().into_owned());
    projection.rebuild(&committed, projected_records).unwrap();

    assert_eq!(
        schema_columns(&projection, "source_backed_events"),
        [
            "ctx_event_id",
            "event_identity",
            "source_id",
            "ctx_session_id",
            "session_identity",
            "event_seq",
            "event_type",
            "role",
            "occurred_at_ms",
            "fidelity",
            "native_locator_json",
            "record_digest",
        ]
    );
    assert_eq!(
        schema_columns(&projection, "ctx_events"),
        [
            "ctx_event_id",
            "ctx_session_id",
            "history_record_id",
            "provider",
            "provider_session_id",
            "event_seq",
            "event_type",
            "role",
            "occurred_at_ms",
            "fidelity",
            "cwd",
            "source_path",
            "source_format",
            "source_root",
            "source_identity",
            "branch",
            "workspace",
        ]
    );
    let event_view_definition: String = projection
        .conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'view' AND name = 'ctx_events'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let event_view_definition = event_view_definition.to_lowercase();
    for forbidden_schema_term in [
        "payload",
        "content",
        "body",
        "preview",
        "search_text",
        "raw_json",
    ] {
        assert!(!event_view_definition.contains(forbidden_schema_term));
    }

    projection
        .conn
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    for entry in fs::read_dir(database_path.parent().unwrap()).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("relational.sqlite") {
            continue;
        }
        let bytes = fs::read(entry.path()).unwrap();
        for marker in forbidden {
            assert!(
                !bytes
                    .windows(marker.len())
                    .any(|window| window == marker.as_bytes()),
                "{} contains forbidden provider content marker {marker}",
                entry.path().display()
            );
        }
        assert!(!bytes
            .windows(b"body_preview".len())
            .any(|window| window == b"body_preview"));
        assert!(!bytes
            .windows(b"body_search".len())
            .any(|window| window == b"body_search"));
        assert!(!bytes
            .windows(b"payload_json".len())
            .any(|window| window == b"payload_json"));
        assert!(!bytes
            .windows(b"content_authority".len())
            .any(|window| window == b"content_authority"));
        assert!(!bytes
            .windows(b"provider_source".len())
            .any(|window| window == b"provider_source"));
    }
}

#[test]
fn read_only_projection_serves_metadata_sql_without_writes() {
    let (temp, mut projection) = projection();
    let source = source(3);
    let committed = generation(vec![certificate(source.clone(), 1, 1)]);
    projection
        .rebuild(&committed, records(source, 1, 1))
        .unwrap();
    drop(projection);

    let reader =
        SourceBackedRelationalProjection::open_read_only(temp.path().join("relational.sqlite"))
            .unwrap();
    assert_eq!(
        query_rows(&reader, "SELECT COUNT(*) FROM ctx_sessions")[0][0],
        RawSqlValue::Integer(1)
    );
    assert!(reader
        .raw_sql_query(
            "DELETE FROM source_backed_sessions",
            RawSqlOptions::default()
        )
        .is_err());
}

#[test]
fn guarded_raw_sql_rejects_empty_parameters_writes_and_multiple_statements() {
    let (_temp, projection) = projection();

    assert!(matches!(
        projection
            .raw_sql_query("", RawSqlOptions::default())
            .unwrap_err(),
        RelationalProjectionError::RawSqlEmpty
    ));
    assert!(matches!(
        projection
            .raw_sql_query("SELECT ?1", RawSqlOptions::default())
            .unwrap_err(),
        RelationalProjectionError::RawSqlHasParameters
    ));
    assert!(matches!(
        projection
            .raw_sql_query("CREATE TABLE nope(x INTEGER)", RawSqlOptions::default())
            .unwrap_err(),
        RelationalProjectionError::RawSqlNotReadOnly
    ));
    assert!(matches!(
        projection
            .raw_sql_query("SELECT 1; SELECT 2", RawSqlOptions::default())
            .unwrap_err(),
        RelationalProjectionError::Sql(rusqlite::Error::MultipleStatement)
    ));
}

#[test]
fn guarded_raw_sql_caps_rows_and_values() {
    let (_temp, projection) = projection();
    let result = projection
        .raw_sql_query(
            "SELECT 'abcdef' AS text_value, X'01020304' AS blob_value UNION ALL SELECT 'ghijkl', X'05060708'",
            RawSqlOptions {
                max_rows: 1,
                max_value_bytes: 3,
                ..RawSqlOptions::default()
            },
        )
        .unwrap();

    assert_eq!(result.returned_rows, 1);
    assert_eq!(result.columns[0].name, "text_value");
    assert_eq!(result.columns[1].name, "blob_value");
    assert_eq!(
        result.rows[0][0],
        RawSqlValue::Text {
            value: "abc".to_owned(),
            bytes: 6,
            truncated: true,
        }
    );
    assert_eq!(
        result.rows[0][1],
        RawSqlValue::Blob {
            bytes: 4,
            preview_hex: "010203".to_owned(),
            truncated: true,
        }
    );
    assert!(result.truncated.rows);
    assert!(result.truncated.values);
}

#[test]
fn guarded_raw_sql_rejects_excessive_result_preview_budget() {
    let (_temp, projection) = projection();
    let many_columns = (0..RAW_SQL_MAX_COLUMNS_CAP)
        .map(|index| format!("1 AS c{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let error = projection
        .raw_sql_query(
            &format!("SELECT {many_columns}"),
            RawSqlOptions {
                max_rows: RAW_SQL_MAX_ROWS_CAP,
                max_columns: RAW_SQL_MAX_COLUMNS_CAP,
                max_value_bytes: 32,
                ..RawSqlOptions::default()
            },
        )
        .unwrap_err();

    assert!(matches!(
        error,
        RelationalProjectionError::RawSqlResultBudgetTooLarge {
            max_result_bytes: RAW_SQL_MAX_RESULT_PREVIEW_BYTES,
            ..
        }
    ));
}

#[test]
fn guarded_raw_sql_budgets_against_actual_column_count() {
    let (_temp, projection) = projection();
    let result = projection
        .raw_sql_query(
            "SELECT 1",
            RawSqlOptions {
                max_rows: RAW_SQL_MAX_ROWS_CAP,
                max_columns: RAW_SQL_MAX_COLUMNS_CAP,
                max_value_bytes: 32,
                ..RawSqlOptions::default()
            },
        )
        .unwrap();

    assert_eq!(result.returned_rows, 1);
    assert_eq!(result.rows[0][0], RawSqlValue::Integer(1));
}

#[test]
fn guarded_raw_sql_times_out_long_running_queries() {
    let (_temp, projection) = projection();
    let error = projection
        .raw_sql_query(
            r#"
            WITH RECURSIVE numbers(x) AS (
                SELECT 1
                UNION ALL
                SELECT x + 1 FROM numbers WHERE x < 100000000
            )
            SELECT sum(x) FROM numbers
            "#,
            RawSqlOptions {
                timeout: Duration::from_millis(1),
                ..RawSqlOptions::default()
            },
        )
        .unwrap_err();

    assert!(matches!(
        error,
        RelationalProjectionError::RawSqlTimedOut { .. }
    ));
}

#[test]
fn guarded_raw_sql_enforces_sqlite_value_length_limit() {
    let (_temp, projection) = projection();
    let error = projection
        .raw_sql_query(
            "SELECT length(randomblob(200000))",
            RawSqlOptions::default(),
        )
        .unwrap_err();

    assert!(matches!(
        error,
        RelationalProjectionError::Sql(rusqlite::Error::SqliteFailure(error, _))
            if error.code == ErrorCode::TooBig
    ));
}
