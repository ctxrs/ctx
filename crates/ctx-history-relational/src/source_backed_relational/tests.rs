use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use ctx_history_core::{
    core_record_contract_fingerprint, derive_event_id, derive_session_id, CoreContent,
    CoreContentPolicyStatus, CoreRecord, EventIdentityInput, GitObjectFormat, GitObjectId,
    NativeItemKey, NativeSessionKey, RepositoryAlias, RepositoryAliasKind, RepositoryBinding,
    RepositoryCandidateEvidence, RepositoryEvidence, RepositoryEvidenceConfidence,
    RepositoryEvidenceKind, RepositoryFileObservation, RepositoryFileObservationKind,
    RepositoryOutcomeKind, RepositoryOutcomeLinkage, RepositoryOutcomeObservation,
    RepositoryVcsObservation, RepositoryVcsObservationKind, SessionIdentityInput, SourceAnchor,
    SourceKey, StableEntityId, TypedKey, CORE_CONTENT_POLICY_REVISION, CORE_NORMALIZATION_REVISION,
    CORE_RECORD_VERSION, CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
};
use tempfile::TempDir;

use rusqlite::{ffi, Connection};

use super::*;

mod raw_sql;

const BODY_SENTINEL: &str = "complete-transcript-body-must-never-enter-relational";
const STRUCTURED_SENTINEL: &str = "structured-content-must-never-enter-relational";

fn source(name: &str) -> SourceKey {
    source_for("codex", "codex_session_jsonl", name)
}

fn source_for(provider: &str, source_format: &str, name: &str) -> SourceKey {
    SourceKey::derive(
        provider,
        source_format,
        "session",
        1,
        SourceAnchor::provider_native("session", TypedKey::utf8(name).unwrap()).unwrap(),
    )
    .unwrap()
}

fn identities(source: &SourceKey, sequence: u64) -> (StableEntityId, StableEntityId) {
    let native_session = NativeSessionKey::native_id(
        "session",
        TypedKey::utf8(format!("session-{}", source.identity())).unwrap(),
    )
    .unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &native_session,
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
    (session_id, event_id)
}

fn repository_binding(binding_id: &str, logical_id: &str) -> RepositoryBinding {
    RepositoryBinding {
        binding_id: binding_id.to_owned(),
        logical_repository_id: logical_id.to_owned(),
        checkout_id: Some(format!("checkout-{binding_id}")),
        worktree_id: Some(format!("worktree-{binding_id}")),
        aliases: vec![RepositoryAlias {
            kind: RepositoryAliasKind::Forge,
            host: "github.com".to_owned(),
            namespace: vec!["ctxrs".to_owned()],
            name: logical_id.to_owned(),
            remote_name: None,
        }],
        git_object_format: Some(GitObjectFormat::Sha1),
        local_root_authorization: None,
        evidence: vec![RepositoryEvidence {
            kind: RepositoryEvidenceKind::FileActivity,
            confidence: RepositoryEvidenceConfidence::High,
        }],
        association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    }
}

fn record(source: &SourceKey, sequence: u64) -> CoreRecord {
    let (session_id, event_id) = identities(source, sequence);
    CoreRecord {
        record_version: CORE_RECORD_VERSION,
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        provider_session_id: Some("provider-session".to_owned()),
        native_event_id: Some(TypedKey::U64(sequence)),
        event_sequence: sequence,
        occurred_at_unix_ms: Some(1_700_000_000_000 + sequence as i64),
        event_type: "message".to_owned(),
        role: Some("user".to_owned()),
        agent_type: "primary".to_owned(),
        is_primary: true,
        workspace: Some("ctx".to_owned()),
        branch: Some("main".to_owned()),
        cwd: Some("/work/ctx".to_owned()),
        parser_revision: "parser-v1".to_owned(),
        normalization_revision: CORE_NORMALIZATION_REVISION,
        content: CoreContent {
            policy_revision: CORE_CONTENT_POLICY_REVISION,
            policy_status: CoreContentPolicyStatus::Selected,
            normalized_body: Some(format!("{BODY_SENTINEL}-{sequence}")),
            structured_content: Some(serde_json::json!({"secret": STRUCTURED_SENTINEL})),
        },
        metadata: BTreeMap::new(),
        repository_candidate_evidence: RepositoryCandidateEvidence::default(),
        repository_bindings: Vec::new(),
        repository_abstentions: Vec::new(),
        repository_file_observations: Vec::new(),
        repository_vcs_observations: Vec::new(),
    }
}

fn repository_record(source: &SourceKey, sequence: u64) -> CoreRecord {
    let mut event = record(source, sequence);
    event.repository_bindings = vec![repository_binding("repo-shared", "ctx")];
    event.repository_file_observations = vec![RepositoryFileObservation {
        repository_binding_id: "repo-shared".to_owned(),
        relative_path: format!("src/event-{sequence}.rs"),
        kind: RepositoryFileObservationKind::Modified,
        prior_relative_path: None,
    }];
    event.repository_vcs_observations = vec![RepositoryVcsObservation {
        repository_binding_id: "repo-shared".to_owned(),
        kind: RepositoryVcsObservationKind::Commit,
        object_id: Some(GitObjectId {
            format: GitObjectFormat::Sha1,
            hex: format!("{sequence:040x}"),
        }),
        parent_object_ids: vec![GitObjectId {
            format: GitObjectFormat::Sha1,
            hex: "b".repeat(40),
        }],
        reference: Some("refs/heads/main".to_owned()),
        relative_path: None,
    }];
    event
}

fn source_metadata(source: &SourceKey, revision: u8, events: u64) -> RelationalSourceMetadata {
    RelationalSourceMetadata {
        source: source.clone(),
        parser_revision: "parser-v1".to_owned(),
        revision_digest: [revision; 32],
        indexed_event_count: events,
        health: RelationalSourceHealth::Ready,
    }
}

fn generation(
    generation_byte: u8,
    sources: Vec<RelationalSourceMetadata>,
) -> CommittedCoreGeneration {
    CommittedCoreGeneration {
        generation_id: format!("{generation_byte:02x}").repeat(32),
        manifest_version: 6,
        core_record_version: CORE_RECORD_VERSION,
        core_record_contract_fingerprint: core_record_contract_fingerprint(),
        lexical_schema_version: 6,
        policy_schema_hash: "core-policy-v1".to_owned(),
        indexed_documents: sources
            .iter()
            .map(|source| source.indexed_event_count)
            .sum(),
        sources,
    }
}

fn records(
    metadata: RelationalSourceMetadata,
    records: Vec<CoreRecord>,
) -> Vec<RelationalProjectionRecord> {
    let source_id = metadata.source.identity().as_uuid();
    std::iter::once(RelationalProjectionRecord::BeginSource(Box::new(metadata)))
        .chain(
            records
                .into_iter()
                .map(|record| RelationalProjectionRecord::CoreRecord(Box::new(record))),
        )
        .chain(std::iter::once(RelationalProjectionRecord::EndSource {
            source_id,
        }))
        .collect()
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
                max_rows: 100,
                max_value_bytes: 4 * 1024,
                ..RawSqlOptions::default()
            },
        )
        .unwrap()
        .rows
}

fn view_columns(projection: &SourceBackedRelationalProjection, view: &str) -> Vec<String> {
    query_rows(
        projection,
        &format!("SELECT name FROM pragma_table_info('{view}')"),
    )
    .into_iter()
    .filter_map(|row| match row.into_iter().next() {
        Some(RawSqlValue::Text { value, .. }) => Some(value),
        _ => None,
    })
    .collect()
}

fn query_plan(conn: &Connection, sql: &str) -> Vec<String> {
    let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    statement
        .query_map([], |row| row.get(3))
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
    projection
        .conn
        .execute_batch("PRAGMA cache_size = -64; PRAGMA shrink_memory;")
        .unwrap();
    sqlite_cache_misses(&projection.conn, true);
    let progress_calls = Arc::new(AtomicU64::new(0));
    let measured_calls = Arc::clone(&progress_calls);
    projection.conn.progress_handler(
        1,
        Some(move || {
            measured_calls.fetch_add(1, Ordering::Relaxed);
            false
        }),
    );

    operation(projection);

    projection.conn.progress_handler(0, None::<fn() -> bool>);
    ProjectionWork {
        vm_steps: progress_calls.load(Ordering::Relaxed),
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
    let unchanged = source(&format!("unchanged-{unchanged_event_count}"));
    let changing = source(&format!("changing-{unchanged_event_count}"));
    let unchanged_metadata = source_metadata(&unchanged, 1, unchanged_event_count);
    let changing_metadata = source_metadata(&changing, 1, 1);
    let initial = generation(
        30,
        vec![unchanged_metadata.clone(), changing_metadata.clone()],
    );
    let mut initial_records = records(
        unchanged_metadata.clone(),
        (1..=unchanged_event_count)
            .map(|sequence| record(&unchanged, sequence))
            .collect(),
    );
    initial_records.extend(records(changing_metadata, vec![record(&changing, 1)]));
    projection.rebuild(&initial, initial_records).unwrap();

    let changing_v2 = source_metadata(&changing, 2, 2);
    let appended = generation(31, vec![unchanged_metadata.clone(), changing_v2.clone()]);
    let append = measured_projection_work(&mut projection, |projection| {
        let receipt = projection
            .catch_up(
                &appended,
                records(
                    changing_v2,
                    vec![record(&changing, 1), record(&changing, 2)],
                ),
            )
            .unwrap();
        assert_eq!(receipt.event_count, unchanged_event_count + 2);
    });

    let deletion = generation(32, vec![unchanged_metadata]);
    let delete = measured_projection_work(&mut projection, |projection| {
        let receipt = projection.catch_up(&deletion, Vec::new()).unwrap();
        assert_eq!(receipt.event_count, unchanged_event_count);
    });
    (append, delete)
}

#[test]
fn full_initial_projection_uses_only_intentional_core_metadata() {
    let (_temp, mut projection) = projection();
    let source = source("full");
    let metadata = source_metadata(&source, 1, 2);
    let generation = generation(1, vec![metadata.clone()]);
    let receipt = projection
        .rebuild(
            &generation,
            records(metadata, vec![record(&source, 1), record(&source, 2)]),
        )
        .unwrap();

    assert_eq!(receipt.core_generation_id, generation.generation_id);
    assert_eq!(
        receipt.relational_schema_version,
        RELATIONAL_PROJECTION_SCHEMA_VERSION
    );
    assert_eq!(
        receipt.materializer_revision,
        RELATIONAL_MATERIALIZER_REVISION
    );
    assert_eq!(
        (
            receipt.source_count,
            receipt.session_count,
            receipt.event_count
        ),
        (1, 1, 2)
    );
    assert_eq!(
        query_rows(
            &projection,
            "SELECT provider, source_format, health, indexed_event_count FROM ctx_sources"
        )[0],
        vec![
            RawSqlValue::Text {
                value: "codex".to_owned(),
                bytes: 5,
                truncated: false,
            },
            RawSqlValue::Text {
                value: "codex_session_jsonl".to_owned(),
                bytes: 19,
                truncated: false,
            },
            RawSqlValue::Text {
                value: "ready".to_owned(),
                bytes: 5,
                truncated: false,
            },
            RawSqlValue::Integer(2),
        ]
    );
    assert_eq!(
        view_columns(&projection, "ctx_events"),
        [
            "ctx_event_id",
            "ctx_session_id",
            "source_id",
            "provider",
            "source_format",
            "provider_session_id",
            "native_event_id_json",
            "event_seq",
            "event_type",
            "role",
            "occurred_at_ms",
            "parser_revision",
            "normalization_revision",
            "content_policy_revision",
            "content_policy_status",
            "branch",
            "workspace",
            "cwd",
        ]
    );
    assert_eq!(
        view_columns(&projection, "ctx_sessions"),
        [
            "ctx_session_id",
            "parent_ctx_session_id",
            "root_ctx_session_id",
            "source_id",
            "provider",
            "source_format",
            "provider_session_id",
            "agent_type",
            "is_primary",
            "branch",
            "workspace",
            "cwd",
            "started_at_ms",
            "ended_at_ms",
            "health",
        ]
    );
    assert_eq!(
        view_columns(&projection, "ctx_sources"),
        [
            "source_id",
            "provider",
            "source_format",
            "schema_variant",
            "provider_identity_version",
            "parser_revision",
            "indexed_event_count",
            "health",
        ]
    );
    assert_eq!(
        view_columns(&projection, "ctx_files_touched"),
        [
            "ctx_file_touch_id",
            "ctx_event_id",
            "ctx_session_id",
            "source_id",
            "provider",
            "source_format",
            "repository_binding_id",
            "logical_repository_id",
            "path",
            "old_path",
            "observation_kind",
            "observed_at_ms",
        ]
    );
}

#[test]
fn every_public_view_keeps_the_exact_v9_column_contract() {
    let (_temp, projection) = projection();
    let contracts: [(&str, &[&str]); 8] = [
        (
            "ctx_sessions",
            &[
                "ctx_session_id",
                "parent_ctx_session_id",
                "root_ctx_session_id",
                "source_id",
                "provider",
                "source_format",
                "provider_session_id",
                "agent_type",
                "is_primary",
                "branch",
                "workspace",
                "cwd",
                "started_at_ms",
                "ended_at_ms",
                "health",
            ],
        ),
        (
            "ctx_events",
            &[
                "ctx_event_id",
                "ctx_session_id",
                "source_id",
                "provider",
                "source_format",
                "provider_session_id",
                "native_event_id_json",
                "event_seq",
                "event_type",
                "role",
                "occurred_at_ms",
                "parser_revision",
                "normalization_revision",
                "content_policy_revision",
                "content_policy_status",
                "branch",
                "workspace",
                "cwd",
            ],
        ),
        (
            "ctx_files_touched",
            &[
                "ctx_file_touch_id",
                "ctx_event_id",
                "ctx_session_id",
                "source_id",
                "provider",
                "source_format",
                "repository_binding_id",
                "logical_repository_id",
                "path",
                "old_path",
                "observation_kind",
                "observed_at_ms",
            ],
        ),
        (
            "ctx_sources",
            &[
                "source_id",
                "provider",
                "source_format",
                "schema_variant",
                "provider_identity_version",
                "parser_revision",
                "indexed_event_count",
                "health",
            ],
        ),
        (
            "ctx_repositories",
            &[
                "ctx_event_id",
                "ctx_session_id",
                "repository_binding_id",
                "logical_repository_id",
                "checkout_id",
                "worktree_id",
                "git_object_format",
                "association_policy_revision",
            ],
        ),
        (
            "ctx_vcs_observations",
            &[
                "ctx_event_id",
                "ctx_session_id",
                "repository_binding_id",
                "logical_repository_id",
                "observation_kind",
                "object_format",
                "object_id",
                "reference_name",
                "relative_path",
                "outcome_json",
                "observed_at_ms",
            ],
        ),
        (
            "ctx_repository_abstentions",
            &[
                "ctx_event_id",
                "ctx_session_id",
                "evidence_kind",
                "reason",
                "association_policy_revision",
            ],
        ),
        (
            "ctx_projection_metadata",
            &[
                "schema_version",
                "contract_version",
                "materializer_revision",
                "build_generation",
                "core_generation_id",
                "target_core_generation_id",
                "status",
                "source_count",
                "session_count",
                "event_count",
                "repository_binding_count",
                "file_touch_count",
                "vcs_observation_count",
                "last_error",
                "core_manifest_version",
                "core_record_version",
                "core_record_contract_fingerprint",
                "core_lexical_schema_version",
                "core_policy_schema_hash",
            ],
        ),
    ];
    for (view, expected) in contracts {
        assert_eq!(
            view_columns(&projection, view),
            expected
                .iter()
                .map(|column| (*column).to_owned())
                .collect::<Vec<_>>(),
            "public view contract changed for {view}"
        );
    }
}

#[test]
fn exact_generation_noop_does_not_poll_the_record_stream_or_write() {
    let (_temp, mut projection) = projection();
    let source = source("noop");
    let metadata = source_metadata(&source, 1, 1);
    let generation = generation(2, vec![metadata.clone()]);
    let first = projection
        .rebuild(&generation, records(metadata, vec![record(&source, 1)]))
        .unwrap();
    let never = std::iter::from_fn(|| -> Option<Result<RelationalProjectionRecord>> {
        panic!("an exact no-op must not poll its Core record stream")
    });

    let second = projection.catch_up_stream(&generation, never).unwrap();

    assert_eq!(second, first);
    assert_eq!(projection.metadata().unwrap().build_generation, 1);
}

#[test]
fn exact_generation_with_mismatched_core_identity_metadata_is_not_a_noop() {
    let (_temp, mut projection) = projection();
    let source = source("identity-mismatch");
    let metadata = source_metadata(&source, 1, 1);
    let generation = generation(20, vec![metadata.clone()]);
    projection
        .rebuild(&generation, records(metadata, vec![record(&source, 1)]))
        .unwrap();
    projection
        .conn
        .execute(
            "UPDATE core_relational_state SET active_policy_schema_hash = 'tampered'",
            [],
        )
        .unwrap();

    assert_eq!(
        projection.plan_generation(&generation).unwrap(),
        RelationalProjectionPlan::CatchUp {
            changed_source_ids: Default::default(),
        }
    );
}

#[test]
fn event_replacement_and_source_deletion_are_atomic_and_deterministic() {
    let (_temp, mut projection) = projection();
    let retained = source("retained");
    let deleted = source("deleted");
    let retained_v1 = source_metadata(&retained, 1, 1);
    let deleted_v1 = source_metadata(&deleted, 1, 1);
    let initial = generation(3, vec![retained_v1.clone(), deleted_v1.clone()]);
    let mut initial_records = records(retained_v1, vec![record(&retained, 1)]);
    initial_records.extend(records(deleted_v1, vec![record(&deleted, 1)]));
    projection.rebuild(&initial, initial_records).unwrap();

    let retained_v2 = source_metadata(&retained, 2, 1);
    let replacement = generation(4, vec![retained_v2.clone()]);
    projection
        .catch_up(
            &replacement,
            records(retained_v2, vec![record(&retained, 2)]),
        )
        .unwrap();

    assert_eq!(
        query_rows(&projection, "SELECT event_seq FROM ctx_events"),
        vec![vec![text_value("00000000000000000002")]]
    );
    assert_eq!(
        query_rows(&projection, "SELECT COUNT(*) FROM ctx_sources")[0][0],
        RawSqlValue::Integer(1)
    );
}

#[test]
fn deleting_cross_source_parent_retains_external_lineage_without_fabricating_rows() {
    let (_temp, mut projection) = projection();
    let parent_source = source("lineage-parent");
    let child_source = source("lineage-child");
    let parent_record = record(&parent_source, 1);
    let parent_session_id = parent_record.session_id;
    let mut child_record = record(&child_source, 1);
    child_record.parent_session_id = Some(parent_session_id);
    child_record.root_session_id = parent_session_id;
    child_record.validate_contract().unwrap();

    let parent_metadata = source_metadata(&parent_source, 1, 1);
    let child_metadata = source_metadata(&child_source, 1, 1);
    let initial = generation(22, vec![parent_metadata.clone(), child_metadata.clone()]);
    let mut initial_records = records(parent_metadata, vec![parent_record]);
    initial_records.extend(records(child_metadata.clone(), vec![child_record]));
    projection.rebuild(&initial, initial_records).unwrap();

    let deletion = generation(23, vec![child_metadata]);
    let receipt = projection.catch_up(&deletion, Vec::new()).unwrap();

    assert_eq!(
        (
            receipt.source_count,
            receipt.session_count,
            receipt.event_count
        ),
        (1, 1, 1)
    );
    let lineage_id = parent_session_id.as_uuid().to_string();
    assert_eq!(
        query_rows(
            &projection,
            "SELECT parent_ctx_session_id, root_ctx_session_id FROM ctx_sessions"
        ),
        vec![vec![text_value(&lineage_id), text_value(&lineage_id)]]
    );
    assert_eq!(
        query_rows(
            &projection,
            "SELECT (SELECT COUNT(*) FROM core_sessions),
                    (SELECT COUNT(*) FROM core_events)"
        ),
        vec![vec![RawSqlValue::Integer(1), RawSqlValue::Integer(1)]]
    );
}

#[test]
fn crush_high_bit_event_sequences_round_trip_and_keep_unsigned_order() {
    let (_temp, mut projection) = projection();
    let source = source_for("crush", "crush_sqlite", "high-bit-sequence");
    let metadata = source_metadata(&source, 1, 3);
    let generation = generation(21, vec![metadata.clone()]);
    // Crush uses FNV-1a over the native message ID. `message-a` hashes to this
    // valid high-bit u64 value.
    let crush_sequence = 17_325_798_824_308_570_846_u64;
    let mut high = record(&source, crush_sequence);
    high.branch = Some("high-first".to_owned());
    let mut maximum = record(&source, u64::MAX);
    maximum.branch = Some("maximum".to_owned());
    let mut low = record(&source, 7);
    low.branch = Some("low-first".to_owned());

    projection
        .rebuild(&generation, records(metadata, vec![high, maximum, low]))
        .unwrap();

    assert_eq!(
        query_rows(
            &projection,
            "SELECT event_seq FROM ctx_events ORDER BY event_seq"
        ),
        vec![
            vec![text_value("00000000000000000007")],
            vec![text_value("17325798824308570846")],
            vec![text_value("18446744073709551615")],
        ]
    );
    assert_eq!(
        query_rows(&projection, "SELECT branch FROM ctx_sessions"),
        vec![vec![text_value("low-first")]]
    );
}

#[test]
fn old_only_relational_schema_is_missing_and_requires_disposable_rebuild() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("relational.sqlite");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE source_backed_relational_state (
             singleton INTEGER PRIMARY KEY,
             schema_version INTEGER NOT NULL,
             contract_version INTEGER NOT NULL
         );
         INSERT INTO source_backed_relational_state
             (singleton, schema_version, contract_version) VALUES (1, 5, 5);",
    )
    .unwrap();
    drop(conn);

    let error = match SourceBackedRelationalProjection::open_read_only(&path) {
        Ok(_) => panic!("old-only schema must require a disposable rebuild"),
        Err(error) => error,
    };
    assert!(matches!(error, RelationalProjectionError::MissingSchema));
}

#[test]
fn missing_relational_state_is_classified_for_disposable_rebuild() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("relational.sqlite");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE pre_cutover_events (event_id TEXT PRIMARY KEY);")
        .unwrap();
    drop(conn);

    let error = match SourceBackedRelationalProjection::open_read_only(&path) {
        Ok(_) => panic!("missing relational state must require a disposable rebuild"),
        Err(error) => error,
    };
    assert!(matches!(error, RelationalProjectionError::MissingSchema));
}

#[test]
fn repository_file_and_vcs_rows_cannot_cross_repository_bindings() {
    let (_temp, mut projection) = projection();
    let source = source("repositories");
    let metadata = source_metadata(&source, 1, 1);
    let generation = generation(5, vec![metadata.clone()]);
    let mut event = record(&source, 1);
    event.repository_bindings = vec![
        repository_binding("repo-a", "alpha"),
        repository_binding("repo-b", "beta"),
    ];
    event.repository_file_observations = vec![
        RepositoryFileObservation {
            repository_binding_id: "repo-a".to_owned(),
            relative_path: "src/a.rs".to_owned(),
            kind: RepositoryFileObservationKind::Modified,
            prior_relative_path: None,
        },
        RepositoryFileObservation {
            repository_binding_id: "repo-b".to_owned(),
            relative_path: "src/b.rs".to_owned(),
            kind: RepositoryFileObservationKind::Created,
            prior_relative_path: None,
        },
    ];
    event.repository_vcs_observations = vec![RepositoryVcsObservation {
        repository_binding_id: "repo-b".to_owned(),
        kind: RepositoryVcsObservationKind::Commit,
        object_id: Some(GitObjectId {
            format: GitObjectFormat::Sha1,
            hex: "a".repeat(40),
        }),
        parent_object_ids: vec![GitObjectId {
            format: GitObjectFormat::Sha1,
            hex: "b".repeat(40),
        }],
        reference: Some("refs/heads/main".to_owned()),
        relative_path: None,
    }];

    projection
        .rebuild(&generation, records(metadata, vec![event]))
        .unwrap();

    assert_eq!(
        query_rows(
            &projection,
            "SELECT logical_repository_id, path FROM ctx_files_touched ORDER BY path"
        ),
        vec![
            vec![text_value("alpha"), text_value("src/a.rs")],
            vec![text_value("beta"), text_value("src/b.rs")],
        ]
    );
    assert_eq!(
        query_rows(
            &projection,
            "SELECT logical_repository_id, object_id FROM ctx_vcs_observations"
        ),
        vec![vec![text_value("beta"), text_value(&"a".repeat(40))]]
    );
}

#[test]
fn structured_repository_outcomes_are_projected_without_losing_the_payload() {
    let (_temp, mut projection) = projection();
    let source = source("repository-outcome");
    let metadata = source_metadata(&source, 1, 1);
    let generation = generation(6, vec![metadata.clone()]);
    let mut event = record(&source, 1);
    event.repository_bindings = vec![repository_binding("repo", "ctx")];
    let outcome = RepositoryOutcomeObservation {
        kind: RepositoryOutcomeKind::Commit,
        produced_object_ids: vec![GitObjectId {
            format: GitObjectFormat::Sha1,
            hex: "a".repeat(40),
        }],
        replacement_lineage: Vec::new(),
        pull_request: None,
        observed_at_unix_ms: 1_700_000_000_001,
        linkage: RepositoryOutcomeLinkage {
            provider: "codex".to_owned(),
            origin_call_id: "call-1".to_owned(),
            result_call_id: "result-1".to_owned(),
            origin_event_sequence: 1,
            continuation_call_id_sha256: Vec::new(),
            result_record_sha256: [7; 32],
        },
        outcome_capture_revision: CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
    };
    event.repository_vcs_observations = vec![RepositoryVcsObservation {
        repository_binding_id: "repo".to_owned(),
        kind: RepositoryVcsObservationKind::Outcome(Box::new(outcome.clone())),
        object_id: None,
        parent_object_ids: Vec::new(),
        reference: None,
        relative_path: None,
    }];

    projection
        .rebuild(&generation, records(metadata, vec![event]))
        .unwrap();

    assert_eq!(
        query_rows(
            &projection,
            "SELECT observation_kind, outcome_json, object_id FROM ctx_vcs_observations"
        ),
        vec![vec![
            text_value("outcome"),
            text_value(&serde_json::to_string(&outcome).unwrap()),
            RawSqlValue::Null,
        ]]
    );
}

#[test]
fn repeated_repository_descriptors_are_shared_without_changing_public_cardinality() {
    let (_temp, mut projection) = projection();
    let source = source("shared-repository-descriptor");
    let metadata = source_metadata(&source, 1, 64);
    let generation = generation(50, vec![metadata.clone()]);
    let receipt = projection
        .rebuild(
            &generation,
            records(
                metadata,
                (1..=64)
                    .map(|sequence| repository_record(&source, sequence))
                    .collect(),
            ),
        )
        .unwrap();

    assert_eq!(
        (
            receipt.repository_binding_count,
            receipt.file_touch_count,
            receipt.vcs_observation_count,
        ),
        (64, 64, 64)
    );
    assert_eq!(
        query_rows(
            &projection,
            "SELECT
                (SELECT COUNT(*) FROM core_repository_bindings),
                (SELECT COUNT(*) FROM core_event_repositories),
                (SELECT COUNT(*) FROM core_repository_aliases),
                (SELECT COUNT(*) FROM core_repository_evidence),
                (SELECT COUNT(*) FROM ctx_repositories)"
        ),
        vec![vec![
            RawSqlValue::Integer(1),
            RawSqlValue::Integer(64),
            RawSqlValue::Integer(1),
            RawSqlValue::Integer(1),
            RawSqlValue::Integer(64),
        ]]
    );
}

#[test]
fn integer_key_foreign_keys_cascade_every_event_owned_row() {
    let (_temp, mut projection) = projection();
    let source = source("cascade");
    let metadata = source_metadata(&source, 1, 1);
    let generation = generation(51, vec![metadata.clone()]);
    projection
        .rebuild(
            &generation,
            records(metadata, vec![repository_record(&source, 1)]),
        )
        .unwrap();

    projection
        .conn
        .execute(
            "DELETE FROM core_sources WHERE source_id = ?1",
            [source.identity().as_uuid().to_string()],
        )
        .unwrap();
    assert_eq!(
        query_rows(
            &projection,
            "SELECT
                (SELECT COUNT(*) FROM core_sessions),
                (SELECT COUNT(*) FROM core_events),
                (SELECT COUNT(*) FROM core_event_repositories),
                (SELECT COUNT(*) FROM core_file_observations),
                (SELECT COUNT(*) FROM core_vcs_observations),
                (SELECT COUNT(*) FROM core_vcs_parent_objects),
                (SELECT COUNT(*) FROM pragma_foreign_key_check)"
        ),
        vec![vec![
            RawSqlValue::Integer(0),
            RawSqlValue::Integer(0),
            RawSqlValue::Integer(0),
            RawSqlValue::Integer(0),
            RawSqlValue::Integer(0),
            RawSqlValue::Integer(0),
            RawSqlValue::Integer(0),
        ]]
    );
    materialization::prune_orphan_repository_bindings(&projection.conn).unwrap();
    assert_eq!(
        query_rows(&projection, "SELECT COUNT(*) FROM core_repository_bindings")[0][0],
        RawSqlValue::Integer(0)
    );
}

#[test]
fn full_digest_validation_rejects_a_uuid_collision() {
    let (_temp, mut projection) = projection();
    let source = source("collision");
    let metadata = source_metadata(&source, 1, 1);
    let generation = generation(52, vec![metadata.clone()]);
    let event = record(&source, 1);
    projection
        .rebuild(&generation, records(metadata, vec![event.clone()]))
        .unwrap();

    let mut colliding_digest = event.event_id.digest();
    colliding_digest[31] ^= 0xff;
    let error = projection
        .conn
        .execute(
            "INSERT INTO core_events (
                 ctx_event_id, event_digest, session_key, event_seq, event_type,
                 normalization_revision, content_policy_revision, content_policy_status
             ) SELECT ?1, ?2, session_key, '00000000000000000002', 'message',
                      normalization_revision, content_policy_revision, content_policy_status
               FROM core_events WHERE ctx_event_id = ?1",
            rusqlite::params![
                event.event_id.as_uuid().to_string(),
                colliding_digest.as_slice(),
            ],
        )
        .unwrap_err();
    assert!(matches!(error, rusqlite::Error::SqliteFailure(_, _)));
    let stored_digest: Vec<u8> = projection
        .conn
        .query_row(
            "SELECT event_digest FROM core_events WHERE ctx_event_id = ?1",
            [event.event_id.as_uuid().to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_digest, event.event_id.digest());
}

#[test]
fn compact_internal_tables_drop_derivable_columns_and_use_required_indexes() {
    let (_temp, projection) = projection();
    for removed in [
        "source_id",
        "ctx_session_id",
        "session_identity",
        "parser_revision",
    ] {
        assert!(
            !view_columns(&projection, "core_events").contains(&removed.to_owned()),
            "core_events retained derivable column {removed}"
        );
    }
    for removed in ["ctx_event_id", "source_id", "ctx_session_id", "binding_id"] {
        assert!(
            !view_columns(&projection, "core_file_observations").contains(&removed.to_owned()),
            "core_file_observations retained derivable column {removed}"
        );
    }
    let without_rowid: i64 = projection
        .conn
        .query_row(
            "SELECT COUNT(*)
             FROM sqlite_schema
             WHERE type = 'table'
               AND name IN (
                   'core_event_repositories', 'core_repository_aliases',
                   'core_repository_evidence', 'core_repository_abstentions',
                   'core_file_observations', 'core_vcs_observations',
                   'core_vcs_parent_objects'
               )
               AND sql LIKE '%WITHOUT ROWID%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(without_rowid, 7);

    let source_count_plan = query_plan(
        &projection.conn,
        "SELECT COUNT(*)
         FROM core_sessions AS session INDEXED BY core_sessions_source
         JOIN core_events AS event INDEXED BY core_events_session_seq
           ON event.session_key = session.session_key
         WHERE session.source_key = 1",
    )
    .join("\n");
    assert!(source_count_plan.contains("core_sessions_source"));
    assert!(source_count_plan.contains("core_events_session_seq"));

    let reverse_plan = query_plan(
        &projection.conn,
        "SELECT child.session_key
         FROM core_sessions AS child INDEXED BY core_sessions_parent
         WHERE child.parent_ctx_session_id = '00000000-0000-8000-8000-000000000000'",
    )
    .join("\n");
    assert!(reverse_plan.contains("core_sessions_parent"));

    let binding_plan = query_plan(
        &projection.conn,
        "SELECT event_key FROM core_event_repositories
         WHERE repository_binding_key = 1",
    )
    .join("\n");
    assert!(binding_plan.contains("core_event_repositories_binding"));
}

#[test]
fn incremental_validation_work_is_independent_of_unchanged_event_volume() {
    let (small_append, small_delete) = incremental_work_with_unchanged_events(8);
    let (large_append, large_delete) = incremental_work_with_unchanged_events(2_048);

    eprintln!(
        "v8 incremental work: small_append={small_append:?} large_append={large_append:?} \
         small_delete={small_delete:?} large_delete={large_delete:?}"
    );
    assert!(
        large_append.vm_steps <= small_append.vm_steps + 750,
        "append validation scaled with unchanged events: {small_append:?} -> {large_append:?}"
    );
    assert!(
        large_delete.vm_steps <= small_delete.vm_steps + 750,
        "delete validation scaled with unchanged events: {small_delete:?} -> {large_delete:?}"
    );
    for (operation, work) in [
        ("small append", small_append),
        ("large append", large_append),
        ("small delete", small_delete),
        ("large delete", large_delete),
    ] {
        assert!(
            work.page_cache_misses <= 512,
            "{operation} exceeded the source-scoped page budget: {work:?}"
        );
    }
}

#[test]
fn complete_body_and_structured_content_are_not_persisted() {
    let (temp, mut projection) = projection();
    let source = source("privacy");
    let metadata = source_metadata(&source, 1, 1);
    let generation = generation(6, vec![metadata.clone()]);
    projection
        .rebuild(&generation, records(metadata, vec![record(&source, 1)]))
        .unwrap();
    drop(projection);

    let path = temp.path().join("relational.sqlite");
    let mut bytes = std::fs::read(&path).unwrap();
    for suffix in ["-wal", "-shm"] {
        if let Ok(sidecar) = std::fs::read(format!("{}{suffix}", path.display())) {
            bytes.extend(sidecar);
        }
    }
    assert!(!contains(&bytes, BODY_SENTINEL));
    assert!(!contains(&bytes, STRUCTURED_SENTINEL));
}

#[test]
fn materializer_revision_mismatch_forces_a_full_deterministic_rebuild() {
    let (_temp, mut projection) = projection();
    let source = source("revision");
    let metadata = source_metadata(&source, 1, 1);
    let generation = generation(7, vec![metadata.clone()]);
    let source_records = records(metadata.clone(), vec![record(&source, 1)]);
    projection.rebuild(&generation, source_records).unwrap();
    projection
        .conn
        .execute(
            "UPDATE core_relational_state SET active_materializer_revision = 0",
            [],
        )
        .unwrap();
    assert_eq!(
        projection.plan_generation(&generation).unwrap(),
        RelationalProjectionPlan::Rebuild
    );

    let receipt = projection
        .catch_up(&generation, records(metadata, vec![record(&source, 1)]))
        .unwrap();

    assert_eq!(receipt.build_generation, 2);
    assert_eq!(
        receipt.materializer_revision,
        RELATIONAL_MATERIALIZER_REVISION
    );
    assert_eq!(
        query_rows(&projection, "SELECT COUNT(*) FROM ctx_events")[0][0],
        RawSqlValue::Integer(1)
    );
}

#[test]
fn failed_catch_up_keeps_last_coherent_generation_and_marks_explicit_lag() {
    let (_temp, mut projection) = projection();
    let source = source("failure");
    let metadata_v1 = source_metadata(&source, 1, 1);
    let initial = generation(8, vec![metadata_v1.clone()]);
    projection
        .rebuild(&initial, records(metadata_v1, vec![record(&source, 1)]))
        .unwrap();
    let metadata_v2 = source_metadata(&source, 2, 1);
    let target = generation(9, vec![metadata_v2.clone()]);
    let failed_stream = vec![
        Ok(RelationalProjectionRecord::BeginSource(Box::new(
            metadata_v2,
        ))),
        Err(RelationalProjectionError::InvalidRecord(
            "injected Core page failure".to_owned(),
        )),
    ];

    assert!(projection.catch_up_stream(&target, failed_stream).is_err());

    let metadata = projection.metadata().unwrap();
    assert_eq!(metadata.status, RelationalProjectionStatus::Behind);
    assert_eq!(
        metadata.active_core_generation_id.as_deref(),
        Some(initial.generation_id.as_str())
    );
    assert_eq!(
        metadata.target_core_generation_id.as_deref(),
        Some(target.generation_id.as_str())
    );
    assert_eq!(
        query_rows(&projection, "SELECT event_seq FROM ctx_events"),
        vec![vec![text_value("00000000000000000001")]]
    );
}

fn text_value(value: &str) -> RawSqlValue {
    RawSqlValue::Text {
        value: value.to_owned(),
        bytes: value.len(),
        truncated: false,
    }
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|candidate| candidate == needle.as_bytes())
}
