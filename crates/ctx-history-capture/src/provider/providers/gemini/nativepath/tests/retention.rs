use super::*;

#[test]
fn gemini_nativepath_result_only_failure_retains_only_a_sparse_core_diagnostic() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("root-session", "main"),
            json!({
                "id": "result-only",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "call-1",
                    "name": "run_shell_command",
                    "result": {
                        "content": "result-only-secret",
                        "error": "failure is excluded too"
                    }
                }]
            }),
        ],
    );
    let source = rediscover(&root, &path);

    let (outcome, rows) = scan_collect(&source, None);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].event_type, EventType::ToolOutput);
    assert_eq!(rows[0].role, EventRole::Tool);
    assert!(rows[0].preview.is_empty());
    assert!(rows[0].searchable_text.is_empty());
    let serialized = serde_json::to_string(&rows).unwrap();
    assert!(!serialized.contains("result-only-secret"));
    assert!(!serialized.contains("failure is excluded too"));
    assert!(!serialized.contains("locator"));
    assert!(!outcome.signals.emitted_zero_rows);
    assert!(!outcome.signals.source_has_zero_retained_rows);
    assert_eq!(outcome.metrics.native_result_records_observed, 1);
    assert!(outcome.metrics.result_body_bytes_decoded_or_allocated > 0);
    assert_eq!(outcome.metrics.result_body_hashes_created, 1);
    assert_eq!(outcome.metrics.result_previews_created, 1);
    assert_eq!(outcome.metrics.result_file_touches_created, 0);
}

#[test]
fn gemini_nativepath_matches_c0_retention_counts_without_header_notices() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut baseline = vec![header("baseline-session", "main")];
    let kinds = [
        "user",
        "assistant",
        "tool_call",
        "tool_output",
        "state",
        "assistant",
    ];
    for index in 0..20 {
        let record = match kinds[index % kinds.len()] {
            "user" => json!({
                "id": format!("baseline-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": format!("user {index}")
            }),
            "assistant" => json!({
                "id": format!("baseline-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "content": format!("assistant {index}")
            }),
            "tool_call" => json!({
                "id": format!("baseline-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{"id": format!("call-{index}"), "name": "write_file", "args": {"path": "safe.txt"}}]
            }),
            "tool_output" => json!({
                "id": format!("baseline-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{"id": format!("call-{index}"), "name": "write_file", "result": {"content": format!("NATIVEPATH_SYNTHETIC_OUTPUT_{index}")}}]
            }),
            "state" => json!({
                "id": format!("baseline-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "$set": {"summary": format!("state {index}")}
            }),
            unexpected => panic!("unexpected synthetic event kind {unexpected}"),
        };
        baseline.push(record);
    }
    let path = write_transcript(&root, &baseline);
    let source = rediscover(&root, &path);
    let (baseline_outcome, baseline_rows) = scan_collect(&source, None);
    assert_eq!(baseline_rows.len(), 17);
    assert_eq!(baseline_outcome.metrics.header_records, 1);
    assert_eq!(baseline_outcome.metrics.native_result_records_observed, 3);
    assert_eq!(baseline_outcome.metrics.retained_messages, 11);
    assert_eq!(baseline_outcome.metrics.retained_tool_calls, 3);
    assert_eq!(baseline_outcome.metrics.retained_notices, 3);

    let mut output_heavy = vec![header("output-session", "main")];
    for index in 0..20 {
        let record = match index {
            0 | 10 => json!({
                "id": format!("output-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": format!("user {index}")
            }),
            1 | 11 => json!({
                "id": format!("output-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "content": format!("assistant {index}")
            }),
            index if index % 2 == 0 => json!({
                "id": format!("output-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{"id": format!("call-{index}"), "name": "write_file", "args": {"path": "safe.txt"}}]
            }),
            _ => json!({
                "id": format!("output-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{"id": format!("call-{index}"), "name": "write_file", "result": {"content": format!("NATIVEPATH_SYNTHETIC_OUTPUT_HEAVY_{index}")}}]
            }),
        };
        output_heavy.push(record);
    }
    fs::write(&path, jsonl(&output_heavy)).unwrap();
    let source = rediscover(&root, &path);
    let (output_outcome, output_rows) = scan_collect(&source, None);
    assert_eq!(output_rows.len(), 12);
    assert_eq!(output_outcome.metrics.header_records, 1);
    assert_eq!(output_outcome.metrics.native_result_records_observed, 8);
    assert_eq!(output_outcome.metrics.retained_messages, 4);
    assert_eq!(output_outcome.metrics.retained_tool_calls, 8);
    assert_eq!(output_outcome.metrics.retained_notices, 0);
    assert!(output_rows
        .iter()
        .all(|row| !format!("{row:?}").contains("NATIVEPATH_SYNTHETIC_OUTPUT")));
}

#[test]
fn gemini_nativepath_file_touch_set_is_deterministic_and_rejects_count_overflow() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let calls: Vec<_> = (0..MAX_GEMINI_FILE_TOUCHES_PER_EVENT)
        .rev()
        .map(|index| {
            json!({
                "id": format!("call-{index}"),
                "name": "write_file",
                "args": {"path": format!("path-{index:04}.txt")}
            })
        })
        .collect();
    let path = write_transcript(
        &root,
        &[
            header("touch-count-session", "main"),
            json!({
                "id": "touch-count-boundary",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": calls
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let (boundary, rows) = scan_collect(&source, None);

    assert_eq!(boundary.rejected_records, 0);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].safe_file_touches.len(),
        MAX_GEMINI_FILE_TOUCHES_PER_EVENT
    );
    assert!(rows[0]
        .safe_file_touches
        .windows(2)
        .all(|pair| pair[0] < pair[1]));

    let overflow_calls: Vec<_> = (0..=MAX_GEMINI_FILE_TOUCHES_PER_EVENT)
        .map(|index| {
            json!({
                "id": format!("overflow-call-{index}"),
                "name": "write_file",
                "args": {"path": format!("overflow-{index:04}.txt")}
            })
        })
        .collect();
    fs::write(
        &path,
        jsonl(&[
            header("touch-count-session", "main"),
            json!({
                "id": "touch-count-overflow",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": overflow_calls
            }),
            json!({
                "id": "after-touch-count-overflow",
                "type": "user",
                "content": "later valid"
            }),
        ]),
    )
    .unwrap();
    let source = rediscover(&root, &path);
    let (outcome, rows) = scan_collect(&source, None);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].native_order.raw_ordinal, 2);
    assert_eq!(outcome.rejected_records, 1);
    assert!(outcome.rejections[0]
        .reason
        .contains("256 unique file-touch limit"));
}

#[test]
fn gemini_nativepath_file_touch_set_enforces_exact_byte_boundary() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("touch-byte-session", "main"),
            json!({
                "id": "touch-byte-boundary",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "call-boundary",
                    "name": "write_file",
                    "args": {"path": "x".repeat(MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT)}
                }]
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let (boundary, rows) = scan_collect(&source, None);

    assert_eq!(boundary.rejected_records, 0);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].safe_file_touches[0].len(),
        MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT
    );

    fs::write(
        &path,
        jsonl(&[
            header("touch-byte-session", "main"),
            json!({
                "id": "touch-byte-overflow",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "call-overflow",
                    "name": "write_file",
                    "args": {"path": "x".repeat(MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT + 1)}
                }]
            }),
            json!({
                "id": "after-touch-byte-overflow",
                "type": "user",
                "content": "later valid"
            }),
        ]),
    )
    .unwrap();
    let source = rediscover(&root, &path);
    let (outcome, rows) = scan_collect(&source, None);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].native_order.raw_ordinal, 2);
    assert_eq!(outcome.rejected_records, 1);
    assert!(outcome.rejections[0]
        .reason
        .contains("65536 file-touch byte limit"));
}

#[test]
fn gemini_nativepath_streams_local_scale_without_accumulating_rows_or_results() {
    const PAIRS: usize = 2_000;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = transcript_path(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = fs::File::create(&path).unwrap();
    serde_json::to_writer(&mut file, &header("scale-session", "main")).unwrap();
    file.write_all(b"\n").unwrap();
    let output_payload = "x".repeat(1_024);
    for index in 0..PAIRS {
        serde_json::to_writer(
            &mut file,
            &json!({
                "id": format!("request-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": format!("call-{index}"),
                    "name": "write_file",
                    "args": {"path": format!("safe-{index}.txt")}
                }]
            }),
        )
        .unwrap();
        file.write_all(b"\n").unwrap();
        serde_json::to_writer(
            &mut file,
            &json!({
                "id": format!("result-{index}"),
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": format!("call-{index}"),
                    "name": "write_file",
                    "result": {
                        "content": output_payload,
                        "path": format!("/workspace/nativepath-fixture/output-only/{index}")
                    }
                }]
            }),
        )
        .unwrap();
        file.write_all(b"\n").unwrap();
    }
    drop(file);
    let source = rediscover(&root, &path);
    let (outcome, retained) = scan_collect(&source, None);
    for event in &retained {
        assert!(event
            .safe_file_touches
            .iter()
            .all(|path| !path.contains("output-only")));
    }

    assert_eq!(retained.len(), PAIRS);
    assert_eq!(outcome.metrics.retained_tool_calls, PAIRS as u64);
    assert_eq!(outcome.metrics.native_result_records_observed, PAIRS as u64);
    assert!(
        outcome.metrics.native_result_record_bytes_observed > (PAIRS as u64).saturating_mul(1_024)
    );
    assert_eq!(outcome.metrics.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(outcome.metrics.result_body_hashes_created, 0);
    assert_eq!(outcome.metrics.result_previews_created, 0);
    assert_eq!(outcome.metrics.result_file_touches_created, 0);
    assert_eq!(outcome.checkpoint.next_raw_ordinal, 1 + (PAIRS as u64 * 2));
}
