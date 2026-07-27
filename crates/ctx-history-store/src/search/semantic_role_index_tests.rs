use ctx_history_core::{EventRole, EventType};
use rusqlite::params;
use uuid::Uuid;

use super::{fixed_time, insert_session, session_event, tempdir, with_occurred_at};
use crate::Store;

fn insert_run(store: &Store, run_id: Uuid, session_id: Uuid) {
    store
        .conn
        .execute(
            r#"
            INSERT INTO runs
            (id, session_id, run_type, status, started_at_ms, created_at_ms, updated_at_ms)
            VALUES (?1, ?2, 'agent_turn', 'succeeded', 1, 1, 1)
            "#,
            params![run_id.to_string(), session_id.to_string()],
        )
        .unwrap();
}

#[test]
fn semantic_partial_role_indexes_preserve_run_and_session_recall_and_order() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let run_session = Uuid::parse_str("018f45d0-0000-7000-8000-000000080020").unwrap();
    let session_only = Uuid::parse_str("018f45d0-0000-7000-8000-000000080021").unwrap();
    let run_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000080022").unwrap();
    insert_session(&store, run_session);
    insert_session(&store, session_only);
    insert_run(&store, run_id, run_session);

    let mut run_first_user = with_occurred_at(
        session_event(
            1,
            run_session,
            EventType::Message,
            Some(EventRole::User),
            "Run-scoped first prompt",
        ),
        0,
    );
    run_first_user.run_id = Some(run_id);
    let mut run_early_assistant = with_occurred_at(
        session_event(
            2,
            run_session,
            EventType::Message,
            Some(EventRole::Assistant),
            "Run-scoped early draft",
        ),
        1,
    );
    run_early_assistant.run_id = Some(run_id);
    let mut tool_call = with_occurred_at(
        session_event(
            3,
            run_session,
            EventType::ToolCall,
            Some(EventRole::Assistant),
            "non-message row between semantic messages",
        ),
        2,
    );
    tool_call.run_id = Some(run_id);
    let mut deleted_assistant = with_occurred_at(
        session_event(
            4,
            run_session,
            EventType::Message,
            Some(EventRole::Assistant),
            "deleted assistant must not be recalled",
        ),
        3,
    );
    deleted_assistant.run_id = Some(run_id);
    deleted_assistant.sync.deleted_at = Some(fixed_time());
    let mut run_final_assistant = with_occurred_at(
        session_event(
            5,
            run_session,
            EventType::Message,
            Some(EventRole::Assistant),
            "Run-scoped final answer",
        ),
        4,
    );
    run_final_assistant.run_id = Some(run_id);
    let mut run_second_user = with_occurred_at(
        session_event(
            6,
            run_session,
            EventType::Message,
            Some(EventRole::User),
            "Run-scoped second prompt",
        ),
        5,
    );
    run_second_user.run_id = Some(run_id);
    let mut run_second_assistant = with_occurred_at(
        session_event(
            7,
            run_session,
            EventType::Message,
            Some(EventRole::Assistant),
            "Run-scoped second answer",
        ),
        6,
    );
    run_second_assistant.run_id = Some(run_id);
    let session_user = with_occurred_at(
        session_event(
            8,
            session_only,
            EventType::Message,
            Some(EventRole::User),
            "Session-scoped prompt",
        ),
        7,
    );
    let session_assistant = with_occurred_at(
        session_event(
            9,
            session_only,
            EventType::Message,
            Some(EventRole::Assistant),
            "Session-scoped answer",
        ),
        8,
    );

    for event in [
        &run_first_user,
        &run_early_assistant,
        &tool_call,
        &deleted_assistant,
        &run_final_assistant,
        &run_second_user,
        &run_second_assistant,
        &session_user,
        &session_assistant,
    ] {
        store.upsert_event(event).unwrap();
    }

    let docs = store.recent_event_embedding_documents(None, 10).unwrap();
    assert_eq!(
        docs.iter().map(|doc| doc.event_id).collect::<Vec<_>>(),
        vec![session_user.id, run_second_user.id, run_first_user.id]
    );
    let first_run_turn = docs
        .iter()
        .find(|doc| doc.event_id == run_first_user.id)
        .unwrap();
    assert!(first_run_turn.text.contains("Run-scoped final answer"));
    assert!(!first_run_turn.text.contains("Run-scoped early draft"));
    assert!(!first_run_turn.text.contains("deleted assistant"));
    assert!(!first_run_turn.text.contains("non-message row"));

    assert_eq!(store.count_event_embedding_documents_exact().unwrap(), 3);
}
