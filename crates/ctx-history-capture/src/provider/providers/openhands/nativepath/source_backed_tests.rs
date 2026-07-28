use std::{fs, path::Path};

use ctx_history_core::{HydrationFailureKind, NativeRecordCoordinate, TypedKey};
use serde_json::{json, Value};

use super::source_backed::{
    project_openhands_source_backed_v1, OpenHandsLocatorResolverV1, OpenHandsSourceBackedErrorV1,
};

#[test]
fn source_backed_cold_projection_is_stable_and_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    write_event(
        &root,
        "conversation-cold",
        "event-a.json",
        message("event-a", &"x".repeat(8_000)),
    );
    write_event(
        &root,
        "conversation-cold",
        "event-b.json",
        successful_output("event-b", "private successful output"),
    );
    fs::create_dir_all(root.join("v1_conversations").join("conversation-cold")).unwrap();
    fs::write(
        root.join("v1_conversations")
            .join("conversation-cold")
            .join("malformed.json"),
        b"{not-json",
    )
    .unwrap();

    let first = project_openhands_source_backed_v1(&root).unwrap();
    let second = project_openhands_source_backed_v1(&root).unwrap();

    assert_eq!(first.inventory().observed_sources(), 1);
    assert_eq!(first.sources().len(), 1);
    assert_eq!(first.sources()[0].counts().complete_records, 3);
    assert_eq!(first.sources()[0].counts().retained_records, 1);
    assert_eq!(first.sources()[0].counts().ignored_records, 1);
    assert_eq!(first.sources()[0].counts().rejected_records, 1);
    assert_eq!(first.documents().len(), 1);
    assert!(first.documents()[0].body.chars().count() <= 2_048);
    assert!(!first.documents()[0]
        .body
        .contains("private successful output"));
    assert_eq!(first.rejections().len(), 1);

    assert_eq!(
        first.sources()[0].observation().source().identity(),
        second.sources()[0].observation().source().identity()
    );
    assert_eq!(
        first.sources()[0].content_digest(),
        second.sources()[0].content_digest()
    );
    assert_eq!(
        first.documents()[0].event_id,
        second.documents()[0].event_id
    );
    assert_eq!(
        first.documents()[0].session_id,
        second.documents()[0].session_id
    );
}

#[test]
fn new_event_preserves_existing_ids_and_exact_leaf_hydration() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    write_event(
        &root,
        "conversation-new",
        "event-a.json",
        message("event-a", "first exact body"),
    );

    let cold = project_openhands_source_backed_v1(&root).unwrap();
    let old_event_id = cold.documents()[0].event_id;
    let old_session_id = cold.documents()[0].session_id;
    let old_source_id = cold.sources()[0].observation().source().identity();
    let old_locator = cold.documents()[0].locator.clone();

    write_event(
        &root,
        "conversation-new",
        "event-b.json",
        message("event-b", "second exact body"),
    );
    let appended = project_openhands_source_backed_v1(&root).unwrap();
    assert_eq!(appended.documents().len(), 2);
    let old_after = appended
        .documents()
        .iter()
        .find(|document| document.event_id == old_event_id)
        .unwrap();
    assert_eq!(old_after.session_id, old_session_id);
    assert_eq!(
        appended.sources()[0].observation().source().identity(),
        old_source_id
    );

    let resolver = OpenHandsLocatorResolverV1::discover(&root).unwrap();
    let hydrated = resolver.hydrate(&old_locator).unwrap();
    assert_eq!(hydrated.decoded_display_text, "first exact body");
}

#[test]
fn replacement_keeps_native_ids_but_invalidates_old_leaf_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    let path = write_event(
        &root,
        "conversation-replace",
        "event.json",
        message("stable-event-id", "before replacement"),
    );
    let before = project_openhands_source_backed_v1(&root).unwrap();
    let before_id = before.documents()[0].event_id;
    let before_session = before.documents()[0].session_id;
    let before_locator = before.documents()[0].locator.clone();

    fs::write(
        &path,
        serde_json::to_vec(&message("stable-event-id", "after replacement")).unwrap(),
    )
    .unwrap();
    let after = project_openhands_source_backed_v1(&root).unwrap();
    assert_eq!(after.documents()[0].event_id, before_id);
    assert_eq!(after.documents()[0].session_id, before_session);
    assert_eq!(after.documents()[0].body, "after replacement");
    assert_ne!(
        after.sources()[0].content_digest(),
        before.sources()[0].content_digest()
    );

    let resolver = OpenHandsLocatorResolverV1::discover(&root).unwrap();
    assert!(matches!(
        resolver.hydrate(&before_locator),
        Err(OpenHandsSourceBackedErrorV1::LeafRevisionMismatch)
    ));
    let hydrated = resolver.hydrate(&after.documents()[0].locator).unwrap();
    assert_eq!(hydrated.decoded_display_text, "after replacement");
}

#[test]
fn exact_locator_uses_relative_leaf_and_native_object_coordinate() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    let value = message("event-coordinate", "exact provider bytes");
    let bytes = serde_json::to_vec(&value).unwrap();
    let path = root
        .join("v1_conversations")
        .join("conversation-exact")
        .join("events")
        .join("leaf.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, &bytes).unwrap();

    let projection = project_openhands_source_backed_v1(&root).unwrap();
    let document = &projection.documents()[0];
    let NativeRecordCoordinate::TreeRecord {
        relative_file_key,
        record_coordinate,
    } = document.locator.coordinate()
    else {
        panic!("expected OpenHands tree locator");
    };
    assert_eq!(
        relative_file_key,
        &TypedKey::Utf8("events/leaf.json".to_owned())
    );
    let TypedKey::Composite(parts) = record_coordinate else {
        panic!("expected object coordinate");
    };
    assert_eq!(
        parts.get(1),
        Some(&TypedKey::Utf8("event-coordinate".to_owned()))
    );
    assert!(matches!(parts.get(2), Some(TypedKey::Bytes(bytes)) if bytes.len() == 32));

    let resolver = OpenHandsLocatorResolverV1::discover(&root).unwrap();
    let hydrated = resolver.hydrate(&document.locator).unwrap();
    assert_eq!(hydrated.provider_bytes, bytes);
    assert_eq!(hydrated.decoded_display_text, "exact provider bytes");
}

#[test]
fn current_cli_conversation_events_remain_detected_but_unsupported() {
    let temp = tempfile::tempdir().unwrap();
    let conversation = temp.path().join("conversations").join("current-cli");
    let event = conversation.join("events").join("event-1.json");
    fs::create_dir_all(event.parent().unwrap()).unwrap();
    fs::write(&event, b"{}").unwrap();

    assert!(matches!(
        project_openhands_source_backed_v1(&conversation),
        Err(OpenHandsSourceBackedErrorV1::UnsupportedCurrentCliFormat { .. })
    ));
    assert!(matches!(
        OpenHandsLocatorResolverV1::discover(&conversation),
        Err(OpenHandsSourceBackedErrorV1::UnsupportedCurrentCliFormat { .. })
    ));

    let failure = super::source_backed::hydration_failure(
        OpenHandsSourceBackedErrorV1::UnsupportedCurrentCliFormat { root: conversation },
    );
    assert_eq!(
        failure.kind,
        HydrationFailureKind::UnsupportedParserRevision
    );
}

fn write_event(root: &Path, conversation: &str, file: &str, value: Value) -> std::path::PathBuf {
    let path = root.join("v1_conversations").join(conversation).join(file);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    path
}

fn message(id: &str, body: &str) -> Value {
    json!({
        "id": id,
        "timestamp": "2026-07-28T12:00:00Z",
        "kind": "MessageEvent",
        "source": "agent",
        "llm_message": {
            "role": "assistant",
            "content": body,
        },
    })
}

fn successful_output(id: &str, body: &str) -> Value {
    json!({
        "id": id,
        "timestamp": "2026-07-28T12:00:01Z",
        "kind": "ObservationEvent",
        "source": "environment",
        "observation": {
            "kind": "ExecuteBashObservation",
            "content": body,
            "exit_code": 0,
        },
    })
}
