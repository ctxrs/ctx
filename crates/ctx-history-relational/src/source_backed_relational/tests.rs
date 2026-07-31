use std::collections::BTreeMap;

use ctx_history_core::{
    core_record_contract_fingerprint, derive_event_id, derive_session_id, CoreContent,
    CoreContentPolicyStatus, CoreRecord, EventIdentityInput, GitObjectFormat, GitObjectId,
    NativeItemKey, NativeSessionKey, RepositoryAlias, RepositoryAliasKind, RepositoryBinding,
    RepositoryCandidateEvidence, RepositoryEvidence, RepositoryEvidenceConfidence,
    RepositoryEvidenceKind, RepositoryFileObservation, RepositoryFileObservationKind,
    RepositoryVcsObservation, RepositoryVcsObservationKind, SessionIdentityInput, SourceAnchor,
    SourceKey, StableEntityId, TypedKey, CORE_CONTENT_POLICY_REVISION, CORE_NORMALIZATION_REVISION,
    CORE_RECORD_VERSION,
};
use tempfile::TempDir;

use super::*;

mod raw_sql;

const BODY_SENTINEL: &str = "complete-transcript-body-must-never-enter-relational";
const STRUCTURED_SENTINEL: &str = "structured-content-must-never-enter-relational";

fn source(name: &str) -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
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
        association_policy_revision: 1,
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
        manifest_version: 4,
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
        vec![vec![RawSqlValue::Integer(2)]]
    );
    assert_eq!(
        query_rows(&projection, "SELECT COUNT(*) FROM ctx_sources")[0][0],
        RawSqlValue::Integer(1)
    );
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
        vec![vec![RawSqlValue::Integer(1)]]
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
