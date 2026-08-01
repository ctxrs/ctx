use super::*;

#[test]
fn gemini_nativepath_unchanged_and_append_emit_only_provider_native_delta() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("root-session", "main"),
            json!({
                "id": "user-1",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "baseline"
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let (baseline, baseline_rows) = scan_collect(&source, None);
    let old_identity = baseline_rows[0].identity.clone();
    let old_order = baseline_rows[0].native_order;
    let previous = previous(&baseline, true);

    let source = rediscover(&root, &path);
    reset_gemini_parse_counters();
    let (unchanged, unchanged_rows) = scan_collect(&source, Some(&previous));
    assert!(unchanged_rows.is_empty());
    assert_eq!(
        gemini_resume_work_counters(),
        (0, baseline.checkpoint.complete_prefix_end)
    );
    assert_eq!(
        unchanged.signals.source_change,
        GeminiSourceChange::Unchanged
    );
    assert_eq!(
        unchanged.signals.publication_shape,
        GeminiPublicationShape::ObservationOnly
    );

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(&jsonl(&[json!({
        "id": "assistant-2",
        "timestamp": "2026-01-01T00:00:02.000Z",
        "type": "gemini",
        "content": "appended"
    })]))
    .unwrap();
    drop(file);
    let source = rediscover(&root, &path);
    reset_gemini_parse_counters();
    let (append, append_rows) = scan_collect(&source, Some(&previous));

    assert_eq!(append.signals.source_change, GeminiSourceChange::Append);
    assert_eq!(
        gemini_resume_work_counters(),
        (1, baseline.checkpoint.complete_prefix_end)
    );
    assert_eq!(
        append.signals.publication_shape,
        GeminiPublicationShape::AppendDelta
    );
    assert_eq!(append_rows.len(), 1);
    assert_eq!(
        append_rows[0].identity,
        GeminiEventIdentity::NativeRecordId("assistant-2".to_owned())
    );
    assert_eq!(append_rows[0].native_order.raw_ordinal, 2);
    assert_eq!(append.checkpoint.retained_event_count, 2);
    assert_eq!(
        old_identity,
        GeminiEventIdentity::NativeRecordId("user-1".to_owned())
    );
    assert_eq!(old_order.raw_ordinal, 1);
}
#[test]
fn gemini_nativepath_repeated_appends_hash_the_prefix_but_parse_only_the_delta() {
    const APPENDS: usize = 96;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(&root, &[header("linear-append-work", "main")]);
    let source = rediscover(&root, &path);
    let (mut prior, baseline_rows) = scan_collect(&source, None);
    assert!(baseline_rows.is_empty());

    let mut total_record_reads = 0_u64;
    let mut total_prefix_bytes = 0_u64;
    for index in 0..APPENDS {
        let expected_prefix_bytes = prior.checkpoint.complete_prefix_end;
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&jsonl(&[json!({
                "id": format!("append-{index:03}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "content": format!("delta-{index:03}")
            })]))
            .unwrap();

        let source = rediscover(&root, &path);
        reset_gemini_parse_counters();
        let (next, rows) = scan_collect(&source, Some(&previous(&prior, true)));
        let (record_reads, prefix_bytes) = gemini_resume_work_counters();

        assert_eq!(next.signals.source_change, GeminiSourceChange::Append);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].native_order.raw_ordinal, index as u64 + 1);
        assert_eq!(record_reads, 1, "append {index} replayed prior records");
        assert_eq!(
            prefix_bytes, expected_prefix_bytes,
            "append {index} did not hash the complete committed prefix"
        );
        total_record_reads += record_reads;
        total_prefix_bytes += prefix_bytes;
        prior = next;
    }

    assert_eq!(total_record_reads, APPENDS as u64);
    assert!(total_prefix_bytes > prior.checkpoint.complete_prefix_end);
    assert_eq!(prior.checkpoint.next_raw_ordinal, APPENDS as u64 + 1);
    assert_eq!(prior.checkpoint.retained_event_count, APPENDS as u64);
}
#[test]
fn gemini_nativepath_full_prefix_hash_rejects_byte_zero_rewrite_with_preserved_tail() {
    const PRESERVED_TAIL_BYTES: usize = 64 * 1024;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = transcript_path(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut baseline_bytes = jsonl(&[
        header("full-prefix-proof", "main"),
        json!({
            "id": "early-event",
            "type": "user",
            "content": "early event remains semantically identical"
        }),
        json!({
            "id": "large-middle",
            "type": "gemini",
            "content": "m".repeat(PRESERVED_TAIL_BYTES + 8 * 1024)
        }),
        json!({
            "id": "tail-event",
            "type": "gemini",
            "content": "the old boundary tail is preserved byte-for-byte"
        }),
    ]);
    baseline_bytes.insert(0, b' ');
    fs::write(&path, &baseline_bytes).unwrap();

    let source = rediscover(&root, &path);
    let mut baseline_reader = read_gemini_transcript_pages(&source, None).unwrap();
    let baseline_page = baseline_reader.next_page().unwrap().unwrap();
    assert!(baseline_reader.next_page().unwrap().is_none());
    let baseline_outcome = baseline_reader.outcome().unwrap().clone();
    let frontier = baseline_page.next_safe_frontier;
    assert!(frontier.complete_prefix_end > PRESERVED_TAIL_BYTES as u64);

    let old_prefix_end = usize::try_from(frontier.complete_prefix_end).unwrap();
    let old_tail = baseline_bytes[old_prefix_end - PRESERVED_TAIL_BYTES..old_prefix_end].to_vec();
    let mut rewritten = baseline_bytes;
    assert_eq!(rewritten[0], b' ');
    rewritten[0] = b'\t';
    fs::write(&path, &rewritten).unwrap();
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&jsonl(&[json!({
            "id": "appended-after-rewrite",
            "type": "gemini",
            "content": "later append must not authorize the rewritten prefix"
        })]))
        .unwrap();
    let rewritten_with_append = fs::read(&path).unwrap();
    assert_eq!(
        &rewritten_with_append[old_prefix_end - PRESERVED_TAIL_BYTES..old_prefix_end],
        old_tail
    );
    let changed_source = rediscover(&root, &path);

    reset_gemini_parse_counters();
    assert!(matches!(
        read_gemini_transcript_pages_from_frontier(&changed_source, &frontier),
        Err(GeminiScanError::Capture(
            CaptureError::SourceChangedDuringCapture
        ))
    ));
    assert_eq!(
        gemini_resume_work_counters(),
        (0, frontier.complete_prefix_end)
    );

    reset_gemini_parse_counters();
    let baseline_previous = previous(&baseline_outcome, true);
    let mut fallback =
        read_gemini_transcript_pages(&changed_source, Some(&baseline_previous)).unwrap();
    assert_eq!(
        gemini_resume_work_counters(),
        (0, baseline_outcome.checkpoint.complete_prefix_end)
    );
    let fallback_page = fallback.next_page().unwrap().unwrap();
    assert_eq!(fallback_page.expected_frontier.complete_prefix_end, 0);
    assert!(fallback.next_page().unwrap().is_none());
    assert_eq!(
        fallback.outcome().unwrap().signals.source_change,
        GeminiSourceChange::Rewrite
    );
    assert!(fallback_page.events.iter().any(|event| {
        event.identity == GeminiEventIdentity::NativeRecordId("appended-after-rewrite".to_owned())
    }));
}

#[test]
fn gemini_nativepath_classifies_rewrite_truncation_and_replacement() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let baseline_values = [
        header("root-session", "main"),
        json!({
            "id": "user-1",
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "user",
            "content": "alpha"
        }),
        json!({
            "id": "assistant-1",
            "timestamp": "2026-01-01T00:00:02.000Z",
            "type": "gemini",
            "content": "reply"
        }),
    ];
    let path = write_transcript(&root, &baseline_values);
    let source = rediscover(&root, &path);
    let (baseline, _) = scan_collect(&source, None);
    let previous_baseline = previous(&baseline, true);

    let rewrite_values = [
        header("root-session", "main"),
        json!({
            "id": "user-1",
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "user",
            "content": "omega"
        }),
        baseline_values[2].clone(),
    ];
    assert_eq!(jsonl(&baseline_values).len(), jsonl(&rewrite_values).len());
    fs::write(&path, jsonl(&rewrite_values)).unwrap();
    let source = rediscover(&root, &path);
    let (rewrite, rewrite_rows) = scan_collect(&source, Some(&previous_baseline));
    assert_eq!(rewrite.signals.source_change, GeminiSourceChange::Rewrite);
    assert_eq!(rewrite_rows.len(), 2);
    assert!(rewrite_rows[0].searchable_text.contains("omega"));

    fs::write(&path, jsonl(&rewrite_values[..2])).unwrap();
    let source = rediscover(&root, &path);
    let (truncation, truncation_rows) = scan_collect(&source, Some(&previous_baseline));
    assert_eq!(
        truncation.signals.source_change,
        GeminiSourceChange::Truncation
    );
    assert_eq!(truncation_rows.len(), 1);

    let replacement_values = [
        header("replacement-session", "main"),
        json!({
            "id": "replacement-user",
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "user",
            "content": "replacement"
        }),
    ];
    fs::write(&path, jsonl(&replacement_values)).unwrap();
    let source = rediscover(&root, &path);
    let (replacement, replacement_rows) = scan_collect(&source, Some(&previous_baseline));
    assert_eq!(
        replacement.signals.source_change,
        GeminiSourceChange::Replacement
    );
    assert_eq!(replacement_rows.len(), 1);
    assert_eq!(
        replacement
            .checkpoint
            .session
            .as_ref()
            .unwrap()
            .native_session_id,
        "replacement-session"
    );
}

#[test]
fn gemini_nativepath_distinguishes_relocation_from_live_copy() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("root-session", "main"),
            json!({
                "id": "user-1",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "portable"
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let (baseline, _) = scan_collect(&source, None);
    let moved = root.join("tmp/relocated/chats/session-root.jsonl");
    fs::create_dir_all(moved.parent().unwrap()).unwrap();
    fs::copy(&path, &moved).unwrap();

    let moved_source = rediscover(&root, &moved);
    let (live_copy, live_copy_rows) = scan_collect(&moved_source, Some(&previous(&baseline, true)));
    assert_eq!(
        live_copy.signals.source_change,
        GeminiSourceChange::LiveCopy
    );
    assert_eq!(live_copy_rows.len(), 1);

    fs::remove_file(&path).unwrap();
    let moved_source = rediscover(&root, &moved);
    let (relocation, relocation_rows) =
        scan_collect(&moved_source, Some(&previous(&baseline, false)));
    assert_eq!(
        relocation.signals.source_change,
        GeminiSourceChange::Relocation
    );
    assert_eq!(relocation_rows.len(), 1);
    assert_eq!(relocation.checkpoint.session, baseline.checkpoint.session);
}

#[test]
fn gemini_nativepath_treats_divergent_routes_as_independent_replacements() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("root-session", "main"),
            json!({
                "id": "user-1",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "original"
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let (baseline, _) = scan_collect(&source, None);

    let divergent = root.join("tmp/divergent/chats/session-root.jsonl");
    fs::create_dir_all(divergent.parent().unwrap()).unwrap();
    fs::write(
        &divergent,
        jsonl(&[
            header("root-session", "main"),
            json!({
                "id": "user-1",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "divergent"
            }),
        ]),
    )
    .unwrap();
    let divergent_source = rediscover(&root, &divergent);
    let (divergent_outcome, divergent_rows) =
        scan_collect(&divergent_source, Some(&previous(&baseline, false)));

    assert_eq!(divergent_rows.len(), 1);
    assert_eq!(
        divergent_outcome.signals.source_change,
        GeminiSourceChange::Replacement
    );
    assert_eq!(
        divergent_outcome.signals.publication_shape,
        GeminiPublicationShape::AuthoritativeSnapshot
    );
    assert!(divergent_outcome.signals.cursor_advance_allowed);
    assert!(!divergent_outcome.signals.emitted_zero_rows);
    assert_ne!(
        divergent_outcome.checkpoint.source_sha256,
        baseline.checkpoint.source_sha256
    );

    let incompatible = root.join("tmp/relocated/chats/root-session/session-root.jsonl");
    fs::create_dir_all(incompatible.parent().unwrap()).unwrap();
    fs::copy(&path, &incompatible).unwrap();
    let incompatible_source = rediscover(&root, &incompatible);
    let (incompatible_outcome, incompatible_rows) =
        scan_collect(&incompatible_source, Some(&previous(&baseline, false)));

    assert_eq!(incompatible_rows.len(), 1);
    assert_eq!(
        incompatible_outcome.signals.source_change,
        GeminiSourceChange::Replacement
    );
    assert!(incompatible_outcome.signals.cursor_advance_allowed);
    assert_eq!(
        incompatible_outcome
            .checkpoint
            .session
            .as_ref()
            .unwrap()
            .parent_native_session_id
            .as_deref(),
        Some("root-session")
    );
}

#[test]
fn gemini_source_backed_rejects_cross_page_duplicate_before_canonical_writer() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut records = vec![header("duplicate-session", "main")];
    records.extend((0..MAX_GEMINI_NATIVE_PAGE_RECORDS - 1).map(|index| {
        json!({
            "id": if index == 0 {
                "duplicate-id".to_owned()
            } else {
                format!("valid-{index:02}")
            },
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "user",
            "content": format!("retained request {index}")
        })
    }));
    records.push(json!({
        "id": "duplicate-id",
        "timestamp": "2026-01-01T00:00:02.000Z",
        "type": "gemini",
        "content": "same canonical native identity on the next bounded page"
    }));
    records.push(json!({
        "id": "later-valid",
        "timestamp": "2026-01-01T00:00:03.000Z",
        "type": "gemini",
        "content": "later sibling survives"
    }));
    let mut native_item_ids = GeminiSourceNativeItemIds::default();
    let admitted = records
        .iter()
        .map(|record| serde_json::to_vec(record).unwrap())
        .map(|record| native_item_ids.admit(&record))
        .collect::<Vec<_>>();
    assert!(admitted[..MAX_GEMINI_NATIVE_PAGE_RECORDS]
        .iter()
        .all(|admitted| *admitted));
    assert!(!admitted[MAX_GEMINI_NATIVE_PAGE_RECORDS]);
    assert!(admitted[MAX_GEMINI_NATIVE_PAGE_RECORDS + 1]);
    let path = write_transcript(&root, &records);
    let source = rediscover(&root, &path);
    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();

    let first_page = reader.next_page().unwrap().unwrap();
    assert_eq!(first_page.physical_records, MAX_GEMINI_NATIVE_PAGE_RECORDS);
    assert_eq!(first_page.events.len(), MAX_GEMINI_NATIVE_PAGE_RECORDS - 1);
    assert!(first_page.rejections.is_empty());
    let committed_valid_identity = first_page.identity;

    let second_page = reader.next_page().unwrap().unwrap();
    assert_eq!(second_page.expected_frontier, first_page.next_safe_frontier);
    assert!(second_page.rejections.is_empty());
    assert_eq!(second_page.events.len(), 2);
    assert_eq!(
        second_page.events[0].identity,
        GeminiEventIdentity::NativeRecordId("duplicate-id".to_owned())
    );
    assert_eq!(
        first_page.events[0].identity,
        second_page.events[0].identity
    );
    assert_eq!(
        second_page.events[1].identity,
        GeminiEventIdentity::NativeRecordId("later-valid".to_owned())
    );
    let mut retained_events = first_page.events.clone();
    retained_events.extend(second_page.events.clone());
    let projected = project_gemini_test_events(&source, retained_events).unwrap();
    assert_eq!(projected.len(), MAX_GEMINI_NATIVE_PAGE_RECORDS);
    assert_eq!(
        projected
            .iter()
            .map(|record| record.event_id.digest())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        projected.len()
    );
    assert!(!serde_json::to_string(&projected)
        .unwrap()
        .contains("same canonical native identity on the next bounded page"));
    let replay_frontier = second_page.expected_frontier.clone();
    let second_identity = second_page.identity;
    assert!(reader.next_page().unwrap().is_none());
    assert_eq!(reader.outcome().unwrap().rejected_records, 0);

    let mut replay = read_gemini_transcript_pages_from_frontier(&source, &replay_frontier).unwrap();
    let replayed = replay.next_page().unwrap().unwrap();
    assert_eq!(replayed.identity, second_identity);
    assert_eq!(replayed.rejections, second_page.rejections);
    assert_eq!(replayed.events, second_page.events);
    assert_ne!(committed_valid_identity, replayed.identity);
}

#[test]
fn gemini_nativepath_rejects_duplicates_within_one_bounded_page() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let duplicate_sentinel = "WITHIN_PAGE_DUPLICATE_MUST_STAY_PRIVATE";
    let path = write_transcript(
        &root,
        &[
            header("within-page-duplicate", "main"),
            json!({
                "id": "duplicate-id",
                "type": "user",
                "content": "first canonical observation"
            }),
            json!({
                "id": "duplicate-id",
                "type": "gemini",
                "content": duplicate_sentinel
            }),
            json!({
                "id": "later-valid",
                "type": "gemini",
                "content": "later sibling survives"
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();

    let page = reader.next_page().unwrap().unwrap();
    assert_eq!(page.physical_records, 4);
    assert_eq!(page.events.len(), 2);
    assert_eq!(page.rejections.len(), 1);
    assert!(page.rejections[0]
        .reason
        .contains("duplicate Gemini native event id"));
    assert_eq!(
        page.events
            .iter()
            .map(|event| &event.identity)
            .collect::<Vec<_>>(),
        [
            &GeminiEventIdentity::NativeRecordId("duplicate-id".to_owned()),
            &GeminiEventIdentity::NativeRecordId("later-valid".to_owned())
        ]
    );
    assert!(!format!("{page:?}").contains(duplicate_sentinel));
    assert!(reader.next_page().unwrap().is_none());
    assert_eq!(reader.outcome().unwrap().rejected_records, 1);
}

#[test]
fn gemini_nativepath_native_event_identity_state_has_exact_count_and_byte_bounds() {
    let mut count_bounded = GeminiNativeEventIds::with_limits(2, 100);
    count_bounded.insert("first".to_owned(), 0).unwrap();
    count_bounded.insert("second".to_owned(), 1).unwrap();
    assert!(matches!(
        count_bounded.insert("third".to_owned(), 2),
        Err(GeminiScanError::NativeEventIdentityCountOverflow { limit: 2 })
    ));

    let mut byte_bounded = GeminiNativeEventIds::with_limits(10, 5);
    byte_bounded.insert("12345".to_owned(), 0).unwrap();
    assert!(matches!(
        byte_bounded.insert("6".to_owned(), 1),
        Err(GeminiScanError::NativeEventIdentityBytesOverflow { limit: 5 })
    ));

    let mut duplicate = GeminiNativeEventIds::with_limits(1, 3);
    duplicate.insert("one".to_owned(), 7).unwrap();
    assert!(matches!(
        duplicate.insert("one".to_owned(), 9),
        Err(GeminiScanError::DuplicateNativeEventId {
            first_raw_ordinal: 7,
            duplicate_raw_ordinal: 9,
            ..
        })
    ));
}
