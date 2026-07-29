use std::fs;

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CertifiedSourceDeletion,
    CertifiedSourceInventory, Confidence, EventIdentityInput, EventType, Fidelity,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    ScannedSourceCounts, SessionIdentityInput, SessionStatus, SourceAnchor,
    SourceInventoryObservation, SourceObservation, SourceRecordLocator, TypedKey,
};
use serde_json::Value;
use tempfile::TempDir;

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
            [event_index as u8 + 1; 32],
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
        "SELECT ctx_event_id, ctx_session_id, event_type, role, payload_json,
                branch, workspace
         FROM ctx_events",
    );
    assert_eq!(events.len(), 1);
    let RawSqlValue::Text { value: payload, .. } = &events[0][4] else {
        panic!("payload_json was not text");
    };
    let payload: Value = serde_json::from_str(payload).unwrap();
    assert_eq!(payload["content_authority"], "provider_source");
    assert_eq!(payload.as_object().unwrap().len(), 1);
    assert!(payload.get("body_preview").is_none());
    assert!(payload.get("preview").is_none());
    assert!(payload.get("text").is_none());
    assert!(payload.get("body").is_none());
    assert!(payload.get("content").is_none());

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
            "SELECT COUNT(*) FROM pragma_table_info('source_backed_events')
             WHERE lower(name) IN ('body', 'content', 'provider_body', 'raw_json')",
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
        "SELECT ctx_event_id, ctx_session_id, event_seq, payload_json
         FROM ctx_events ORDER BY ctx_event_id",
    );
    let mut complete = records(first.clone(), 2, 2);
    complete.extend(records(second.clone(), 2, 2));
    projection.rebuild(&generation_three, complete).unwrap();
    assert_eq!(
        baseline,
        query_rows(
            &projection,
            "SELECT ctx_event_id, ctx_session_id, event_seq, payload_json
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
fn manifest_v3_schema_v5_and_exact_policy_hash_fail_closed() {
    let (_temp, mut projection) = projection();
    let source = source(9);
    let current = generation(vec![certificate(source, 1, 1)]);
    let manifest: GenerationManifest = serde_json::from_slice(&current.manifest_json).unwrap();
    assert_eq!(manifest.manifest_version, 3);
    assert_eq!(manifest.lexical_schema_version, 5);
    assert_eq!(
        manifest.policy_schema_hash,
        "255eb2b901f1dfb1c9c521c1d177dbe7a416491b5c0b8532494135bcb8b42ede"
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

    let mut wrong_policy = current.clone();
    let mut wrong_policy_contract = manifest.clone();
    wrong_policy_contract.policy_schema_hash = "0".repeat(64);
    replace_manifest(&mut wrong_policy, &wrong_policy_contract);
    assert!(matches!(
        projection.rebuild(&wrong_policy, Vec::new()),
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
fn old_relational_schema_requires_disposable_rebuild() {
    let (temp, projection) = projection();
    let path = projection.path().to_path_buf();
    drop(projection);
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE source_backed_relational_state
         SET schema_version = 1, contract_version = 1
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
            schema_version: 1,
            contract_version: 1,
        }
    ));
    assert!(error
        .to_string()
        .contains("delete and rebuild the disposable relational projection"));
    assert!(temp.path().exists());
}

#[test]
fn sqlite_bytes_and_views_exclude_provider_body_search_text_and_preview() {
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

    let payload: String = projection
        .conn
        .query_row("SELECT payload_json FROM ctx_events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(payload, r#"{"content_authority":"provider_source"}"#);
    let forbidden_view_columns: i64 = projection
        .conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('ctx_events')
             WHERE lower(name) LIKE '%body%'
                OR lower(name) LIKE '%preview%'
                OR lower(name) LIKE '%search_text%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(forbidden_view_columns, 0);
    let view_definitions: String = projection
        .conn
        .query_row(
            "SELECT group_concat(sql, ' ') FROM sqlite_schema WHERE type = 'view'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!view_definitions.to_lowercase().contains("body_preview"));
    assert!(!view_definitions.to_lowercase().contains("body_search"));

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
