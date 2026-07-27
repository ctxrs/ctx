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
    assert_eq!(discovery.sources.len(), 0);
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
    let source = discover_one(&path, "ordinal-owner");
    let (scan, sink) = scan_collect(source, None);

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
fn core_row_preserves_verified_truncated_message_locator_without_complete_body() {
    let header = session_meta("locator-owner");
    let complete_message = format!("MESSAGE_BEGIN-{}-MESSAGE_END", "m".repeat(20_000));
    let message_record = message("assistant", &complete_message);
    let message_start = header.len() as u64;
    let message_end = message_start + message_record.len() as u64;
    let contents = [header, message_record.clone()].concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "locator-owner"), None);

    assert!(scan.rejections.is_empty());
    assert_eq!(sink.rows.len(), 1);
    let message = &sink.rows[0];
    assert_eq!(message.raw_ordinal, 1);
    assert_eq!(
        message.provider_event.payload["text"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        crate::complete_content::COMPLETE_CONTENT_INDEXED_MESSAGE_LIMIT_CHARS
    );
    assert_eq!(
        message.provider_event.metadata["source_record_ordinal"],
        json!(1)
    );
    assert_eq!(
        message.provider_event.metadata["source_record_subrecord_index"],
        json!(0)
    );

    let locators = VerifiedContentLocatorsV1::from_metadata_value(
        &message.provider_event.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let locator = locators.locator(VerifiedContentRole::MessageBody).unwrap();
    assert_eq!(locator.kind(), JSONL_COMPLETE_CONTENT_LOCATOR_KIND);
    assert_eq!(locator.native_record_id(), "line-2");
    assert!(locator.content_ref().verifies(complete_message.as_bytes()));
    assert_eq!(
        locator.record_sha256().as_str(),
        CompleteContentBodyDigest::from_bytes(
            message_record.strip_suffix('\n').unwrap().as_bytes()
        )
        .as_str()
    );
    let source_locator = locator.source_locator().unwrap();
    let range = source_locator.value();
    assert_eq!(
        u64::from_be_bytes(range[..8].try_into().unwrap()),
        message_start
    );
    assert_eq!(
        u64::from_be_bytes(range[8..].try_into().unwrap()),
        message_end
    );

    let serialized_core = serde_json::to_string(&sink.rows).unwrap();
    assert!(!serialized_core.contains("MESSAGE_END"));
    assert!(!serialized_core.contains(&complete_message));
}

#[test]
fn output_heavy_scan_never_constructs_result_bodies_hashes_or_previews() {
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
    let source = discover_one(&path, "output-owner");
    let (scan, sink) = scan_collect(source, None);

    assert_eq!(sink.rows.len(), 3);
    assert_eq!(scan.counters.native_result_records, 1);
    assert!(scan.counters.native_result_record_bytes > secret.len() as u64);
    assert_eq!(scan.counters.structural_json_parses, 5);
    assert_eq!(scan.counters.structural_output_probes, 1);
    assert_eq!(scan.counters.typed_json_parses, 4);
    assert_eq!(scan.counters.typed_output_parses, 0);
    assert_eq!(scan.counters.retained_json_parses, 3);
    assert_eq!(scan.counters.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(scan.counters.result_hashes_created, 0);
    assert_eq!(scan.counters.result_previews_created, 0);
    assert_eq!(scan.counters.result_touches_created, 0);
    assert_eq!(scan.counters.result_fts_rows_created, 0);
    assert_eq!(scan.counters.result_handoffs_created, 0);
    let prepared_rows = format!("{:?}", sink.rows);
    assert!(!prepared_rows.contains("RESULT_ONLY_MARKER_"));
}

#[test]
fn core_and_pro_profiles_match_while_pro_receives_success_failure_timeout_and_unknown() {
    let success_marker = "SUCCESS_OUTPUT_ONLY_MARKER";
    let failure_marker = "FAILURE_BODY_MUST_NOT_SURVIVE";
    let timeout_marker = "TIMEOUT_BODY_MUST_NOT_SURVIVE";
    let unknown_marker = "UNKNOWN_OUTPUT_ONLY_MARKER";
    let contents = [
        session_meta("fanout-owner"),
        message("user", "run both"),
        tool_call("call-success"),
        successful_tool_output("call-success", success_marker),
        tool_call("call-failure"),
        failed_tool_output("call-failure", failure_marker),
        tool_call("call-timeout"),
        timed_out_tool_output("call-timeout", timeout_marker),
        tool_call("call-unknown"),
        tool_output("call-unknown", unknown_marker),
        message("assistant", "done"),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);

    let (core_scan, core) = scan_collect_profile(
        discover_one(&path, "fanout-owner"),
        None,
        CodexNativeProfile::CoreOnly,
    );
    let (fanout_scan, fanout) = scan_collect_profile(
        discover_one(&path, "fanout-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    );

    assert_eq!(core.rows, fanout.rows);
    assert_eq!(
        core_scan.full_revision_sha256,
        fanout_scan.full_revision_sha256
    );
    assert_eq!(
        core_scan.complete_prefix_sha256,
        fanout_scan.complete_prefix_sha256
    );
    assert_eq!(core_scan.next_raw_ordinal, fanout_scan.next_raw_ordinal);
    assert_eq!(core_scan.rejections, fanout_scan.rejections);
    assert_eq!(core.core_receipts, fanout.core_receipts);
    assert!(core.pro_outputs.is_empty());
    assert_eq!(fanout.pro_outputs.len(), 4);

    let output = &fanout.pro_outputs[0];
    assert_eq!(output.outcome.outcome, crate::OutputOutcome::Success);
    assert_eq!(
        output.coordinate.unit_key,
        "codex/nativepath/fanout-owner/3/0"
    );
    assert_eq!(output.coordinate.native_sequence, 3);
    assert_eq!(output.coordinate.source_record_ordinal, Some(3));
    assert_eq!(output.coordinate.source_record_subrecord_index, Some(0));
    assert_eq!(output.call_id.as_deref(), Some("call-success"));
    assert!(String::from_utf8_lossy(&output.content).contains(success_marker));
    assert!(!String::from_utf8_lossy(&output.content).contains(failure_marker));
    assert_eq!(
        fanout
            .pro_outputs
            .iter()
            .map(|output| output.outcome.outcome)
            .collect::<Vec<_>>(),
        vec![
            crate::OutputOutcome::Success,
            crate::OutputOutcome::Failure,
            crate::OutputOutcome::Timeout,
            crate::OutputOutcome::Unknown,
        ]
    );
    assert_eq!(
        fanout
            .pro_outputs
            .iter()
            .map(|output| output.coordinate.native_sequence)
            .collect::<Vec<_>>(),
        vec![3, 5, 7, 9]
    );
    for (output, marker) in fanout.pro_outputs.iter().zip([
        success_marker,
        failure_marker,
        timeout_marker,
        unknown_marker,
    ]) {
        assert!(String::from_utf8_lossy(&output.content).contains(marker));
        assert_eq!(output.kind, crate::OutputObservationKind::Command);
        assert!(output.command.is_some());
    }
    let locator: Value = serde_json::from_slice(&output.locator.payload).unwrap();
    assert_eq!(locator["raw_ordinal"], json!(3));
    assert_eq!(locator["source_path"], json!(path));

    let core_debug = format!("{:?}", core.rows);
    assert!(!core_debug.contains(success_marker));
    assert!(!core_debug.contains(failure_marker));
    assert!(!core_debug.contains(timeout_marker));
    assert!(!core_debug.contains(unknown_marker));
    assert_eq!(
        core.rows
            .iter()
            .filter(|row| matches!(
                row.provider_event.event_type,
                EventType::CommandOutput | EventType::ToolOutput
            ))
            .count(),
        2
    );
    let failure = core.rows.iter().find(|row| row.raw_ordinal == 5).unwrap();
    assert_eq!(failure.provider_event.event_type, EventType::CommandOutput);
    assert_eq!(failure.provider_event.payload["exit_code"], 7);
    assert_eq!(failure.provider_event.payload["duration_ms"], 250);
    assert_eq!(failure.provider_event.payload["timed_out"], false);
    assert_eq!(
        failure.provider_event.payload["output_bytes"],
        format!("Process exited with code 7\nWall time: 0.25 seconds\n{failure_marker}").len()
    );
    assert_eq!(failure.provider_event.payload["command"], "printf retained");
    let timeout = core.rows.iter().find(|row| row.raw_ordinal == 7).unwrap();
    assert_eq!(timeout.provider_event.event_type, EventType::CommandOutput);
    assert_eq!(timeout.provider_event.payload["timed_out"], true);
    assert_eq!(timeout.provider_event.payload["duration_ms"], 9_000);
    assert_eq!(
        timeout.provider_event.payload["output_bytes"],
        format!("command timed out\n{timeout_marker}").len()
    );
    assert_eq!(core_scan.counters.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(core_scan.counters.result_handoffs_created, 0);
    assert_eq!(core_scan.counters.structural_json_parses, 11);
    assert_eq!(core_scan.counters.structural_output_probes, 4);
    assert_eq!(core_scan.counters.typed_json_parses, 7);
    assert_eq!(core_scan.counters.typed_output_parses, 0);
    assert_eq!(fanout_scan.counters.structural_json_parses, 11);
    assert_eq!(fanout_scan.counters.structural_output_probes, 4);
    assert_eq!(fanout_scan.counters.typed_json_parses, 11);
    assert_eq!(fanout_scan.counters.typed_output_parses, 4);
    assert_eq!(fanout_scan.counters.result_hashes_created, 0);
    assert_eq!(fanout_scan.counters.result_previews_created, 0);
    assert_eq!(fanout_scan.counters.result_touches_created, 0);
    assert_eq!(fanout_scan.counters.result_fts_rows_created, 0);
    assert_eq!(fanout_scan.counters.result_handoffs_created, 4);
    assert_eq!(fanout.pages.len(), 1);
    assert_eq!(fanout.pages[0].0, fanout.rows.len());
    assert_eq!(fanout.pro_pages.len(), 1);
    assert_eq!(fanout.pro_pages[0].0, fanout.pro_outputs.len());
}

#[test]
fn pending_call_checkpoint_makes_fresh_and_append_outputs_identical_in_both_profiles() {
    for profile in [CodexNativeProfile::CoreOnly, CodexNativeProfile::CoreAndPro] {
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

        let (initial_scan, _) =
            scan_collect_profile(discover_one(&path, "split-owner"), None, profile);
        let proof = initial_scan
            .bind_checkpoint("canonical-split", CodexCheckpointGeneration::new(90))
            .unwrap()
            .unwrap();
        let checkpoint_wire = serde_json::from_slice::<Value>(
            &proof.checkpoint.encode().expect("checkpoint should encode"),
        )
        .unwrap();
        assert_eq!(checkpoint_wire["version"], 4);
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
        let (append_scan, append) =
            scan_collect_profile(discover_one(&path, "split-owner"), Some(&proof), profile);
        let (fresh_scan, fresh) =
            scan_collect_profile(discover_one(&path, "split-owner"), None, profile);

        let fresh_output_rows = fresh
            .rows
            .iter()
            .filter(|row| {
                matches!(
                    row.provider_event.event_type,
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
                .map(|row| (
                    row.raw_ordinal,
                    row.provider_event.event_type,
                    row.normalized_body_hash.as_str()
                ))
                .collect::<Vec<_>>(),
            fresh_output_rows
                .iter()
                .map(|row| (
                    row.raw_ordinal,
                    row.provider_event.event_type,
                    row.normalized_body_hash.as_str()
                ))
                .collect::<Vec<_>>()
        );
        assert_eq!(append.pro_outputs.len(), fresh.pro_outputs.len());
        for (append_output, fresh_output) in append.pro_outputs.iter().zip(&fresh.pro_outputs) {
            assert_eq!(append_output.kind, fresh_output.kind);
            assert_eq!(append_output.coordinate, fresh_output.coordinate);
            assert_eq!(
                append_output.occurred_at_unix_ms,
                fresh_output.occurred_at_unix_ms
            );
            assert_eq!(append_output.associations, fresh_output.associations);
            assert_eq!(append_output.call_id, fresh_output.call_id);
            assert_eq!(append_output.command, fresh_output.command);
            assert_eq!(append_output.outcome, fresh_output.outcome);
            assert_eq!(append_output.locator, fresh_output.locator);
            assert_eq!(append_output.content, fresh_output.content);
        }
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

        if profile == CodexNativeProfile::CoreOnly {
            assert!(append.pro_outputs.is_empty());
        } else {
            assert_eq!(
                append
                    .pro_outputs
                    .iter()
                    .map(|output| output.outcome.outcome)
                    .collect::<Vec<_>>(),
                vec![
                    crate::OutputOutcome::Success,
                    crate::OutputOutcome::Failure,
                    crate::OutputOutcome::Timeout,
                    crate::OutputOutcome::Unknown,
                ]
            );
            assert!(append.pro_outputs[3].content.is_empty());
            for output in &append.pro_outputs {
                assert_eq!(output.kind, crate::OutputObservationKind::Command);
                let command = output.command.as_ref().unwrap();
                assert_eq!(command.tool_name, "exec_command");
                assert_eq!(command.command, "printf retained");
                assert_eq!(command.working_directory.as_deref(), Some("/workspace"));
            }
        }
    }
}
