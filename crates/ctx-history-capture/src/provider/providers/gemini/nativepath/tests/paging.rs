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
fn gemini_nativepath_core_retains_failure_and_timeout_diagnostics() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("diagnostics", "main"),
            json!({
                "id": "success",
                "type": "gemini",
                "toolCalls": [{
                    "id": "success-call",
                    "result": {"content": "successful secret", "exitCode": 0}
                }]
            }),
            json!({
                "id": "failure",
                "type": "gemini",
                "toolCalls": [{
                    "id": "failure-call",
                    "result": {"content": "failure secret", "error": true, "exitCode": 1}
                }]
            }),
            json!({
                "id": "timeout",
                "type": "gemini",
                "toolCalls": [{
                    "id": "timeout-call",
                    "result": {"content": "timeout secret", "timedOut": true}
                }]
            }),
        ],
    );
    let source = rediscover(&root, &path);

    let (outcome, rows) = scan_collect(&source, None);
    let diagnostic_outcomes = rows
        .iter()
        .filter_map(|row| match &row.body {
            GeminiEventBody::OutputDiagnostic { outcome, .. } => Some(outcome.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let serialized = serde_json::to_string(&rows).unwrap();

    assert_eq!(diagnostic_outcomes, ["failure", "timeout"]);
    for secret in ["successful secret", "failure secret", "timeout secret"] {
        assert!(!serialized.contains(secret));
    }
    assert_eq!(outcome.metrics.result_body_hashes_created, 2);
    assert_eq!(outcome.metrics.result_previews_created, 2);
}

#[test]
fn gemini_nativepath_core_bounds_large_failure_diagnostic_parsing() {
    const FAILURE_BYTES: usize = 3 * 1024 * 1024;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let canary = "FULL_FAILURE_BODY_MUST_NOT_BE_CONSTRUCTED_IN_CORE";
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
                    "result": {"content": failure, "error": true, "exitCode": 1}
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
    assert!(!serde_json::to_string(&rows).unwrap().contains(canary));
    assert!(
        outcome.metrics.result_body_bytes_decoded_or_allocated <= PROVIDER_MAX_PREVIEW_CHARS as u64
    );
    assert_eq!(gemini_parse_counters(), (2, 1));
}
