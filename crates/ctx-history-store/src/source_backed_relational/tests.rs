use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, Confidence, EventIdentityInput, EventType,
    Fidelity, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    ScannedSourceCounts, SessionIdentityInput, SessionStatus, SourceAnchor, SourceObservation,
    SourceRecordLocator, TypedKey,
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

fn generation(mut sources: Vec<CertifiedSource>) -> CommittedCoreGeneration {
    sources.sort_by_key(|source| source.observation().source().identity().digest());
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
        indexed_documents,
        certified_source_bytes,
        sources,
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
            bounded_preview: Some(format!("bounded preview {event_index}")),
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
fn source_backed_projection_preserves_stable_view_shape_without_bodies() {
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
    assert_eq!(payload["body_preview"], "bounded preview 0");
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
}

#[test]
fn rebuild_is_deterministic_and_catch_up_replaces_only_changed_sources() {
    let (_temp, mut projection) = projection();
    let first = source(1);
    let second = source(2);
    let generation_one = generation(vec![
        certificate(first.clone(), 1, 1),
        certificate(second.clone(), 1, 1),
    ]);
    let mut complete = records(first.clone(), 1, 1);
    complete.extend(records(second.clone(), 1, 1));
    projection
        .rebuild(&generation_one, complete.clone())
        .unwrap();
    let baseline = query_rows(
        &projection,
        "SELECT ctx_event_id, ctx_session_id, event_seq, payload_json
         FROM ctx_events ORDER BY ctx_event_id",
    );

    projection.rebuild(&generation_one, complete).unwrap();
    assert_eq!(
        baseline,
        query_rows(
            &projection,
            "SELECT ctx_event_id, ctx_session_id, event_seq, payload_json
             FROM ctx_events ORDER BY ctx_event_id",
        )
    );
    assert_eq!(projection.metadata().unwrap().build_generation, 2);

    let generation_two = generation(vec![
        certificate(first.clone(), 1, 1),
        certificate(second.clone(), 2, 2),
    ]);
    projection
        .catch_up(&generation_two, records(second.clone(), 2, 2))
        .unwrap();
    let counts = query_rows(
        &projection,
        "SELECT provider, COUNT(*) FROM ctx_events GROUP BY provider",
    );
    assert_eq!(counts[0][1], RawSqlValue::Integer(3));
    assert_eq!(
        projection
            .metadata()
            .unwrap()
            .active_core_generation_id
            .as_deref(),
        Some(generation_two.generation_id.as_str())
    );

    let generation_three = generation(vec![certificate(first, 1, 1)]);
    projection.catch_up(&generation_three, Vec::new()).unwrap();
    assert_eq!(
        query_rows(&projection, "SELECT COUNT(*) FROM ctx_events")[0][0],
        RawSqlValue::Integer(1)
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
fn schema_v4_manifest_and_preview_bounds_fail_closed() {
    let (_temp, mut projection) = projection();
    let source = source(9);
    let source_certificate = certificate(source.clone(), 1, 1);
    let mut committed = generation(vec![source_certificate]);
    let mut manifest: GenerationManifest =
        serde_json::from_slice(&committed.manifest_json).unwrap();
    manifest.lexical_schema_version = 3;
    committed.manifest_json = serde_json::to_vec(&manifest).unwrap();
    committed.generation_id = hex(&Sha256::digest(&committed.manifest_json));
    assert!(matches!(
        projection.rebuild(&committed, Vec::new()),
        Err(RelationalProjectionError::InvalidCoreGeneration(_))
    ));

    let committed = generation(vec![certificate(source.clone(), 1, 1)]);
    let mut oversized = records(source, 1, 1);
    let RelationalProjectionRecord::Event(event) = &mut oversized[2] else {
        panic!("fixture event ordering changed");
    };
    event.bounded_preview = Some("x".repeat(RELATIONAL_EVENT_PREVIEW_MAX_CHARS + 1));
    assert!(matches!(
        projection.rebuild(&committed, oversized),
        Err(RelationalProjectionError::InvalidRecord(_))
    ));
}

#[test]
fn read_only_projection_serves_bounded_sql_without_writes() {
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
