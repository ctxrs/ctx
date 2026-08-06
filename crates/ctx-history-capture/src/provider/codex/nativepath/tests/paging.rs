use super::*;

#[test]
fn ctx_retrieval_link_survives_source_backed_page_rollover() {
    let call_id = "paged-ctx-retrieval";
    let mut contents = session_meta("paged-ctx-retrieval-owner");
    contents.push_str(&jsonl(json!({
        "timestamp": "2026-08-05T14:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "exec_command",
            "call_id": call_id,
            "arguments": {"cmd": "ctx search paged"}
        }
    })));
    for index in 0..MAX_CODEX_PAGE_ROWS {
        contents.push_str(&message("assistant", &format!("page filler {index}")));
    }
    let output = concat!(
        "Chunk ID: 123abc\n",
        "Wall time: 0.062 seconds\n",
        "Process exited with code 0\n",
        "Final output:\n",
        "{\"results\":[]}"
    );
    contents.push_str(&tool_output(call_id, output));
    let (_temp, path) = write_source(&contents);

    let (_, sink) = scan_collect(discover_one(&path, "paged-ctx-retrieval-owner"), None);
    assert!(sink.pages.len() >= 2);
    let result = sink
        .rows
        .iter()
        .find(|row| row.event_type == EventType::CommandOutput)
        .unwrap();
    assert_eq!(result.lexical_body, output);
    assert_eq!(
        result.discovery_exclusion,
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
}

#[test]
fn terminal_authority_defers_a_verified_append_and_retry_advances_the_prefix() {
    let initial = [
        session_meta("mutation-owner"),
        message("user", "before mutation"),
        successful_tool_output("mutation-call", "stable output"),
    ]
    .concat();
    let (_temp, path) = write_source(&initial);
    let mut scanner =
        CodexNativeScanner::new_source_backed_v0(discover_one(&path, "mutation-owner"), None)
            .unwrap();
    let mut safe_frontier = None;
    while let Some(page) = scanner.next_page().unwrap() {
        let CodexNativeOwnedPage::Core(page) = page;
        assert!(page.terminal);
        safe_frontier = Some(page.next_safe_frontier);
    }
    let safe_frontier = safe_frontier.expect("terminal Core page");

    let appended = message("assistant", "after mutation");
    fs::write(&path, format!("{initial}{appended}")).unwrap();
    let frozen = scanner.finish().unwrap();
    assert_eq!(frozen.complete_prefix_end, initial.len() as u64);
    assert_eq!(frozen.next_raw_ordinal, 3);
    assert_eq!(frozen.before_observation, frozen.after_observation);

    let (retry_scan, retry) = scan_collect(discover_one(&path, "mutation-owner"), None);
    assert_eq!(
        retry.frontiers[0].0,
        CodexNativeFrontier {
            complete_prefix_end: 0,
            next_raw_ordinal: 0,
            complete_prefix_sha256: Sha256::digest([]).into(),
        }
    );
    assert_eq!(safe_frontier.next_raw_ordinal, 3);
    assert_eq!(
        retry
            .rows
            .iter()
            .map(|row| row.raw_ordinal)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(retry_scan.next_raw_ordinal, 4);
}

#[test]
fn c0_shapes_retain_conversation_summaries_calls_and_results() {
    let mut baseline = session_meta("c0-baseline");
    for index in 0_usize..11 {
        baseline.push_str(&message(
            if index.is_multiple_of(3) {
                "user"
            } else {
                "assistant"
            },
            &format!("message-{index}"),
        ));
    }
    for index in 0..3 {
        baseline.push_str(&reasoning(&format!("reasoning-{index}")));
        baseline.push_str(&tool_call(&format!("call-{index}")));
        baseline.push_str(&tool_output(&format!("call-{index}"), "excluded"));
    }
    let (_temp, path) = write_source(&baseline);
    let (scan, sink) = scan_collect(discover_one(&path, "c0-baseline"), None);
    assert_eq!(sink.rows.len(), 20);
    assert_eq!(scan.counters.retained_records, 20);
    assert_eq!(scan.counters.native_result_records, 3);
    assert_eq!(
        sink.rows
            .iter()
            .filter(|row| row.event_type == EventType::Message)
            .count(),
        11
    );
    assert_eq!(
        sink.rows
            .iter()
            .filter(|row| row.event_type == EventType::Summary)
            .count(),
        3
    );
    assert_eq!(
        sink.rows
            .iter()
            .filter(|row| row.event_type == EventType::CommandOutput)
            .count(),
        3
    );
    assert_eq!(
        sink.rows
            .iter()
            .filter(|row| row.event_type == EventType::ToolCall)
            .count(),
        3
    );

    let mut output_heavy = session_meta("c0-output-heavy");
    for index in 0_usize..4 {
        output_heavy.push_str(&message(
            if index.is_multiple_of(2) {
                "user"
            } else {
                "assistant"
            },
            &format!("conversation-{index}"),
        ));
    }
    for index in 0..8 {
        output_heavy.push_str(&tool_call(&format!("heavy-call-{index}")));
        output_heavy.push_str(&tool_output(
            &format!("heavy-call-{index}"),
            &"excluded-result".repeat(128),
        ));
    }
    fs::write(&path, output_heavy).unwrap();
    let (scan, sink) = scan_collect(discover_one(&path, "c0-output-heavy"), None);
    assert_eq!(sink.rows.len(), 20);
    assert_eq!(scan.counters.native_result_records, 8);
}

#[test]
fn compacted_payloads_and_known_result_aliases_retain_known_result_content() {
    let contents = [
        session_meta("shape-owner"),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "compacted",
            "payload": [{"summary_text": "compacted summary"}]
        })),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "tool_result",
                "result": "future result survives"
            }
        })),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "event_msg",
            "payload": {
                "type": "patch_apply_end",
                "stdout": "must not survive either"
            }
        })),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "shape-owner"), None);

    assert_eq!(sink.rows.len(), 2);
    assert_eq!(sink.rows[0].event_type, EventType::Summary);
    assert_eq!(sink.rows[0].raw_ordinal, 1);
    assert_eq!(sink.rows[1].event_type, EventType::ToolOutput);
    assert_eq!(sink.rows[1].raw_ordinal, 2);
    assert_eq!(sink.rows[1].lexical_body, "future result survives");
    assert_eq!(scan.counters.native_result_records, 2);
    assert_eq!(scan.counters.retained_json_parses, 1);
}

#[test]
fn unknown_result_like_discriminators_are_ignored_without_losing_neighbors_or_bodies() {
    let binary_body = "iVBORw0KGgo=issue-247-binary-body";
    let unknown_body = "future-result-body-must-not-be-indexed";
    let contents = [
        session_meta("unknown-result-like-owner"),
        message("user", "valid before unknown records"),
        jsonl(json!({
            "timestamp": "2026-05-31T20:33:10Z",
            "type": "event_msg",
            "payload": {
                "type": "image_generation_end",
                "call_id": "ig-issue-247",
                "status": "generating",
                "revised_prompt": "a red square",
                "result": binary_body,
                "saved_path": "/tmp/repro/img.png"
            }
        })),
        jsonl(json!({
            "timestamp": "2026-05-31T20:33:11Z",
            "type": "response_item",
            "payload": {
                "type": "future_tool_result",
                "result": unknown_body
            }
        })),
        jsonl(json!({
            "timestamp": "2026-05-31T20:33:12Z",
            "type": "event_msg",
            "payload": {
                "type": "future_tool_response",
                "output": unknown_body
            }
        })),
        jsonl(json!({
            "timestamp": "2026-05-31T20:33:13Z",
            "type": "event_msg",
            "payload": {
                "type": "future_tool_end",
                "result": unknown_body
            }
        })),
        message("assistant", "valid after unknown records"),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "unknown-result-like-owner"), None);

    assert_eq!(
        sink.rows
            .iter()
            .map(|row| (row.raw_ordinal, row.lexical_body.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (1, "valid before unknown records"),
            (6, "valid after unknown records")
        ]
    );
    assert_eq!(scan.counters.complete_records, 7);
    assert_eq!(scan.counters.retained_records, 2);
    assert_eq!(scan.counters.ignored_records, 4);
    assert_eq!(scan.counters.rejected_complete_records, 0);
    assert_eq!(scan.counters.native_result_records, 0);
    assert_eq!(scan.counters.prefiltered_records, 4);
    assert_eq!(scan.complete_prefix_end, contents.len() as u64);
    assert_eq!(scan.next_raw_ordinal, 7);
    for row in &sink.rows {
        assert!(!row.lexical_body.contains(binary_body));
        assert!(!row.lexical_body.contains(unknown_body));
        let structured = row
            .structured_content
            .as_ref()
            .map(Value::to_string)
            .unwrap_or_default();
        assert!(!structured.contains(binary_body));
        assert!(!structured.contains(unknown_body));
    }
}
