use super::*;

#[test]
fn gemini_nativepath_pull_reader_pages_at_physical_record_bound() {
    const EVENTS: usize = MAX_GEMINI_NATIVE_PAGE_RECORDS * 2 + 7;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut values = vec![header("page-records", "main")];
    values.extend((0..EVENTS).map(|index| {
        json!({
            "id": format!("event-{index}"),
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "gemini",
            "content": format!("message {index}")
        })
    }));
    let path = write_transcript(&root, &values);
    let source = rediscover(&root, &path);

    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();
    let mut physical_records = 0_usize;
    let mut retained_events = 0_usize;
    let mut pages = 0_usize;
    while let Some(page) = reader.next_page().unwrap() {
        assert!(page.physical_records <= MAX_GEMINI_NATIVE_PAGE_RECORDS);
        assert!(page.retained_event_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES);
        physical_records += page.physical_records;
        retained_events += page.events.len();
        pages += 1;
    }
    let outcome = reader.outcome().unwrap();

    assert_eq!(physical_records, EVENTS + 1);
    assert_eq!(retained_events, EVENTS);
    assert_eq!(pages, 3);
    assert_eq!(outcome.checkpoint.next_raw_ordinal, (EVENTS + 1) as u64);
}

#[test]
fn gemini_nativepath_pull_reader_pages_at_retained_byte_bound() {
    const CONTENT_BYTES: usize = 2_100_000;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("page-bytes", "main"),
            json!({
                "id": "large-1",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "content": "a".repeat(CONTENT_BYTES)
            }),
            json!({
                "id": "large-2",
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "gemini",
                "content": "b".repeat(CONTENT_BYTES)
            }),
        ],
    );
    let source = rediscover(&root, &path);

    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();
    let mut page_bytes = Vec::new();
    let mut retained_events = 0_usize;
    while let Some(page) = reader.next_page().unwrap() {
        assert!(page.physical_records <= MAX_GEMINI_NATIVE_PAGE_RECORDS);
        assert!(page.conservative_serialized_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES);
        if !page.events.is_empty() {
            assert!(page.retained_event_bytes > CONTENT_BYTES * 2);
            page_bytes.push(page.retained_event_bytes);
        }
        retained_events += page.events.len();
    }
    let outcome = reader.outcome().unwrap();

    assert_eq!(page_bytes.len(), 2);
    assert!(page_bytes
        .iter()
        .all(|bytes| *bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES));
    assert_eq!(retained_events, 2);
    assert_eq!(outcome.rejected_records, 0);
}

#[test]
fn gemini_nativepath_safe_pages_rewind_before_an_uncommitted_overflow_record() {
    const CONTENT_BYTES: usize = 2_100_000;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut records = vec![header("safe-frontier-pages", "main")];
    records.extend((0..3).map(|index| {
        json!({
            "id": format!("large-{index}"),
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "gemini",
            "content": "x".repeat(CONTENT_BYTES)
        })
    }));
    let path = write_transcript(&root, &records);
    let source = rediscover(&root, &path);

    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();
    let mut previous_frontier = None;
    let mut identities = Vec::new();
    let mut event_ids = Vec::new();
    while let Some(page) = reader.next_page().unwrap() {
        if let Some(previous) = previous_frontier.as_ref() {
            assert_eq!(&page.expected_frontier, previous);
        }
        assert!(page.physical_records <= MAX_GEMINI_NATIVE_PAGE_RECORDS);
        assert!(page.logical_units <= MAX_GEMINI_NATIVE_PAGE_RECORDS);
        assert!(page.conservative_serialized_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES);
        assert_ne!(page.identity.as_bytes(), &[0; 32]);
        identities.push(page.identity);
        event_ids.extend(page.events.iter().map(|event| match &event.identity {
            GeminiEventIdentity::NativeRecordId(id) => id.clone(),
        }));
        previous_frontier = Some(page.next_safe_frontier);
    }
    let outcome = reader.outcome().unwrap();

    assert_eq!(event_ids, vec!["large-0", "large-1", "large-2"]);
    assert_eq!(identities.len(), 3);
    assert_eq!(
        previous_frontier.unwrap().complete_prefix_end,
        outcome.checkpoint.complete_prefix_end
    );
    assert_eq!(outcome.rejected_records, 0);
}

#[test]
fn gemini_nativepath_profile_gates_successful_output_hydration_from_core() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let success_body = "PRO_SUCCESSFUL_OUTPUT_CONTENT";
    let failure_body = "PRO_FAILED_OUTPUT_CONTENT";
    let timeout_body = "PRO_TIMEOUT_OUTPUT_CONTENT";
    let unknown_body = "PRO_UNKNOWN_OUTPUT_CONTENT";
    let path = write_transcript(
        &root,
        &[
            header("profile-gate", "main"),
            json!({
                "id": "success-result-record",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "success-call",
                    "name": "run_shell_command",
                    "result": {
                        "content": success_body,
                        "error": false,
                        "exitCode": 0,
                        "durationMs": 17
                    }
                }]
            }),
            json!({
                "id": "failure-result-record",
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "failure-call",
                    "name": "run_shell_command",
                    "result": {
                        "content": failure_body,
                        "error": "command failed",
                        "exitCode": 1
                    }
                }]
            }),
            json!({
                "id": "safe-message",
                "timestamp": "2026-01-01T00:00:03.000Z",
                "type": "gemini",
                "content": "safe core message"
            }),
            json!({
                "id": "timeout-result-record",
                "timestamp": "2026-01-01T00:00:04.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "timeout-call",
                    "name": "run_shell_command",
                    "result": {
                        "content": timeout_body,
                        "timedOut": true,
                        "durationMs": 2_000
                    }
                }]
            }),
            json!({
                "id": "unknown-result-record",
                "timestamp": "2026-01-01T00:00:05.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "unknown-call",
                    "name": "run_shell_command",
                    "result": {
                        "content": unknown_body,
                        "timedOut": false,
                        "timeout": false
                    }
                }]
            }),
        ],
    );
    let source = rediscover(&root, &path);

    let mut core_reader = read_gemini_transcript_pages(&source, None).unwrap();
    let mut core_page_ids = Vec::new();
    let mut core_rows = Vec::new();
    while let Some(page) = core_reader.next_page().unwrap() {
        assert!(page.output_pages.is_empty());
        core_page_ids.push(page.identity);
        core_rows.extend(page.events);
    }
    let core_outcome = core_reader.outcome().unwrap();
    assert_eq!(core_rows.len(), 3);
    let serialized_core = serde_json::to_string(&core_rows).unwrap();
    for body in [success_body, failure_body, timeout_body, unknown_body] {
        assert!(!serialized_core.contains(body));
    }
    assert!(!serialized_core.contains("locator"));
    assert!(!serialized_core.contains("output_preview"));
    let diagnostic_outcomes = core_rows
        .iter()
        .filter_map(|row| match &row.body {
            GeminiEventBody::OutputDiagnostic { outcome, .. } => Some(outcome.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(diagnostic_outcomes, ["failure", "timeout"]);
    assert_eq!(
        core_outcome.metrics.result_body_bytes_decoded_or_allocated,
        u64::try_from(failure_body.len() + timeout_body.len()).unwrap()
    );
    assert_eq!(core_outcome.metrics.result_body_hashes_created, 2);
    assert_eq!(core_outcome.metrics.result_previews_created, 2);
    assert_eq!(core_outcome.metrics.result_file_touches_created, 0);
    assert_eq!(core_outcome.metrics.result_fts_documents_created, 0);
    assert_eq!(core_outcome.metrics.result_handoffs_created, 0);

    let mut pro_reader = read_gemini_transcript_pages_with_profile(
        &source,
        None,
        GeminiNativePathProfile::CoreAndTransientOutputs,
    )
    .unwrap();
    let mut pro_page_ids = Vec::new();
    let mut pro_output_page_ids = Vec::new();
    let mut outputs = Vec::new();
    let mut pro_rows = Vec::new();
    let mut saw_terminal_page = false;
    while let Some(mut page) = pro_reader.next_page().unwrap() {
        assert!(page.logical_units <= MAX_GEMINI_NATIVE_PAGE_RECORDS);
        assert!(page.conservative_serialized_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES);
        saw_terminal_page |= page.terminal;
        pro_page_ids.push(page.identity);
        for mut output_page in page.output_pages {
            assert!(output_page.logical_units <= MAX_GEMINI_NATIVE_PAGE_RECORDS);
            assert!(output_page.conservative_serialized_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES);
            pro_output_page_ids.push(output_page.identity);
            outputs.append(&mut output_page.outputs);
        }
        pro_rows.append(&mut page.events);
    }
    let pro_outcome = pro_reader.outcome().unwrap();

    assert_eq!(pro_rows, core_rows);
    assert_eq!(outputs.len(), 4);
    assert_eq!(outputs[0].content, success_body.as_bytes());
    assert_eq!(outputs[0].outcome.outcome, OutputOutcome::Success);
    assert_eq!(outputs[0].outcome.exit_code, Some(0));
    assert_eq!(outputs[0].outcome.duration_ms, Some(17));
    assert_eq!(outputs[1].content, failure_body.as_bytes());
    assert_eq!(outputs[1].outcome.outcome, OutputOutcome::Failure);
    assert_eq!(outputs[1].outcome.exit_code, Some(1));
    assert_eq!(outputs[2].content, timeout_body.as_bytes());
    assert_eq!(outputs[2].outcome.outcome, OutputOutcome::Timeout);
    assert_eq!(outputs[2].outcome.duration_ms, Some(2_000));
    assert_eq!(outputs[3].content, unknown_body.as_bytes());
    assert_eq!(outputs[3].outcome.outcome, OutputOutcome::Success);
    assert_eq!(
        outputs[0].coordinate.native_record_id.as_deref(),
        Some("success-result-record")
    );
    assert_eq!(outputs[0].coordinate.source_record_subrecord_index, Some(0));
    assert_eq!(outputs[0].call_id.as_deref(), Some("success-call"));
    assert_eq!(
        pro_outcome.metrics.result_body_bytes_decoded_or_allocated,
        u64::try_from(
            success_body.len() + failure_body.len() + timeout_body.len() + unknown_body.len()
        )
        .unwrap()
    );
    assert_eq!(pro_outcome.metrics.result_handoffs_created, 4);
    assert_eq!(pro_outcome.metrics.result_body_hashes_created, 2);
    assert_eq!(pro_outcome.metrics.result_previews_created, 2);
    assert_eq!(pro_outcome.metrics.result_file_touches_created, 0);
    assert_eq!(pro_outcome.metrics.result_fts_documents_created, 0);
    assert!(saw_terminal_page);
    assert_eq!(core_page_ids, pro_page_ids);
    assert!(pro_output_page_ids
        .iter()
        .all(|identity| identity.as_bytes() != &[0; 32]));
    assert_eq!(pro_outcome.terminal_source_observation, source.observation);
    assert_eq!(
        pro_outcome.terminal_source_observation,
        pro_outcome.checkpoint.source_observation
    );
}

#[test]
fn gemini_nativepath_core_pages_are_profile_invariant_under_output_unit_pressure() {
    const RESULT_RECORDS: usize = 33;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut values = vec![header("profile-unit-pressure", "main")];
    values.extend((0..RESULT_RECORDS).map(|index| {
        let first_content = if index == 0 {
            String::new()
        } else {
            format!("first-output-{index:02}")
        };
        json!({
            "id": format!("result-{index:02}"),
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "gemini",
            "toolCalls": [
                {
                    "id": format!("first-call-{index:02}"),
                    "result": {"content": first_content}
                },
                {
                    "id": format!("second-call-{index:02}"),
                    "result": {"content": format!("second-output-{index:02}")}
                }
            ]
        })
    }));
    let path = write_transcript(&root, &values);
    let source = rediscover(&root, &path);

    let collect = |profile| {
        let mut reader = read_gemini_transcript_pages_with_profile(&source, None, profile).unwrap();
        let mut core_pages = Vec::new();
        let mut output_pages = Vec::new();
        while let Some(mut page) = reader.next_page().unwrap() {
            output_pages.append(&mut page.output_pages);
            core_pages.push((
                page.expected_frontier,
                page.next_safe_frontier,
                page.identity,
                page.terminal,
                page.physical_records,
                page.logical_units,
                page.retained_event_bytes,
                page.conservative_serialized_bytes,
                page.events,
                page.rejections,
            ));
        }
        let metrics = reader.outcome().unwrap().metrics.clone();
        (core_pages, output_pages, metrics)
    };

    let (core_only_pages, core_only_output_pages, core_only_metrics) =
        collect(GeminiNativePathProfile::CoreOnly);
    let (pro_core_pages, pro_output_pages, pro_metrics) =
        collect(GeminiNativePathProfile::CoreAndTransientOutputs);

    assert_eq!(core_only_pages, pro_core_pages);
    assert_eq!(core_only_pages.len(), 1);
    assert_eq!(core_only_pages[0].4, RESULT_RECORDS + 1);
    assert_eq!(core_only_pages[0].5, 0);
    assert!(core_only_output_pages.is_empty());
    assert_eq!(
        pro_output_pages
            .iter()
            .map(|page| page.logical_units)
            .collect::<Vec<_>>(),
        [MAX_GEMINI_NATIVE_PAGE_RECORDS, 2]
    );
    assert_eq!(
        pro_output_pages
            .iter()
            .map(|page| page.page_ordinal)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert!(pro_output_pages.iter().all(|page| {
        page.logical_units <= MAX_GEMINI_NATIVE_PAGE_RECORDS
            && page.conservative_serialized_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES
            && page.identity.as_bytes() != &[0; 32]
    }));
    assert_ne!(pro_output_pages[0].identity, pro_output_pages[1].identity);
    let outputs = pro_output_pages
        .iter()
        .flat_map(|page| page.outputs.iter())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), RESULT_RECORDS * 2);
    assert!(outputs[0].content.is_empty());
    assert_eq!(outputs[0].coordinate.source_record_subrecord_index, Some(0));
    assert_eq!(outputs[1].coordinate.source_record_subrecord_index, Some(1));
    assert_eq!(core_only_metrics.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(core_only_metrics.result_handoffs_created, 0);
    assert_eq!(
        pro_metrics.result_handoffs_created,
        (RESULT_RECORDS * 2) as u64
    );
}

#[test]
fn gemini_nativepath_core_pages_are_profile_invariant_under_output_byte_pressure() {
    const RESULT_RECORDS: usize = 4;
    const CONTENT_BYTES: usize = 1_650_000;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut values = vec![header("profile-byte-pressure", "main")];
    values.extend((0..RESULT_RECORDS).map(|index| {
        json!({
            "id": format!("large-result-{index}"),
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "gemini",
            "toolCalls": [{
                "id": format!("large-call-{index}"),
                "result": {"content": char::from(b'a' + index as u8).to_string().repeat(CONTENT_BYTES)}
            }]
        })
    }));
    let path = write_transcript(&root, &values);
    let source = rediscover(&root, &path);

    let collect = |profile| {
        let mut reader = read_gemini_transcript_pages_with_profile(&source, None, profile).unwrap();
        let mut core_pages = Vec::new();
        let mut output_pages = Vec::new();
        while let Some(mut page) = reader.next_page().unwrap() {
            output_pages.append(&mut page.output_pages);
            core_pages.push((
                page.expected_frontier,
                page.next_safe_frontier,
                page.identity,
                page.terminal,
                page.physical_records,
                page.logical_units,
                page.retained_event_bytes,
                page.conservative_serialized_bytes,
                page.events,
                page.rejections,
            ));
        }
        let metrics = reader.outcome().unwrap().metrics.clone();
        (core_pages, output_pages, metrics)
    };

    let (core_only_pages, core_only_output_pages, core_only_metrics) =
        collect(GeminiNativePathProfile::CoreOnly);
    let (pro_core_pages, pro_output_pages, pro_metrics) =
        collect(GeminiNativePathProfile::CoreAndTransientOutputs);

    assert_eq!(core_only_pages, pro_core_pages);
    assert_eq!(core_only_pages.len(), 1);
    assert_eq!(core_only_pages[0].4, RESULT_RECORDS + 1);
    assert_eq!(core_only_pages[0].5, 0);
    assert!(core_only_output_pages.is_empty());
    assert!(pro_output_pages.len() > 1);
    assert!(pro_output_pages.iter().all(|page| {
        page.logical_units <= MAX_GEMINI_NATIVE_PAGE_RECORDS
            && page.conservative_serialized_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES
            && page.identity.as_bytes() != &[0; 32]
    }));
    assert!(
        pro_output_pages
            .iter()
            .map(|page| page.conservative_serialized_bytes)
            .sum::<usize>()
            > MAX_GEMINI_NATIVE_PAGE_BYTES
    );
    let outputs = pro_output_pages
        .iter()
        .flat_map(|page| page.outputs.iter())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), RESULT_RECORDS);
    assert!(outputs
        .iter()
        .all(|output| output.content.len() == CONTENT_BYTES));
    assert_eq!(core_only_metrics.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(core_only_metrics.result_handoffs_created, 0);
    assert_eq!(
        pro_metrics.result_body_bytes_decoded_or_allocated,
        (RESULT_RECORDS * CONTENT_BYTES) as u64
    );
    assert_eq!(pro_metrics.result_handoffs_created, RESULT_RECORDS as u64);
}

#[test]
fn gemini_nativepath_reads_each_record_once_and_performs_one_full_pro_hydration() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("one-pass", "main"),
            json!({
                "id": "failure",
                "type": "gemini",
                "toolCalls": [{
                    "id": "failure-call",
                    "result": {"content": "bounded failure", "error": true}
                }]
            }),
            json!({
                "id": "success",
                "type": "gemini",
                "toolCalls": [{
                    "id": "success-call",
                    "result": {"content": "complete success", "success": true}
                }]
            }),
            json!({
                "id": "message",
                "type": "user",
                "content": "later message"
            }),
        ],
    );
    let source = rediscover(&root, &path);

    reset_gemini_parse_counters();
    let (core_outcome, core_rows) = scan_collect(&source, None);
    assert_eq!(core_rows.len(), 2);
    assert_eq!(core_outcome.rejected_records, 0);
    assert_eq!(gemini_parse_counters(), (4, 2, 0));

    reset_gemini_parse_counters();
    let mut pro = read_gemini_transcript_pages_with_profile(
        &source,
        None,
        GeminiNativePathProfile::CoreAndTransientOutputs,
    )
    .unwrap();
    let mut outputs = Vec::new();
    while let Some(page) = pro.next_page().unwrap() {
        outputs.extend(page.output_pages.into_iter().flat_map(|page| page.outputs));
    }
    assert_eq!(outputs.len(), 2);
    assert_eq!(gemini_parse_counters(), (4, 2, 2));
}

#[test]
fn gemini_nativepath_core_only_excludes_large_failure_body_and_locator() {
    const FAILURE_BYTES: usize = 3 * 1024 * 1024;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let canary = "FULL_FAILURE_BODY_MUST_NOT_BE_CONSTRUCTED_IN_CORE";
    // Newlines force escaped JSON string decoding. CoreOnly must still use
    // the raw bounded visitor rather than ask serde_json for an owned body.
    let failure = format!("{}{}", "\n".repeat(FAILURE_BYTES), canary);
    let path = write_transcript(
        &root,
        &[
            header("bounded-failure", "main"),
            json!({
                "id": "large-failure",
                "type": "gemini",
                "toolCalls": [{
                    "id": "failure-call",
                    "result": {
                        "content": failure,
                        "error": true,
                        "exitCode": 1
                    }
                }]
            }),
        ],
    );
    let source = rediscover(&root, &path);

    reset_gemini_parse_counters();
    let (outcome, rows) = scan_collect(&source, None);

    assert_eq!(rows.len(), 1);
    assert!(matches!(
        &rows[0].body,
        GeminiEventBody::OutputDiagnostic {
            call_id: Some(call_id),
            ..
        } if call_id == "failure-call"
    ));
    let serialized = serde_json::to_string(&rows).unwrap();
    assert!(!serialized.contains(canary));
    assert!(!serialized.contains("output_preview"));
    assert!(!serialized.contains("locator"));
    assert!(
        outcome.metrics.result_body_bytes_decoded_or_allocated <= PROVIDER_MAX_PREVIEW_CHARS as u64
    );
    assert_eq!(gemini_parse_counters(), (2, 1, 0));
}

#[test]
fn gemini_nativepath_two_three_mib_outputs_emit_on_independent_pro_pages() {
    const CONTENT_BYTES: usize = 3 * 1024 * 1024;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("independent-output-sizing", "main"),
            json!({
                "id": "two-large-outputs",
                "type": "gemini",
                "toolCalls": [
                    {
                        "id": "first",
                        "result": {"content": "a".repeat(CONTENT_BYTES)}
                    },
                    {
                        "id": "second",
                        "result": {"content": "b".repeat(CONTENT_BYTES)}
                    }
                ]
            }),
        ],
    );
    let source = rediscover(&root, &path);

    let mut core = read_gemini_transcript_pages(&source, None).unwrap();
    let core_page = core.next_page().unwrap().unwrap();
    let core_identity = core_page.identity;
    assert!(core_page.rejections.is_empty());
    assert!(core.next_page().unwrap().is_none());

    let mut pro = read_gemini_transcript_pages_with_profile(
        &source,
        None,
        GeminiNativePathProfile::CoreAndTransientOutputs,
    )
    .unwrap();
    let pro_core_page = pro.next_page().unwrap().unwrap();
    assert_eq!(pro_core_page.identity, core_identity);
    assert_eq!(pro_core_page.output_pages.len(), 2);
    assert!(pro_core_page.output_pages.iter().all(|page| {
        page.logical_units == 1
            && page.conservative_serialized_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES
    }));
    let outputs = pro_core_page
        .output_pages
        .into_iter()
        .flat_map(|page| page.outputs)
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0].content.len(), CONTENT_BYTES);
    assert_eq!(outputs[1].content.len(), CONTENT_BYTES);
    assert!(pro.next_page().unwrap().is_none());
}

#[test]
fn gemini_nativepath_oversized_output_is_local_and_core_identity_stays_profile_invariant() {
    const OVERSIZED_CONTENT_BYTES: usize = 6 * 1024 * 1024;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("oversized-output-local", "main"),
            json!({
                "id": "mixed-size-outputs",
                "type": "gemini",
                "toolCalls": [
                    {
                        "id": "oversized",
                        "result": {"content": "x".repeat(OVERSIZED_CONTENT_BYTES)}
                    },
                    {
                        "id": "small-sibling",
                        "result": {"content": "small sibling survives"}
                    }
                ]
            }),
            json!({
                "id": "later-message",
                "type": "gemini",
                "content": "later record survives"
            }),
        ],
    );
    let source = rediscover(&root, &path);

    let collect = |profile| {
        let mut reader = read_gemini_transcript_pages_with_profile(&source, None, profile).unwrap();
        let mut core = Vec::new();
        let mut outputs = Vec::new();
        while let Some(mut page) = reader.next_page().unwrap() {
            outputs.extend(
                page.output_pages
                    .drain(..)
                    .flat_map(|output_page| output_page.outputs),
            );
            core.push((
                page.identity,
                page.expected_frontier,
                page.next_safe_frontier,
                page.events,
                page.rejections,
            ));
        }
        (core, outputs, reader.outcome().unwrap().clone())
    };

    let (core_only, core_only_outputs, core_outcome) = collect(GeminiNativePathProfile::CoreOnly);
    let (core_and_pro, pro_outputs, pro_outcome) =
        collect(GeminiNativePathProfile::CoreAndTransientOutputs);

    assert_eq!(core_only, core_and_pro);
    assert!(core_only_outputs.is_empty());
    assert_eq!(pro_outputs.len(), 1);
    assert_eq!(pro_outputs[0].content, b"small sibling survives");
    assert_eq!(
        pro_outputs[0].coordinate.source_record_subrecord_index,
        Some(1)
    );
    assert_eq!(core_only.len(), 1);
    assert_eq!(core_only[0].3.len(), 1);
    assert_eq!(
        core_only[0].3[0].identity,
        GeminiEventIdentity::NativeRecordId("later-message".to_owned())
    );
    assert_eq!(core_only[0].4.len(), 1);
    assert!(core_only[0].4[0].reason.contains("output subrecord 0"));
    assert_eq!(core_outcome.rejected_records, 1);
    assert_eq!(pro_outcome.rejected_records, 1);
}
