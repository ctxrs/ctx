use super::*;

#[test]
fn gemini_nativepath_output_selection_matches_transient_subrecord_oracle() {
    let fixtures = [
        ("direct-string", json!("direct-string")),
        (
            "empty-content",
            json!({
                "content": "",
                "output": "EMPTY_CONTENT_MUST_NOT_FALL_THROUGH",
                "text": "lower-priority"
            }),
        ),
        (
            "array-content",
            json!({
                "content": [],
                "output": "ARRAY_CONTENT_MUST_NOT_FALL_THROUGH"
            }),
        ),
        (
            "object-content",
            json!({
                "content": {"nested": "unsupported"},
                "output": "OBJECT_CONTENT_MUST_NOT_FALL_THROUGH"
            }),
        ),
        (
            "null-content",
            json!({
                "content": null,
                "output": "NULL_CONTENT_MUST_NOT_FALL_THROUGH"
            }),
        ),
        (
            "multiple-fields",
            json!({
                "content": "content-wins",
                "output": "output-loses",
                "text": "text-loses"
            }),
        ),
        (
            "output-before-text",
            json!({
                "output": "output-wins",
                "text": "text-loses"
            }),
        ),
        ("text-only", json!({"text": "text-wins"})),
    ];

    for (case, result) in fixtures {
        let temp = TempDir::new().unwrap();
        let root = fixture_root(&temp);
        let record = json!({
            "id": format!("record-{case}"),
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "gemini",
            "toolCalls": [{
                "id": format!("call-{case}"),
                "result": result
            }]
        });
        let oracle = gemini_result_subrecord_oracle_for_tests(&record).map(|subrecords| {
            subrecords
                .into_iter()
                .next()
                .and_then(|(_, content, _)| content)
        });
        let path = write_transcript(
            &root,
            &[header(&format!("precedence-{case}"), "main"), record],
        );
        let source = rediscover(&root, &path);

        let collect = |profile| {
            let mut reader =
                read_gemini_transcript_pages_with_profile(&source, None, profile).unwrap();
            let mut core_pages = Vec::new();
            let mut outputs = Vec::new();
            while let Some(page) = reader.next_page().unwrap() {
                outputs.extend(
                    page.output_pages
                        .into_iter()
                        .flat_map(|page| page.outputs)
                        .map(|output| output.content),
                );
                core_pages.push((
                    page.identity,
                    page.expected_frontier,
                    page.next_safe_frontier,
                    page.terminal,
                    page.physical_records,
                    page.logical_units,
                    page.conservative_serialized_bytes,
                    page.events,
                    page.rejections,
                ));
            }
            let rejected_records = reader.outcome().unwrap().rejected_records;
            (core_pages, outputs, rejected_records)
        };

        let (core_pages, core_outputs, core_rejections) =
            collect(GeminiNativePathProfile::CoreOnly);
        let (pro_core_pages, pro_outputs, pro_rejections) =
            collect(GeminiNativePathProfile::CoreAndTransientOutputs);
        assert_eq!(core_pages, pro_core_pages, "{case}");
        assert!(core_outputs.is_empty(), "{case}");
        assert_eq!(core_rejections, pro_rejections, "{case}");

        match oracle {
            Ok(Some(expected)) => {
                assert_eq!(pro_outputs, [expected.into_bytes()], "{case}");
            }
            Ok(None) | Err(NativeJsonlResultExtractionError::InvalidShape) => {
                assert!(pro_outputs.is_empty(), "{case}");
            }
            other => panic!("unexpected output oracle result for {case}: {other:?}"),
        }
    }
}

#[test]
fn gemini_nativepath_outcomes_match_shared_transient_subrecord_oracle() {
    let fixtures = [
        (
            "empty-array-error",
            json!({"result": {"content": "empty-array", "error": []}}),
        ),
        (
            "empty-object-error",
            json!({"result": {"content": "empty-object", "error": {}}}),
        ),
        (
            "nonempty-array-error",
            json!({"result": {"content": "nonempty-array", "error": ["failure"]}}),
        ),
        (
            "string-false-error",
            json!({"result": {"content": "string-false", "error": "false"}}),
        ),
        (
            "floating-error",
            json!({"result": {"content": "floating-error", "error": 1.5}}),
        ),
        (
            "status-timeout",
            json!({"status": "timeout", "result": {"content": "status-timeout"}}),
        ),
        (
            "boolean-timeout",
            json!({"timeout": true, "result": {"content": "boolean-timeout"}}),
        ),
        (
            "spaced-timeout-status",
            json!({"status": "timed out", "result": {"content": "spaced-timeout"}}),
        ),
        (
            "false-timeout-success",
            json!({"timedOut": false, "result": {"content": "false-timeout"}}),
        ),
        (
            "false-ok-unknown",
            json!({"ok": false, "result": {"content": "false-ok"}}),
        ),
        (
            "nested-meta-redacted",
            json!({
                "result": {
                    "content": "nested-redaction-is-not-authoritative",
                    "meta": {"redacted": true}
                }
            }),
        ),
    ];

    for (case, mut call) in fixtures {
        call["id"] = json!(format!("call-{case}"));
        let record = json!({
            "id": format!("record-{case}"),
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "gemini",
            "toolCalls": [call]
        });
        let oracle = gemini_result_subrecord_oracle_for_tests(&record).unwrap();
        assert_eq!(oracle.len(), 1, "{case}");
        let (expected_subordinal, expected_content, expected_outcome) = &oracle[0];
        assert_eq!(*expected_subordinal, 0, "{case}");

        let temp = TempDir::new().unwrap();
        let root = fixture_root(&temp);
        let path = write_transcript(&root, &[header(&format!("outcome-{case}"), "main"), record]);
        let source = rediscover(&root, &path);
        let collect = |profile| {
            let mut reader =
                read_gemini_transcript_pages_with_profile(&source, None, profile).unwrap();
            let mut core_pages = Vec::new();
            let mut outputs = Vec::new();
            while let Some(page) = reader.next_page().unwrap() {
                outputs.extend(
                    page.output_pages
                        .into_iter()
                        .flat_map(|page| page.outputs)
                        .map(|output| (output.content, output.outcome)),
                );
                core_pages.push((
                    page.identity,
                    page.expected_frontier,
                    page.next_safe_frontier,
                    page.terminal,
                    page.physical_records,
                    page.logical_units,
                    page.conservative_serialized_bytes,
                    page.events,
                    page.rejections,
                ));
            }
            (core_pages, outputs)
        };

        let (core_pages, core_outputs) = collect(GeminiNativePathProfile::CoreOnly);
        let (pro_core_pages, pro_outputs) =
            collect(GeminiNativePathProfile::CoreAndTransientOutputs);
        assert_eq!(core_pages, pro_core_pages, "{case}");
        assert!(core_outputs.is_empty(), "{case}");
        assert_eq!(
            pro_outputs.len(),
            usize::from(expected_content.is_some()),
            "{case}"
        );
        if let Some(expected_content) = expected_content {
            assert_eq!(pro_outputs[0].0, expected_content.as_bytes(), "{case}");
            assert_eq!(&pro_outputs[0].1, expected_outcome, "{case}");
        }
        let retained_core_events = core_pages.iter().map(|page| page.7.len()).sum::<usize>();
        assert_eq!(
            retained_core_events,
            usize::from(matches!(
                expected_outcome.outcome,
                OutputOutcome::Failure | OutputOutcome::Timeout
            )),
            "{case}"
        );
    }
}

#[test]
fn gemini_nativepath_redaction_suppresses_core_and_transient_output() {
    let base_record = |case: &str| {
        json!({
            "id": format!("record-{case}"),
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "gemini",
            "toolCalls": [{
                "id": format!("call-{case}"),
                "timedOut": true,
                "result": {
                    "content": format!("SECRET_REDACTED_DIAGNOSTIC_{case}")
                }
            }]
        })
    };
    let mut fixtures = Vec::new();

    let mut record = base_record("record-true");
    record["redacted"] = json!(true);
    fixtures.push(("record-true", record));

    let mut record = base_record("record-null");
    record["redacted"] = Value::Null;
    fixtures.push(("record-null", record));

    let mut record = base_record("call-string");
    record["toolCalls"][0]["isRedacted"] = json!("false");
    fixtures.push(("call-string", record));

    let mut record = base_record("call-number");
    record["toolCalls"][0]["is_redacted"] = json!(0);
    fixtures.push(("call-number", record));

    let mut record = base_record("result-array");
    record["toolCalls"][0]["result"]["redacted"] = json!([]);
    fixtures.push(("result-array", record));

    let mut record = base_record("result-object");
    record["toolCalls"][0]["result"]["isRedacted"] = json!({});
    fixtures.push(("result-object", record));

    let mut record = base_record("record-status");
    record["status"] = json!("redacted");
    fixtures.push(("record-status", record));

    let mut record = base_record("call-state");
    record["toolCalls"][0]["state"] = json!("output-redacted");
    fixtures.push(("call-state", record));

    let mut record = base_record("false-control");
    record["toolCalls"][0]["result"]["redacted"] = json!(false);
    fixtures.push(("false-control", record));

    let mut record = base_record("case-sensitive-control");
    record["toolCalls"][0]["result"]["status"] = json!("Redacted");
    fixtures.push(("case-sensitive-control", record));

    for (case, record) in fixtures {
        let oracle = gemini_result_subrecord_oracle_for_tests(&record)
            .expect("redaction fixture must preserve its output coordinate");
        let expected_content = oracle
            .into_iter()
            .next()
            .and_then(|(_, content, _)| content);
        let expected_redacted = expected_content.is_none();
        assert_eq!(
            expected_redacted,
            !matches!(case, "false-control" | "case-sensitive-control"),
            "fixture disagrees with the shared oracle: {case}"
        );
        let temp = TempDir::new().unwrap();
        let root = fixture_root(&temp);
        let path = write_transcript(
            &root,
            &[header(&format!("redaction-{case}"), "main"), record],
        );
        let source = rediscover(&root, &path);
        let collect = |profile| {
            let mut reader =
                read_gemini_transcript_pages_with_profile(&source, None, profile).unwrap();
            let mut core_pages = Vec::new();
            let mut outputs = Vec::new();
            while let Some(page) = reader.next_page().unwrap() {
                outputs.extend(
                    page.output_pages
                        .into_iter()
                        .flat_map(|page| page.outputs)
                        .map(|output| output.content),
                );
                core_pages.push((
                    page.identity,
                    page.expected_frontier,
                    page.next_safe_frontier,
                    page.terminal,
                    page.physical_records,
                    page.logical_units,
                    page.conservative_serialized_bytes,
                    page.events,
                    page.rejections,
                ));
            }
            let rejected_records = reader.outcome().unwrap().rejected_records;
            (core_pages, outputs, rejected_records)
        };

        let (core_pages, core_outputs, core_rejections) =
            collect(GeminiNativePathProfile::CoreOnly);
        let (pro_core_pages, pro_outputs, pro_rejections) =
            collect(GeminiNativePathProfile::CoreAndTransientOutputs);
        assert_eq!(core_pages, pro_core_pages, "{case}");
        assert!(core_outputs.is_empty(), "{case}");
        assert_eq!(core_rejections, pro_rejections, "{case}");

        if expected_redacted {
            assert!(core_pages.iter().all(|page| page.7.is_empty()), "{case}");
            assert!(pro_outputs.is_empty(), "{case}");
            assert_eq!(core_rejections, 0, "{case}");
            assert!(
                !format!("{core_pages:?}{pro_outputs:?}")
                    .contains(&format!("SECRET_REDACTED_DIAGNOSTIC_{case}")),
                "{case}"
            );
        } else {
            assert_eq!(
                core_pages.iter().map(|page| page.7.len()).sum::<usize>(),
                1,
                "{case}"
            );
            assert_eq!(
                pro_outputs,
                [expected_content.unwrap().into_bytes()],
                "{case}"
            );
        }
    }
}

#[test]
fn gemini_nativepath_page_identity_is_deterministic_for_one_profile() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut values = vec![header("deterministic-pages", "main")];
    values.extend((0..MAX_GEMINI_NATIVE_PAGE_RECORDS + 4).map(|index| {
        json!({
            "id": format!("message-{index:03}"),
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "user",
            "content": format!("deterministic-{index:03}")
        })
    }));
    let path = write_transcript(&root, &values);
    let first_page = |source: &GeminiTranscriptSource| {
        let mut reader = read_gemini_transcript_pages(source, None).unwrap();
        reader.next_page().unwrap().unwrap()
    };

    let source = rediscover(&root, &path);
    let original = first_page(&source);
    let repeated = first_page(&source);
    assert_eq!(original.identity, repeated.identity);
    assert_eq!(original.expected_frontier, repeated.expected_frontier);
    assert_eq!(original.next_safe_frontier, repeated.next_safe_frontier);

    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&jsonl(&[json!({
            "id": "appended-after-first-page",
            "timestamp": "2026-01-01T00:00:02.000Z",
            "type": "user",
            "content": "append does not re-identify a certified prefix"
        })]))
        .unwrap();
    let appended_source = rediscover(&root, &path);
    assert_eq!(original.identity, first_page(&appended_source).identity);

    let mut mutated = fs::read(&path).unwrap();
    let needle = b"deterministic-000";
    let position = mutated
        .windows(needle.len())
        .position(|window| window == needle)
        .unwrap();
    mutated[position + needle.len() - 1] = b'x';
    fs::write(&path, mutated).unwrap();
    let mutated_source = rediscover(&root, &path);
    assert_ne!(original.identity, first_page(&mutated_source).identity);
}

#[test]
fn gemini_nativepath_reopens_exactly_from_a_lagging_safe_frontier() {
    const EVENTS: usize = MAX_GEMINI_NATIVE_PAGE_RECORDS * 2 + 3;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut values = vec![header("frontier-resume", "main")];
    values.extend((0..EVENTS).map(|index| {
        json!({
            "id": format!("event-{index:03}"),
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "gemini",
            "content": format!("message {index}")
        })
    }));
    let path = write_transcript(&root, &values);
    let source = rediscover(&root, &path);

    let mut first_reader = read_gemini_transcript_pages(&source, None).unwrap();
    let first_page = first_reader.next_page().unwrap().unwrap();
    let frontier = first_page.next_safe_frontier.clone();
    let mut all_ids: Vec<_> = first_page
        .events
        .iter()
        .map(|event| match &event.identity {
            GeminiEventIdentity::NativeRecordId(id) => id.clone(),
        })
        .collect();
    drop(first_reader);

    let mut resumed = read_gemini_transcript_pages_from_frontier(
        &source,
        &frontier,
        GeminiNativePathProfile::CoreOnly,
    )
    .unwrap();
    let mut first_resumed_page = true;
    while let Some(page) = resumed.next_page().unwrap() {
        if first_resumed_page {
            assert_eq!(page.expected_frontier, frontier);
            first_resumed_page = false;
        }
        all_ids.extend(page.events.iter().map(|event| match &event.identity {
            GeminiEventIdentity::NativeRecordId(id) => id.clone(),
        }));
    }
    assert_eq!(all_ids.len(), EVENTS);
    assert_eq!(
        all_ids,
        (0..EVENTS)
            .map(|index| format!("event-{index:03}"))
            .collect::<Vec<_>>()
    );

    let mut mutated = fs::read(&path).unwrap();
    let needle = b"event-000";
    let position = mutated
        .windows(needle.len())
        .position(|window| window == needle)
        .unwrap();
    mutated[position + needle.len() - 1] = b'x';
    fs::write(&path, mutated).unwrap();
    let changed_source = rediscover(&root, &path);
    assert!(matches!(
        read_gemini_transcript_pages_from_frontier(
            &changed_source,
            &frontier,
            GeminiNativePathProfile::CoreOnly
        ),
        Err(GeminiScanError::Capture(
            CaptureError::SourceChangedDuringCapture
        ))
    ));
}

#[test]
fn gemini_nativepath_output_fanout_failure_retains_the_prior_frontier_for_retry() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let calls: Vec<_> = (0..=MAX_GEMINI_NATIVE_PAGE_RECORDS)
        .map(|index| {
            json!({
                "id": format!("call-{index}"),
                "result": {"content": format!("output-{index}")}
            })
        })
        .collect();
    let path = write_transcript(
        &root,
        &[
            header("bounded-output-fanout", "main"),
            json!({
                "id": "too-many-outputs",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": calls
            }),
            json!({
                "id": "later-valid",
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "user",
                "content": "valid sibling survives"
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let mut reader = read_gemini_transcript_pages_with_profile(
        &source,
        None,
        GeminiNativePathProfile::CoreAndTransientOutputs,
    )
    .unwrap();
    let mut rows = Vec::new();
    let mut outputs = Vec::new();
    let mut rejections = Vec::new();
    while let Some(mut page) = reader.next_page().unwrap() {
        rows.append(&mut page.events);
        rejections.append(&mut page.rejections);
        for mut output_page in page.output_pages {
            outputs.append(&mut output_page.outputs);
        }
    }
    assert!(outputs.is_empty());
    assert_eq!(rejections.len(), 1);
    assert!(rejections[0].reason.contains("exceeds the 64 output limit"));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].native_order.raw_ordinal, 2);
    assert_eq!(reader.outcome().unwrap().rejected_records, 1);
}
