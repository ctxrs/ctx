use super::super::lifecycle;
use super::*;

#[test]
fn protobuf_oneofs_are_last_wins_at_message_and_result_boundaries() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-oneof", None, "Oneof");

    let unknown_last = message(
        "message-unknown",
        "task-oneof",
        "request-unknown",
        1,
        &[
            text_arm(3, "must be suppressed"),
            field(17, &field(1, b"future arm")),
        ],
    );
    let result_then_assistant = message(
        "message-retained",
        "task-oneof",
        "request-retained",
        2,
        &[
            tool_result_arm("request-retained", b"stale output"),
            text_arm(3, "last assistant wins"),
        ],
    );
    let assistant_then_result = message(
        "message-output",
        "task-oneof",
        "request-output",
        3,
        &[
            text_arm(3, "stale assistant"),
            tool_result_arm("request-output", b"selected output"),
        ],
    );
    let mut run_shell = field(5, &field(1, b"stale success output"));
    run_shell.extend(field(6, &[]));
    let mut failure_result = field(1, b"request-failure");
    failure_result.extend(field(2, &run_shell));
    let nested_failure = message(
        "message-failure",
        "task-oneof",
        "request-failure",
        4,
        &[field(5, &failure_result)],
    );
    insert_task(
        &conn,
        "conversation-oneof",
        "task-oneof",
        &[
            unknown_last,
            result_then_assistant,
            assistant_then_result,
            nested_failure,
        ],
    );
    drop(conn);

    let (authority, sink) = scan(&path);
    assert_eq!(sink.events().len(), 2);
    assert_eq!(sink.events()[0].body, "last assistant wins");
    let resolver_task = task(
        "task-oneof",
        &[
            message(
                "message-unknown",
                "task-oneof",
                "request-unknown",
                1,
                &[
                    text_arm(3, "must be suppressed"),
                    field(17, &field(1, b"future arm")),
                ],
            ),
            message(
                "message-retained",
                "task-oneof",
                "request-retained",
                2,
                &[
                    tool_result_arm("request-retained", b"stale output"),
                    text_arm(3, "last assistant wins"),
                ],
            ),
        ],
    );
    let reopened = super::super::super::warp_message_content_at(
        &resolver_task,
        "conversation-oneof",
        "task-oneof",
        1,
    )
    .unwrap()
    .unwrap();
    assert_eq!(reopened.text, sink.events()[0].body);
    assert_eq!(
        reopened.normalized_payload_hash.as_deref(),
        Some(sink.events()[0].content_hash.as_str())
    );
    assert_eq!(sink.events()[1].body, "tool result: run_shell_command");
    assert_eq!(
        sink.events()[1].result_outcome,
        Some(crate::OutputOutcome::Failure)
    );
    assert_eq!(sink.events()[1].call_id.as_deref(), Some("request-failure"));
    assert_eq!(authority.counters.unknown_oneofs, 1);
    assert_eq!(authority.counters.native_result_records, 2);
    assert_eq!(authority.counters.native_results_success, 1);
    assert_eq!(authority.counters.native_results_failure, 1);
    assert_eq!(
        authority.counters.native_result_body_bytes_observed,
        b"selected output".len() as u64
    );
}

#[test]
fn empty_source_and_pre_certification_mutation_publish_zero_pages() {
    let directory = tempdir().unwrap();
    let empty_path = directory.path().join("empty.sqlite");
    let conn = Connection::open(&empty_path).unwrap();
    create_schema(&conn);
    drop(conn);
    let (empty, sink) = scan(&empty_path);
    assert!(empty.source_complete);
    assert!(empty.zero_authoritative_rows);
    assert!(!empty.has_useful_content);
    assert!(sink.pages.is_empty());
    assert!(empty.persisted_state.checkpoint_is_terminal());
    let empty_frontier = empty.persisted_state.checkpoint_frontier();
    assert_eq!(empty_frontier.phase, WarpNativeFrontierPhase::Start);
    assert_eq!(empty_frontier.completed_conversation_rows, 0);
    assert_eq!(empty_frontier.completed_task_rows, 0);
    assert_eq!(empty_frontier.retained_events, 0);
    assert_ne!(empty_frontier.source_digest, [0; 32]);
    assert_ne!(empty_frontier.core_digest, [0; 32]);
    assert!(matches!(
        prepare_warp_nativepath_lifecycle(
            &empty_path,
            std::slice::from_ref(empty.persisted_state.as_ref())
        ),
        WarpNativePreparationOutcome::ExactNoOp { .. }
    ));
    let encoded_empty = serde_json::to_vec(&empty.persisted_state).unwrap();
    let persisted_empty: lifecycle::WarpNativePersistedState =
        serde_json::from_slice(&encoded_empty).unwrap();
    assert!(!persisted_empty.checkpoint_is_terminal());
    let restarted_empty = match prepare_warp_nativepath_lifecycle(&empty_path, &[persisted_empty]) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        WarpNativePreparationOutcome::ExactNoOp { .. } => {
            panic!("persisted empty EOF crossed the runtime authority boundary")
        }
        _ => panic!("persisted empty EOF did not prepare for recertification"),
    };
    assert_eq!(
        restarted_empty.inputs.action,
        lifecycle::WarpNativePreparationAction::ResumeExactSnapshot
    );
    let mut restarted_empty_sink = CollectingSink::default();
    let recertified_empty = complete(
        scan_prepared_warp_nativepath(
            *restarted_empty,
            WarpNativeProfile::CoreOnly,
            &mut restarted_empty_sink,
        )
        .unwrap(),
    );
    assert!(restarted_empty_sink.pages.is_empty());
    assert!(recertified_empty.persisted_state.checkpoint_is_terminal());
    assert_eq!(
        recertified_empty.persisted_state.checkpoint_frontier(),
        empty_frontier
    );

    let changed_path = directory.path().join("changed.sqlite");
    let conn = Connection::open(&changed_path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-change", None, "Before");
    insert_task(
        &conn,
        "conversation-change",
        "task-change",
        &[message(
            "message-change",
            "task-change",
            "request-change",
            1,
            &[text_arm(2, "must not publish")],
        )],
    );
    drop(conn);
    let mut sink = CollectingSink::default();
    let hook_path = changed_path.clone();
    let outcome = scan_warp_nativepath_with_certification_hook(
        &changed_path,
        WarpNativeProfile::CoreOnly,
        &mut sink,
        || {
            let conn = Connection::open(&hook_path)?;
            conn.execute(
                "update agent_conversations
             set conversation_data = '{\"agent_name\":\"After\"}'
             where conversation_id = 'conversation-change'",
                [],
            )?;
            Ok(())
        },
    )
    .unwrap();
    let WarpNativeScanOutcome::Incomplete(incomplete) = outcome else {
        panic!("mutated source was incorrectly marked complete");
    };
    assert!(!incomplete.source_complete);
    assert_eq!(
        incomplete.reason,
        publication::WarpNativeIncompleteReason::SnapshotCertificationRace
    );
    assert_eq!(incomplete.pages_emitted, 0);
    assert_eq!(incomplete.pro_output_pages_emitted, 0);
    assert!(sink.pages.is_empty());
    assert!(sink.pro_pages.is_empty());
}

#[test]
fn preparation_exact_noop_and_nonterminal_resume_are_store_ready() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-1", None, "Baseline");
    insert_task(
        &conn,
        "conversation-1",
        "task-1",
        &[message(
            "message-1",
            "task-1",
            "request-1",
            1,
            &[text_arm(2, "baseline")],
        )],
    );
    drop(conn);

    let (baseline, _) = scan(&path);
    let partial = match prepare_warp_nativepath_lifecycle(
        &path,
        std::slice::from_ref(baseline.persisted_state.as_ref()),
    ) {
        WarpNativePreparationOutcome::ExactNoOp {
            inputs,
            persisted_state,
        } => {
            assert_eq!(
                inputs.action,
                lifecycle::WarpNativePreparationAction::ExactNoOp
            );
            assert_eq!(persisted_state, baseline.persisted_state);
            let partial = inputs
                .persisted_state_at(persisted_state.checkpoint_frontier().clone())
                .unwrap();
            assert!(!partial.checkpoint_is_terminal());
            partial
        }
        _ => panic!("terminal exact generation did not prepare as an exact no-op"),
    };

    match prepare_warp_nativepath_lifecycle(&path, std::slice::from_ref(&partial)) {
        WarpNativePreparationOutcome::Ready(prepared) => {
            assert_eq!(
                prepared.inputs.action,
                lifecycle::WarpNativePreparationAction::ResumeExactSnapshot
            );
            assert_eq!(
                prepared.inputs.resume_frontier.as_ref(),
                Some(partial.checkpoint_frontier())
            );
        }
        _ => panic!("non-terminal exact generation did not prepare for bounded resume"),
    }
}

#[test]
fn lifecycle_certified_snapshot_remains_publishable_after_live_source_changes() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-1", None, "Before");
    insert_task(&conn, "conversation-1", "task-1", &[]);
    drop(conn);

    let prepared = match prepare_warp_nativepath_lifecycle(&path, &[]) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        _ => panic!("fresh Warp source did not produce a frozen preparation"),
    };
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "update agent_conversations
         set conversation_data = '{\"agent_name\":\"After\"}'
         where conversation_id = 'conversation-1'",
        [],
    )
    .unwrap();
    drop(conn);

    let mut sink = CollectingSink::default();
    let authority = complete(
        scan_prepared_warp_nativepath(*prepared, WarpNativeProfile::CoreOnly, &mut sink).unwrap(),
    );
    assert!(authority.source_complete);
    assert!(!sink.pages.is_empty());
    assert_eq!(sink.sessions()[0].title, "Before");
}

#[test]
fn preparation_schema_and_index_drift_are_typed_and_never_resume_stale_authority() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-1", None, "Schema");
    insert_task(&conn, "conversation-1", "task-1", &[]);
    drop(conn);
    let (baseline, _) = scan(&path);

    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();
    drop(conn);
    let prepared = match prepare_warp_nativepath_lifecycle(
        &path,
        std::slice::from_ref(baseline.persisted_state.as_ref()),
    ) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        _ => panic!("compatible schema drift did not produce an authoritative snapshot"),
    };
    assert_eq!(
        prepared.inputs.action,
        lifecycle::WarpNativePreparationAction::AuthoritativeScan
    );
    assert_ne!(
        prepared.inputs.capability_digest,
        baseline.persisted_state.capability_digest
    );

    let incompatible_path = directory.path().join("incompatible.sqlite");
    let conn = Connection::open(&incompatible_path).unwrap();
    conn.execute_batch(
        "create table agent_conversations (
             conversation_id text not null,
             conversation_data text not null,
             last_modified_at text not null
         );
         create table agent_tasks (
             conversation_id text not null,
             task_id text not null,
             task blob not null,
             last_modified_at text not null
         );",
    )
    .unwrap();
    drop(conn);
    match prepare_warp_nativepath_lifecycle(&incompatible_path, &[]) {
        WarpNativePreparationOutcome::Failed(failure) => assert_eq!(
            failure.kind,
            lifecycle::WarpNativeSourceFailureKind::SchemaIncompatible
        ),
        _ => panic!("missing Warp keyset index was not a typed schema failure"),
    }
}

#[test]
fn preparation_and_scan_failures_remain_narrowly_typed() {
    let directory = tempdir().unwrap();
    let missing = directory.path().join("missing.sqlite");
    match prepare_warp_nativepath_lifecycle(&missing, &[]) {
        WarpNativePreparationOutcome::Failed(failure) => assert_eq!(
            failure.kind,
            lifecycle::WarpNativeSourceFailureKind::NotFound
        ),
        _ => panic!("missing Warp source was not typed"),
    }

    let corrupt = directory.path().join("corrupt.sqlite");
    fs::write(&corrupt, b"not a sqlite database").unwrap();
    match prepare_warp_nativepath_lifecycle(&corrupt, &[]) {
        WarpNativePreparationOutcome::Failed(failure) => {
            assert_eq!(
                failure.kind,
                lifecycle::WarpNativeSourceFailureKind::Corrupt
            );
        }
        _ => panic!("corrupt Warp source was not typed"),
    }

    let locked = lifecycle::WarpNativeSourceFailure::from_capture(
        &corrupt,
        CaptureError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database is locked".to_owned()),
        )),
        false,
    );
    assert_eq!(locked.kind, lifecycle::WarpNativeSourceFailureKind::Locked);

    let path = directory.path().join("rows.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-1", None, "Rows");
    insert_task(&conn, "conversation-1", "task-1", &[]);
    conn.execute(
        "insert into agent_tasks
         (conversation_id, task_id, task, last_modified_at)
         values ('conversation-1', 'task-bad', x'0A2078', '2026-07-24 12:00:01')",
        [],
    )
    .unwrap();
    drop(conn);
    let (_, sink) = scan(&path);
    assert_eq!(sink.rejection_count(), 1);

    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "insert into agent_tasks
         (conversation_id, task_id, task, last_modified_at)
         values ('conversation-1', 'task-incomplete', 7, '2026-07-24 12:00:01')",
        [],
    )
    .unwrap();
    drop(conn);
    let (_, sink) = scan(&path);
    assert_eq!(sink.rejection_count(), 2);
}

#[test]
fn persisted_checkpoint_is_bounded_and_has_no_artifact_authority() {
    const TASKS: usize = 512;

    let directory = tempdir().unwrap();
    let path = directory.path().join("bounded.sqlite");
    let mut conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    let transaction = conn.transaction().unwrap();
    insert_conversation(&transaction, "conversation-1", None, "Bounded");
    for index in 0..TASKS {
        insert_task(
            &transaction,
            "conversation-1",
            &format!("task-{index:04}"),
            &[],
        );
    }
    transaction.commit().unwrap();
    drop(conn);

    let (authority, _) = scan(&path);
    let encoded = serde_json::to_vec(&authority.persisted_state).unwrap();
    assert!(encoded.len() < lifecycle::WARP_NATIVE_PERSISTED_STATE_MAX_BYTES);
    let decoded: lifecycle::WarpNativePersistedState = serde_json::from_slice(&encoded).unwrap();
    assert!(!decoded.checkpoint_is_terminal());
    assert_eq!(
        decoded.checkpoint_frontier(),
        authority.persisted_state.checkpoint_frontier()
    );
    assert!(decoded.is_supported());
    assert!(authority
        .persisted_state
        .checkpoint_frontier()
        .last_task_rowid
        .is_some());

    let json: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        json["checkpoint"]["inventory"]["task_rows"].as_u64(),
        Some(TASKS as u64)
    );
    let checkpoint = json["checkpoint"].as_object().unwrap();
    assert!(!checkpoint.contains_key("keyset"));
    assert!(!checkpoint.contains_key("exact_evidence_sha256"));
    assert!(!json.as_object().unwrap().contains_key("artifact_path"));
    assert!(!json.as_object().unwrap().contains_key("evidence_path"));

    for digest_field in ["source_integrity_digest", "core_generation_digest"] {
        let mut mismatched = json.clone();
        let digest = mismatched[digest_field].as_str().unwrap();
        let first = if digest.starts_with('0') { "1" } else { "0" };
        mismatched[digest_field] = serde_json::Value::String(format!("{first}{}", &digest[1..]));
        let mismatched: lifecycle::WarpNativePersistedState =
            serde_json::from_value(mismatched).unwrap();
        assert!(
            !mismatched.is_supported(),
            "{digest_field} was not tied to its checkpoint frontier bytes"
        );
    }
}

#[test]
fn giant_provider_key_never_enters_bounded_durable_state() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("giant-key.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-1", None, "Giant key");
    let giant_task_id = "k".repeat(200_000);
    insert_task(&conn, "conversation-1", &giant_task_id, &[]);
    drop(conn);

    let (authority, sink) = scan(&path);
    assert!(authority.source_complete);
    assert!(!sink.pages.is_empty());
    let encoded = serde_json::to_vec(&authority.persisted_state).unwrap();
    assert!(encoded.len() < lifecycle::WARP_NATIVE_PERSISTED_STATE_MAX_BYTES);
    assert!(!encoded
        .windows(giant_task_id.len())
        .any(|window| window == giant_task_id.as_bytes()));
    assert!(authority
        .persisted_state
        .checkpoint_frontier()
        .last_task_rowid
        .is_some());
}

#[test]
fn unrepresentable_cursor_key_returns_compatibility_before_sink_mutation() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("cursor-key.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-1", None, "Cursor key");
    conn.execute(
        "insert into agent_tasks
         (id, conversation_id, task_id, task, last_modified_at)
         values (-1, 'conversation-1', 'negative-rowid', ?1, '2026-07-24 12:00:01')",
        [task("negative-rowid", &[])],
    )
    .unwrap();
    drop(conn);

    let mut sink = CollectingSink::default();
    let error = scan_warp_nativepath(&path, &mut sink).unwrap_err();
    assert!(error.to_string().contains("positive 64-bit source rowids"));
    assert!(sink.pages.is_empty());
    assert!(sink.pro_pages.is_empty());
    match prepare_warp_nativepath_lifecycle(&path, &[]) {
        WarpNativePreparationOutcome::Failed(failure) => assert_eq!(
            failure.kind,
            lifecycle::WarpNativeSourceFailureKind::SchemaIncompatible
        ),
        _ => panic!("unrepresentable Warp cursor key was not a typed compatibility failure"),
    }
}

#[test]
fn local_scale_scan_stays_within_page_bounds() {
    const SESSIONS: usize = 80;
    const MESSAGES_PER_SESSION: usize = 100;

    let directory = tempdir().unwrap();
    let path = directory.path().join("scale.sqlite");
    let mut conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    let transaction = conn.transaction().unwrap();
    for session_index in 0..SESSIONS {
        let conversation_id = format!("conversation-{session_index:04}");
        let parent = (session_index > 0).then_some("conversation-0000");
        insert_conversation(&transaction, &conversation_id, parent, "Scale");
        let task_id = format!("task-{session_index:04}");
        let mut messages = Vec::with_capacity(MESSAGES_PER_SESSION);
        for message_index in 0..MESSAGES_PER_SESSION {
            let sequence = session_index * MESSAGES_PER_SESSION + message_index;
            let arm = if message_index % 10 == 9 {
                tool_result_arm(&format!("request-{sequence:08}"), &[b'x'; 128])
            } else if message_index % 10 == 8 {
                tool_call_arm(2)
            } else if message_index % 2 == 0 {
                text_arm(2, &format!("user {sequence:08}"))
            } else {
                text_arm(3, &format!("assistant {sequence:08}"))
            };
            messages.push(message(
                &format!("message-{sequence:08}"),
                &task_id,
                &format!("request-{sequence:08}"),
                sequence as u64,
                &[arm],
            ));
        }
        insert_task(&transaction, &conversation_id, &task_id, &messages);
    }
    transaction.commit().unwrap();
    drop(conn);

    let (authority, sink) = scan(&path);
    let total_messages = (SESSIONS * MESSAGES_PER_SESSION) as u64;
    let excluded = (SESSIONS * (MESSAGES_PER_SESSION / 10)) as u64;
    assert_eq!(authority.counters.task_rows, SESSIONS as u64);
    assert_eq!(authority.counters.native_result_records, excluded);
    assert_eq!(
        authority.counters.retained_events,
        total_messages - excluded
    );
    assert!(sink.pages.len() > 1);
    assert!(sink.pages.iter().all(|page| {
        page.row_count() <= WARP_NATIVE_PAGE_MAX_ROWS
            && page.estimated_bytes <= WARP_NATIVE_PAGE_MAX_BYTES
    }));
}

#[test]
fn hundred_thousand_events_keep_identity_retention_task_local() {
    const TASKS: usize = 1_001;
    const MESSAGES_PER_TASK: usize = 100;
    const EVENTS: usize = TASKS * MESSAGES_PER_TASK;

    let directory = tempdir().unwrap();
    let path = directory.path().join("identity-scale.sqlite");
    let mut conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    let transaction = conn.transaction().unwrap();
    insert_conversation(&transaction, "conversation-scale", None, "Identity Scale");
    for task_index in 0..TASKS {
        let task_id = format!("task-{task_index:04}");
        let mut messages = Vec::with_capacity(MESSAGES_PER_TASK);
        for message_index in 0..MESSAGES_PER_TASK {
            messages.push(message(
                &format!("message-{message_index:03}"),
                &task_id,
                &format!("request-{message_index:03}"),
                (task_index * MESSAGES_PER_TASK + message_index) as u64,
                &[text_arm(
                    if message_index % 2 == 0 { 2 } else { 3 },
                    "bounded identity event",
                )],
            ));
        }
        insert_task(&transaction, "conversation-scale", &task_id, &messages);
    }
    transaction.commit().unwrap();
    drop(conn);

    let mut sink = DiscardingSink::default();
    let authority = complete(scan_warp_nativepath(&path, &mut sink).unwrap());

    assert_eq!(authority.counters.task_rows, TASKS as u64);
    assert_eq!(authority.counters.retained_events, EVENTS as u64);
    assert_eq!(
        authority.counters.peak_task_identity_entries,
        MESSAGES_PER_TASK as u64
    );
    assert_eq!(authority.counters.hierarchy_nodes_retained, 1);
    assert_eq!(authority.counters.peak_session_metadata_rows, 1);
    assert_eq!(sink.sessions, 1);
    assert_eq!(sink.events, EVENTS);
    assert_eq!(sink.rejections, 0);
    assert!(sink.pages > 1);
    assert!(sink.max_page_rows <= WARP_NATIVE_PAGE_MAX_ROWS);
    assert!(sink.max_page_bytes <= WARP_NATIVE_PAGE_MAX_BYTES);
}
