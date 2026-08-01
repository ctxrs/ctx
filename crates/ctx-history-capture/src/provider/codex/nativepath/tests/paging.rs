use super::*;

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
fn compacted_payloads_and_future_result_aliases_retain_known_result_content() {
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
