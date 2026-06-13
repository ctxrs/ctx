use super::fixtures::session_id;
use ctx_core::models::{
    WorkspaceActiveSnapshotClientMessage, WorkspaceActiveSnapshotSessionReplay,
};

#[test]
fn replay_deserializes_subscribe_message_with_explicit_replay_modes() {
    let message = serde_json::from_str::<WorkspaceActiveSnapshotClientMessage>(
        r#"{
            "type":"subscribe",
            "sessions":[
                {
                    "session_id":"00000000-0000-0000-0000-000000000001",
                    "intent":"head",
                    "replay":{"mode":"auto"}
                },
                {
                    "session_id":"00000000-0000-0000-0000-000000000002",
                    "replay":{"mode":"resume","after_seq":12}
                },
                {
                    "session_id":"00000000-0000-0000-0000-000000000003",
                    "replay":{"mode":"reset"}
                }
            ]
        }"#,
    )
    .unwrap();

    let WorkspaceActiveSnapshotClientMessage::Subscribe { sessions, .. } = message;
    assert_eq!(sessions.len(), 3);
    assert_eq!(
        sessions[0].intent,
        Some(ctx_core::models::WorkspaceActiveSnapshotSessionIntent::Head)
    );
    assert!(matches!(
        sessions[0].replay,
        WorkspaceActiveSnapshotSessionReplay::Auto
    ));
    assert!(matches!(
        sessions[1].replay,
        WorkspaceActiveSnapshotSessionReplay::Resume {
            after_seq: 12,
            after_projection_rev: 0,
        }
    ));
    assert!(matches!(
        sessions[2].replay,
        WorkspaceActiveSnapshotSessionReplay::Reset
    ));
}

#[test]
fn replay_deserialization_keeps_session_ids_and_explicit_sessions_distinct() {
    let message = serde_json::from_str::<WorkspaceActiveSnapshotClientMessage>(
        r#"{
            "type":"subscribe",
            "session_ids":[
                "00000000-0000-0000-0000-000000000001",
                "00000000-0000-0000-0000-000000000002"
            ],
            "sessions":[
                {
                    "session_id":"00000000-0000-0000-0000-000000000003",
                    "replay":{"mode":"reset"}
                }
            ]
        }"#,
    )
    .unwrap();

    let WorkspaceActiveSnapshotClientMessage::Subscribe {
        session_ids,
        sessions,
        ..
    } = message;
    assert_eq!(
        session_ids,
        vec![
            session_id("00000000-0000-0000-0000-000000000001"),
            session_id("00000000-0000-0000-0000-000000000002"),
        ]
    );
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].session_id,
        session_id("00000000-0000-0000-0000-000000000003")
    );
    assert!(matches!(
        sessions[0].replay,
        WorkspaceActiveSnapshotSessionReplay::Reset
    ));
}

#[test]
fn replay_deserialization_reads_foreground_session_id() {
    let message = serde_json::from_str::<WorkspaceActiveSnapshotClientMessage>(
        r#"{
            "type":"subscribe",
            "foreground_session_id":"00000000-0000-0000-0000-000000000001"
        }"#,
    )
    .unwrap();

    let WorkspaceActiveSnapshotClientMessage::Subscribe {
        foreground_session_id,
        ..
    } = message;
    assert_eq!(
        foreground_session_id,
        Some(session_id("00000000-0000-0000-0000-000000000001"))
    );
}
