use super::*;

#[test]
fn real_task_result_retains_only_bounded_result_evidence_and_outcome() {
    let raw = json!({
        "id": "result-1",
        "role": "tool",
        "tool_use_id": "tool-1",
        "exit_code": 0,
        "content": [{
            "type": "tool_result",
            "text": "[main 0123456789ab] private narrative",
        }],
    });
    let event = task_json_event(
        task_json_provider(CaptureProvider::Cline),
        "task-1",
        TaskJsonEventInput {
            source: "api_conversation_history",
            native_index: 0,
            raw,
        },
        0,
        "2026-07-21T12:00:00Z".parse().unwrap(),
    );
    assert_eq!(event.payload["result_outcome"], "success");
    assert_eq!(
        event.payload["result_evidence"],
        json!([
            {"kind": "call_id", "value": "tool-1"},
            {"kind": "git_commit_summary_id", "value": "0123456789ab"},
        ])
    );
    assert!(!event.payload.to_string().contains("private narrative"));
}

#[test]
fn result_profile_is_shared_and_has_no_whole_record_fallback() {
    let result = json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "not a result"},
            {"type": "tool_result", "content": [
                {"text": "first"},
                {"output": "second"}
            ]}
        ]
    });
    assert_eq!(
        task_json_result_content(&result, "api_conversation_history").as_deref(),
        Some("first\nsecond")
    );

    let command = json!({"type": "command", "text": "command output"});
    assert_eq!(
        task_json_result_content(&command, "ui_messages").as_deref(),
        Some("command output")
    );

    let label_only = json!({"type": "tool_result", "tool_name": "shell"});
    assert_eq!(
        task_json_result_content(&label_only, "api_conversation_history"),
        None
    );
}

#[test]
fn task_directory_traversal_does_not_collect_the_tree() {
    let temp = tempdir().unwrap();
    let tasks = temp.path().join("tasks");
    fs::create_dir_all(tasks.join("first")).unwrap();
    fs::create_dir_all(tasks.join("second")).unwrap();
    fs::write(tasks.join("first/task_metadata.json"), b"{}").unwrap();
    fs::write(tasks.join("second/ui_messages.json"), b"[]").unwrap();
    let spec = task_json_provider(CaptureProvider::Cline);
    let mut visited = Vec::new();

    let count = visit_task_json_dirs(temp.path(), spec, &mut |path| {
        visited.push(path.to_path_buf());
        Ok(())
    })
    .unwrap();

    visited.sort();
    assert_eq!(count, 2);
    assert_eq!(visited, vec![tasks.join("first"), tasks.join("second")]);
}

#[test]
fn projector_emits_cline_messages_once_in_native_source_order() {
    let temp = tempdir().unwrap();
    let task_dir = temp.path().join("task");
    fs::create_dir(&task_dir).unwrap();
    fs::write(
        task_dir.join("task_metadata.json"),
        br#"{"taskId":"cline-one-pass","createdAt":"2026-07-18T11:00:00Z"}"#,
    )
    .unwrap();
    fs::write(
        task_dir.join("api_conversation_history.json"),
        br#"[{"id":"api","role":"user","content":"api first"}]"#,
    )
    .unwrap();
    fs::write(
        task_dir.join("ui_messages.json"),
        br#"[{"id":"ui","type":"say","text":"ui second"}]"#,
    )
    .unwrap();
    let spec = task_json_provider(CaptureProvider::Cline);
    let observation = TaskJsonTaskObservation::read(&task_dir, &[], spec).unwrap();
    let context = test_context(&task_dir);
    let (session, failures) =
        task_json_session_state(&task_dir, &observation, &context, spec).unwrap();
    let mut projector = TaskJsonCapturedBatchProjector::fresh(
        spec,
        context,
        task_dir.display().to_string(),
        session,
        failures,
    )
    .unwrap();
    let mut producer = TaskJsonBatchProducer::new(
        test_source(&observation, spec),
        ProviderRecordKind::new(TASK_JSON_RECORD_KIND).unwrap(),
        test_message_sources(&observation, spec),
        TaskJsonStreamPosition::initial(),
    )
    .unwrap();
    let mut output = CollectingProjectionOutput::default();
    let mut last_batch = None;
    while let Some(batch) = producer.next_batch().unwrap() {
        for record in batch.records() {
            projector.project_record(record, &mut output).unwrap();
        }
        projector.finish_cursor(&batch).unwrap();
        last_batch = Some(batch);
    }

    assert!(output.rejections.is_empty());
    let captures = output
        .normalizations
        .iter()
        .flat_map(|normalization| &normalization.captures)
        .collect::<Vec<_>>();
    assert_eq!(captures.len(), 2);
    assert_eq!(
        captures[0].1.event.as_ref().unwrap().provider_event_index,
        0
    );
    assert_eq!(
        captures[0].1.event.as_ref().unwrap().payload["text"],
        "api first"
    );
    assert_eq!(
        captures[1].1.event.as_ref().unwrap().provider_event_index,
        1
    );
    assert_eq!(
        captures[1].1.event.as_ref().unwrap().payload["text"],
        "ui second"
    );
    assert_eq!(projector.checkpoint.accepted_events, 2);
    assert!(projector.checkpoint.terminal_seen);
    assert_eq!(
        task_json_decode_position(last_batch.unwrap().range_end()).unwrap(),
        TaskJsonStreamPosition::done(3)
    );
}

#[test]
fn roo_history_item_fallback_is_projected_by_the_terminal_record() {
    let temp = tempdir().unwrap();
    let task_dir = temp.path().join("roo-task");
    fs::create_dir(&task_dir).unwrap();
    fs::write(
        task_dir.join("history_item.json"),
        br#"{"id":"roo-fallback","task":"fallback prompt","ts":1784372400000}"#,
    )
    .unwrap();
    let spec = task_json_provider(CaptureProvider::RooCode);
    let observation = TaskJsonTaskObservation::read(&task_dir, &[], spec).unwrap();
    let context = test_context(&task_dir);
    let (session, failures) =
        task_json_session_state(&task_dir, &observation, &context, spec).unwrap();
    let mut projector = TaskJsonCapturedBatchProjector::fresh(
        spec,
        context,
        task_dir.display().to_string(),
        session,
        failures,
    )
    .unwrap();
    let mut producer = TaskJsonBatchProducer::new(
        test_source(&observation, spec),
        ProviderRecordKind::new(TASK_JSON_RECORD_KIND).unwrap(),
        Vec::new(),
        TaskJsonStreamPosition::initial(),
    )
    .unwrap();
    let batch = producer.next_batch().unwrap().unwrap();
    let mut output = CollectingProjectionOutput::default();
    assert_eq!(batch.records().len(), 1);
    assert_eq!(
        task_json_decode_position(batch.range_before()).unwrap(),
        TaskJsonStreamPosition::initial()
    );
    projector
        .project_record(&batch.records()[0], &mut output)
        .unwrap();

    assert!(output.rejections.is_empty());
    assert_eq!(output.normalizations.len(), 1);
    let event = output.normalizations[0].captures[0]
        .1
        .event
        .as_ref()
        .unwrap();
    assert_eq!(event.provider_event_index, 0);
    assert_eq!(event.event_type, EventType::Summary);
    assert_eq!(event.payload["text"], "fallback prompt");
}
