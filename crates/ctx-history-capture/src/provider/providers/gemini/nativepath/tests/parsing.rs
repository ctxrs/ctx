use super::*;

#[test]
fn gemini_nativepath_retains_core_rows_without_header_or_result_material() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let output_sentinel = "NATIVEPATH_SYNTHETIC_OUTPUT_GEMINI_PRIVATE";
    let path = write_transcript(
        &root,
        &[
            header("root-session", "main"),
            json!({
                "id": "user-1",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "hello Gemini"
            }),
            json!({
                "id": "assistant-1",
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "gemini",
                "content": "hello user",
                "model": "gemini-test"
            }),
            json!({
                "id": "request-1",
                "timestamp": "2026-01-01T00:00:03.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "call-1",
                    "name": "write_file",
                    "args": {"path": "safe-request.txt", "content": "safe"}
                }]
            }),
            json!({
                "id": "result-1",
                "timestamp": "2026-01-01T00:00:04.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "call-1",
                    "name": "write_file",
                    "result": {
                        "content": output_sentinel,
                        "path": "/workspace/nativepath-fixture/output-only/leak.txt"
                    }
                }]
            }),
            json!({
                "id": "state-1",
                "timestamp": "2026-01-01T00:00:05.000Z",
                "$set": {"summary": "checkpoint state", "synthetic": true}
            }),
            json!({
                "id": "future-1",
                "timestamp": "2026-01-01T00:00:06.000Z",
                "type": "future_record",
                "content": "must not fabricate a notice"
            }),
        ],
    );
    let source = rediscover(&root, &path);

    let (outcome, rows) = scan_collect(&source, None);

    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows.iter()
            .map(|row| row.native_order.raw_ordinal)
            .collect::<Vec<_>>(),
        [1, 2, 3, 5]
    );
    assert_eq!(
        rows.iter().map(|row| row.event_type).collect::<Vec<_>>(),
        [
            EventType::Message,
            EventType::Message,
            EventType::ToolCall,
            EventType::Notice
        ]
    );
    assert_eq!(rows[0].role, EventRole::User);
    assert_eq!(rows[1].role, EventRole::Assistant);
    assert_eq!(
        rows[2].safe_file_touches,
        vec!["safe-request.txt".to_owned()]
    );
    assert!(matches!(rows[3].body, GeminiEventBody::StateNotice { .. }));
    assert!(rows.iter().all(|row| {
        !format!("{row:?}").contains(output_sentinel)
            && !row
                .safe_file_touches
                .iter()
                .any(|path| path.contains("output-only"))
    }));
    assert_eq!(outcome.metrics.header_records, 1);
    assert_eq!(outcome.metrics.native_result_records_observed, 1);
    assert!(outcome.metrics.native_result_record_bytes_observed > 0);
    assert_eq!(outcome.metrics.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(outcome.metrics.result_body_hashes_created, 0);
    assert_eq!(outcome.metrics.result_previews_created, 0);
    assert_eq!(outcome.metrics.result_file_touches_created, 0);
    assert_eq!(outcome.metrics.result_fts_documents_created, 0);
    assert_eq!(outcome.metrics.result_handoffs_created, 0);
    assert_eq!(outcome.checkpoint.retained_event_count, 4);
    assert_eq!(outcome.signals.source_change, GeminiSourceChange::Fresh);
    assert_eq!(
        outcome.signals.publication_shape,
        GeminiPublicationShape::AuthoritativeSnapshot
    );
    assert!(!outcome.signals.emitted_zero_rows);
    assert!(outcome.signals.cursor_advance_allowed);
}

#[test]
fn gemini_nativepath_malformed_record_is_local_and_incomplete_tail_is_nonterminal() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = transcript_path(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = jsonl(&[
        header("root-session", "main"),
        json!({
            "id": "before",
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "user",
            "content": "before malformed"
        }),
    ]);
    bytes.extend_from_slice(b"{not-json}\n");
    bytes.extend_from_slice(&jsonl(&[json!({
        "id": "after",
        "timestamp": "2026-01-01T00:00:03.000Z",
        "type": "gemini",
        "content": "after malformed"
    })]));
    let complete_prefix_end = bytes.len() as u64;
    bytes.extend_from_slice(
        br#"{"id":"partial","timestamp":"2026-01-01T00:00:04.000Z","type":"gemini","content":"unfinished"#,
    );
    fs::write(&path, bytes).unwrap();
    let source = rediscover(&root, &path);

    let (outcome, rows) = scan_collect(&source, None);

    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .map(|row| row.native_order.raw_ordinal)
            .collect::<Vec<_>>(),
        [1, 3]
    );
    assert_eq!(outcome.rejected_records, 1);
    assert_eq!(outcome.rejections.len(), 1);
    assert!(outcome.rejections[0]
        .reason
        .contains("malformed Gemini JSONL"));
    assert_eq!(outcome.checkpoint.complete_prefix_end, complete_prefix_end);
    assert_eq!(outcome.checkpoint.next_raw_ordinal, 4);
    assert!(!outcome.checkpoint.terminal);
    assert_eq!(
        outcome.signals.completeness,
        GeminiCompleteness::NonterminalCompletePrefix {
            end: complete_prefix_end
        }
    );
    assert!(outcome.signals.cursor_advance_allowed);
}

#[test]
fn gemini_nativepath_structural_rejections_advance_with_durable_detail_and_resume_exactly() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = transcript_path(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = jsonl(&[header("structural-rejections", "main")]);
    bytes.extend_from_slice(b"{malformed-json}\n");
    bytes.extend_from_slice(&jsonl(&[json!({
        "id": "after-malformed",
        "timestamp": "2026-01-01T00:00:01.000Z",
        "type": "user",
        "content": "later sibling"
    })]));
    fs::write(&path, bytes).unwrap();
    let source = rediscover(&root, &path);

    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();
    let page = reader.next_page().unwrap().unwrap();
    assert_eq!(page.rejections.len(), 1);
    assert_eq!(page.rejections[0].raw_ordinal, 1);
    assert!(matches!(
        page.rejections[0].kind,
        GeminiRejectionKind::InvalidRecord
    ));
    assert!(page.rejections[0].reason.contains("malformed Gemini JSONL"));
    assert_eq!(page.next_safe_frontier.rejected_records, 1);
    assert_eq!(page.next_safe_frontier.next_raw_ordinal, 3);
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].native_order.raw_ordinal, 2);
    let page_identity = page.identity;
    let expected_frontier = page.expected_frontier.clone();
    let committed_frontier = page.next_safe_frontier.clone();
    assert!(reader.next_page().unwrap().is_none());
    let outcome = reader.outcome().unwrap();
    assert_eq!(outcome.checkpoint.rejected_records, 1);
    assert!(outcome.signals.cursor_advance_allowed);

    let mut replay =
        read_gemini_transcript_pages_from_frontier(&source, &expected_frontier).unwrap();
    let replayed_page = replay.next_page().unwrap().unwrap();
    assert_eq!(replayed_page.identity, page_identity);
    assert_eq!(replayed_page.rejections, page.rejections);
    assert_eq!(replayed_page.events, page.events);

    let mut after_commit =
        read_gemini_transcript_pages_from_frontier(&source, &committed_frontier).unwrap();
    assert!(after_commit.next_page().unwrap().is_none());
    assert_eq!(after_commit.outcome().unwrap().rejected_records, 1);

    let mut oversized = jsonl(&[header("structural-rejections", "main")]);
    oversized.extend_from_slice(br#"{"payload":""#);
    oversized.extend(std::iter::repeat_n(
        b'x',
        MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(64),
    ));
    oversized.extend_from_slice(b"\"}\n");
    oversized.extend_from_slice(&jsonl(&[json!({
        "id": "after-oversized",
        "timestamp": "2026-01-01T00:00:02.000Z",
        "type": "gemini",
        "content": "still replayable"
    })]));
    fs::write(&path, oversized).unwrap();
    let source = rediscover(&root, &path);
    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();
    let page = reader.next_page().unwrap().unwrap();
    assert_eq!(page.rejections.len(), 1);
    assert_eq!(page.rejections[0].raw_ordinal, 1);
    assert!(page.rejections[0].reason.contains("byte limit"));
    assert_eq!(page.next_safe_frontier.rejected_records, 1);
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].native_order.raw_ordinal, 2);
    assert!(reader.next_page().unwrap().is_none());
    assert!(reader.outcome().unwrap().signals.cursor_advance_allowed);
}

#[test]
fn gemini_nativepath_hydration_failure_keeps_later_siblings_replayable() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("hydration-retry", "main"),
            json!({
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "missing native id"
            }),
            json!({
                "id": "later-valid",
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "gemini",
                "content": "must not be skipped"
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let (outcome, rows) = scan_collect(&source, None);

    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].identity,
        GeminiEventIdentity::NativeRecordId("later-valid".to_owned())
    );
    assert_eq!(outcome.checkpoint.next_raw_ordinal, 3);
    assert_eq!(outcome.checkpoint.retained_event_count, 1);
    assert_eq!(outcome.checkpoint.rejected_records, 1);
    assert!(outcome.rejections[0]
        .reason
        .contains("missing a nonempty native id"));
}

#[test]
fn gemini_nativepath_invalid_hydration_size_and_touch_rows_do_not_block_later_records() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let overflow_calls = (0..=MAX_GEMINI_FILE_TOUCHES_PER_EVENT)
        .map(|index| {
            json!({
                "id": format!("call-{index}"),
                "name": "write_file",
                "args": {"path": format!("path-{index}.txt")}
            })
        })
        .collect::<Vec<_>>();
    let path = write_transcript(
        &root,
        &[
            header("independent-invalid-rows", "main"),
            json!({
                "type": "user",
                "content": "missing native id"
            }),
            json!({
                "id": "retained-size-overflow",
                "type": "user",
                "content": "x".repeat(MAX_GEMINI_NATIVE_PAGE_BYTES / 2 + 1024)
            }),
            json!({
                "id": "touch-overflow",
                "type": "gemini",
                "toolCalls": overflow_calls
            }),
            json!({
                "id": "later-valid",
                "type": "gemini",
                "content": "later record survives all independent rejections"
            }),
        ],
    );
    let source = rediscover(&root, &path);

    let (outcome, rows) = scan_collect(&source, None);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].native_order.raw_ordinal, 4);
    assert_eq!(outcome.rejected_records, 3);
    assert!(outcome
        .rejections
        .iter()
        .any(|rejection| rejection.reason.contains("missing a nonempty native id")));
    assert!(outcome
        .rejections
        .iter()
        .any(|rejection| rejection.reason.contains("retained event exceeds")));
    assert!(outcome
        .rejections
        .iter()
        .any(|rejection| rejection.reason.contains("file-touch limit")));
    assert_eq!(outcome.checkpoint.next_raw_ordinal, 5);
    assert!(outcome.signals.cursor_advance_allowed);
}

#[test]
fn gemini_nativepath_every_unterminated_final_record_stays_uncommitted() {
    let mut oversized = vec![b'x'; MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(64)];
    oversized.insert(0, b'{');
    let cases = [
        (
            "valid-json",
            serde_json::to_vec(&json!({
                "id": "committed-before-tail",
                "type": "user",
                "content": "valid but unterminated"
            }))
            .unwrap(),
        ),
        ("syntax-error", br#"{"id":"broken",]}"#.to_vec()),
        ("oversized", oversized),
    ];

    for (case, tail) in cases {
        let temp = TempDir::new().unwrap();
        let root = fixture_root(&temp);
        let path = transcript_path(&root);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let committed = jsonl(&[
            header(&format!("unterminated-{case}"), "main"),
            json!({
                "id": "committed-before-tail",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "committed"
            }),
        ]);
        let committed_end = u64::try_from(committed.len()).unwrap();
        let mut bytes = committed;
        bytes.extend_from_slice(&tail);
        fs::write(&path, bytes).unwrap();
        let source = rediscover(&root, &path);
        let mut reader = read_gemini_transcript_pages(&source, None).unwrap();

        let page = reader.next_page().unwrap().unwrap();
        assert!(!page.terminal, "{case}");
        assert_eq!(page.physical_records, 2, "{case}");
        assert_eq!(page.events.len(), 1, "{case}");
        assert_eq!(page.next_safe_frontier.complete_prefix_end, committed_end);
        assert_eq!(page.next_safe_frontier.next_raw_ordinal, 2);
        assert_eq!(page.next_safe_frontier.rejected_records, 0);
        assert!(reader.next_page().unwrap().is_none());
        let outcome = reader.outcome().unwrap();
        assert_eq!(page.terminal, outcome.checkpoint.terminal, "{case}");
        assert!(!outcome.checkpoint.terminal, "{case}");
        assert_eq!(outcome.checkpoint.complete_prefix_end, committed_end);
        assert_eq!(outcome.checkpoint.next_raw_ordinal, 2);
        assert_eq!(outcome.rejected_records, 0);
    }
}

#[test]
fn gemini_nativepath_resumes_at_incomplete_record_boundary_when_tail_completes() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = transcript_path(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = jsonl(&[
        header("root-session", "main"),
        json!({
            "id": "complete-user",
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "user",
            "content": "complete"
        }),
    ]);
    bytes.extend_from_slice(
        br#"{"id":"tail-assistant","timestamp":"2026-01-01T00:00:02.000Z","type":"gemini","content":"tail"#,
    );
    fs::write(&path, bytes).unwrap();
    let source = rediscover(&root, &path);
    let (incomplete, incomplete_rows) = scan_collect(&source, None);
    assert_eq!(incomplete_rows.len(), 1);
    assert!(!incomplete.checkpoint.terminal);

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"\"}\n").unwrap();
    drop(file);
    let source = rediscover(&root, &path);
    let (completed, delta_rows) = scan_collect(&source, Some(&previous(&incomplete, true)));

    assert_eq!(completed.signals.source_change, GeminiSourceChange::Append);
    assert_eq!(
        completed.signals.publication_shape,
        GeminiPublicationShape::AppendDelta
    );
    assert!(completed.checkpoint.terminal);
    assert_eq!(delta_rows.len(), 1);
    assert_eq!(
        delta_rows[0].identity,
        GeminiEventIdentity::NativeRecordId("tail-assistant".to_owned())
    );
    assert_eq!(delta_rows[0].native_order.raw_ordinal, 2);
    assert_eq!(completed.checkpoint.retained_event_count, 2);
}

#[test]
fn gemini_nativepath_physical_growth_with_only_incomplete_bytes_is_append_delta() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("root-session", "main"),
            json!({
                "id": "complete-user",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "complete"
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let (baseline, _) = scan_collect(&source, None);
    let boundary = baseline.checkpoint.complete_prefix_end;
    let boundary_hash = baseline.checkpoint.complete_prefix_sha256;

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(
        br#"{"id":"partial-append","timestamp":"2026-01-01T00:00:02.000Z","type":"gemini","content":"still incomplete"#,
    )
    .unwrap();
    drop(file);
    let source = rediscover(&root, &path);
    let (partial, partial_rows) = scan_collect(&source, Some(&previous(&baseline, true)));

    assert!(partial_rows.is_empty());
    assert_eq!(partial.signals.source_change, GeminiSourceChange::Append);
    assert_eq!(
        partial.signals.publication_shape,
        GeminiPublicationShape::AppendDelta
    );
    assert_eq!(
        partial.signals.completeness,
        GeminiCompleteness::NonterminalCompletePrefix { end: boundary }
    );
    assert_eq!(partial.checkpoint.complete_prefix_end, boundary);
    assert_eq!(partial.checkpoint.complete_prefix_sha256, boundary_hash);
    assert_eq!(
        partial.checkpoint.retained_event_count,
        baseline.checkpoint.retained_event_count
    );
    assert!(partial.checkpoint.append_boundary_safe);
    assert!(partial.signals.cursor_advance_allowed);
    assert!(partial.signals.content_changed);

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"\"}\n").unwrap();
    drop(file);
    let source = rediscover(&root, &path);
    let (completed, completed_rows) = scan_collect(&source, Some(&previous(&partial, true)));

    assert_eq!(completed.signals.source_change, GeminiSourceChange::Append);
    assert_eq!(
        completed.signals.publication_shape,
        GeminiPublicationShape::AppendDelta
    );
    assert_eq!(completed_rows.len(), 1);
    assert_eq!(completed_rows[0].native_order.raw_ordinal, 2);
}
