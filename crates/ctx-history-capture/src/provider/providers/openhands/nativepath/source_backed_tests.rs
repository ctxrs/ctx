use std::{collections::BTreeMap, fs, path::Path};

use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, EventHydrationRequest, HydrationFailureKind,
    LocatorRevisionPolicy, NativeRecordCoordinate, SourceAnchor, SourceKey, SourceRecordLocator,
    TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde_json::{json, Value};

use crate::{
    provider::providers::openhands::source::{OpenHandsFileObservation, OpenHandsObservedTime},
    provider_sources::{count_event_file_io, EventFileInventoryError},
    OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
};

use super::source_backed::{
    hydration_failure, leaf_revision_digest, openhands_route_error, project_group, projection_jobs,
    source_key, source_locator, validate_locator, OpenHandsEventFileAdapterV2,
    OpenHandsEventFileSourcePlan, OpenHandsSourceBackedErrorV2, OpenHandsSourceBackedResultV2,
};

struct TestProjection {
    plan: OpenHandsEventFileSourcePlan,
    source: ctx_history_core::CertifiedSource,
    documents: Vec<LexicalDocument>,
}

fn project(root: &Path) -> OpenHandsSourceBackedResultV2<Vec<TestProjection>> {
    let adapter = OpenHandsEventFileAdapterV2::new(root.to_path_buf());
    let inventory = adapter.open_inventory()?;
    let mut projected = Vec::new();
    for group in inventory.groups() {
        let plan = adapter.bind_group(group)?;
        let mut documents = Vec::new();
        let source = project_group(group, &plan, |document| {
            documents.push(document);
            Ok(())
        })?;
        projected.push(TestProjection {
            plan,
            source,
            documents,
        });
    }
    Ok(projected)
}

#[test]
fn cold_projection_preserves_full_body_outcomes_counts_and_exact_semantics() {
    const TAIL: &str = "openhandspostsixteenkilobytesentinel";

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
    write_event(
        &root,
        "conversation-cold",
        "0002-success.json",
        output("event-success", "private successful output", Some(0), false),
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
        output("event-timeout", "", None, true),
    );
    let mut combined_success = output(
        "event-combined-success",
        "combined private successful output",
        Some(0),
        false,
    );
    combined_success["llm_message"] = json!({
        "role": "assistant",
        "content": "combined private successful output",
    });
    write_event(
        &root,
        "conversation-cold",
        "0005-combined-success.json",
        combined_success,
    );
    let (projection, io) = count_event_file_io(|| project(&root).unwrap());
    let projection = &projection[0];
    assert_eq!(io.inventory_opens, 1);
    assert_eq!(io.inventory_walks, 1);
    assert_eq!(io.body_reads, 5);
    assert_eq!(io.leaf_lookups, 5);
    assert_eq!(io.peak_transient_leaf_handles, 1);
    assert!(io.peak_transient_directory_handles <= 4);
    assert_eq!(io.active_transient_leaf_handles, 0);
    assert_eq!(io.active_transient_directory_handles, 0);
    assert_eq!(projection.source.counts().complete_records, 5);
    assert_eq!(projection.source.counts().retained_records, 3);
    assert_eq!(projection.source.counts().ignored_records, 2);
    assert_eq!(projection.source.counts().rejected_records, 0);
    assert_eq!(projection.source.counts().indexed_documents, 3);
    assert_eq!(projection.documents[0].body, full_body);
    let structured: Value = serde_json::from_str(&projection.documents[0].body).unwrap();
    assert_eq!(
        structured
            .pointer("/arguments/tail")
            .and_then(Value::as_str),
        Some(TAIL)
    );
    assert_eq!(projection.documents[1].body, "failure output");
    assert_eq!(projection.documents[2].body, "OpenHands command timed out");
    assert!(projection
        .documents
        .iter()
        .all(|document| !document.body.contains("private successful output")));
    assert_eq!(projection.documents[0].parent_session_id, None);
    assert_eq!(
        projection.documents[0].root_session_id,
        projection.documents[0].session_id
    );
    assert_eq!(
        projection.documents[0].provider_session_id.as_deref(),
        Some("conversation-cold")
    );
    assert_eq!(projection.documents[0].agent_type, "primary");
    assert!(projection.documents[0].is_primary);
    assert_eq!(projection.documents[0].event_sequence, 0);
    assert_eq!(projection.documents[1].event_sequence, 2);
    assert_eq!(projection.documents[2].event_sequence, 3);
    assert_eq!(
        projection.documents[0].occurred_at_unix_ms,
        Some(1_785_240_000_000)
    );
    assert_eq!(projection.documents[0].event_type, "message");
    assert_eq!(projection.documents[0].role.as_deref(), Some("assistant"));
    assert_eq!(
        projection.source.parser_revision(),
        "openhands-source-backed-v2"
    );
    assert_eq!(
        projection.source.observation().revision_kind(),
        "openhands-v1-conversation-leaves-v2"
    );
}

#[test]
fn file_touched_result_policy_hides_success_body_and_retains_meaningful_failure() {
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
    assert_eq!(projection.source.counts().ignored_records, 1);
    assert_eq!(projection.source.counts().retained_records, 1);
    assert_eq!(projection.documents.len(), 1);
    assert_eq!(projection.documents[0].event_type, "file_touched");
    assert_eq!(
        projection.documents[0].body,
        "meaningful editor failure remains searchable"
    );
    assert_eq!(
        projection.documents[0].touched_files,
        vec!["src/failure.rs".to_owned()]
    );
    assert!(!projection.documents[0]
        .body
        .contains("successful editor output"));
}

#[test]
fn two_thousand_leaf_cold_projection_reads_once_with_constant_descriptors() {
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
    assert_eq!(projection.len(), 1);
    assert_eq!(projection[0].documents.len(), LEAF_COUNT);
    assert_eq!(
        projection[0].source.counts().complete_records,
        LEAF_COUNT as u64
    );
    assert_eq!(io.inventory_opens, 1);
    assert_eq!(io.inventory_walks, 1);
    assert_eq!(io.body_reads, LEAF_COUNT);
    assert_eq!(io.leaf_lookups, LEAF_COUNT);
    assert_eq!(io.group_digest_builds, 1);
    assert_eq!(io.inventory_digest_builds, 1);
    assert_eq!(io.peak_transient_leaf_handles, 1);
    assert!(io.peak_transient_directory_handles <= 4);
    assert_eq!(io.active_transient_leaf_handles, 0);
    assert_eq!(io.active_transient_directory_handles, 0);
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
    assert_eq!(
        first
            .leaves()
            .iter()
            .map(|leaf| leaf.coordinates().relative_file_key.as_str())
            .collect::<Vec<_>>(),
        vec!["nested/a.json", "z.json"]
    );
}

#[test]
fn nested_locator_touch_projection_and_filename_fallback_remain_exact() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    write_event(
        &root,
        "conversation-semantics",
        "events/0001-touch.json",
        json!({
            "id": "touch-event",
            "timestamp": "2026-07-28T12:00:00Z",
            "kind": "ActionEvent",
            "source": "agent",
            "action": {
                "kind": "FileEditorAction",
                "path": "src/main.rs",
                "command": "view"
            }
        }),
    );
    write_event(
        &root,
        "conversation-semantics",
        "events/fallback-native-id.json",
        json!({
            "timestamp": "2026-07-28T12:00:01Z",
            "kind": "MessageEvent",
            "source": "user",
            "llm_message": {"role": "user", "content": "fallback body"}
        }),
    );

    let projection = project(&root).unwrap().remove(0);
    assert_eq!(projection.documents.len(), 2);
    assert_eq!(
        projection.documents[0].touched_files,
        vec!["src/main.rs".to_owned()]
    );
    assert_eq!(projection.documents[0].event_type, "tool_call");
    assert_eq!(projection.documents[0].role.as_deref(), Some("assistant"));
    assert_eq!(projection.documents[1].role.as_deref(), Some("user"));

    let NativeRecordCoordinate::TreeRecord {
        relative_file_key,
        record_coordinate,
    } = projection.documents[1].locator.coordinate()
    else {
        panic!("expected OpenHands tree locator");
    };
    assert_eq!(
        relative_file_key,
        &TypedKey::Utf8("events/fallback-native-id.json".to_owned())
    );
    let TypedKey::Composite(parts) = record_coordinate else {
        panic!("expected OpenHands object coordinate");
    };
    assert_eq!(
        parts.get(1),
        Some(&TypedKey::Utf8("fallback-native-id".to_owned()))
    );
}

#[test]
fn literal_old_adapter_identity_locator_and_content_goldens_are_pinned() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    write_event(
        &root,
        "conversation-golden",
        "events/leaf.json",
        message("event-golden", "literal golden body"),
    );

    let projection = project(&root).unwrap().remove(0);
    let document = &projection.documents[0];
    assert_eq!(
        projection
            .source
            .observation()
            .source()
            .identity()
            .to_string(),
        "c79f34a6-87cc-8830-b426-755b6430f5a4"
    );
    assert_eq!(
        projection.source.observation().source().provider(),
        CaptureProvider::OpenHands.as_str()
    );
    assert_eq!(
        projection.source.observation().source().source_format(),
        "openhands_file_events"
    );
    assert_eq!(
        projection.source.observation().source().schema_variant(),
        "openhands-v1-conversation-tree-v1"
    );
    assert_eq!(
        projection
            .source
            .observation()
            .source()
            .provider_identity_version(),
        1
    );
    assert_eq!(
        document.session_id.to_string(),
        "826e2cf0-0888-8a3e-999f-0f405cde2e70"
    );
    assert_eq!(
        document.event_id.to_string(),
        "c472543c-79c4-850d-97ff-629ff4003a49"
    );
    assert_eq!(
        hex(projection.source.content_digest()),
        "829a32ead63bdd55ec626ec76071a681c727f4da6b22aaad7f75d9e6660ad546"
    );
    assert_eq!(
        hex(document.locator.record_digest()),
        "7eb624dbc227939b91ae2a34e1ccf3297dddb62662360f730e10948dc2ddc5b1"
    );
    assert!(document.source_path.as_deref().is_some_and(
        |path| path.ends_with("v1_conversations/conversation-golden/events/leaf.json")
    ));

    let fixed_observation = OpenHandsFileObservation {
        length: 321,
        modified: OpenHandsObservedTime {
            before_epoch: false,
            seconds: 1_722_168_000,
            nanos: 123_456_789,
        },
        readonly: false,
        device: Some(41),
        inode: Some(99),
    };
    let record_digest = hex_32("7eb624dbc227939b91ae2a34e1ccf3297dddb62662360f730e10948dc2ddc5b1");
    let leaf_revision =
        leaf_revision_digest("events/leaf.json", &fixed_observation, record_digest).unwrap();
    assert_eq!(
        hex(&leaf_revision),
        "209debcaa1f8e4193fbb104b7c32a8d50af50d8490b6e642c400d3bc348f3234"
    );
    let source = source_key("conversation-golden").unwrap();
    let locator = source_locator(
        &source,
        "events/leaf.json",
        "event-golden",
        leaf_revision,
        record_digest,
    )
    .unwrap();
    assert_eq!(
        locator.revision_policy(),
        LocatorRevisionPolicy::StableRecordEvidence
    );
    assert_eq!(locator.certified_source_revision_digest(), None);
    assert_eq!(
        locator.coordinate(),
        &NativeRecordCoordinate::TreeRecord {
            relative_file_key: TypedKey::Utf8("events/leaf.json".to_owned()),
            record_coordinate: TypedKey::Composite(vec![
                TypedKey::Utf8("openhands-event-object-v1".to_owned()),
                TypedKey::Utf8("event-golden".to_owned()),
                TypedKey::Bytes(leaf_revision.to_vec()),
            ]),
        }
    );
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
    let cold = project(&root).unwrap();
    let base = cold
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
                adapter.exact_replay_matches(base.get(group.group_key()).unwrap(), &plan)
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(unchanged, vec![true, true]);
    assert_eq!(unchanged_io.inventory_opens, 1);
    assert_eq!(unchanged_io.inventory_walks, 1);
    assert_eq!(unchanged_io.body_reads, 0);
    assert_eq!(unchanged_io.leaf_lookups, 0);
    assert_eq!(unchanged_io.group_digest_builds, 2);
    assert_eq!(unchanged_io.inventory_digest_builds, 1);
    assert_eq!(unchanged_io.peak_transient_leaf_handles, 1);
    assert_eq!(unchanged_io.active_transient_leaf_handles, 0);
    assert_eq!(unchanged_io.active_transient_directory_handles, 0);

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
            if adapter.exact_replay_matches(base.get(group.group_key()).unwrap(), &plan) {
                continue;
            }
            let certificate = project_group(group, &plan, |_| Ok(())).unwrap();
            replaced.push((plan.conversation_id, certificate.counts().complete_records));
        }
        replaced
    });
    assert_eq!(replaced, vec![("conversation-a".to_owned(), 2)]);
    assert_eq!(replaced_io.inventory_opens, 1);
    assert_eq!(replaced_io.inventory_walks, 1);
    assert_eq!(replaced_io.body_reads, 2);
    assert_eq!(replaced_io.leaf_lookups, 2);
    assert_eq!(replaced_io.peak_transient_leaf_handles, 1);
    assert_eq!(replaced_io.active_transient_leaf_handles, 0);
    assert_eq!(replaced_io.active_transient_directory_handles, 0);
}

#[test]
fn active_source_family_contract_event_files_add_siblings_and_stale_rewrites() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    let first_path = write_event(
        &root,
        "conversation-change",
        "event-a.json",
        message("event-a", "first exact body"),
    );
    let before = project(&root).unwrap().remove(0);
    let old = before.documents[0].clone();
    let old_source = before.source.observation().source().identity();

    write_event(
        &root,
        "conversation-change",
        "event-b.json",
        message("event-b", "second exact body"),
    );
    let after_append = project(&root).unwrap().remove(0);
    assert_eq!(after_append.documents.len(), 2);
    let appended = after_append
        .documents
        .iter()
        .find(|document| document.event_id != old.event_id)
        .unwrap();
    let adapter = OpenHandsEventFileAdapterV2::new(root.clone());
    assert_eq!(
        adapter
            .hydrate_event(
                &EventHydrationRequest::new(appended.event_id, appended.locator.clone()).unwrap()
            )
            .unwrap()
            .provider_bytes,
        b"second exact body"
    );
    assert_eq!(
        after_append.source.observation().source().identity(),
        old_source
    );
    let old_after = after_append
        .documents
        .iter()
        .find(|document| document.event_id == old.event_id)
        .unwrap();
    assert_eq!(old_after.session_id, old.session_id);
    let hydrated = adapter
        .hydrate_event(&EventHydrationRequest::new(old.event_id, old.locator.clone()).unwrap())
        .unwrap();
    assert_eq!(hydrated.provider_bytes, b"first exact body");

    fs::write(
        first_path,
        serde_json::to_vec(&message("event-a", "rewritten exact body")).unwrap(),
    )
    .unwrap();
    let rewritten = project(&root).unwrap().remove(0);
    let rewritten_event = rewritten
        .documents
        .iter()
        .find(|document| document.event_id == old.event_id)
        .unwrap();
    assert_eq!(rewritten_event.event_id, old.event_id);
    assert_eq!(rewritten_event.session_id, old.session_id);
    let failure = adapter
        .hydrate_event(&EventHydrationRequest::new(old.event_id, old.locator).unwrap())
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleSourceEvidence);
}

#[test]
fn same_native_event_in_two_conversations_has_distinct_source_session_and_event_ids() {
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
    assert_ne!(
        projected[0].plan.source.identity(),
        projected[1].plan.source.identity()
    );
    assert_ne!(projected[0].plan.session_id, projected[1].plan.session_id);
    assert_ne!(
        projected[0].documents[0].event_id,
        projected[1].documents[0].event_id
    );
}

#[test]
fn duplicate_native_event_id_within_one_conversation_fails_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    write_event(
        &root,
        "conversation-duplicate",
        "0001.json",
        message("same-event", "one"),
    );
    write_event(
        &root,
        "conversation-duplicate",
        "0002.json",
        message("same-event", "two"),
    );

    assert!(matches!(
        project(&root),
        Err(OpenHandsSourceBackedErrorV2::DuplicateEventId {
            conversation_id,
            event_id,
        }) if conversation_id == "conversation-duplicate" && event_id == "same-event"
    ));
}

#[test]
fn exact_file_and_empty_directory_are_authoritative_while_missing_is_unavailable() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    let exact = write_event(
        &root,
        "conversation-exact",
        "event.json",
        message("event", "exact"),
    );
    let adapter = OpenHandsEventFileAdapterV2::new(exact);
    let inventory = adapter.open_inventory().unwrap();
    assert!(inventory.selected_file());
    assert_eq!(inventory.groups().len(), 1);

    let empty = temp.path().join("empty-profile");
    fs::create_dir_all(empty.join("v1_conversations")).unwrap();
    let adapter = OpenHandsEventFileAdapterV2::new(&empty);
    let inventory = adapter.open_inventory().unwrap();
    assert!(inventory.is_empty());
    let complete = adapter.plan_inventory(&inventory).unwrap();
    let complete = complete.complete_inventory();
    assert_eq!(complete.observed_sources(), 0);

    let missing = temp.path().join("missing");
    let error = OpenHandsEventFileAdapterV2::new(missing)
        .open_inventory()
        .unwrap_err();
    assert!(matches!(
        error,
        OpenHandsSourceBackedErrorV2::EventFiles(EventFileInventoryError::Unavailable { .. })
    ));
    assert_eq!(
        openhands_route_error(error).kind,
        crate::provider::source_backed::SourceBackedRouteErrorKind::Unavailable
    );
}

#[test]
fn current_cli_remains_detected_but_unsupported() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let conversation = temp.path().join("conversations").join("current-cli");
    let event = conversation.join("events").join("event-1.json");
    fs::create_dir_all(event.parent().unwrap()).unwrap();
    fs::write(&event, b"{}").unwrap();

    let error = OpenHandsEventFileAdapterV2::new(conversation.clone())
        .open_inventory()
        .unwrap_err();
    assert!(matches!(
        error,
        OpenHandsSourceBackedErrorV2::UnsupportedCurrentCliFormat { .. }
    ));
    let failure = hydration_failure(OpenHandsSourceBackedErrorV2::UnsupportedCurrentCliFormat {
        root: conversation,
    });
    assert_eq!(
        failure.kind,
        HydrationFailureKind::UnsupportedParserRevision
    );
}

#[test]
fn hydrate_two_of_two_thousand_reads_only_requested_bodies_and_preserves_order() {
    const LEAF_COUNT: usize = 2_000;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    for index in 0..LEAF_COUNT {
        write_event(
            &root,
            "conversation-hydrate",
            &format!("{index:04}.json"),
            message(&format!("event-{index:04}"), &format!("body-{index:04}")),
        );
    }
    let projection = project(&root).unwrap().remove(0);
    let requests = [LEAF_COUNT - 1, 0]
        .into_iter()
        .map(|index| &projection.documents[index])
        .map(|document| {
            EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap()
        })
        .collect::<Vec<_>>();
    let batch = BatchHydrationRequest::new(requests.clone()).unwrap();
    let adapter = OpenHandsEventFileAdapterV2::new(root);

    let (result, io) = count_event_file_io(|| adapter.hydrate_batch(&batch).unwrap());
    assert_eq!(io.inventory_opens, 1);
    assert_eq!(io.inventory_walks, 1);
    assert_eq!(io.body_reads, 2);
    assert_eq!(io.leaf_lookups, 2);
    assert_eq!(io.group_digest_builds, 1);
    assert_eq!(io.inventory_digest_builds, 1);
    assert_eq!(io.peak_transient_leaf_handles, 1);
    assert_eq!(io.active_transient_leaf_handles, 0);
    assert_eq!(io.active_transient_directory_handles, 0);
    assert_eq!(
        result
            .records()
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        requests
            .iter()
            .map(EventHydrationRequest::event_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(result.records()[0].provider_bytes, b"body-1999");
    assert_eq!(result.records()[1].provider_bytes, b"body-0000");
}

#[test]
fn grouped_hydration_fails_atomically_and_rejects_source_coordinate_revision_and_digest_tampering()
{
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    write_event(
        &root,
        "conversation-tamper",
        "0001.json",
        message("event-1", "first"),
    );
    let second_path = write_event(
        &root,
        "conversation-tamper",
        "0002.json",
        message("event-2", "second"),
    );
    let projection = project(&root).unwrap().remove(0);
    let requests = projection
        .documents
        .iter()
        .map(|document| {
            EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap()
        })
        .collect::<Vec<_>>();
    fs::write(
        second_path,
        serde_json::to_vec(&message("event-2", "changed")).unwrap(),
    )
    .unwrap();
    let adapter = OpenHandsEventFileAdapterV2::new(root.clone());
    let batch = BatchHydrationRequest::new(requests).unwrap();
    let (failure, io) = count_event_file_io(|| adapter.hydrate_batch(&batch).unwrap_err());
    assert!(matches!(
        failure.kind,
        HydrationFailureKind::StaleSourceEvidence | HydrationFailureKind::StaleRecordEvidence
    ));
    assert_eq!(io.inventory_opens, 1);
    assert_eq!(io.body_reads, 2);
    assert_eq!(io.peak_transient_leaf_handles, 1);
    assert_eq!(io.active_transient_leaf_handles, 0);
    assert_eq!(io.active_transient_directory_handles, 0);

    fs::write(
        root.join("v1_conversations")
            .join("conversation-tamper")
            .join("0002.json"),
        serde_json::to_vec(&message("event-2", "second")).unwrap(),
    )
    .unwrap();
    let projection = project(&root).unwrap().remove(0);
    let document = &projection.documents[0];

    let wrong_digest = SourceRecordLocator::new(
        document.source.clone(),
        document.locator.coordinate().clone(),
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        [0; 32],
    )
    .unwrap();
    let failure = adapter
        .hydrate_event(&EventHydrationRequest::new(document.event_id, wrong_digest).unwrap())
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleRecordEvidence);

    let wrong_revision = source_locator(
        &document.source,
        "0001.json",
        "event-1",
        [0; 32],
        *document.locator.record_digest(),
    )
    .unwrap();
    let failure = adapter
        .hydrate_event(&EventHydrationRequest::new(document.event_id, wrong_revision).unwrap())
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleSourceEvidence);

    let NativeRecordCoordinate::TreeRecord {
        record_coordinate, ..
    } = document.locator.coordinate()
    else {
        panic!("expected tree locator");
    };
    let traversal = SourceRecordLocator::new(
        document.source.clone(),
        NativeRecordCoordinate::TreeRecord {
            relative_file_key: TypedKey::Utf8("../0001.json".to_owned()),
            record_coordinate: record_coordinate.clone(),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        *document.locator.record_digest(),
    )
    .unwrap();
    let failure = adapter
        .hydrate_event(&EventHydrationRequest::new(document.event_id, traversal).unwrap())
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::InvalidLocator);

    let wrong_coordinate = source_locator(
        &document.source,
        "0001.json",
        "different-native-event",
        [0; 32],
        *document.locator.record_digest(),
    )
    .unwrap();
    let failure = adapter
        .hydrate_event(&EventHydrationRequest::new(document.event_id, wrong_coordinate).unwrap())
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::InvalidLocator);

    let wrong_source = SourceKey::derive(
        CaptureProvider::OpenHands.as_str(),
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        "wrong-schema",
        1,
        SourceAnchor::provider_native(
            "openhands.v1-conversation",
            TypedKey::utf8("conversation-tamper").unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let wrong_source_locator = SourceRecordLocator::new(
        wrong_source,
        document.locator.coordinate().clone(),
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        *document.locator.record_digest(),
    )
    .unwrap();
    assert!(matches!(
        validate_locator(&wrong_source_locator),
        Err(OpenHandsSourceBackedErrorV2::InvalidLocator)
    ));
}

#[test]
fn selected_root_swap_cannot_hydrate_old_locator() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    write_event(
        &root,
        "conversation-root-swap",
        "event.json",
        message("stable-event-id", "trusted body"),
    );
    let projection = project(&root).unwrap().remove(0);
    let document = projection.documents[0].clone();

    let displaced = temp.path().join("displaced-profile");
    fs::rename(&root, &displaced).unwrap();
    write_event(
        &root,
        "conversation-root-swap",
        "event.json",
        message("stable-event-id", "trusted body"),
    );

    let failure = OpenHandsEventFileAdapterV2::new(root)
        .hydrate_event(&EventHydrationRequest::new(document.event_id, document.locator).unwrap())
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleSourceEvidence);
}

#[test]
fn deleted_leaf_remains_typed_as_missing_record() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("profile");
    let event = write_event(
        &root,
        "conversation-deleted",
        "event.json",
        message("event-deleted", "before deletion"),
    );
    let document = project(&root).unwrap().remove(0).documents.remove(0);
    fs::remove_file(event).unwrap();

    let failure = OpenHandsEventFileAdapterV2::new(root)
        .hydrate_event(&EventHydrationRequest::new(document.event_id, document.locator).unwrap())
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::MissingRecord);
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
        "observation": {
            "kind": "ExecuteBashObservation",
            "content": body,
            "exit_code": exit_code,
            "timeout": timeout,
        },
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_32(value: &str) -> [u8; 32] {
    assert_eq!(value.len(), 64);
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).unwrap();
    }
    bytes
}
