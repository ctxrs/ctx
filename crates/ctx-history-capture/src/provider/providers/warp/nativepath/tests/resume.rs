use super::super::lifecycle;
use super::*;

#[test]
fn persisted_terminal_forgery_replays_exact_suffix_and_retries_failed_page_idempotently() {
    const TASKS: usize = 5;
    const MESSAGES_PER_TASK: usize = 70;

    let directory = tempdir().unwrap();
    let path = directory.path().join("restart.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    let mut task_rowids = Vec::new();
    for task_index in 0..TASKS {
        let conversation_id = format!("conversation-restart-{task_index:02}");
        insert_conversation(&conn, &conversation_id, None, "Restart");
        let task_id = format!("task-restart-{task_index:02}");
        let messages = (0..MESSAGES_PER_TASK)
            .map(|message_index| {
                let sequence = task_index * MESSAGES_PER_TASK + message_index;
                message(
                    &format!("message-{sequence:03}"),
                    &task_id,
                    &format!("request-{sequence:03}"),
                    sequence as u64,
                    &[text_arm(2, &format!("restart body {sequence:03}"))],
                )
            })
            .collect::<Vec<_>>();
        insert_task(&conn, &conversation_id, &task_id, &messages);
        task_rowids.push(conn.last_insert_rowid());
    }
    drop(conn);

    let (full_authority, full_sink) = scan(&path);
    assert!(full_sink.pages.len() > 3);

    let prepared = match prepare_warp_nativepath_lifecycle(&path, &[]) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        _ => panic!("fresh source did not produce a certified snapshot"),
    };
    let preparation_inputs = prepared.inputs.clone();
    let mut crashing = CrashSink::new(3);
    let error =
        scan_prepared_warp_nativepath(*prepared, WarpNativeProfile::CoreOnly, &mut crashing)
            .unwrap_err();
    assert!(error
        .to_string()
        .contains("injected Warp page commit crash"));
    assert_eq!(crashing.committed.len(), 2);
    let attempted = crashing.attempted.clone().unwrap();
    let committed_frontier = crashing
        .committed
        .last()
        .unwrap()
        .next_safe_frontier
        .clone();
    assert_eq!(attempted.expected_frontier, committed_frontier);
    assert!(committed_frontier.next_message_ordinal > 0);
    assert_eq!(committed_frontier.last_task_rowid, Some(task_rowids[1]));
    let partial_state = preparation_inputs
        .persisted_state_at(committed_frontier.clone())
        .unwrap();
    assert!(!partial_state.checkpoint_is_terminal());
    assert_eq!(partial_state.checkpoint_frontier(), &committed_frontier);
    let mut forged_wire = serde_json::to_value(&partial_state).unwrap();
    assert_eq!(forged_wire["checkpoint"]["terminal"], false);
    forged_wire["checkpoint"]["terminal"] = serde_json::Value::Bool(true);
    let persisted_partial: lifecycle::WarpNativePersistedState =
        serde_json::from_value(forged_wire).unwrap();
    assert!(!persisted_partial.checkpoint_is_terminal());
    assert_eq!(persisted_partial.checkpoint_frontier(), &committed_frontier);

    let resumed = match prepare_warp_nativepath_lifecycle(&path, &[persisted_partial]) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        WarpNativePreparationOutcome::ExactNoOp { .. } => {
            panic!("forged persisted terminal authority produced an exact no-op")
        }
        _ => panic!("exact partial snapshot did not prepare for resume"),
    };
    assert_eq!(
        resumed.inputs.action,
        lifecycle::WarpNativePreparationAction::ResumeExactSnapshot
    );
    query::start_native_task_hydration_trace();
    let mut suffix_sink = CollectingSink::default();
    let resumed_authority = complete(
        scan_prepared_warp_nativepath(*resumed, WarpNativeProfile::CoreOnly, &mut suffix_sink)
            .unwrap(),
    );
    let hydrated = query::take_native_task_hydration_trace();

    assert_eq!(hydrated, task_rowids[1..].to_vec());
    assert_eq!(
        suffix_sink.pages.len(),
        full_sink.pages.len() - crashing.committed.len()
    );
    assert_eq!(
        suffix_sink
            .pages
            .iter()
            .map(|page| page.identity)
            .collect::<Vec<_>>(),
        full_sink
            .pages
            .iter()
            .skip(crashing.committed.len())
            .map(|page| page.identity)
            .collect::<Vec<_>>()
    );
    let committed_events = crashing
        .committed
        .iter()
        .map(|page| page.events.len())
        .sum::<usize>();
    assert_eq!(
        suffix_sink.events().len(),
        full_sink.events().len() - committed_events
    );
    assert_eq!(
        suffix_sink.pages.first().unwrap().identity,
        attempted.identity
    );
    assert_eq!(
        suffix_sink.pages.first().unwrap().next_safe_frontier,
        attempted.next_frontier
    );
    assert_eq!(
        resumed_authority.counters.retained_events,
        suffix_sink.events().len() as u64
    );
    assert_eq!(resumed_authority.counters.task_rows, (TASKS - 1) as u64);
    assert!(resumed_authority.counters.task_rows < full_authority.counters.task_rows);
    assert_eq!(
        resumed_authority.counters.conversation_json_objects_parsed,
        (TASKS - 1) as u64
    );
    assert!(
        resumed_authority.counters.conversation_json_objects_parsed
            < full_authority.counters.conversation_json_objects_parsed
    );
    assert_eq!(
        resumed_authority.source_integrity_digest,
        full_authority.source_integrity_digest
    );
    assert_eq!(
        resumed_authority.core_generation_digest,
        full_authority.core_generation_digest
    );
    assert!(resumed_authority.persisted_state.checkpoint_is_terminal());

    assert!(matches!(
        prepare_warp_nativepath_lifecycle(
            &path,
            std::slice::from_ref(resumed_authority.persisted_state.as_ref())
        ),
        WarpNativePreparationOutcome::ExactNoOp { .. }
    ));

    let encoded_terminal = serde_json::to_vec(&resumed_authority.persisted_state).unwrap();
    let persisted_terminal: lifecycle::WarpNativePersistedState =
        serde_json::from_slice(&encoded_terminal).unwrap();
    assert!(!persisted_terminal.checkpoint_is_terminal());
    let restarted = match prepare_warp_nativepath_lifecycle(&path, &[persisted_terminal]) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        WarpNativePreparationOutcome::ExactNoOp { .. } => {
            panic!("persisted terminal observation crossed the runtime authority boundary")
        }
        _ => panic!("legitimate terminal restart did not prepare for EOF recertification"),
    };
    assert_eq!(
        restarted.inputs.action,
        lifecycle::WarpNativePreparationAction::ResumeExactSnapshot
    );
    query::start_native_task_hydration_trace();
    let mut restart_sink = CollectingSink::default();
    let restarted_authority = complete(
        scan_prepared_warp_nativepath(*restarted, WarpNativeProfile::CoreOnly, &mut restart_sink)
            .unwrap(),
    );
    assert!(query::take_native_task_hydration_trace().is_empty());
    assert!(restart_sink.pages.is_empty());
    assert!(restarted_authority.persisted_state.checkpoint_is_terminal());
    assert_eq!(
        restarted_authority.source_integrity_digest,
        resumed_authority.source_integrity_digest
    );
    assert_eq!(
        restarted_authority.core_generation_digest,
        resumed_authority.core_generation_digest
    );
}

#[test]
fn exact_snapshot_conversation_restart_seeks_after_the_committed_rowid() {
    const CONVERSATIONS: usize = 130;

    let directory = tempdir().unwrap();
    let path = directory.path().join("conversation-restart.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    for index in 0..CONVERSATIONS {
        insert_conversation(
            &conn,
            &format!("conversation-restart-{index:03}"),
            None,
            "Conversation restart",
        );
    }
    drop(conn);

    let (full_authority, full_sink) = scan(&path);
    assert_eq!(full_sink.pages.len(), 3);

    let prepared = match prepare_warp_nativepath_lifecycle(&path, &[]) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        _ => panic!("fresh conversation source did not produce a certified snapshot"),
    };
    let preparation_inputs = prepared.inputs.clone();
    let mut crashing = CrashSink::new(2);
    scan_prepared_warp_nativepath(*prepared, WarpNativeProfile::CoreOnly, &mut crashing)
        .unwrap_err();
    assert_eq!(crashing.committed.len(), 1);
    let attempted = crashing.attempted.clone().unwrap();
    let committed_frontier = crashing.committed[0].next_safe_frontier.clone();
    assert_eq!(committed_frontier.completed_conversation_rows, 64);
    assert!(committed_frontier.last_conversation_rowid.is_some());
    assert_eq!(attempted.expected_frontier, committed_frontier);
    let partial_state = preparation_inputs
        .persisted_state_at(committed_frontier)
        .unwrap();
    assert!(!partial_state.checkpoint_is_terminal());

    let resumed = match prepare_warp_nativepath_lifecycle(&path, &[partial_state]) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        _ => panic!("exact partial conversation snapshot did not resume"),
    };
    let mut suffix_sink = CollectingSink::default();
    let resumed_authority = complete(
        scan_prepared_warp_nativepath(*resumed, WarpNativeProfile::CoreOnly, &mut suffix_sink)
            .unwrap(),
    );

    assert_eq!(suffix_sink.pages.len(), 2);
    assert_eq!(
        suffix_sink.pages[0].expected_frontier,
        attempted.expected_frontier
    );
    assert_eq!(
        suffix_sink.pages[0].next_safe_frontier,
        attempted.next_frontier
    );
    assert_eq!(suffix_sink.pages[0].identity, attempted.identity);
    assert_eq!(
        resumed_authority.counters.conversation_rows,
        (CONVERSATIONS - 64) as u64
    );
    assert_eq!(
        resumed_authority.counters.conversation_rows_hydrated,
        (CONVERSATIONS - 64) as u64
    );
    assert_eq!(
        resumed_authority.counters.conversation_json_objects_parsed,
        (CONVERSATIONS - 64) as u64
    );
    assert_eq!(
        resumed_authority.counters.sessions_retained,
        (CONVERSATIONS - 64) as u64
    );
    assert_eq!(
        resumed_authority.source_integrity_digest,
        full_authority.source_integrity_digest
    );
    assert_eq!(
        resumed_authority.core_generation_digest,
        full_authority.core_generation_digest
    );
}

#[test]
fn oversized_successful_output_is_local_and_later_output_survives() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(&conn, "conversation-output-bound", None, "Output Bound");
    let oversized = vec![b'x'; publication::WARP_NATIVE_PRO_OUTPUT_MAX_BODY_BYTES + 1];
    insert_task(
        &conn,
        "conversation-output-bound",
        "task-output-bound",
        &[
            message(
                "message-too-large",
                "task-output-bound",
                "request-too-large",
                1,
                &[tool_result_arm("call-too-large", &oversized)],
            ),
            message(
                "message-small",
                "task-output-bound",
                "request-small",
                2,
                &[tool_result_arm("call-small", b"small successful output")],
            ),
            message(
                "message-after",
                "task-output-bound",
                "request-after",
                3,
                &[text_arm(3, "valid retained sibling after outputs")],
            ),
        ],
    );
    drop(conn);

    let (core_authority, core) = scan_profile(&path, WarpNativeProfile::CoreOnly);
    let (pro_authority, pro) = scan_profile(&path, WarpNativeProfile::CoreAndPro);

    assert_eq!(
        core_authority.core_generation_digest,
        pro_authority.core_generation_digest
    );
    assert_eq!(core.sessions(), pro.sessions());
    assert_eq!(core.events(), pro.events());
    assert_eq!(core.rejections(), pro.rejections());
    assert_core_pages_identical(&core.pages, &pro.pages);
    assert_eq!(core_authority.counters.oversized_output_records, 0);
    assert_eq!(pro_authority.counters.oversized_output_records, 1);
    assert_eq!(pro.outputs().len(), 1);
    assert_eq!(pro.outputs()[0].content, b"small successful output");
    assert_eq!(pro_authority.counters.result_body_strings_allocated, 0);
    assert_eq!(
        pro_authority.counters.result_body_bytes_decoded,
        b"small successful output".len() as u64
    );
    assert_eq!(core.rejection_count(), 0);
    assert_eq!(pro.rejection_count(), 0);
    assert!(core.output_rejections().is_empty());
    assert_eq!(pro.events().len(), 1);
    assert_eq!(pro.events()[0].body, "valid retained sibling after outputs");
    let output_rejections = pro.output_rejections();
    assert_eq!(output_rejections.len(), 1);
    let rejection = output_rejections[0];
    assert_eq!(
        rejection.kind,
        publication::WarpNativeOutputRejectionKind::Oversized
    );
    assert!(rejection.reason.contains(&oversized.len().to_string()));
    assert!(rejection.native_key.len() <= 512);
    assert!(rejection.reason.len() <= 1_024);
    assert!(oversized.len() > 8 * 1024 * 1024);
    assert!(oversized.len() < 16 * 1024 * 1024);
    assert_safe_page_chain(&core.pages);
    assert_safe_page_chain(&pro.pages);
}

#[test]
fn normalized_message_between_eight_and_sixteen_mib_is_locally_rejected() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("warp.sqlite");
    let conn = Connection::open(&path).unwrap();
    create_schema(&conn);
    insert_conversation(
        &conn,
        "conversation-normalized-bound",
        None,
        "Normalized Bound",
    );
    let oversized_message_id = "m".repeat(WARP_NATIVE_PAGE_MAX_BYTES + 1);
    assert!(oversized_message_id.len() > 8 * 1024 * 1024);
    assert!(oversized_message_id.len() < 16 * 1024 * 1024);
    insert_task(
        &conn,
        "conversation-normalized-bound",
        "task-normalized-bound",
        &[
            message(
                &oversized_message_id,
                "task-normalized-bound",
                "request-too-large",
                1,
                &[text_arm(2, "unit must be rejected")],
            ),
            message(
                "message-valid-after",
                "task-normalized-bound",
                "request-valid-after",
                2,
                &[text_arm(3, "valid after oversized normalized message")],
            ),
        ],
    );
    drop(conn);

    let (authority, sink) = scan(&path);

    assert_eq!(authority.counters.oversized_normalized_units, 1);
    assert_eq!(sink.events().len(), 1);
    assert_eq!(
        sink.events()[0].body,
        "valid after oversized normalized message"
    );
    assert_eq!(sink.rejection_count(), 1);
    let rejection = sink.rejections()[0];
    assert_eq!(
        rejection.kind,
        publication::WarpNativeRejectionKind::OversizedNormalizedUnit
    );
    assert_eq!(rejection.native_key, "task-normalized-bound:message:0");
    assert!(rejection.native_key.len() <= 512);
    assert!(rejection.reason.len() <= 1_024);
    assert_eq!(
        sink.pages
            .last()
            .unwrap()
            .next_safe_frontier
            .completed_task_rows,
        1
    );
    assert_safe_page_chain(&sink.pages);
}
