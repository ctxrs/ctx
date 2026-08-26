use std::{collections::BTreeMap, fs, path::Path};

use ctx_history_core::{CaptureProvider, CoreRecord, SourceAnchorScope, TypedKey};
use serde_json::{json, Value};

use crate::{
    provider_sources::{count_event_file_io, EventFileInventoryError},
    MAX_PROVIDER_JSONL_LINE_BYTES, OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
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
    project_scoped(root, SourceAnchorScope::Unqualified)
}

fn project_scoped(
    root: &Path,
    source_anchor_scope: SourceAnchorScope,
) -> OpenHandsSourceBackedResultV2<Vec<TestProjection>> {
    let adapter =
        OpenHandsEventFileAdapterV2::<()>::new_scoped(root.to_path_buf(), source_anchor_scope);
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

fn project_current_scoped(
    root: &Path,
    source_anchor_scope: SourceAnchorScope,
) -> OpenHandsSourceBackedResultV2<Vec<TestProjection>> {
    let adapter = OpenHandsEventFileAdapterV2::<()>::new_current_conversations_scoped(
        root.to_path_buf(),
        source_anchor_scope,
    );
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
fn root_scope_distinguishes_native_sessions_and_unqualified_is_unchanged() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    write_event(
        &root,
        "same-native-session",
        "0001.json",
        message("same-native-event", "body"),
    );

    let legacy = project(&root).unwrap().remove(0);
    let unqualified = project_scoped(&root, SourceAnchorScope::Unqualified)
        .unwrap()
        .remove(0);
    let first = project_scoped(&root, SourceAnchorScope::Lineage([1; 32]))
        .unwrap()
        .remove(0);
    let second = project_scoped(&root, SourceAnchorScope::Lineage([2; 32]))
        .unwrap()
        .remove(0);

    assert!(legacy
        .plan
        .source
        .exact_descriptor_eq(&unqualified.plan.source));
    assert_eq!(legacy.plan.session_id, unqualified.plan.session_id);
    assert_ne!(first.plan.source.identity(), second.plan.source.identity());
    assert_ne!(first.plan.session_id, second.plan.session_id);
    assert_ne!(first.records[0].event_id, second.records[0].event_id);
}

#[test]
fn current_conversations_scan_excludes_nested_legacy_persistence() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let selected = temp.path().join("conversations");
    write_current_event_at_root(
        &selected,
        "current-conversation",
        "event-00001-current.json",
        message("current-event", "current body"),
    );
    write_event(
        &selected,
        "legacy-conversation",
        "legacy-event.json",
        message("legacy-event", "legacy body"),
    );

    let current = project_current_scoped(&selected, SourceAnchorScope::Unqualified).unwrap();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].plan.conversation_id, "current-conversation");
    assert_eq!(body(&current[0].records[0]), "current body");

    let compatible = project(&selected).unwrap();
    assert_eq!(compatible.len(), 2);
    assert!(compatible
        .iter()
        .any(|projection| projection.plan.conversation_id == "legacy-conversation"));
}

#[test]
fn cold_projection_preserves_complete_bodies_and_core_semantics() {
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
    let large_activity = projection.records[1].content.activity.as_ref().unwrap();
    assert!(large_activity.provider_call_id.is_some());
    assert!(matches!(
        large_activity
            .result
            .as_ref()
            .map(|result| &result.structured_content),
        Some(ctx_history_core::ActivityJsonCapture::Omitted { reason, .. })
            if reason == "size_limit"
    ));

    let first = &projection.records[0];
    assert_eq!(first.parent_session_id, None);
    assert_eq!(first.root_session_id, None);
    assert_eq!(
        first.provider_session_id.as_deref(),
        Some("conversation-cold")
    );
    assert_eq!(
        first.native_event_id,
        Some(TypedKey::Utf8("event-message".to_owned()))
    );
    assert_eq!(first.event_sequence, 0);
    assert_eq!(projection.records[1].event_sequence, 1);
    assert_eq!(projection.records[2].event_sequence, 2);
    assert_eq!(first.occurred_at_unix_ms, Some(1_785_240_000_000));
    assert_eq!(first.event_type, "message");
    assert_eq!(first.role.as_deref(), Some("assistant"));
    assert_eq!(
        first.agent_scope,
        Some(ctx_history_core::AgentScope::Primary)
    );
    assert_eq!(
        projection.source.parser_revision(),
        "openhands-source-backed-v7-naive-time"
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
    assert_eq!(projection.records[0].event_type, "tool_output");
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
    let adapter = OpenHandsEventFileAdapterV2::<()>::new(root);
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

    let adapter = OpenHandsEventFileAdapterV2::<()>::new(root.clone());
    let (unchanged, unchanged_io) = count_event_file_io(|| {
        let inventory = adapter.open_inventory().unwrap();
        inventory
            .groups()
            .map(|group| {
                let plan = adapter.bind_group(group).unwrap();
                let certified = base.get(group.group_key()).unwrap();
                certified.observation() == &plan.opening
                    && certified.parser_revision() == "openhands-source-backed-v7-naive-time"
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
                && certified.parser_revision() == "openhands-source-backed-v7-naive-time"
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
fn current_cli_official_event_shapes_use_the_authoritative_decoder() {
    // These are the small, publishable message/action fields from the official
    // OpenHands-CLI `simple_echo_hello_world` trajectory, synthesized in place.
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    write_current_event(
        &root,
        "conversation-current",
        "event-00001-0df665dd-b023-4473-9c48-80d35edbb4f5.json",
        json!({
            "id": "0df665dd-b023-4473-9c48-80d35edbb4f5",
            "timestamp": "2026-01-27T03:56:17.815138",
            "source": "user",
            "llm_message": {
                "role": "user",
                "content": [{
                    "cache_prompt": false,
                    "type": "text",
                    "text": "echo hello world",
                    "enable_truncation": true
                }],
                "thinking_blocks": []
            },
            "activated_skills": [],
            "extended_content": [],
            "kind": "MessageEvent"
        }),
    );
    write_current_event(
        &root,
        "conversation-current",
        "event-00002-3097395b-9d0e-40ea-9721-318280129892.json",
        json!({
            "id": "3097395b-9d0e-40ea-9721-318280129892",
            "timestamp": "2026-01-27T03:56:22.718970",
            "source": "agent",
            "action": {
                "command": "echo hello world",
                "is_input": false,
                "reset": false,
                "kind": "TerminalAction"
            },
            "tool_call_id": "toolu_current",
            "kind": "ActionEvent"
        }),
    );
    fs::write(
        root.join("conversations")
            .join("conversation-current")
            .join("base_state.json"),
        b"{}",
    )
    .unwrap();
    fs::write(
        root.join("conversations")
            .join("conversation-current")
            .join("events")
            .join("not-an-event.json"),
        b"{}",
    )
    .unwrap();

    let projection = project(&root).unwrap().remove(0);
    assert_eq!(projection.source.counts().complete_records, 2);
    assert_eq!(projection.records.len(), 2);
    assert_eq!(projection.plan.conversation_id, "conversation-current");
    assert_eq!(body(&projection.records[0]), "echo hello world");
    assert_eq!(projection.records[0].event_type, "message");
    assert_eq!(projection.records[0].role.as_deref(), Some("user"));
    assert_eq!(body(&projection.records[1]), "echo hello world");
    assert_eq!(projection.records[1].event_type, "tool_call");
    assert_eq!(projection.records[0].occurred_at_unix_ms, None);
    assert_eq!(projection.records[1].occurred_at_unix_ms, None);
    assert_eq!(
        projection.records[1]
            .content
            .activity
            .as_ref()
            .and_then(|activity| activity.invocation.as_ref())
            .and_then(|invocation| invocation.started_at_unix_ms),
        None
    );
    assert_eq!(
        projection.records[1].native_event_id,
        Some(TypedKey::Utf8(
            "3097395b-9d0e-40ea-9721-318280129892".to_owned()
        ))
    );
}

#[test]
fn current_cli_direct_root_accepts_an_arbitrary_directory_name() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("official-override-without-reserved-name");
    let event = write_current_event_at_root(
        &root,
        "conversation-direct",
        "event-00001-direct.json",
        message("event-direct", "direct current body"),
    );

    let from_root = project(&root).unwrap().remove(0);
    let from_leaf = project(&event).unwrap().remove(0);
    assert_eq!(from_root.plan.conversation_id, "conversation-direct");
    assert_eq!(from_root.plan.source, from_leaf.plan.source);
    assert_eq!(from_root.plan.session_id, from_leaf.plan.session_id);
    assert_eq!(from_root.records[0].event_id, from_leaf.records[0].event_id);
    assert_eq!(body(&from_root.records[0]), "direct current body");
}

#[test]
fn layout_migration_preserves_identity_and_mixed_overlap_fails_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let legacy_root = temp.path().join("legacy");
    let current_root = temp.path().join("current");
    let event = message("event-stable", "stable body");
    write_event(
        &legacy_root,
        "conversation-stable",
        "0001.json",
        event.clone(),
    );
    write_current_event(
        &current_root,
        "conversation-stable",
        "event-00001-event-stable.json",
        event.clone(),
    );

    let legacy = project(&legacy_root).unwrap().remove(0);
    let current = project(&current_root).unwrap().remove(0);
    assert_eq!(legacy.plan.source, current.plan.source);
    assert_eq!(legacy.plan.session_id, current.plan.session_id);
    assert_eq!(legacy.records[0].session_id, current.records[0].session_id);
    assert_eq!(legacy.records[0].event_id, current.records[0].event_id);

    let mixed = temp.path().join("mixed-distinct");
    write_event(
        &mixed,
        "conversation-legacy",
        "0001.json",
        message("legacy-event", "legacy"),
    );
    write_current_event(
        &mixed,
        "conversation-current",
        "event-00001-current-event.json",
        message("current-event", "current"),
    );
    let mixed_projection = project(&mixed).unwrap();
    assert_eq!(mixed_projection.len(), 2);
    assert_eq!(
        mixed_projection
            .iter()
            .map(|projection| projection.plan.conversation_id.as_str())
            .collect::<Vec<_>>(),
        vec!["conversation-current", "conversation-legacy"]
    );

    let overlap = temp.path().join("mixed-overlap");
    write_event(&overlap, "conversation-overlap", "0001.json", event.clone());
    write_current_event(
        &overlap,
        "conversation-overlap",
        "event-00001-event-stable.json",
        event,
    );
    assert!(matches!(
        project(&overlap),
        Err(OpenHandsSourceBackedErrorV2::DuplicateConversationId(conversation_id))
            if conversation_id == "conversation-overlap"
    ));
}

#[test]
fn legacy_nested_reserved_component_preserves_first_marker_identity() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let nested_root = temp.path().join("nested");
    let nested_path = nested_root
        .join("v1_conversations")
        .join("conversation-first")
        .join("subtree")
        .join("v1_conversations")
        .join("conversation-second")
        .join("event.json");
    fs::create_dir_all(nested_path.parent().unwrap()).unwrap();
    fs::write(
        &nested_path,
        serde_json::to_vec(&message("stable-event", "nested body")).unwrap(),
    )
    .unwrap();

    let flat_root = temp.path().join("flat");
    write_event(
        &flat_root,
        "conversation-first",
        "event.json",
        message("stable-event", "nested body"),
    );

    let nested = project(&nested_root).unwrap().remove(0);
    let flat = project(&flat_root).unwrap().remove(0);
    assert_eq!(nested.plan.conversation_id, "conversation-first");
    assert_eq!(nested.plan.source, flat.plan.source);
    assert_eq!(nested.plan.session_id, flat.plan.session_id);
    assert_eq!(nested.records[0].event_id, flat.records[0].event_id);
}

#[test]
fn current_cli_append_rewrite_and_deletion_converge_with_stable_ids() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    let first_path = write_current_event(
        &root,
        "conversation-lifecycle",
        "event-00001-event-a.json",
        message("event-a", "first exact body"),
    );
    let before = project(&root).unwrap().remove(0);
    let original = before.records[0].clone();

    let second_path = write_current_event(
        &root,
        "conversation-lifecycle",
        "event-00002-event-b.json",
        message("event-b", "second exact body"),
    );
    let appended = project(&root).unwrap().remove(0);
    assert_eq!(appended.records.len(), 2);
    assert_eq!(appended.records[0].event_id, original.event_id);
    assert_eq!(appended.records[0].session_id, original.session_id);

    fs::write(
        &first_path,
        serde_json::to_vec(&message("event-a", "rewritten exact body")).unwrap(),
    )
    .unwrap();
    let rewritten = project(&root).unwrap().remove(0);
    assert_eq!(rewritten.records[0].event_id, original.event_id);
    assert_eq!(body(&rewritten.records[0]), "rewritten exact body");

    fs::remove_file(second_path).unwrap();
    let after_event_deletion = project(&root).unwrap().remove(0);
    assert_eq!(after_event_deletion.records.len(), 1);
    assert_eq!(after_event_deletion.records[0].event_id, original.event_id);
    assert_eq!(
        body(&after_event_deletion.records[0]),
        "rewritten exact body"
    );

    fs::remove_file(first_path).unwrap();
    assert!(project(&root).unwrap().is_empty());
}

#[test]
fn current_cli_rejects_duplicate_malformed_and_oversized_records() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let malformed_root = temp.path().join("malformed");
    write_current_event(
        &malformed_root,
        "conversation-malformed",
        "event-00001-valid.json",
        message("valid", "valid peer"),
    );
    let malformed = write_current_event(
        &malformed_root,
        "conversation-malformed",
        "event-00002-malformed.json",
        message("unused", "unused"),
    );
    fs::write(malformed, b"{not-json").unwrap();
    let malformed_projection = project(&malformed_root).unwrap().remove(0);
    assert_eq!(malformed_projection.source.counts().complete_records, 2);
    assert_eq!(malformed_projection.source.counts().retained_records, 1);
    assert_eq!(malformed_projection.source.counts().rejected_records, 1);
    assert_eq!(body(&malformed_projection.records[0]), "valid peer");

    let duplicate_root = temp.path().join("duplicate");
    write_current_event(
        &duplicate_root,
        "conversation-duplicate",
        "event-00001-first.json",
        message("same-event", "first"),
    );
    write_current_event(
        &duplicate_root,
        "conversation-duplicate",
        "event-00002-second.json",
        message("same-event", "second"),
    );
    assert!(matches!(
        project(&duplicate_root),
        Err(OpenHandsSourceBackedErrorV2::DuplicateEventId {
            conversation_id,
            event_id,
        }) if conversation_id == "conversation-duplicate" && event_id == "same-event"
    ));

    let oversized_root = temp.path().join("oversized");
    let oversized = write_current_event(
        &oversized_root,
        "conversation-oversized",
        "event-00001-oversized.json",
        message("oversized", "unused"),
    );
    fs::write(&oversized, vec![b'x'; MAX_PROVIDER_JSONL_LINE_BYTES + 1]).unwrap();
    assert!(matches!(
        OpenHandsEventFileAdapterV2::<()>::new(oversized_root).open_inventory(),
        Err(OpenHandsSourceBackedErrorV2::EventFiles(
            EventFileInventoryError::RecordTooLarge { .. }
        ))
    ));
}

#[cfg(unix)]
#[test]
fn current_cli_rejects_symlinked_event_files() {
    use std::os::unix::fs::symlink;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    let target = temp.path().join("outside.json");
    fs::write(
        &target,
        serde_json::to_vec(&message("outside", "outside")).unwrap(),
    )
    .unwrap();
    let linked = root
        .join("conversations")
        .join("conversation-linked")
        .join("events")
        .join("event-00001-linked.json");
    fs::create_dir_all(linked.parent().unwrap()).unwrap();
    symlink(target, linked).unwrap();

    assert!(matches!(
        OpenHandsEventFileAdapterV2::<()>::new(root).open_inventory(),
        Err(OpenHandsSourceBackedErrorV2::EventFiles(
            EventFileInventoryError::Unavailable { .. }
        ))
    ));
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
fn conflicting_openhands_activity_aliases_retain_records_and_abstain_exactly() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    let conflicting_identity = json!({
        "id": "conflicting-identity",
        "timestamp": "2026-07-28T12:00:00Z",
        "kind": "ActionEvent",
        "source": "agent",
        "tool_call_id": "call-one",
        "toolCallId": "call-two",
        "action": {"kind": "first_tool", "name": "second_tool", "input": {"x": 1}}
    });
    write_event(
        &root,
        "conversation",
        "0001.json",
        conflicting_identity.clone(),
    );
    let conflicting_arguments = json!({
        "id": "conflicting-arguments",
        "timestamp": "2026-07-28T12:00:01Z",
        "kind": "ActionEvent",
        "source": "agent",
        "tool_call_id": "call-args",
        "action": {
            "kind": "exact_tool",
            "arguments": {"x": 1},
            "input": {"x": 2}
        }
    });
    write_event(
        &root,
        "conversation",
        "0002.json",
        conflicting_arguments.clone(),
    );

    let projection = project(&root).unwrap().remove(0);
    assert_eq!(projection.records.len(), 2);
    let identity = &projection.records[0];
    assert_eq!(
        identity.content.structured_content.as_ref(),
        Some(&conflicting_identity)
    );
    assert!(identity.content.activity.is_none());

    let arguments = &projection.records[1];
    assert_eq!(
        arguments.content.structured_content.as_ref(),
        Some(&conflicting_arguments)
    );
    assert!(arguments.content.activity.is_none());
}

#[test]
fn nested_openhands_metadata_keys_never_escape_into_facts() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    let mut event = message("nested-metadata", "exact OpenHands body");
    event["metadata"] = json!({
        "path": "src/top-level-decoy.rs",
        "nested": {
            "branch": "nested-decoy",
            "commit": "nested-commit",
            "command": "nested-command"
        }
    });
    event["llm_message"]["metadata"] = json!({
        "file": "src/message-decoy.rs",
        "workdir": "/message/decoy"
    });
    write_event(&root, "conversation", "0001.json", event.clone());

    let projection = project(&root).unwrap().remove(0);
    assert_eq!(projection.records.len(), 1);
    let record = &projection.records[0];
    assert_eq!(record.content.structured_content.as_ref(), Some(&event));
    assert!(record.content.activity.is_none());
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
    let inventory = OpenHandsEventFileAdapterV2::<()>::new(exact)
        .open_inventory()
        .unwrap();
    assert!(inventory.selected_file());
    assert_eq!(inventory.groups().len(), 1);

    let empty = temp.path().join("empty-profile");
    fs::create_dir_all(empty.join("v1_conversations")).unwrap();
    let adapter = OpenHandsEventFileAdapterV2::<()>::new(&empty);
    let inventory = adapter.open_inventory().unwrap();
    assert!(inventory.is_empty());

    let missing_error = OpenHandsEventFileAdapterV2::<()>::new(temp.path().join("missing"))
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

    let current_root = temp.path().join("current-profile");
    let current = write_current_event(
        &current_root,
        "current-cli",
        "event-00001-current.json",
        message("current", "current exact"),
    );
    let inventory = OpenHandsEventFileAdapterV2::<()>::new(current)
        .open_inventory()
        .unwrap();
    assert!(inventory.selected_file());
    assert_eq!(inventory.groups().len(), 1);
}

fn write_event(root: &Path, conversation: &str, file: &str, value: Value) -> std::path::PathBuf {
    let path = root.join("v1_conversations").join(conversation).join(file);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    path
}

fn write_current_event(
    root: &Path,
    conversation: &str,
    file: &str,
    value: Value,
) -> std::path::PathBuf {
    write_current_event_at_root(&root.join("conversations"), conversation, file, value)
}

fn write_current_event_at_root(
    root: &Path,
    conversation: &str,
    file: &str,
    value: Value,
) -> std::path::PathBuf {
    let path = root.join(conversation).join("events").join(file);
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
