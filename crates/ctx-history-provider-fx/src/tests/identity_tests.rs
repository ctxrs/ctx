use ctx_history_core::{CoreContentPolicyStatus, EventRole, EventType};
use serde_json::json;

use crate::{project_canonical_state, CanonicalState, ProjectionBinding, MAX_HISTORY_TURN_BYTES};

use super::support::{assistant_turn, canonical_state, source, SESSION};

fn state(history: Vec<serde_json::Value>) -> CanonicalState {
    serde_json::from_value(canonical_state(history, 20)).expect("canonical test state")
}

fn project(state: &CanonicalState) -> Vec<ctx_history_core::CoreRecord> {
    let source = source(7);
    project_canonical_state(
        ProjectionBinding {
            source: &source,
            native_session_id: SESSION,
        },
        state,
    )
    .expect("state projects")
}

fn summary(removed_turn_count: u64, compaction_count: u64) -> serde_json::Value {
    json!({
        "kind": "compacted_summary",
        "summary": "durable compacted context",
        "removed_turn_count": removed_turn_count,
        "compaction_count": compaction_count
    })
}

#[test]
fn cumulative_removed_count_preserves_surviving_turn_identity_across_compaction() {
    let turns = vec![
        assistant_turn("u0", "a0"),
        assistant_turn("u1", "a1"),
        assistant_turn("u2", "a2"),
        assistant_turn("u3", "a3"),
    ];
    let full = project(&state(turns.clone()));

    let once = project(&state(vec![
        summary(2, 1),
        turns[2].clone(),
        turns[3].clone(),
    ]));
    assert_eq!(once.len(), 5);
    assert_eq!(once[0].event_type, EventType::Summary.as_str());
    assert_eq!(once[1].event_id, full[4].event_id);
    assert_eq!(once[2].event_id, full[5].event_id);
    assert_eq!(once[3].event_id, full[6].event_id);
    assert_eq!(once[4].event_id, full[7].event_id);

    let twice = project(&state(vec![summary(3, 2), turns[3].clone()]));
    assert_eq!(twice[1].event_id, full[6].event_id);
    assert_eq!(twice[2].event_id, full[7].event_id);
    assert_ne!(once[0].event_id, twice[0].event_id);
}

#[test]
fn repeated_equal_turns_have_distinct_absolute_ordinal_identities() {
    let equal = assistant_turn("same user", "same assistant");
    let records = project(&state(vec![equal.clone(), equal]));
    assert_eq!(records.len(), 4);
    for (index, record) in records.iter().enumerate() {
        assert!(records[index + 1..]
            .iter()
            .all(|peer| peer.event_id != record.event_id));
    }
    assert_eq!(
        records
            .iter()
            .map(|record| record.event_sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(records[0].role.as_deref(), Some(EventRole::User.as_str()));
    assert_eq!(
        records[1].role.as_deref(),
        Some(EventRole::Assistant.as_str())
    );
}

#[test]
fn small_authentic_turn_is_searchable_and_keeps_structured_content() {
    let turn = json!({
        "kind": "assistant",
        "user": {"text": "searchable user", "images": []},
        "assistant": "searchable assistant",
        "execution": {
            "schema_version": 1,
            "tool_steps": [{
                "assistant": "tool preface",
                "tool_calls": [{
                    "id": "call-1",
                    "name": "shell",
                    "arguments_json": "{\"cmd\":\"pwd\"}",
                    "provider_result": null
                }],
                "tool_results": [{
                    "tool_call_id": "call-1",
                    "tool_name": "shell",
                    "status": "success",
                    "output": "complete tool output",
                    "output_handle": null,
                    "preview": null,
                    "output_bytes": 20,
                    "stored_output_bytes": 20,
                    "truncated": false,
                    "provider_native": false,
                    "created_at_ms": 2
                }]
            }],
            "files": []
        }
    });
    let records = project(&state(vec![turn]));
    assert_eq!(records.len(), 2);
    let user_body = records[0]
        .content
        .normalized_body
        .as_deref()
        .expect("body retained");
    let assistant_body = records[1]
        .content
        .normalized_body
        .as_deref()
        .expect("body retained");
    assert!(user_body.contains("searchable user"));
    assert!(!user_body.contains("searchable assistant"));
    assert!(assistant_body.contains("searchable assistant"));
    assert!(!assistant_body.contains("searchable user"));
    assert!(!assistant_body.contains("complete tool output"));
    assert!(records
        .iter()
        .all(|record| record.content.structured_content.is_some()));
    assert!(matches!(
        &records[1].content.policy_status,
        CoreContentPolicyStatus::Selected
    ));
}

#[test]
fn large_tool_output_does_not_hide_small_searchable_conversation_text() {
    let turn = json!({
        "kind": "assistant",
        "user": {"text": "findable user text", "images": []},
        "assistant": "findable assistant text",
        "execution": {
            "schema_version": 1,
            "tool_steps": [{
                "assistant": null,
                "tool_calls": [],
                "tool_results": [{
                    "tool_call_id": "call-1",
                    "tool_name": "shell",
                    "status": "success",
                    "output": "x".repeat(MAX_HISTORY_TURN_BYTES / 2),
                    "output_handle": null,
                    "preview": null,
                    "output_bytes": MAX_HISTORY_TURN_BYTES / 2,
                    "stored_output_bytes": MAX_HISTORY_TURN_BYTES / 2,
                    "truncated": false,
                    "provider_native": false,
                    "created_at_ms": 2
                }]
            }],
            "files": []
        }
    });
    let records = project(&state(vec![turn]));
    assert_eq!(records.len(), 2);
    let user_body = records[0]
        .content
        .normalized_body
        .as_deref()
        .expect("conversation text remains searchable");
    let assistant_body = records[1]
        .content
        .normalized_body
        .as_deref()
        .expect("conversation text remains searchable");
    assert!(user_body.contains("findable user text"));
    assert!(assistant_body.contains("findable assistant text"));
    assert!(!assistant_body.contains("xxxxxxxx"));
}

#[test]
fn near_limit_valid_turn_projects_without_becoming_source_fatal() {
    let fixed = serde_json::to_vec(&assistant_turn("", ""))
        .expect("sizing turn")
        .len();
    let payload = "x".repeat(MAX_HISTORY_TURN_BYTES - fixed - 1);
    let turn = assistant_turn(&payload, "");
    let encoded = serde_json::to_vec(&turn).expect("large turn encodes");
    assert!(encoded.len() <= MAX_HISTORY_TURN_BYTES);

    let records = project(&state(vec![turn]));
    assert_eq!(records.len(), 2);
    for record in records {
        assert!(matches!(
            &record.content.policy_status,
            CoreContentPolicyStatus::Selected | CoreContentPolicyStatus::Omitted { .. }
        ));
        record.validate_contract().expect("Core bounds hold");
    }
}
