use super::*;

#[test]
fn catalog_discovery_is_deterministic_and_rejects_bad_exact_observations() {
    let (_temp, path) = write_source(&session_meta("catalog-owner"));
    let valid = catalog_session(&path, "catalog-owner");
    let mut wrong_provider = valid.clone();
    wrong_provider.provider = CaptureProvider::Claude;
    let mut missing_token = valid.clone();
    missing_token.source_path = path
        .with_file_name("missing-token.jsonl")
        .display()
        .to_string();
    missing_token.metadata = json!({});

    let discovery =
        discover_codex_catalog_sources(&[wrong_provider, missing_token, valid.clone(), valid]);
    assert_eq!(discovery.ineligible, 1);
    assert!(discovery.sources.is_empty());
    assert_eq!(
        discovery
            .rejections
            .iter()
            .map(|rejection| rejection.reason)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "Codex catalog change token is missing",
            "duplicate Codex catalog path",
        ])
    );
}

#[test]
fn raw_ordinals_include_headers_outputs_malformed_and_ignored_records() {
    let contents = [
        session_meta("ordinal-owner"),
        message("user", "first retained"),
        tool_output("call-1", "excluded body"),
        "{malformed json}\n".to_owned(),
        tool_call("call-2"),
        jsonl(json!({"type": "turn_context", "payload": {"cwd": "/workspace"}})),
        message("assistant", "last retained"),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "ordinal-owner"), None);

    assert_eq!(
        sink.rows
            .iter()
            .map(|row| row.raw_ordinal)
            .collect::<Vec<_>>(),
        vec![1, 4, 6]
    );
    assert_eq!(scan.next_raw_ordinal, 7);
    assert_eq!(scan.counters.complete_records, 7);
    assert_eq!(scan.counters.native_result_records, 1);
    assert_eq!(scan.counters.malformed_records, 1);
    assert_eq!(scan.rejections[0].raw_ordinal, 3);
}

#[test]
fn valid_retained_records_without_indexable_text_are_ignored_not_rejected() {
    let contents = [
        session_meta("unmaterialized-owner"),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "reasoning",
                "encrypted_content": "opaque",
                "summary": []
            }
        })),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "compacted",
            "payload": {
                "message": "replacement history is source-only",
                "replacement_history": []
            }
        })),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "unmaterialized-owner"), None);

    assert!(sink.rows.is_empty());
    assert!(scan.rejections.is_empty());
    assert_eq!(scan.counters.complete_records, 3);
    assert_eq!(scan.counters.ignored_records, 2);
    assert_eq!(scan.counters.malformed_records, 0);
}

#[test]
fn malformed_retained_shapes_remain_exact_ordinal_rejections() {
    let contents = [
        session_meta("malformed-retained-owner"),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {"type": "message", "role": "user"}
        })),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "response_item",
            "payload": {"type": "message", "role": "operator", "content": "invalid role"}
        })),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "response_item",
            "payload": {"type": "reasoning", "summary": []}
        })),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:04Z",
            "type": "response_item",
            "payload": {
                "type": "reasoning",
                "encrypted_content": "opaque",
                "summary": {"unexpected": "shape"}
            }
        })),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:05Z",
            "type": "compacted",
            "payload": {"message": "missing replacement history"}
        })),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:06Z",
            "type": "compacted",
            "payload": {
                "message": "invalid replacement history",
                "replacement_history": "not an array"
            }
        })),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "malformed-retained-owner"), None);

    assert!(sink.rows.is_empty());
    assert_eq!(scan.counters.complete_records, 7);
    assert_eq!(scan.counters.ignored_records, 0);
    assert_eq!(scan.counters.malformed_records, 6);
    assert_eq!(
        scan.rejections
            .iter()
            .map(|rejection| (rejection.raw_ordinal, rejection.reason))
            .collect::<Vec<_>>(),
        vec![
            (1, "malformed retained Codex message"),
            (2, "malformed retained Codex message"),
            (3, "malformed retained Codex reasoning"),
            (4, "malformed retained Codex reasoning"),
            (5, "malformed retained Codex compacted record"),
            (6, "malformed retained Codex compacted record"),
        ]
    );
}

#[test]
fn source_backed_row_preserves_full_lexical_text_and_exact_record_evidence() {
    let header = session_meta("locator-owner");
    let complete_message = format!("MESSAGE_BEGIN-{}-MESSAGE_END", "m".repeat(20_000));
    let message_record = message("assistant", &complete_message);
    let message_start = header.len() as u64;
    let contents = [header, message_record.clone()].concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "locator-owner"), None);

    assert!(scan.rejections.is_empty());
    assert_eq!(sink.rows.len(), 1);
    let row = &sink.rows[0];
    assert_eq!(row.raw_ordinal, 1);
    assert!(row.lexical_body.contains("MESSAGE_BEGIN"));
    assert!(row.lexical_body.ends_with("MESSAGE_END"));
    assert_eq!(row.source_record.byte_offset, message_start);
    assert_eq!(row.source_record.byte_length, message_record.len() as u64);
    assert_eq!(
        row.source_record.record_digest,
        Sha256::digest(message_record.as_bytes()).into()
    );
}

#[test]
fn output_heavy_scan_never_hydrates_non_display_result_bodies() {
    let secret = "RESULT_ONLY_MARKER_".repeat(32_768);
    let contents = [
        session_meta("output-owner"),
        message("user", "small request"),
        tool_call("call-output"),
        tool_output("call-output", &secret),
        message("assistant", "small response"),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "output-owner"), None);

    assert_eq!(sink.rows.len(), 3);
    assert_eq!(scan.counters.native_result_records, 1);
    assert!(scan.counters.native_result_record_bytes > secret.len() as u64);
    assert_eq!(scan.counters.structural_json_parses, 5);
    assert_eq!(scan.counters.structural_output_probes, 1);
    assert_eq!(scan.counters.typed_json_parses, 4);
    assert_eq!(scan.counters.retained_json_parses, 3);
    assert!(!format!("{:?}", sink.rows).contains("RESULT_ONLY_MARKER_"));
}

#[test]
fn source_backed_projection_prefilters_with_exact_scan_accounting() {
    let ignored = jsonl(json!({
        "type": "event_msg",
        "payload": {"type": "token_count", "info": {"total_token_usage": {"input_tokens": 42}}}
    }));
    let ineligible_result = jsonl(json!({
        "type": "event_msg",
        "payload": {"type": "patch_apply_end", "success": true}
    }));
    let retained = message("user", "source-backed retained body");
    let contents = [
        session_meta("source-backed-prefilter-owner"),
        ignored,
        ineligible_result.clone(),
        retained.clone(),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "source-backed-prefilter-owner"), None);

    assert_eq!(sink.rows.len(), 1);
    assert_eq!(sink.core_receipts.len(), 1);
    assert_eq!(sink.core_receipts[0].accepted_core_rows, 1);
    assert_eq!(scan.counters.complete_records, 4);
    assert_eq!(scan.counters.retained_records, 1);
    assert_eq!(scan.counters.ignored_records, 1);
    assert_eq!(scan.counters.native_result_records, 1);
    assert_eq!(
        scan.counters.native_result_record_bytes,
        ineligible_result.len() as u64
    );
    assert_eq!(scan.counters.prefiltered_records, 2);
    assert_eq!(scan.counters.structural_json_parses, 2);
    assert_eq!(scan.counters.typed_json_parses, 2);

    let row = &sink.rows[0];
    let retained_start = contents.len().saturating_sub(retained.len()) as u64;
    assert_eq!(row.raw_ordinal, 3);
    assert_eq!(row.source_record.byte_offset, retained_start);
    assert_eq!(row.source_record.byte_length, retained.len() as u64);
    assert_eq!(
        row.source_record.record_digest,
        Sha256::digest(retained.as_bytes()).into()
    );
    assert_eq!(scan.counters.legacy_body_json_serializations, 0);
    assert_eq!(scan.counters.legacy_row_json_serializations, 0);
    assert_eq!(scan.counters.legacy_json_serialized_bytes, 0);
    assert_eq!(scan.counters.legacy_file_touch_rows_created, 0);
    assert_eq!(scan.counters.legacy_complete_content_locators_created, 0);
    assert_eq!(scan.counters.legacy_page_owner_json_serializations, 0);
    assert_eq!(
        scan.counters.legacy_page_identity_owner_json_serializations,
        0
    );
    assert_eq!(
        scan.counters.legacy_page_identity_row_json_serializations,
        0
    );
}

#[test]
fn source_backed_projection_batches_ignored_records_in_one_bounded_page() {
    const IGNORED_RECORDS: usize = 256;

    let ignored = jsonl(json!({
        "type": "event_msg",
        "payload": {"type": "token_count", "info": {"total_token_usage": {"input_tokens": 42}}}
    }));
    let mut contents = session_meta("source-backed-batching-owner");
    for _ in 0..IGNORED_RECORDS {
        contents.push_str(&ignored);
    }
    contents.push_str(&message("assistant", "one retained projection"));
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "source-backed-batching-owner"), None);

    assert_eq!(sink.physical_records, vec![(IGNORED_RECORDS + 2) as u64]);
    assert_eq!(scan.counters.emitted_pages, 1);
    assert_eq!(sink.rows.len(), 1);
    assert_eq!(scan.next_raw_ordinal, (IGNORED_RECORDS + 2) as u64);
}

#[test]
fn pending_call_checkpoint_keeps_fresh_and_append_source_backed_outputs_identical() {
    let initial = [
        session_meta("split-owner"),
        tool_call("split-success"),
        tool_call("split-failure"),
        tool_call("split-timeout"),
        tool_call("split-unknown"),
    ]
    .concat();
    let appended = [
        successful_tool_output("split-success", "success"),
        failed_tool_output("split-failure", "failure"),
        timed_out_tool_output("split-timeout", "timeout"),
        tool_output("split-unknown", ""),
    ]
    .concat();
    let complete = format!("{initial}{appended}");
    let (_temp, path) = write_source(&initial);

    let (initial_scan, _) = scan_collect(discover_one(&path, "split-owner"), None);
    let proof = initial_scan
        .bind_checkpoint("canonical-split", CodexCheckpointGeneration::new(90))
        .unwrap()
        .unwrap();
    let checkpoint_wire =
        serde_json::from_slice::<Value>(&proof.checkpoint.encode().unwrap()).unwrap();
    assert_eq!(
        checkpoint_wire["pending_tool_authorities"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let checkpoint_text = serde_json::to_string(&checkpoint_wire).unwrap();
    assert!(!checkpoint_text.contains("split-success"));
    assert!(!checkpoint_text.contains("printf retained"));

    fs::write(&path, &complete).unwrap();
    let (append_scan, append) = scan_collect(discover_one(&path, "split-owner"), Some(&proof));
    let (fresh_scan, fresh) = scan_collect(discover_one(&path, "split-owner"), None);
    let fresh_output_rows = fresh
        .rows
        .iter()
        .filter(|row| {
            matches!(
                row.event_type,
                EventType::CommandOutput | EventType::ToolOutput
            )
        })
        .cloned()
        .collect::<Vec<_>>();

    assert_eq!(append.rows, fresh_output_rows);
    assert_eq!(
        append
            .rows
            .iter()
            .map(|row| row.raw_ordinal)
            .collect::<Vec<_>>(),
        vec![6, 7]
    );
    assert_eq!(append_scan.rejections, fresh_scan.rejections);
    assert_eq!(append_scan.counters.bytes_read, complete.len() as u64);
    assert_eq!(
        append_scan.counters.checkpoint_validation_bytes,
        initial.len() as u64
    );
    assert_eq!(
        append_scan.complete_prefix_sha256,
        fresh_scan.complete_prefix_sha256
    );
    assert_eq!(append_scan.next_raw_ordinal, fresh_scan.next_raw_ordinal);
}
