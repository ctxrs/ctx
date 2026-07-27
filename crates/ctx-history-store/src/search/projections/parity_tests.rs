use std::collections::HashSet;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    Event, EventRole, EventType, Fidelity, SyncMetadata, SyncState, Visibility,
};
use uuid::Uuid;

use crate::Store;

use super::super::tests::tempdir;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectionRow {
    event_id: String,
    history_record_id: Option<String>,
    session_id: Option<String>,
    role: Option<String>,
    text: String,
    rank_bucket: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectionSnapshot {
    lexical: Vec<ProjectionRow>,
    scriptgram: Vec<ProjectionRow>,
    semantic_lookup: Vec<ProjectionRow>,
}

#[derive(Debug, Clone, Copy)]
enum ProjectionTable {
    Lexical,
    Scriptgram,
    SemanticLookup,
}

const SYNC_STATES: [SyncState; 5] = [
    SyncState::LocalOnly,
    SyncState::Pending,
    SyncState::Synced,
    SyncState::Failed,
    SyncState::Withheld,
];

struct VisibilityPolicyRow {
    visibility: Visibility,
    searchable_by_sync_state: [bool; 5],
}

const VISIBILITY_POLICY_MATRIX: [VisibilityPolicyRow; 5] = [
    VisibilityPolicyRow {
        visibility: Visibility::LocalOnly,
        searchable_by_sync_state: [true, true, true, true, false],
    },
    VisibilityPolicyRow {
        visibility: Visibility::Reportable,
        searchable_by_sync_state: [true, true, true, true, false],
    },
    VisibilityPolicyRow {
        visibility: Visibility::SyncMetadata,
        searchable_by_sync_state: [true, true, true, true, false],
    },
    VisibilityPolicyRow {
        visibility: Visibility::SyncFull,
        searchable_by_sync_state: [true, true, true, true, false],
    },
    VisibilityPolicyRow {
        visibility: Visibility::Withheld,
        searchable_by_sync_state: [false, false, false, false, false],
    },
];

#[derive(Debug)]
struct SearchPolicyCase {
    event_id: Uuid,
    visibility: Visibility,
    sync_state: SyncState,
    deleted: bool,
    searchable: bool,
}

impl ProjectionTable {
    fn sql(self) -> &'static str {
        match self {
            Self::Lexical => {
                "SELECT event_id, history_record_id, session_id, role, preview_text, rank_bucket
                 FROM event_search
                 ORDER BY event_id"
            }
            Self::Scriptgram => {
                "SELECT event_id, history_record_id, session_id, role, token_text, rank_bucket
                 FROM event_search_scriptgram
                 ORDER BY event_id"
            }
            Self::SemanticLookup => {
                "SELECT event_id, history_record_id, session_id, role, preview_text, rank_bucket
                 FROM event_search_lookup
                 ORDER BY event_id"
            }
        }
    }
}

fn projection_rows(store: &Store, table: ProjectionTable) -> Vec<ProjectionRow> {
    let mut statement = store.conn.prepare(table.sql()).unwrap();
    statement
        .query_map([], |row| {
            Ok(ProjectionRow {
                event_id: row.get(0)?,
                history_record_id: row.get(1)?,
                session_id: row.get(2)?,
                role: row.get(3)?,
                text: row.get(4)?,
                rank_bucket: row.get(5)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn projection_snapshot(store: &Store) -> ProjectionSnapshot {
    ProjectionSnapshot {
        lexical: projection_rows(store, ProjectionTable::Lexical),
        scriptgram: projection_rows(store, ProjectionTable::Scriptgram),
        semantic_lookup: projection_rows(store, ProjectionTable::SemanticLookup),
    }
}

fn assert_policy_projection(store: &Store, cases: &[SearchPolicyCase], path: &str) {
    let expected_ids = cases
        .iter()
        .filter(|case| case.searchable)
        .map(|case| case.event_id.to_string())
        .collect::<HashSet<_>>();
    for table in [
        ProjectionTable::Lexical,
        ProjectionTable::Scriptgram,
        ProjectionTable::SemanticLookup,
    ] {
        let actual_ids = projection_rows(store, table)
            .into_iter()
            .map(|row| row.event_id)
            .collect::<HashSet<_>>();
        for case in cases {
            assert_eq!(
                actual_ids.contains(&case.event_id.to_string()),
                case.searchable,
                "{path} {table:?}: visibility={:?}, sync_state={:?}, deleted={}",
                case.visibility,
                case.sync_state,
                case.deleted
            );
        }
        assert_eq!(actual_ids, expected_ids, "{path} {table:?}");
    }

    let event_ids = cases.iter().map(|case| case.event_id).collect::<Vec<_>>();
    let semantic_ids = store.semantic_eligible_event_ids(&event_ids).unwrap();
    let expected_semantic_ids = expected_ids
        .iter()
        .map(|event_id| Uuid::parse_str(event_id).unwrap())
        .collect::<HashSet<_>>();
    assert_eq!(semantic_ids, expected_semantic_ids, "{path} semantic IDs");
    assert_eq!(
        store.count_event_embedding_documents_exact().unwrap(),
        expected_ids.len(),
        "{path} semantic count"
    );
}

fn fixed_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-07-22T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn sync_metadata() -> SyncMetadata {
    SyncMetadata {
        visibility: Visibility::LocalOnly,
        fidelity: Fidelity::Imported,
        sync_state: SyncState::LocalOnly,
        sync_version: 0,
        deleted_at: None,
        metadata: serde_json::json!({}),
    }
}

fn event(
    id: &str,
    seq: u64,
    event_type: EventType,
    role: Option<EventRole>,
    payload: serde_json::Value,
) -> Event {
    Event {
        id: Uuid::parse_str(id).unwrap(),
        seq,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type,
        role,
        occurred_at: fixed_time(),
        capture_source_id: None,
        payload,
        payload_blob_id: None,
        dedupe_key: None,
        sync: sync_metadata(),
    }
}

fn row(id: &str, role: &str, text: &str, rank_bucket: &str) -> ProjectionRow {
    ProjectionRow {
        event_id: id.to_owned(),
        history_record_id: None,
        session_id: None,
        role: Some(role.to_owned()),
        text: text.to_owned(),
        rank_bucket: rank_bucket.to_owned(),
    }
}

#[test]
fn persisted_projection_goldens_match_incremental_and_rebuild_paths() {
    const USER_ID: &str = "018f45d0-0000-7000-8000-000000090001";
    const ASSISTANT_ID: &str = "018f45d0-0000-7000-8000-000000090002";
    const SYSTEM_ID: &str = "018f45d0-0000-7000-8000-000000090003";
    const SUMMARY_ID: &str = "018f45d0-0000-7000-8000-000000090004";
    const TOOL_CALL_ID: &str = "018f45d0-0000-7000-8000-000000090005";
    const FAILED_OUTPUT_ID: &str = "018f45d0-0000-7000-8000-000000090006";
    const SUCCESS_OUTPUT_ID: &str = "018f45d0-0000-7000-8000-000000090007";
    const CONTROL_ID: &str = "018f45d0-0000-7000-8000-000000090008";

    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let events = [
        event(
            USER_ID,
            1,
            EventType::Message,
            Some(EventRole::User),
            serde_json::json!({"text": "認証 alpha"}),
        ),
        event(
            ASSISTANT_ID,
            2,
            EventType::Message,
            Some(EventRole::Assistant),
            serde_json::json!({"text": "assistant beta"}),
        ),
        event(
            SYSTEM_ID,
            3,
            EventType::Message,
            Some(EventRole::System),
            serde_json::json!({"text": "system gamma"}),
        ),
        event(
            SUMMARY_ID,
            4,
            EventType::Summary,
            Some(EventRole::Assistant),
            serde_json::json!({"summary": "summary delta"}),
        ),
        event(
            TOOL_CALL_ID,
            5,
            EventType::ToolCall,
            Some(EventRole::Assistant),
            serde_json::json!({"command": "cargo test"}),
        ),
        event(
            FAILED_OUTPUT_ID,
            6,
            EventType::CommandOutput,
            Some(EventRole::Tool),
            serde_json::json!({"exit_code": 1, "output_preview": "failed epsilon"}),
        ),
        event(
            SUCCESS_OUTPUT_ID,
            7,
            EventType::CommandOutput,
            Some(EventRole::Tool),
            serde_json::json!({"exit_code": 0, "output_preview": "success zeta"}),
        ),
        event(
            CONTROL_ID,
            8,
            EventType::Message,
            Some(EventRole::User),
            serde_json::json!({
                "text": "<environment_context>fixture</environment_context>"
            }),
        ),
    ];
    for event in &events {
        store.upsert_event(event).unwrap();
    }

    let lexical_golden = vec![
        row(USER_ID, "user", "認証 alpha", "message"),
        row(ASSISTANT_ID, "assistant", "assistant beta", "message"),
        row(SYSTEM_ID, "system", "system gamma", "message"),
        row(SUMMARY_ID, "assistant", "summary delta", "summary"),
        row(TOOL_CALL_ID, "assistant", "cargo test", "tool_call"),
        row(FAILED_OUTPUT_ID, "tool", "failed epsilon", "command_output"),
        row(
            CONTROL_ID,
            "user",
            "<environment_context>fixture</environment_context>",
            "message",
        ),
    ];
    let scriptgram_golden = vec![row(USER_ID, "user", "認証 認証 alpha", "message")];
    let semantic_lookup_golden = vec![
        row(USER_ID, "user", "認証 alpha", "message"),
        row(ASSISTANT_ID, "assistant", "assistant beta", "message"),
        row(
            CONTROL_ID,
            "user",
            "<environment_context>fixture</environment_context>",
            "message",
        ),
    ];
    let incremental = projection_snapshot(&store);
    assert_eq!(incremental.lexical, lexical_golden);
    assert_eq!(incremental.scriptgram, scriptgram_golden);
    assert_eq!(incremental.semantic_lookup, semantic_lookup_golden);

    let eligible = store
        .semantic_eligible_event_ids(&[
            Uuid::parse_str(USER_ID).unwrap(),
            Uuid::parse_str(ASSISTANT_ID).unwrap(),
            Uuid::parse_str(CONTROL_ID).unwrap(),
        ])
        .unwrap();
    assert_eq!(eligible, HashSet::from([Uuid::parse_str(USER_ID).unwrap()]));
    assert_eq!(store.count_event_embedding_documents_exact().unwrap(), 1);

    store.refresh_search_index().unwrap();

    assert_eq!(projection_snapshot(&store), incremental);
    assert_eq!(store.count_event_embedding_documents_exact().unwrap(), 1);
}

#[test]
fn incremental_eligibility_exclusions_remain_characterized() {
    const DELETED_ID: &str = "018f45d0-0000-7000-8000-000000090101";
    const BLANK_ID: &str = "018f45d0-0000-7000-8000-000000090102";

    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut deleted = event(
        DELETED_ID,
        1,
        EventType::Message,
        Some(EventRole::User),
        serde_json::json!({"text": "deleted projection fixture"}),
    );
    deleted.sync.deleted_at = Some(fixed_time());
    let blank = event(
        BLANK_ID,
        2,
        EventType::Message,
        Some(EventRole::User),
        serde_json::json!({"text": "   "}),
    );

    store.upsert_event(&deleted).unwrap();
    store.upsert_event(&blank).unwrap();

    assert_eq!(
        projection_snapshot(&store),
        ProjectionSnapshot {
            lexical: Vec::new(),
            scriptgram: Vec::new(),
            semantic_lookup: Vec::new(),
        }
    );
    assert_eq!(store.count_event_embedding_documents_exact().unwrap(), 0);
}

#[test]
fn visibility_policy_matrix_matches_incremental_and_rebuild_paths() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    // Current tables reject newly written legacy withheld values. Ignoring the
    // CHECK constraints here lets the matrix prove rebuild behavior for rows
    // retained from schemas that allowed those values.
    store
        .conn
        .execute_batch("PRAGMA ignore_check_constraints = ON;")
        .unwrap();

    let mut cases = Vec::new();
    let mut seq = 1_u64;
    for visibility_row in VISIBILITY_POLICY_MATRIX {
        for (sync_index, sync_state) in SYNC_STATES.into_iter().enumerate() {
            for deleted in [false, true] {
                let event_id = Uuid::from_u128(
                    0x018f_45d0_0000_7000_8000_0000_0009_1000_u128 + u128::from(seq),
                );
                let mut candidate = event(
                    &event_id.to_string(),
                    seq,
                    EventType::Message,
                    Some(EventRole::User),
                    serde_json::json!({"text": format!("認証 policy matrix {seq}")}),
                );
                candidate.sync.visibility = visibility_row.visibility;
                candidate.sync.sync_state = sync_state;
                candidate.sync.deleted_at = deleted.then(fixed_time);
                store.upsert_event(&candidate).unwrap();

                cases.push(SearchPolicyCase {
                    event_id,
                    visibility: visibility_row.visibility,
                    sync_state,
                    deleted,
                    searchable: !deleted && visibility_row.searchable_by_sync_state[sync_index],
                });
                seq += 1;
            }
        }
    }

    assert_policy_projection(&store, &cases, "incremental");
    let incremental = projection_snapshot(&store);

    store.refresh_search_index().unwrap();

    assert_eq!(projection_snapshot(&store), incremental);
    assert_policy_projection(&store, &cases, "rebuild");
}
