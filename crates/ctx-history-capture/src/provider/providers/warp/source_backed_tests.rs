use std::fs;

use ctx_history_core::{LocatorRevisionPolicy, NativeRecordCoordinate, TypedKey};
use rusqlite::{params, Connection};
use tempfile::tempdir;

use super::{
    project_selected_warp_sources_v0, project_warp_source_backed_v0, resolve_warp_locator_v0,
    WarpSourceBackedErrorV0, WarpSourceSelectionV0,
};

fn field(number: u32, payload: &[u8]) -> Vec<u8> {
    let mut value = varint(u64::from(number) << 3 | 2);
    value.extend(varint(payload.len() as u64));
    value.extend_from_slice(payload);
    value
}

fn integer_field(number: u32, integer: u64) -> Vec<u8> {
    let mut value = varint(u64::from(number) << 3);
    value.extend(varint(integer));
    value
}

fn varint(mut value: u64) -> Vec<u8> {
    let mut output = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        output.push(byte);
        if value == 0 {
            return output;
        }
    }
}

fn text_task(task_id: &str, message_id: &str, body: &str) -> Vec<u8> {
    let mut timestamp = integer_field(1, 1_782_259_200);
    timestamp.extend(integer_field(2, 0));
    let text = field(2, &field(1, body.as_bytes()));
    let mut message = field(1, message_id.as_bytes());
    message.extend(text);
    message.extend(field(11, task_id.as_bytes()));
    message.extend(field(13, b"request-1"));
    message.extend(field(14, &timestamp));

    let mut task = field(1, task_id.as_bytes());
    task.extend(field(2, b"Task"));
    task.extend(field(5, &message));
    task
}

fn create_source(
    path: &std::path::Path,
    conversation_id: &str,
    task_id: &str,
    message_id: &str,
    body: &str,
) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "pragma user_version = 1;
             create table agent_conversations (
                 id integer primary key,
                 conversation_id text not null unique,
                 conversation_data text not null,
                 last_modified_at text not null
             );
             create table agent_tasks (
                 id integer primary key,
                 conversation_id text not null,
                 task_id text not null unique,
                 task blob not null,
                 last_modified_at text not null
             );
             create table ai_queries (
                 id integer primary key,
                 exchange_id text not null unique,
                 conversation_id text not null,
                 start_ts text not null,
                 input text not null,
                 working_directory text,
                 output_status text not null,
                 model_id text not null,
                 planning_model_id text not null default '',
                 coding_model_id text not null default ''
             );",
        )
        .unwrap();
    connection
        .execute(
            "insert into agent_conversations
             (conversation_id, conversation_data, last_modified_at)
             values (?1, '{\"agent_name\":\"Warp\"}', '2026-07-24 12:00:00')",
            [conversation_id],
        )
        .unwrap();
    connection
        .execute(
            "insert into agent_tasks
             (conversation_id, task_id, task, last_modified_at)
             values (?1, ?2, ?3, '2026-07-24 12:00:01')",
            params![
                conversation_id,
                task_id,
                text_task(task_id, message_id, body)
            ],
        )
        .unwrap();
}

#[test]
fn coexisting_selected_surfaces_project_cold_without_collapsing_lineage() {
    let directory = tempdir().unwrap();
    let gui_path = directory.path().join("gui-warp.sqlite");
    let tui_path = directory.path().join("tui-warp.sqlite");
    let long_body = "g".repeat(3_000);
    create_source(
        &gui_path,
        "same-conversation",
        "same-task",
        "same-message",
        &long_body,
    );
    create_source(
        &tui_path,
        "same-conversation",
        "same-task",
        "same-message",
        "tui body",
    );

    let snapshots = project_selected_warp_sources_v0(&[
        WarpSourceSelectionV0::new(&gui_path, "linux:stable:gui").unwrap(),
        WarpSourceSelectionV0::new(&tui_path, "linux:stable:tui").unwrap(),
    ])
    .unwrap();

    assert_eq!(snapshots.len(), 2);
    assert_ne!(snapshots[0].source, snapshots[1].source);
    assert_ne!(
        snapshots[0].documents[0].session_id,
        snapshots[1].documents[0].session_id
    );
    assert_ne!(
        snapshots[0].documents[0].event_id,
        snapshots[1].documents[0].event_id
    );
    assert_eq!(snapshots[0].documents[0].body.chars().count(), 2_048);
    assert_eq!(snapshots[1].documents[0].body, "tui body");
    for snapshot in &snapshots {
        assert_eq!(snapshot.documents.len(), 1);
        assert_eq!(snapshot.certified_source.counts().retained_records, 1);
        assert_eq!(snapshot.certified_source.counts().indexed_documents, 1);
        assert_eq!(
            snapshot.certified_source.observation().source(),
            &snapshot.source
        );
    }
}

#[test]
fn exact_task_message_locator_reopens_the_unbounded_provider_body() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let body = "exact Warp body ".repeat(300);
    create_source(&path, "conversation-1", "task-1", "message-1", &body);
    let selection = WarpSourceSelectionV0::new(&path, "linux:stable:gui").unwrap();
    let snapshot = project_warp_source_backed_v0(selection.clone()).unwrap();
    let document = &snapshot.documents[0];

    assert_eq!(
        document.locator.revision_policy(),
        LocatorRevisionPolicy::ExactSourceRevision
    );
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = document.locator.coordinate()
    else {
        panic!("Warp locator was not a typed SQLite coordinate");
    };
    assert_eq!(logical_relation, "agent_tasks.task-message.v1");
    assert_eq!(
        primary_key,
        &TypedKey::Composite(vec![TypedKey::I64(1), TypedKey::U64(0)])
    );
    assert_eq!(
        row_version,
        &Some(TypedKey::Bytes(document.locator.record_digest().to_vec()))
    );

    let hydrated = resolve_warp_locator_v0(&selection, &document.locator).unwrap();
    assert_eq!(String::from_utf8(hydrated.provider_bytes).unwrap(), body);
    assert_eq!(hydrated.event_type, "message");
    assert_eq!(hydrated.native_record_id, "message-1");
}

#[test]
fn unchanged_snapshot_repeats_stable_ids_and_certification() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    create_source(
        &path,
        "conversation-1",
        "task-1",
        "message-1",
        "unchanged body",
    );
    let selection = WarpSourceSelectionV0::new(&path, "linux:stable:gui").unwrap();

    let first = project_warp_source_backed_v0(selection.clone()).unwrap();
    let second = project_warp_source_backed_v0(selection).unwrap();

    assert_eq!(first.source, second.source);
    assert_eq!(first.certified_source, second.certified_source);
    assert_eq!(
        first.documents[0].session_id,
        second.documents[0].session_id
    );
    assert_eq!(first.documents[0].event_id, second.documents[0].event_id);
    assert_eq!(first.documents[0].locator, second.documents[0].locator);
}

#[test]
fn replacement_keeps_native_ids_but_invalidates_old_snapshot_evidence() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    create_source(
        &path,
        "conversation-1",
        "task-1",
        "message-1",
        "original body",
    );
    let selection = WarpSourceSelectionV0::new(&path, "linux:stable:gui").unwrap();
    let original = project_warp_source_backed_v0(selection.clone()).unwrap();
    let original_locator = original.documents[0].locator.clone();

    let replacement = directory.path().join("replacement.sqlite");
    create_source(
        &replacement,
        "conversation-1",
        "task-1",
        "message-1",
        "replacement body with a different byte length",
    );
    fs::rename(&replacement, &path).unwrap();
    let current = project_warp_source_backed_v0(selection.clone()).unwrap();

    assert_eq!(original.source, current.source);
    assert_eq!(
        original.documents[0].session_id,
        current.documents[0].session_id
    );
    assert_eq!(
        original.documents[0].event_id,
        current.documents[0].event_id
    );
    assert_ne!(
        original.certified_source.observation().revision(),
        current.certified_source.observation().revision()
    );
    assert_ne!(
        original.documents[0].locator.record_digest(),
        current.documents[0].locator.record_digest()
    );
    assert!(matches!(
        resolve_warp_locator_v0(&selection, &original_locator),
        Err(WarpSourceBackedErrorV0::StaleSourceRevision)
    ));
    let hydrated = resolve_warp_locator_v0(&selection, &current.documents[0].locator).unwrap();
    assert_eq!(
        String::from_utf8(hydrated.provider_bytes).unwrap(),
        "replacement body with a different byte length"
    );
}
