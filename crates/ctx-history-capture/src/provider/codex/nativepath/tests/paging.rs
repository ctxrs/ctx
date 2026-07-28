use super::*;

#[test]
fn byte_overflow_restores_the_record_and_emits_it_once_on_the_next_page() {
    const OUTPUT_BYTES: usize = 2_100_000;

    let mut contents = session_meta("byte-page-owner");
    for index in 0..3 {
        contents.push_str(&successful_tool_output(
            &format!("large-{index}"),
            &char::from(b'a' + index).to_string().repeat(OUTPUT_BYTES),
        ));
    }
    let (_temp, path) = write_source(&contents);
    let (core_scan, core) = scan_collect_profile(
        discover_one(&path, "byte-page-owner"),
        None,
        CodexNativeProfile::CoreOnly,
    );
    let (scan, collected) = scan_collect_profile(
        discover_one(&path, "byte-page-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    );

    assert_eq!(core.rows, collected.rows);
    assert_eq!(core.pages, collected.pages);
    assert_eq!(core.physical_records, collected.physical_records);
    assert_eq!(core.frontiers, collected.frontiers);
    assert_eq!(core.core_receipts, collected.core_receipts);
    assert_eq!(core_scan.rejections, scan.rejections);
    assert_eq!(collected.pages.len(), 1);
    assert_eq!(collected.pro_pages.len(), 2);
    assert_eq!(
        collected
            .pro_outputs
            .iter()
            .map(|output| output.coordinate.native_sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(collected
        .pro_pages
        .iter()
        .all(|(_, bytes)| *bytes <= MAX_CODEX_PAGE_BYTES));
    assert_eq!(
        collected.pro_frontiers[0].1, collected.pro_frontiers[1].0,
        "the overflow output must begin the next independent Pro page"
    );
    assert_eq!(scan.counters.complete_records, 4);
    assert_eq!(scan.counters.bytes_read, contents.len() as u64);
    assert_eq!(scan.next_raw_ordinal, 4);
    assert_eq!(scan.counters.structural_json_parses, 4);
    assert_eq!(scan.counters.structural_output_probes, 3);
    assert_eq!(scan.counters.typed_json_parses, 4);
    assert_eq!(scan.counters.typed_output_parses, 3);
}

#[test]
fn core_page_receipts_are_activation_invariant_at_unit_and_pro_byte_pressure() {
    let mut contents = session_meta("activation-owner");
    for index in 0..130 {
        if index % 3 == 0 {
            contents.push_str(&successful_tool_output(
                &format!("activation-{index}"),
                &format!("output-{index}-{}", "x".repeat(150_000)),
            ));
        } else {
            contents.push_str(&message("assistant", &format!("core-{index}")));
        }
    }
    let (_temp, path) = write_source(&contents);

    let (core_scan, core) = scan_collect_profile(
        discover_one(&path, "activation-owner"),
        None,
        CodexNativeProfile::CoreOnly,
    );
    let (pro_scan, pro) = scan_collect_profile(
        discover_one(&path, "activation-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    );

    assert_eq!(core.rows, pro.rows);
    assert_eq!(core.pages, pro.pages);
    assert_eq!(core.physical_records, pro.physical_records);
    assert_eq!(core.frontiers, pro.frontiers);
    assert_eq!(core.core_receipts, pro.core_receipts);
    assert_eq!(core_scan.rejections, pro_scan.rejections);
    assert_eq!(core.pages.len(), 3);
    assert_eq!(core.physical_records, vec![64, 64, 3]);
    assert!(pro.pro_pages.len() >= 2);
    assert!(pro
        .pro_pages
        .iter()
        .all(|(units, bytes)| *units <= 64 && *bytes <= MAX_CODEX_PAGE_BYTES));
    assert_eq!(
        pro_scan.counters.pro_output_pages_emitted as usize,
        pro.pro_pages.len()
    );
}

#[test]
fn owned_page_can_retry_a_lagging_lane_before_the_scanner_advances() {
    let contents = [
        session_meta("retry-owner"),
        message("user", "request"),
        successful_tool_output("retry-call", "retry-output"),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let mut scanner = CodexNativeScanner::new(
        discover_one(&path, "retry-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    )
    .unwrap();

    let pro_page = match scanner.next_page().unwrap().unwrap() {
        CodexNativeOwnedPage::Pro(page) => page,
        CodexNativeOwnedPage::Core(_) => panic!("Pro lane should flush before terminal Core"),
    };
    let first_attempt = pro_page
        .outputs
        .iter()
        .map(|output| {
            (
                output.coordinate.unit_key.clone(),
                output.coordinate.native_sequence,
                output.content.clone(),
            )
        })
        .collect::<Vec<_>>();
    let retry_attempt = pro_page
        .outputs
        .iter()
        .map(|output| {
            (
                output.coordinate.unit_key.clone(),
                output.coordinate.native_sequence,
                output.content.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(first_attempt, retry_attempt);
    let page = match scanner.next_page().unwrap().unwrap() {
        CodexNativeOwnedPage::Core(page) => page,
        CodexNativeOwnedPage::Pro(_) => panic!("only one Pro page should be emitted"),
    };
    assert_eq!(page.physical_records, 3);
    assert_eq!(
        page.next_safe_frontier.next_raw_ordinal,
        page.expected_frontier.next_raw_ordinal + page.physical_records
    );
    assert!(page.terminal);
    assert!(scanner.next_page().unwrap().is_none());
    let scan = scanner.finish().unwrap();
    assert_eq!(scan.counters.complete_records, 3);
    assert_eq!(scan.counters.emitted_pages, 1);
    assert_eq!(scan.counters.pro_output_pages_emitted, 1);
}

#[test]
fn terminal_authority_rejects_mutation_and_retry_keeps_the_safe_prefix_identity() {
    let initial = [
        session_meta("mutation-owner"),
        message("user", "before mutation"),
        successful_tool_output("mutation-call", "stable output"),
    ]
    .concat();
    let (_temp, path) = write_source(&initial);
    let mut scanner = CodexNativeScanner::new(
        discover_one(&path, "mutation-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    )
    .unwrap();
    let mut safe_frontier = None;
    while let Some(page) = scanner.next_page().unwrap() {
        if let CodexNativeOwnedPage::Core(page) = page {
            assert!(page.terminal);
            safe_frontier = Some(page.next_safe_frontier);
        }
    }
    let safe_frontier = safe_frontier.expect("terminal Core page");

    let appended = message("assistant", "after mutation");
    fs::write(&path, format!("{initial}{appended}")).unwrap();
    let error = scanner.finish().unwrap_err();
    assert!(
        matches!(
            error,
            crate::CaptureError::SourceChangedDuringCapture
                | crate::CaptureError::InvalidPayload(_)
                | crate::CaptureError::InvalidProviderTranscriptPath { .. }
        ),
        "{error:?}"
    );

    let (retry_scan, retry) = scan_collect_profile(
        discover_one(&path, "mutation-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    );
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
        vec![1, 3]
    );
    assert_eq!(retry_scan.next_raw_ordinal, 4);
}

#[test]
fn c0_shapes_retain_conversation_summaries_and_calls_but_no_results() {
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
    assert_eq!(sink.rows.len(), 17);
    assert_eq!(scan.counters.retained_records, 17);
    assert_eq!(scan.counters.native_result_records, 3);
    assert_eq!(
        sink.rows
            .iter()
            .filter(|row| row.provider_event.event_type == EventType::Message)
            .count(),
        11
    );
    assert_eq!(
        sink.rows
            .iter()
            .filter(|row| row.provider_event.event_type == EventType::Summary)
            .count(),
        3
    );
    assert_eq!(
        sink.rows
            .iter()
            .filter(|row| row.provider_event.event_type == EventType::ToolCall)
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
    assert_eq!(sink.rows.len(), 12);
    assert_eq!(scan.counters.native_result_records, 8);
    assert_eq!(scan.counters.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(scan.counters.result_hashes_created, 0);
}

#[test]
fn compacted_payloads_and_future_result_aliases_are_fail_closed() {
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
                "result": "must not survive"
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

    assert_eq!(sink.rows.len(), 1);
    assert_eq!(sink.rows[0].provider_event.event_type, EventType::Summary);
    assert_eq!(sink.rows[0].raw_ordinal, 1);
    assert_eq!(scan.counters.native_result_records, 2);
    assert_eq!(scan.counters.retained_json_parses, 1);
}
