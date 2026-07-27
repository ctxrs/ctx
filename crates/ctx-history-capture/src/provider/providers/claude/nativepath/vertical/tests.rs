use std::{
    collections::BTreeSet,
    fs::File,
    io::Write,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use serde_json::json;

use super::*;
use crate::{test_support_paths::tempdir, ProOutputPageResult, ProviderImportWorkResult};

const MACHINE: &str = "claude-nativepath-production-test";
const SUCCESS_BODY: &str = "CLAUDE_SUCCESS_BODY_MUST_STAY_OUT_OF_CORE";

#[test]
fn preparation_worker_policy_caps_host_and_source_parallelism() {
    assert_eq!(preparation_worker_count(100, 1), 1);
    assert_eq!(preparation_worker_count(100, 4), 4);
    assert_eq!(preparation_worker_count(100, 32), 16);
    assert_eq!(preparation_worker_count(7, 32), 7);
    assert_eq!(preparation_worker_count(0, 32), 0);
}

#[test]
fn preparation_queue_stays_within_one_publication_group() {
    for workers in 1..=CLAUDE_CORE_PREPARATION_MAX_WORKERS {
        let lane_capacity = preparation_lane_capacity(workers);
        assert!(lane_capacity > 0);
        assert!(lane_capacity.saturating_mul(workers) <= CLAUDE_CORE_PREPARATION_QUEUE_MAX_SOURCES);
    }
    assert_eq!(preparation_lane_capacity(8), 8);
    assert_eq!(preparation_lane_capacity(16), 4);
}

#[test]
fn production_worker_policy_has_no_benchmark_environment_control() {
    let source = include_str!("../vertical.rs");
    let benchmark_controls = [
        ["CTX_NATIVEPATH_BENCH", "_PREP_WORKERS"].concat(),
        ["CTX_CLAUDE_NATIVEPATH", "_PHASE_TIMING"].concat(),
    ];
    for control in benchmark_controls {
        assert!(
            !source.contains(&control),
            "{control} must remain benchmark-only"
        );
    }
}

#[test]
fn pipelined_preparation_propagates_failure_in_source_order() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".claude/projects");
    let project = root.join("-workspace");
    let transcripts = (0..3)
        .map(|index| {
            let session = format!("failure-{index:03}");
            let path = project.join(format!("{session}.jsonl"));
            write_records(&path, &[message(&session, "message", &session)]);
            path
        })
        .collect::<Vec<_>>();
    let discovery = discover_projects(&root).unwrap();
    fs::remove_file(&transcripts[1]).unwrap();
    let store_path = temp.path().join("history.sqlite");
    drop(Store::open(&store_path).unwrap());
    let options = ClaudeProjectsImportOptions {
        machine_id: MACHINE.to_owned(),
        source_path: Some(root),
        imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
        import_profile: ImportProfile::CoreOnly,
        ..ClaudeProjectsImportOptions::default()
    };
    let mut consumed = Vec::new();

    let result = prepare_grouped_core_sources_parallel(
        &store_path,
        &discovery.sessions,
        &options,
        &ClaudePreparationTestHooks::default(),
        |source, _| {
            consumed.push(source.canonical_path.clone());
            Ok(())
        },
    );

    assert!(result.is_err());
    assert_eq!(consumed, vec![discovery.sessions[0].canonical_path.clone()]);
}

#[test]
fn preparation_worker_panic_is_typed_after_all_workers_join() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".claude/projects");
    let project = root.join("-workspace");
    for index in 0..4 {
        let session = format!("panic-{index:03}");
        write_records(
            &project.join(format!("{session}.jsonl")),
            &[message(&session, "message", &session)],
        );
    }
    let discovery = discover_projects(&root).unwrap();
    let store_path = temp.path().join("history.sqlite");
    drop(Store::open(&store_path).unwrap());
    let options = ClaudeProjectsImportOptions {
        machine_id: MACHINE.to_owned(),
        source_path: Some(root),
        imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
        import_profile: ImportProfile::CoreOnly,
        ..ClaudeProjectsImportOptions::default()
    };
    let joined = Arc::new(AtomicUsize::new(0));
    let hooks = ClaudePreparationTestHooks {
        available_parallelism: Some(4),
        panic_worker: Some(1),
        joined_workers: Some(joined.clone()),
        ..ClaudePreparationTestHooks::default()
    };

    let error = prepare_grouped_core_sources_parallel(
        &store_path,
        &discovery.sessions,
        &options,
        &hooks,
        |_, _| Ok(()),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        CaptureError::WorkerPanicked("Claude source preparation")
    ));
    assert_eq!(joined.load(Ordering::Relaxed), 4);
}

#[test]
fn preparation_spawn_failure_cancels_and_joins_started_workers() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".claude/projects");
    let project = root.join("-workspace");
    for index in 0..4 {
        let session = format!("spawn-{index:03}");
        write_records(
            &project.join(format!("{session}.jsonl")),
            &[message(&session, "message", &session)],
        );
    }
    let discovery = discover_projects(&root).unwrap();
    let store_path = temp.path().join("history.sqlite");
    drop(Store::open(&store_path).unwrap());
    let options = ClaudeProjectsImportOptions {
        machine_id: MACHINE.to_owned(),
        source_path: Some(root),
        imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
        import_profile: ImportProfile::CoreOnly,
        ..ClaudeProjectsImportOptions::default()
    };
    let joined = Arc::new(AtomicUsize::new(0));
    let cancelled = Arc::new(AtomicUsize::new(0));
    let hooks = ClaudePreparationTestHooks {
        available_parallelism: Some(4),
        fail_spawn_at: Some(2),
        joined_workers: Some(joined.clone()),
        cancelled_workers: Some(cancelled.clone()),
        wait_for_cancellation: true,
        ..ClaudePreparationTestHooks::default()
    };

    let error = prepare_grouped_core_sources_parallel(
        &store_path,
        &discovery.sessions,
        &options,
        &hooks,
        |_, _| Ok(()),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        CaptureError::SystemIo {
            operation: "Claude source preparation worker spawn",
            ..
        }
    ));
    assert_eq!(joined.load(Ordering::Relaxed), 2);
    assert_eq!(cancelled.load(Ordering::Relaxed), 2);
}

#[test]
fn production_store_lifecycle_is_idempotent_and_retires_disappeared_routes() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".claude/projects");
    let transcript = root.join("-workspace/lifecycle.jsonl");
    write_records(
        &transcript,
        &[
            message("lifecycle", "fresh", "fresh body"),
            success_result("lifecycle", "success-1", SUCCESS_BODY),
        ],
    );
    let store_path = temp.path().join("history.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let fresh = import(&root, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(fresh.imported_sessions, 1);
    let session = claude_session(&store);
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 1);
    assert!(!serde_json::to_string(&events)
        .unwrap()
        .contains(SUCCESS_BODY));
    let routed_event = events[0].id;
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_ok());

    let noop = import(&root, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);
    drop(store);
    let mut store = Store::open(&store_path).unwrap();
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly)
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::NoOp
    );

    append_record(&transcript, &message("lifecycle", "append", "append body"));
    let append = import(&root, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(append.imported_events, 1);

    write_records(
        &transcript,
        &[message("lifecycle", "rewrite", "rewritten generation")],
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly)
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::Changed
    );

    write_records(
        &transcript,
        &[message("lifecycle", "short", "short generation")],
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly)
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::Changed
    );

    let replacement = transcript.with_extension("replacement");
    write_records(
        &replacement,
        &[message(
            "lifecycle",
            "replacement",
            "replacement generation",
        )],
    );
    fs::rename(&replacement, &transcript).unwrap();
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly)
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::Changed
    );

    fs::remove_dir_all(&root).unwrap();
    let disappeared = import(&root, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(disappeared.work_result(), ProviderImportWorkResult::Changed);
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_err());
    assert!(!store.events_for_session(session.id).unwrap().is_empty());
}

#[test]
fn pipelined_root_import_preserves_order_across_group_boundary() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".claude/projects");
    let project = root.join("-workspace");
    let transcripts = (0..CLAUDE_GROUP_MAX_SOURCES + 8)
        .map(|index| {
            let session = format!("pipeline-{index:03}");
            let path = project.join(format!("{session}.jsonl"));
            write_records(&path, &[message(&session, "message", &session)]);
            path
        })
        .collect::<Vec<_>>();
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();

    let summary = import(&root, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(summary.imported_sessions, transcripts.len());
    assert_eq!(summary.imported_events, transcripts.len());

    let publication_ids = transcripts
        .iter()
        .map(|transcript| {
            let canonical = fs::canonicalize(transcript).unwrap();
            let locator = provider_path_identity(&canonical).unwrap();
            let stream = provider_source_cursor_stream_for_path(
                CaptureProvider::Claude,
                CLAUDE_PROJECTS_SOURCE_FORMAT,
                &locator,
            );
            let cursor = store
                .get_sync_cursor(None, MACHINE, &stream)
                .unwrap()
                .unwrap();
            decode_native_path_committed_cursor(&cursor.cursor)
                .unwrap()
                .publication_id()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert!(publication_ids[..CLAUDE_GROUP_MAX_SOURCES]
        .iter()
        .all(|publication| publication == &publication_ids[0]));
    assert!(publication_ids[CLAUDE_GROUP_MAX_SOURCES..]
        .iter()
        .all(|publication| publication == &publication_ids[CLAUDE_GROUP_MAX_SOURCES]));
    assert_ne!(
        publication_ids[0],
        publication_ids[CLAUDE_GROUP_MAX_SOURCES]
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly)
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn one_safe_group_remains_bounded_and_resumable() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".claude/projects");
    let project = root.join("-workspace");
    let first_group_sources = CLAUDE_GROUP_MAX_SOURCES;
    for index in 0..first_group_sources + 8 {
        let session = format!("one-group-{index:03}");
        write_records(
            &project.join(format!("{session}.jsonl")),
            &[
                message(&session, "message-1", &format!("{session}-1")),
                message(&session, "message-2", &format!("{session}-2")),
            ],
        );
    }
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
    let options = || ClaudeProjectsImportOptions {
        machine_id: MACHINE.to_owned(),
        source_path: Some(root.clone()),
        imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
        import_profile: ImportProfile::CoreOnly,
        capture_work_limit: CaptureWorkLimit::OneSafeGroup,
        ..ClaudeProjectsImportOptions::default()
    };

    let first = crate::import_claude_projects_jsonl_tree(&root, &mut store, options()).unwrap();
    assert_eq!(first.imported_sessions, first_group_sources);
    assert_eq!(first.imported_events, first_group_sources * 2);
    assert!(first.imported_events > 64);
    assert!(first.work_remaining);

    let second = crate::import_claude_projects_jsonl_tree(&root, &mut store, options()).unwrap();
    assert_eq!(second.imported_sessions, 8);
    assert_eq!(second.imported_events, 16);
    assert!(second.work_remaining);

    let finished = crate::import_claude_projects_jsonl_tree(&root, &mut store, options()).unwrap();
    assert_eq!(finished.work_result(), ProviderImportWorkResult::NoOp);
    assert!(!finished.work_remaining);
}

#[test]
fn one_safe_group_drains_noop_subagents_before_missing_route_retirement() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".claude/projects");
    let project = root.join("-workspace");
    let primary = project.join("root.jsonl");
    let direct = project.join("root/subagents/agent-direct.jsonl");
    let workflow = project.join("root/subagents/workflows/run-a/agent-keep.jsonl");
    let missing = project.join("root/subagents/workflows/run-b/agent-missing.jsonl");
    write_records(&primary, &[message("root", "primary", "primary")]);
    write_records(&direct, &[message("root", "direct", "direct")]);
    write_records(&workflow, &[message("root", "workflow", "workflow")]);
    write_records(&missing, &[message("root", "missing", "missing")]);
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
    import(&root, &mut store, ImportProfile::CoreOnly).unwrap();
    let missing_event = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.external_agent_id.as_deref() == Some("agent-missing"))
        .and_then(|session| store.events_for_session(session.id).unwrap().pop())
        .unwrap();
    fs::remove_file(&missing).unwrap();
    let options = ClaudeProjectsImportOptions {
        machine_id: MACHINE.to_owned(),
        source_path: Some(root.clone()),
        imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
        import_profile: ImportProfile::CoreOnly,
        capture_work_limit: CaptureWorkLimit::OneSafeGroup,
        ..ClaudeProjectsImportOptions::default()
    };

    let summary = crate::import_claude_projects_jsonl_tree(&root, &mut store, options).unwrap();

    assert!(!summary.work_remaining);
    assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);
    assert!(store
        .authorized_source_route_for_event(missing_event.id)
        .is_err());
}

#[test]
fn rewrite_preserves_surviving_ids_updates_payloads_and_retires_omissions() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".claude/projects");
    let transcript = root.join("-workspace/rewrite-identities.jsonl");
    let tool = json!({
        "sessionId": "rewrite-identities",
        "type": "assistant",
        "uuid": "tool-stable",
        "timestamp": "2026-07-25T12:00:03Z",
        "message": {
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "call-stable",
                "name": "Edit",
                "input": {"path": "src/stable.rs"}
            }]
        }
    });
    write_records(
        &transcript,
        &[
            message("rewrite-identities", "unchanged", "same"),
            message("rewrite-identities", "changed", "before"),
            message("rewrite-identities", "deleted", "remove me"),
            tool.clone(),
        ],
    );
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
    import(&root, &mut store, ImportProfile::CoreOnly).unwrap();
    let session = claude_session(&store);
    let before = store
        .events_for_session(session.id)
        .unwrap()
        .into_iter()
        .map(|event| {
            (
                event.payload["native_record_id"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
                event,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let source_id = session.capture_source_id.unwrap();
    let touch_id = store.export_archive().unwrap().files_touched[0].id;

    write_records(
        &transcript,
        &[
            message("rewrite-identities", "inserted", "new"),
            message("rewrite-identities", "changed", "after"),
            tool,
            message("rewrite-identities", "unchanged", "same"),
        ],
    );
    let summary = import(&root, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);

    let after = store
        .events_for_session(session.id)
        .unwrap()
        .into_iter()
        .map(|event| {
            (
                event.payload["native_record_id"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
                event,
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(after["unchanged"].id, before["unchanged"].id);
    assert!(after["unchanged"].sync.deleted_at.is_none());
    assert_eq!(after["changed"].id, before["changed"].id);
    assert_eq!(after["changed"].payload["body"], "after");
    assert!(after["changed"].sync.deleted_at.is_none());
    assert_eq!(after["deleted"].id, before["deleted"].id);
    assert!(after["deleted"].sync.deleted_at.is_some());
    assert_eq!(after["tool-stable"].id, before["tool-stable"].id);
    assert_eq!(after.len(), 5);
    assert_eq!(claude_session(&store).capture_source_id, Some(source_id));
    let archive = store.export_archive().unwrap();
    let stable_touch = archive
        .files_touched
        .iter()
        .find(|touch| touch.path == "src/stable.rs")
        .unwrap();
    assert_eq!(stable_touch.id, touch_id);
    assert!(stable_touch.sync.deleted_at.is_none());
}

#[test]
fn multi_tool_record_publishes_distinct_touch_identities_and_event_links() {
    const PRIVATE_TOOL_INPUT: &str = "CLAUDE_PRIVATE_TOOL_INPUT_MUST_NOT_PERSIST";

    let temp = tempdir().unwrap();
    let root = temp.path().join(".claude/projects");
    let transcript = root.join("-workspace/multi-tool.jsonl");
    write_records(
        &transcript,
        &[json!({
            "sessionId": "multi-tool",
            "type": "assistant",
            "uuid": "multi-tool-record",
            "timestamp": "2026-07-25T12:00:00Z",
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "call-a",
                        "name": "Edit",
                        "input": {
                            "path": "src/a.rs",
                            "command": PRIVATE_TOOL_INPUT
                        }
                    },
                    {
                        "type": "tool_use",
                        "id": "call-b",
                        "name": "Write",
                        "input": {
                            "path": "src/b.rs",
                            "command": PRIVATE_TOOL_INPUT
                        }
                    }
                ]
            }
        })],
    );
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();

    let summary = import(&root, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);
    let events = store.events_for_session(claude_session(&store).id).unwrap();
    let tool_events = events
        .iter()
        .filter(|event| !event.payload["tool_call"].is_null())
        .collect::<Vec<_>>();
    assert_eq!(tool_events.len(), 2);

    let archive = store.export_archive().unwrap();
    assert_eq!(archive.files_touched.len(), 2);
    assert_eq!(
        archive
            .files_touched
            .iter()
            .map(|touch| touch.id)
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
    assert_eq!(
        archive
            .files_touched
            .iter()
            .filter_map(|touch| touch.event_id)
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
    for (path, call_id) in [("src/a.rs", "call-a"), ("src/b.rs", "call-b")] {
        let event = tool_events
            .iter()
            .find(|event| event.payload["tool_call"]["call_id"] == call_id)
            .unwrap();
        let touch = archive
            .files_touched
            .iter()
            .find(|touch| touch.path == path)
            .unwrap();
        assert_eq!(touch.event_id, Some(event.id));
    }
    assert!(!serde_json::to_string(&archive)
        .unwrap()
        .contains(PRIVATE_TOOL_INPUT));

    let bindings = archive
        .files_touched
        .iter()
        .map(|touch| (touch.id, touch.event_id, touch.path.clone()))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly)
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::NoOp
    );
    assert_eq!(
        store
            .export_archive()
            .unwrap()
            .files_touched
            .iter()
            .map(|touch| (touch.id, touch.event_id, touch.path.clone()))
            .collect::<BTreeSet<_>>(),
        bindings
    );
}

#[test]
fn core_commits_before_output_failure_and_later_activation_replays_success_body() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".claude/projects");
    let transcript = root.join("-workspace/output.jsonl");
    write_records(
        &transcript,
        &[
            message("output", "message-1", "core message"),
            success_result("output", "success-1", SUCCESS_BODY),
            failure_result("output", "failure-1", "failure private body"),
        ],
    );
    let store_path = temp.path().join("history.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let failing = Arc::new(RecordingSink::new(store_path.clone(), true));

    let summary = import(
        &root,
        &mut store,
        ImportProfile::CoreAndPro(failing.clone()),
    )
    .unwrap();
    assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);
    assert!(failing.saw_core_before_page.load(Ordering::SeqCst));
    assert_eq!(failing.behind.load(Ordering::SeqCst), 1);
    let events = store.events_for_session(claude_session(&store).id).unwrap();
    let serialized = serde_json::to_string(&events).unwrap();
    assert!(!serialized.contains(SUCCESS_BODY));
    assert!(!serialized.contains("failure private body"));

    let replay = Arc::new(RecordingSink::new(store_path, false));
    let replay_summary =
        import(&root, &mut store, ImportProfile::CoreAndPro(replay.clone())).unwrap();
    assert_eq!(replay_summary.work_result(), ProviderImportWorkResult::NoOp);
    assert!(replay.pages.load(Ordering::SeqCst) > 0);
    assert!(replay
        .bodies
        .lock()
        .unwrap()
        .iter()
        .any(|body| body.as_slice() == SUCCESS_BODY.as_bytes()));

    let pages_before_append = replay.pages.load(Ordering::SeqCst);
    append_record(
        &transcript,
        &success_result("output", "success-2", "later output"),
    );
    let append = import(&root, &mut store, ImportProfile::CoreAndPro(replay.clone())).unwrap();
    assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
    assert!(replay.pages.load(Ordering::SeqCst) > pages_before_append);
    assert!(replay
        .bodies
        .lock()
        .unwrap()
        .iter()
        .any(|body| body.as_slice() == b"later output"));
}

#[test]
fn corrupt_and_incomplete_input_advance_only_certified_complete_boundaries() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".claude/projects");
    let transcript = root.join("-workspace/incomplete.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    let mut file = File::create(&transcript).unwrap();
    writeln!(file, "{}", message("incomplete", "valid", "valid body")).unwrap();
    writeln!(file, "{{malformed").unwrap();
    write!(file, "{}", message("incomplete", "tail", "incomplete tail")).unwrap();
    file.flush().unwrap();
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();

    let first = import(&root, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(first.failed, 1);
    assert_eq!(
        store
            .events_for_session(claude_session(&store).id)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly)
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::NoOp
    );

    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(file).unwrap();
    file.flush().unwrap();
    let completed = import(&root, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(completed.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(
        store
            .events_for_session(claude_session(&store).id)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn only_exact_released_cursor_is_reset_then_native_retry_is_idempotent() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".claude/projects");
    let transcript = root.join("-workspace/migration.jsonl");
    write_records(
        &transcript,
        &[message("migration", "message-1", "migration body")],
    );
    let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
    import(&root, &mut store, ImportProfile::CoreOnly).unwrap();

    let canonical = fs::canonicalize(&transcript).unwrap();
    let locator = provider_path_identity(&canonical).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Claude,
        CLAUDE_PROJECTS_SOURCE_FORMAT,
        &locator,
    );
    let mut cursor = store
        .get_sync_cursor(None, MACHINE, &stream)
        .unwrap()
        .unwrap();
    cursor.cursor = r#"{"released":"captured-batch-cursor"}"#.to_owned();
    store.upsert_sync_cursor(&cursor).unwrap();
    assert!(import(&root, &mut store, ImportProfile::CoreOnly).is_err());

    cursor.cursor = CertifiedProviderCursor::new(
        "released-claude-source-revision",
        CLAUDE_RELEASED_CAPTURE_REVISION,
        CLAUDE_RELEASED_POLICY_REVISION,
        crate::provider::importer::released_jsonl_initial_position_for_test(),
        crate::provider::importer::BoundedParserCheckpoint::from_serializable(
            &ReleasedClaudeParserCheckpoint {
                session: None,
                next_ordinal: 0,
                accepted_captures: 0,
                accepted_events: 0,
                accepted_file_touches: 0,
                rejected_records: 0,
            },
        )
        .unwrap(),
    )
    .unwrap()
    .encode()
    .unwrap();
    store.upsert_sync_cursor(&cursor).unwrap();
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly)
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(matches!(
        decode_store_cursor(
            &store
                .get_sync_cursor(None, MACHINE, &stream)
                .unwrap()
                .unwrap()
                .cursor
        )
        .unwrap(),
        ClaudeStoredCursor::Native(_)
    ));
    assert_eq!(
        import(&root, &mut store, ImportProfile::CoreOnly)
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::NoOp
    );
}

struct RecordingSink {
    store_path: PathBuf,
    fail: AtomicBool,
    progress: Mutex<Option<ProOutputProgress>>,
    pages: AtomicUsize,
    behind: AtomicUsize,
    saw_core_before_page: AtomicBool,
    bodies: Mutex<Vec<Vec<u8>>>,
}

impl RecordingSink {
    fn new(store_path: PathBuf, fail: bool) -> Self {
        Self {
            store_path,
            fail: AtomicBool::new(fail),
            progress: Mutex::new(None),
            pages: AtomicUsize::new(0),
            behind: AtomicUsize::new(0),
            saw_core_before_page: AtomicBool::new(false),
            bodies: Mutex::new(Vec::new()),
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "claude-nativepath-test-materializer-v1"
    }

    fn observe_source(
        &self,
        _source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(self.progress.lock().unwrap().clone())
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        let core = Store::open_read_only(&self.store_path)
            .map_err(|error| ProOutputSinkError::new("test_store", error.to_string()))?;
        if core
            .list_sessions()
            .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
            .iter()
            .any(|session| session.provider == CaptureProvider::Claude)
        {
            self.saw_core_before_page.store(true, Ordering::SeqCst);
        }
        if self.fail.swap(false, Ordering::SeqCst) {
            return Err(ProOutputSinkError::new(
                "intentional_test_failure",
                "intentional output failure",
            ));
        }
        self.pages.fetch_add(1, Ordering::SeqCst);
        self.bodies.lock().unwrap().extend(
            page.observations
                .iter()
                .map(|observation| observation.content.clone()),
        );
        let committed_cursor = page.next_safe_cursor.clone();
        *self.progress.lock().unwrap() = Some(ProOutputProgress {
            source_epoch: page.source_epoch,
            observed_revision: page.observed_revision.clone(),
            cursor: Some(committed_cursor.clone()),
            parser_revision: page.parser_revision.clone(),
            materializer_revision: page.materializer_revision.clone(),
            terminal: page.terminal,
        });
        Ok(ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor,
            accepted_outputs: u32::try_from(page.observations.len()).unwrap(),
            materialized_facts: 0,
            replayed: false,
        })
    }

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.fetch_add(1, Ordering::SeqCst);
    }
}

fn import(root: &Path, store: &mut Store, profile: ImportProfile) -> Result<ProviderImportSummary> {
    crate::import_claude_projects_jsonl_tree(
        root,
        store,
        ClaudeProjectsImportOptions {
            machine_id: MACHINE.to_owned(),
            source_path: Some(root.to_path_buf()),
            imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
            import_profile: profile,
            ..ClaudeProjectsImportOptions::default()
        },
    )
}

fn claude_session(store: &Store) -> Session {
    store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| {
            session.provider == CaptureProvider::Claude
                && session.role_hint.as_deref() != Some("relationship_placeholder")
        })
        .unwrap()
}

fn write_records(path: &Path, records: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = File::create(path).unwrap();
    for record in records {
        writeln!(file, "{record}").unwrap();
    }
    file.flush().unwrap();
}

fn append_record(path: &Path, record: &Value) {
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{record}").unwrap();
    file.flush().unwrap();
}

fn message(session: &str, uuid: &str, body: &str) -> Value {
    json!({
        "sessionId": session,
        "type": "user",
        "uuid": uuid,
        "timestamp": "2026-07-25T12:00:00Z",
        "cwd": "/workspace/project",
        "version": "2.1.219",
        "gitBranch": "main",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": body}]
        }
    })
}

fn success_result(session: &str, uuid: &str, body: &str) -> Value {
    json!({
        "sessionId": session,
        "type": "user",
        "uuid": uuid,
        "timestamp": "2026-07-25T12:00:01Z",
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call-success",
                "content": body
            }]
        },
        "toolUseResult": {"exitCode": 0}
    })
}

fn failure_result(session: &str, uuid: &str, body: &str) -> Value {
    json!({
        "sessionId": session,
        "type": "user",
        "uuid": uuid,
        "timestamp": "2026-07-25T12:00:02Z",
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call-failure",
                "content": body
            }]
        },
        "toolUseResult": {"exitCode": 7}
    })
}
