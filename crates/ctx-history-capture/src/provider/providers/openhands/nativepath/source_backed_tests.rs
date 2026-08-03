use std::{collections::BTreeMap, fs, path::Path};

use ctx_history_core::{CaptureProvider, CoreRecord, TypedKey};
use serde_json::{json, Value};

use crate::{
    provider_sources::{count_event_file_io, EventFileInventoryError},
    OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
};

use super::source_backed::{
    openhands_route_error, project_group, projection_jobs, OpenHandsEventFileAdapterV2,
    OpenHandsEventFileSourcePlan, OpenHandsSourceBackedErrorV2, OpenHandsSourceBackedResultV2,
};

struct TestProjection {
    plan: OpenHandsEventFileSourcePlan,
    source: ctx_history_core::CertifiedSource,
    records: Vec<CoreRecord>,
}

fn project(root: &Path) -> OpenHandsSourceBackedResultV2<Vec<TestProjection>> {
    let adapter = OpenHandsEventFileAdapterV2::new(root.to_path_buf());
    let inventory = adapter.open_inventory()?;
    let mut projected = Vec::new();
    for group in inventory.groups() {
        let plan = adapter.bind_group(group)?;
        let mut records = Vec::new();
        let source = project_group(group, &plan, |record| {
            records.push(record);
            Ok(())
        })?;
        projected.push(TestProjection {
            plan,
            source,
            records,
        });
    }
    Ok(projected)
}

fn body(record: &CoreRecord) -> &str {
    record.content.normalized_body.as_deref().unwrap()
}

#[test]
fn cold_projection_preserves_complete_bodies_outcomes_and_core_semantics() {
    const TAIL: &str = "openhandspostsixteenkilobytesentinel";
    const LARGE_TAIL: &str = "openhands-post-eight-mib-tail";

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    let full_body = format!(
        r#"{{"arguments":{{"padding":"{}","tail":"{TAIL}"}},"tool":"write_file"}}"#,
        "x".repeat(17_000)
    );
    assert!(full_body.find(TAIL).unwrap() > 16 * 1_024);
    write_event(
        &root,
        "conversation-cold",
        "0001-message.json",
        message("event-message", &full_body),
    );
    let large_success = format!("{}{}", "s".repeat(9 * 1024 * 1024), LARGE_TAIL);
    write_event(
        &root,
        "conversation-cold",
        "0002-success.json",
        output("event-success", &large_success, Some(0), false),
    );
    write_event(
        &root,
        "conversation-cold",
        "0003-failure.json",
        output("event-failure", "failure output", Some(7), false),
    );
    write_event(
        &root,
        "conversation-cold",
        "0004-timeout.json",
        output("event-timeout", "timeout output", None, true),
    );
    write_event(
        &root,
        "conversation-cold",
        "0005-unknown.json",
        output("event-unknown", "unknown output", None, false),
    );

    let (projection, io) = count_event_file_io(|| project(&root).unwrap());
    let projection = &projection[0];
    assert_eq!(io.inventory_opens, 1);
    assert_eq!(io.inventory_walks, 1);
    assert_eq!(io.body_reads, 5);
    assert_eq!(io.leaf_lookups, 5);
    assert_eq!(io.peak_transient_leaf_handles, 1);
    assert_eq!(io.active_transient_leaf_handles, 0);
    assert_eq!(projection.source.counts().complete_records, 5);
    assert_eq!(projection.source.counts().retained_records, 5);
    assert_eq!(projection.source.counts().ignored_records, 0);
    assert_eq!(projection.source.counts().indexed_documents, 5);
    assert_eq!(body(&projection.records[0]), full_body);
    let structured: Value = serde_json::from_str(body(&projection.records[0])).unwrap();
    assert_eq!(
        structured
            .pointer("/arguments/tail")
            .and_then(Value::as_str),
        Some(TAIL)
    );
    assert_eq!(body(&projection.records[1]), large_success);
    assert!(body(&projection.records[1]).ends_with(LARGE_TAIL));
    assert_eq!(body(&projection.records[2]), "failure output");
    assert_eq!(body(&projection.records[3]), "timeout output");
    assert_eq!(body(&projection.records[4]), "unknown output");
    assert_eq!(
        projection.records[1]
            .content
            .structured_content
            .as_ref()
            .and_then(|value| value.pointer("/provider_native_tool_result/call_id"))
            .and_then(Value::as_str),
        Some("call-event-success")
    );

    let first = &projection.records[0];
    assert_eq!(first.parent_session_id, None);
    assert_eq!(first.root_session_id, first.session_id);
    assert_eq!(
        first.provider_session_id.as_deref(),
        Some("conversation-cold")
    );
    assert_eq!(
        first.native_event_id,
        Some(TypedKey::Utf8("event-message".to_owned()))
    );
    assert_eq!(first.agent_type, "primary");
    assert!(first.is_primary);
    assert_eq!(first.event_sequence, 0);
    assert_eq!(projection.records[1].event_sequence, 1);
    assert_eq!(projection.records[2].event_sequence, 2);
    assert_eq!(first.occurred_at_unix_ms, Some(1_785_240_000_000));
    assert_eq!(first.event_type, "message");
    assert_eq!(first.role.as_deref(), Some("assistant"));
    assert_eq!(
        projection.source.parser_revision(),
        "openhands-source-backed-v4"
    );
}

#[test]
fn file_result_policy_retains_success_and_meaningful_failure() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    write_event(
        &root,
        "conversation-file-policy",
        "0001-success.json",
        json!({
            "id": "file-success",
            "timestamp": "2026-07-28T12:00:00Z",
            "kind": "ObservationEvent",
            "source": "environment",
            "observation": {
                "kind": "FileEditorObservation",
                "output": "successful editor output must not be searchable",
                "path": "src/success.rs",
                "error": null
            }
        }),
    );
    write_event(
        &root,
        "conversation-file-policy",
        "0002-failure.json",
        json!({
            "id": "file-failure",
            "timestamp": "2026-07-28T12:00:01Z",
            "kind": "ObservationEvent",
            "source": "environment",
            "observation": {
                "kind": "FileEditorObservation",
                "content": "meaningful editor failure remains searchable",
                "path": "src/failure.rs",
                "error": "write failed"
            }
        }),
    );

    let projection = project(&root).unwrap().remove(0);
    assert_eq!(projection.source.counts().complete_records, 2);
    assert_eq!(projection.source.counts().ignored_records, 0);
    assert_eq!(projection.source.counts().retained_records, 2);
    assert_eq!(projection.records.len(), 2);
    assert_eq!(projection.records[0].event_type, "file_touched");
    assert_eq!(
        body(&projection.records[0]),
        "successful editor output must not be searchable"
    );
    assert_eq!(
        body(&projection.records[1]),
        "meaningful editor failure remains searchable"
    );
}

#[test]
fn two_thousand_leaf_projection_reads_each_body_once_with_constant_descriptors() {
    const LEAF_COUNT: usize = 2_000;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("large-profile");
    for index in 0..LEAF_COUNT {
        write_event(
            &root,
            "conversation-large",
            &format!("{index:04}.json"),
            message(&format!("event-{index:04}"), "body"),
        );
    }

    let (projection, io) = count_event_file_io(|| project(&root).unwrap());
    assert_eq!(projection[0].records.len(), LEAF_COUNT);
    assert_eq!(io.body_reads, LEAF_COUNT);
    assert_eq!(io.leaf_lookups, LEAF_COUNT);
    assert_eq!(io.group_digest_builds, 1);
    assert_eq!(io.inventory_digest_builds, 1);
    assert_eq!(io.peak_transient_leaf_handles, 1);
    assert_eq!(io.active_transient_leaf_handles, 0);
}

#[test]
fn projection_jobs_are_stable_independent_group_and_leaf_ordinals() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("jobs");
    write_event(
        &root,
        "conversation-b",
        "event.json",
        message("event-b", "b"),
    );
    write_event(&root, "conversation-a", "z.json", message("event-z", "z"));
    write_event(
        &root,
        "conversation-a",
        "nested/a.json",
        message("event-a", "a"),
    );
    let adapter = OpenHandsEventFileAdapterV2::new(root);
    let inventory = adapter.open_inventory().unwrap();
    let first = inventory.group_at(0).unwrap();
    let plan = adapter.bind_group(first).unwrap();
    let jobs = projection_jobs(first, &plan).unwrap();

    assert_eq!(first.group_key(), "conversation-a");
    assert_eq!(
        jobs.iter()
            .map(|job| (job.group_ordinal(), job.leaf_ordinal()))
            .collect::<Vec<_>>(),
        vec![(0, 0), (0, 1)]
    );
}

#[test]
fn direct_core_identity_is_stable_and_contains_no_source_locator_or_path() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    let leaf = write_event(
        &root,
        "conversation-golden",
        "events/leaf.json",
        message("event-golden", "literal golden body"),
    );

    let first = project(&root).unwrap().remove(0);
    let second = project(&root).unwrap().remove(0);
    let record = &first.records[0];
    assert_eq!(
        first.source.observation().source().provider(),
        CaptureProvider::OpenHands.as_str()
    );
    assert_eq!(
        first.source.observation().source().source_format(),
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT
    );
    assert_eq!(record.session_id, second.records[0].session_id);
    assert_eq!(record.event_id, second.records[0].event_id);
    assert_eq!(record.native_event_id, second.records[0].native_event_id);
    assert_eq!(body(record), "literal golden body");
    let encoded = serde_json::to_string(record).unwrap();
    assert!(!encoded.contains("locator"));
    assert!(!encoded.contains(leaf.to_string_lossy().as_ref()));
}

#[test]
fn unchanged_plan_reads_zero_bodies_and_changed_group_reads_each_leaf_once() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    let changed_leaf = write_event(
        &root,
        "conversation-a",
        "0001.json",
        message("event-a1", "before"),
    );
    write_event(
        &root,
        "conversation-a",
        "0002.json",
        message("event-a2", "stable sibling"),
    );
    write_event(
        &root,
        "conversation-b",
        "0001.json",
        message("event-b1", "unchanged conversation"),
    );
    let base = project(&root)
        .unwrap()
        .into_iter()
        .map(|projection| (projection.plan.conversation_id, projection.source))
        .collect::<BTreeMap<_, _>>();

    let adapter = OpenHandsEventFileAdapterV2::new(root.clone());
    let (unchanged, unchanged_io) = count_event_file_io(|| {
        let inventory = adapter.open_inventory().unwrap();
        inventory
            .groups()
            .map(|group| {
                let plan = adapter.bind_group(group).unwrap();
                let certified = base.get(group.group_key()).unwrap();
                certified.observation() == &plan.opening
                    && certified.parser_revision() == "openhands-source-backed-v4"
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(unchanged, vec![true, true]);
    assert_eq!(unchanged_io.body_reads, 0);
    assert_eq!(unchanged_io.leaf_lookups, 0);

    fs::write(
        changed_leaf,
        serde_json::to_vec(&message("event-a1", "after")).unwrap(),
    )
    .unwrap();
    let (replaced, replaced_io) = count_event_file_io(|| {
        let inventory = adapter.open_inventory().unwrap();
        let mut replaced = Vec::new();
        for group in inventory.groups() {
            let plan = adapter.bind_group(group).unwrap();
            let certified = base.get(group.group_key()).unwrap();
            if certified.observation() == &plan.opening
                && certified.parser_revision() == "openhands-source-backed-v4"
            {
                continue;
            }
            let certificate = project_group(group, &plan, |_| Ok(())).unwrap();
            replaced.push((plan.conversation_id, certificate.counts().complete_records));
        }
        replaced
    });
    assert_eq!(replaced, vec![("conversation-a".to_owned(), 2)]);
    assert_eq!(replaced_io.body_reads, 2);
    assert_eq!(replaced_io.leaf_lookups, 2);
}

#[test]
fn append_and_rewrite_keep_native_identity_and_replace_stored_body() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    let first_path = write_event(
        &root,
        "conversation-change",
        "event-a.json",
        message("event-a", "first exact body"),
    );
    let before = project(&root).unwrap().remove(0);
    let old = before.records[0].clone();

    write_event(
        &root,
        "conversation-change",
        "event-b.json",
        message("event-b", "second exact body"),
    );
    let after_append = project(&root).unwrap().remove(0);
    assert_eq!(after_append.records.len(), 2);
    let old_after = after_append
        .records
        .iter()
        .find(|record| record.event_id == old.event_id)
        .unwrap();
    assert_eq!(old_after.session_id, old.session_id);
    assert_eq!(body(old_after), "first exact body");

    fs::write(
        first_path,
        serde_json::to_vec(&message("event-a", "rewritten exact body")).unwrap(),
    )
    .unwrap();
    let rewritten = project(&root).unwrap().remove(0);
    let rewritten_event = rewritten
        .records
        .iter()
        .find(|record| record.event_id == old.event_id)
        .unwrap();
    assert_eq!(rewritten_event.session_id, old.session_id);
    assert_eq!(body(rewritten_event), "rewritten exact body");
}

#[test]
fn duplicate_and_cross_conversation_native_ids_are_scoped_correctly() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    write_event(
        &root,
        "conversation-a",
        "event.json",
        message("same-event", "one"),
    );
    write_event(
        &root,
        "conversation-b",
        "event.json",
        message("same-event", "two"),
    );
    let projected = project(&root).unwrap();
    assert_ne!(projected[0].plan.source, projected[1].plan.source);
    assert_ne!(projected[0].plan.session_id, projected[1].plan.session_id);
    assert_ne!(
        projected[0].records[0].event_id,
        projected[1].records[0].event_id
    );

    let duplicate_root = temp.path().join("duplicates");
    write_event(
        &duplicate_root,
        "conversation-duplicate",
        "0001.json",
        message("same-event", "one"),
    );
    write_event(
        &duplicate_root,
        "conversation-duplicate",
        "0002.json",
        message("same-event", "two"),
    );
    assert!(matches!(
        project(&duplicate_root),
        Err(OpenHandsSourceBackedErrorV2::DuplicateEventId {
            conversation_id,
            event_id,
        }) if conversation_id == "conversation-duplicate" && event_id == "same-event"
    ));
}

#[test]
fn malformed_leaf_is_rejected_without_hiding_valid_conversation_peers() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    write_event(
        &root,
        "conversation",
        "0001.json",
        message("event-1", "first"),
    );
    let malformed = write_event(
        &root,
        "conversation",
        "0002.json",
        message("event-2", "unused"),
    );
    fs::write(malformed, b"{not-json").unwrap();
    write_event(
        &root,
        "conversation",
        "0003.json",
        message("event-3", "third"),
    );

    let projection = project(&root).unwrap().remove(0);
    assert_eq!(projection.source.counts().complete_records, 3);
    assert_eq!(projection.source.counts().retained_records, 2);
    assert_eq!(projection.source.counts().rejected_records, 1);
    assert_eq!(projection.source.counts().ignored_records, 0);
    assert_eq!(projection.source.counts().indexed_documents, 2);
    assert_eq!(
        projection
            .records
            .iter()
            .map(|record| (record.event_sequence, body(record)))
            .collect::<Vec<_>>(),
        vec![(0, "first"), (2, "third")]
    );
}

#[test]
fn exact_empty_missing_and_current_cli_sources_remain_typed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    let exact = write_event(
        &root,
        "conversation-exact",
        "event.json",
        message("event", "exact"),
    );
    let inventory = OpenHandsEventFileAdapterV2::new(exact)
        .open_inventory()
        .unwrap();
    assert!(inventory.selected_file());
    assert_eq!(inventory.groups().len(), 1);

    let empty = temp.path().join("empty-profile");
    fs::create_dir_all(empty.join("v1_conversations")).unwrap();
    let adapter = OpenHandsEventFileAdapterV2::new(&empty);
    let inventory = adapter.open_inventory().unwrap();
    assert!(inventory.is_empty());

    let missing_error = OpenHandsEventFileAdapterV2::new(temp.path().join("missing"))
        .open_inventory()
        .unwrap_err();
    assert!(matches!(
        missing_error,
        OpenHandsSourceBackedErrorV2::EventFiles(EventFileInventoryError::Unavailable { .. })
    ));
    assert_eq!(
        openhands_route_error(missing_error).kind,
        crate::provider::source_backed::SourceBackedRouteErrorKind::Unavailable
    );

    let current = temp.path().join("conversations").join("current-cli");
    let event = current.join("events").join("event-1.json");
    fs::create_dir_all(event.parent().unwrap()).unwrap();
    fs::write(&event, b"{}").unwrap();
    assert!(matches!(
        OpenHandsEventFileAdapterV2::new(current).open_inventory(),
        Err(OpenHandsSourceBackedErrorV2::UnsupportedCurrentCliFormat { .. })
    ));
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

fn output(id: &str, body: &str, exit_code: Option<i64>, timeout: bool) -> Value {
    json!({
        "id": id,
        "timestamp": "2026-07-28T12:00:01Z",
        "kind": "ObservationEvent",
        "source": "environment",
        "tool_call_id": format!("call-{id}"),
        "observation": {
            "kind": "ExecuteBashObservation",
            "content": body,
            "exit_code": exit_code,
            "timeout": timeout,
        },
    })
}
