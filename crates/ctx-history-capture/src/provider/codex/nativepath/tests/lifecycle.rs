use super::*;

#[test]
fn incomplete_tail_stays_at_its_starting_boundary_and_ordinal() {
    let complete = [session_meta("tail-owner"), message("user", "complete")].concat();
    let partial = r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","output":"partial"#;
    let contents = format!("{complete}{partial}");
    let (_temp, path) = write_source(&contents);
    let source = discover_one(&path, "tail-owner");
    let (scan, sink) = scan_collect(source, None);
    let tail = scan.incomplete_tail.as_ref().unwrap();

    assert_eq!(sink.rows.len(), 1);
    assert_eq!(scan.next_raw_ordinal, 2);
    assert_eq!(tail.raw_ordinal, 2);
    assert_eq!(tail.start_byte, complete.len() as u64);
    assert_eq!(tail.byte_len, partial.len() as u64);
    assert_eq!(scan.complete_prefix_end, complete.len() as u64);
    assert_eq!(
        sink.frontiers.last().unwrap().1.complete_prefix_end,
        complete.len() as u64
    );
    assert_eq!(
        sink.frontiers.last().unwrap().1.complete_prefix_sha256,
        scan.complete_prefix_sha256
    );
    assert!(!scan.terminal());
    assert_eq!(scan.counters.native_result_records, 0);
    assert_eq!(scan.counters.incomplete_records, 1);

    let proof = scan
        .bind_checkpoint("canonical-tail", CodexCheckpointGeneration::new(4))
        .unwrap()
        .unwrap();
    let (replay, replay_sink) = scan_collect(discover_one(&path, "tail-owner"), Some(&proof));
    assert_eq!(replay.disposition, CodexParseDisposition::ObservationReplay);
    assert_eq!(replay.incomplete_tail.as_ref(), Some(tail));
    assert!(replay_sink.rows.is_empty());
    assert_eq!(
        replay.counters.checkpoint_validation_bytes,
        contents.len() as u64
    );
}

#[test]
fn oversized_terminal_nul_padding_is_ignored_and_checkpointed_exactly() {
    let header = session_meta("nul-padding-owner");
    let mut contents = header.into_bytes();
    contents.resize(
        contents
            .len()
            .saturating_add(MAX_CODEX_RECORD_BYTES)
            .saturating_add(64 * 1024),
        0,
    );
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("nul-padding.jsonl");
    fs::write(&path, &contents).unwrap();

    let (scan, sink) = scan_collect(discover_one(&path, "nul-padding-owner"), None);

    assert!(scan.terminal());
    assert_eq!(scan.counters.rejected_complete_records, 0);
    assert_eq!(scan.complete_prefix_end, contents.len() as u64);
    assert_eq!(scan.next_raw_ordinal, 2);
    assert_eq!(scan.counters.complete_records, 2);
    assert_eq!(scan.counters.ignored_records, 1);
    assert_eq!(scan.counters.incomplete_records, 0);
    assert_eq!(scan.counters.oversized_records, 0);
    assert_eq!(scan.counters.peak_line_buffer_bytes, MAX_CODEX_RECORD_BYTES);
    assert_eq!(sink.physical_records, vec![2]);
    let expected_revision: [u8; 32] = Sha256::digest(&contents).into();
    assert_eq!(scan.full_revision_sha256, expected_revision);
    assert_eq!(scan.complete_prefix_sha256, expected_revision);

    let proof = scan
        .bind_checkpoint("canonical-nul-padding", CodexCheckpointGeneration::new(5))
        .unwrap()
        .unwrap();
    let (replay, replay_sink) =
        scan_collect(discover_one(&path, "nul-padding-owner"), Some(&proof));
    assert_eq!(replay.disposition, CodexParseDisposition::ObservationReplay);
    assert_eq!(replay.complete_prefix_end, contents.len() as u64);
    assert_eq!(replay.next_raw_ordinal, 2);
    assert!(replay.terminal());
    assert!(replay_sink.rows.is_empty());
}

#[test]
fn terminal_nul_padding_never_becomes_an_append_boundary() {
    let header = session_meta("nul-mutation-owner");
    let mut initial = header.as_bytes().to_vec();
    initial.resize(initial.len().saturating_add(4 * 1024), 0);
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("nul-mutation.jsonl");
    fs::write(&path, &initial).unwrap();
    let (scan, _) = scan_collect(discover_one(&path, "nul-mutation-owner"), None);
    let proof = scan
        .bind_checkpoint("canonical-nul-mutation", CodexCheckpointGeneration::new(6))
        .unwrap()
        .unwrap();

    let mut appended = initial.clone();
    appended.extend_from_slice(message("assistant", "must force full revalidation").as_bytes());
    fs::write(&path, appended).unwrap();
    let append_error = CodexNativeScanner::new_source_backed_v0(
        discover_one(&path, "nul-mutation-owner"),
        Some(&proof),
    )
    .unwrap_err();
    assert!(
        format!("{append_error}").contains("terminal NUL padding is not an append boundary"),
        "{append_error}"
    );

    let mut rewritten = header.into_bytes();
    rewritten.extend_from_slice(message("assistant", "same length rewrite").as_bytes());
    rewritten.resize(initial.len(), 0);
    fs::write(&path, rewritten).unwrap();
    let rewrite_error = CodexNativeScanner::new_source_backed_v0(
        discover_one(&path, "nul-mutation-owner"),
        Some(&proof),
    )
    .unwrap_err();
    assert!(
        format!("{rewrite_error}").contains("digest, boundary, or raw ordinal"),
        "{rewrite_error}"
    );
}

#[test]
fn append_resumes_at_complete_prefix_and_preserves_suffix_ordinal() {
    let initial = [session_meta("append-owner"), message("user", "first")].concat();
    let (_temp, path) = write_source(&initial);
    let first_source = discover_one(&path, "append-owner");
    let (first, _) = scan_collect(first_source, None);
    let proof = first
        .bind_checkpoint("canonical-append", CodexCheckpointGeneration::new(11))
        .unwrap()
        .unwrap();

    let appended = [tool_output("call-old", "excluded"), tool_call("call-new")].concat();
    fs::write(&path, format!("{initial}{appended}")).unwrap();
    let second_source = discover_one(&path, "append-owner");
    let (second, sink) = scan_collect(second_source, Some(&proof));

    assert_eq!(second.disposition, CodexParseDisposition::AppendDelta);
    assert_eq!(second.counters.prefix_bytes_read, initial.len() as u64);
    assert_eq!(sink.rows.len(), 2);
    assert_eq!(sink.rows[0].raw_ordinal, 2);
    assert_eq!(sink.rows[0].event_type, EventType::ToolOutput);
    assert_eq!(sink.rows[1].raw_ordinal, 3);
    assert_eq!(sink.rows[1].event_type, EventType::ToolCall);
    assert_eq!(second.next_raw_ordinal, 4);
    assert_eq!(second.counters.native_result_records, 1);
}

#[test]
fn append_after_catalog_is_deferred_beyond_the_one_admitted_frozen_eof() {
    let initial = [
        session_meta("moving-catalog-owner"),
        message("user", "captured prefix"),
    ]
    .concat();
    let appended = message("assistant", "deferred append");
    let (_temp, path) = write_source(&initial);
    let catalog_source = discover_one(&path, "moving-catalog-owner");
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(appended.as_bytes())
        .unwrap();

    let (frozen, frozen_rows) = scan_collect(catalog_source, None);
    assert_eq!(frozen.before_observation.len, initial.len() as u64);
    assert_eq!(frozen.after_observation, frozen.before_observation);
    assert_eq!(frozen.complete_prefix_end, initial.len() as u64);
    assert_eq!(frozen_rows.rows.len(), 1);
    assert_eq!(frozen_rows.rows[0].lexical_body, "captured prefix");

    let proof = frozen
        .bind_checkpoint("moving-catalog-source", CodexCheckpointGeneration::new(91))
        .unwrap()
        .unwrap();
    let (replay, replay_rows) =
        scan_collect(discover_one(&path, "moving-catalog-owner"), Some(&proof));
    assert_eq!(replay.disposition, CodexParseDisposition::AppendDelta);
    assert_eq!(replay_rows.rows.len(), 1);
    assert_eq!(replay_rows.rows[0].lexical_body, "deferred append");
}

#[test]
fn rewrite_or_truncate_after_catalog_still_fails_admission() {
    let initial = [
        session_meta("moving-rewrite-owner"),
        message("user", "original body"),
    ]
    .concat();
    let (_temp, path) = write_source(&initial);
    let rewrite_source = discover_one(&path, "moving-rewrite-owner");
    std::thread::sleep(std::time::Duration::from_millis(2));
    let mut rewritten = initial.clone().into_bytes();
    let marker = b"original body";
    let start = rewritten
        .windows(marker.len())
        .position(|window| window == marker)
        .unwrap();
    rewritten[start..start + marker.len()].copy_from_slice(b"rewritten bod");
    fs::write(&path, rewritten).unwrap();
    let rewrite_error = CodexNativeScanner::new_source_backed_v0(rewrite_source, None).unwrap_err();
    assert!(
        format!("{rewrite_error}").contains("changed before NativePath admission"),
        "{rewrite_error}"
    );

    let truncate_source = discover_one(&path, "moving-rewrite-owner");
    std::thread::sleep(std::time::Duration::from_millis(2));
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len((initial.len() / 2) as u64)
        .unwrap();
    let truncate_error =
        CodexNativeScanner::new_source_backed_v0(truncate_source, None).unwrap_err();
    assert!(
        format!("{truncate_error}").contains("changed before NativePath admission"),
        "{truncate_error}"
    );
}

#[test]
fn append_restarts_at_an_incomplete_records_original_ordinal() {
    let complete_prefix = [
        session_meta("partial-append-owner"),
        message("user", "complete"),
    ]
    .concat();
    let completed_output = tool_output("partial-call", "excluded after completion");
    let split = completed_output.len() / 2;
    let initial = format!("{complete_prefix}{}", &completed_output[..split]);
    let (_temp, path) = write_source(&initial);
    let (first, _) = scan_collect(discover_one(&path, "partial-append-owner"), None);
    assert_eq!(first.next_raw_ordinal, 2);
    assert_eq!(first.incomplete_tail.as_ref().unwrap().raw_ordinal, 2);
    let proof = first
        .bind_checkpoint("canonical-partial", CodexCheckpointGeneration::new(12))
        .unwrap()
        .unwrap();

    fs::write(
        &path,
        format!(
            "{complete_prefix}{completed_output}{}",
            message("assistant", "after completed output")
        ),
    )
    .unwrap();
    let (appended, sink) = scan_collect(discover_one(&path, "partial-append-owner"), Some(&proof));

    assert_eq!(appended.disposition, CodexParseDisposition::AppendDelta);
    assert_eq!(
        appended.counters.prefix_bytes_read,
        complete_prefix.len() as u64
    );
    assert_eq!(appended.counters.native_result_records, 1);
    assert_eq!(sink.rows.len(), 2);
    assert_eq!(sink.rows[0].raw_ordinal, 2);
    assert_eq!(sink.rows[1].raw_ordinal, 3);
    assert_eq!(appended.next_raw_ordinal, 4);
}

#[test]
fn malformed_complete_record_does_not_hide_later_valid_content() {
    let contents = [
        session_meta("recovery-owner"),
        "{not valid}\n".to_owned(),
        message("assistant", "survives"),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let source = discover_one(&path, "recovery-owner");
    let (scan, sink) = scan_collect(source, None);

    assert_eq!(sink.rows.len(), 1);
    assert_eq!(sink.rows[0].raw_ordinal, 2);
    assert_eq!(scan.counters.malformed_records, 1);
    assert_eq!(scan.counters.rejected_complete_records, 1);
}

#[test]
fn checkpoint_round_trip_contains_control_state_but_no_event_body() {
    let secret_call = jsonl(json!({
        "timestamp": "2026-01-01T00:00:02Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "exec_command",
            "call_id": "pending-checkpoint-call",
            "arguments": {
                "cmd": "COMMAND_CHECKPOINT_SECRET",
                "token": "ARGUMENT_CHECKPOINT_SECRET"
            }
        }
    }));
    let contents = [
        session_meta("checkpoint-owner"),
        message("user", "event body must stay out of checkpoint"),
        secret_call,
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, _) = scan_collect(discover_one(&path, "checkpoint-owner"), None);
    let checkpoint = scan.checkpoint([0; 32]).unwrap();
    let encoded = checkpoint.encode().unwrap();
    let wire = String::from_utf8(encoded.clone()).unwrap();

    assert!(!wire.contains("event body must stay out of checkpoint"));
    assert!(!wire.contains("pending-checkpoint-call"));
    assert!(!wire.contains("exec_command"));
    assert!(!wire.contains("printf retained"));
    assert!(!wire.contains("COMMAND_CHECKPOINT_SECRET"));
    assert!(!wire.contains("ARGUMENT_CHECKPOINT_SECRET"));
    assert!(!wire.contains("command"));
    assert!(!wire.contains("arguments_preview"));
    let decoded_wire = serde_json::from_str::<Value>(&wire).unwrap();
    assert_eq!(decoded_wire["version"], 9);
    assert_eq!(
        decoded_wire["lineage_dependency_sha256"],
        json!(vec![0; 32])
    );
    assert_eq!(
        decoded_wire["pending_tool_authorities"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(CodexNativeCheckpoint::decode(&encoded).unwrap(), checkpoint);

    let mut old_version = decoded_wire.clone();
    old_version["version"] = json!(8);
    assert!(CodexNativeCheckpoint::decode(&serde_json::to_vec(&old_version).unwrap()).is_err());

    let mut oversized_contexts = decoded_wire;
    let contexts = oversized_contexts["pending_tool_authorities"]
        .as_array_mut()
        .unwrap();
    let context = contexts.first().unwrap().clone();
    for index in 0..25 {
        let mut context = context.clone();
        context["raw_ordinal"] = json!(100 + index);
        context["record_start"] = json!(100 + index);
        context["record_end"] = json!(101 + index);
        contexts.push(context);
    }
    assert!(
        CodexNativeCheckpoint::decode(&serde_json::to_vec(&oversized_contexts).unwrap()).is_err()
    );
}

#[test]
fn terminal_checkpoint_boundary_tamper_rejects_during_decode() {
    let contents = [
        session_meta("terminal-tamper-owner"),
        message("user", "complete"),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, _) = scan_collect(discover_one(&path, "terminal-tamper-owner"), None);
    let checkpoint = scan.checkpoint([0; 32]).unwrap();
    let mut wire = serde_json::from_slice::<Value>(&checkpoint.encode().unwrap()).unwrap();

    wire["boundary"]["complete_eof"] = json!(contents.len() as u64 - 1);
    let tampered = serde_json::to_vec(&wire).unwrap();
    assert!(CodexNativeCheckpoint::decode(&tampered).is_err());
}

#[test]
fn unchanged_replay_revalidates_raw_ordinal_boundary_and_digests() {
    let complete = [
        session_meta("checkpoint-validation-owner"),
        message("user", "complete"),
    ]
    .concat();
    let partial = r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","output":"partial"#;
    let contents = format!("{complete}{partial}");
    let (_temp, path) = write_source(&contents);
    let (scan, _) = scan_collect(discover_one(&path, "checkpoint-validation-owner"), None);
    let checkpoint = scan.checkpoint([0; 32]).unwrap();
    let encoded = checkpoint.encode().unwrap();

    let mut bad_length = serde_json::from_slice::<Value>(&encoded).unwrap();
    bad_length["boundary"]["incomplete_tail_len"] = json!(0);
    assert!(CodexNativeCheckpoint::decode(&serde_json::to_vec(&bad_length).unwrap()).is_err());

    let mut bad_boundary = serde_json::from_slice::<Value>(&encoded).unwrap();
    bad_boundary["boundary"]["complete_prefix_end"] = json!(complete.len() as u64 - 1);
    bad_boundary["boundary"]["incomplete_tail_len"] = json!(partial.len() as u64 + 1);
    let decoded_boundary =
        CodexNativeCheckpoint::decode(&serde_json::to_vec(&bad_boundary).unwrap()).unwrap();
    assert_checkpoint_replay_rejected(&path, "checkpoint-validation-owner", decoded_boundary);

    let mut bad_ordinal = serde_json::from_slice::<Value>(&encoded).unwrap();
    bad_ordinal["complete_record_count"] = json!(99);
    let decoded_ordinal =
        CodexNativeCheckpoint::decode(&serde_json::to_vec(&bad_ordinal).unwrap()).unwrap();
    assert_checkpoint_replay_rejected(&path, "checkpoint-validation-owner", decoded_ordinal);

    let mut bad_digest = serde_json::from_slice::<Value>(&encoded).unwrap();
    bad_digest["boundary"]["incomplete_tail_sha256"][0] = json!(255);
    let decoded_digest =
        CodexNativeCheckpoint::decode(&serde_json::to_vec(&bad_digest).unwrap()).unwrap();
    assert_checkpoint_replay_rejected(&path, "checkpoint-validation-owner", decoded_digest);
}

#[test]
fn append_proof_cannot_cross_canonical_locator_identity() {
    let contents = [
        session_meta("proof-owner"),
        message("user", "same physical bytes"),
    ]
    .concat();
    let (temp, first_path) = write_source(&contents);
    let second_path = temp.path().join("second.jsonl");
    fs::write(&second_path, &contents).unwrap();

    let (first, _) = scan_collect(discover_one(&first_path, "proof-owner"), None);
    let proof = first
        .bind_checkpoint("canonical-proof-a", CodexCheckpointGeneration::new(73))
        .unwrap()
        .unwrap();
    assert_eq!(proof.generation.get(), 73);
    assert_eq!(proof.identity.canonical_source_key, "canonical-proof-a");
    assert_eq!(proof.identity.source_path, first_path);

    let error = CodexNativeScanner::new_source_backed_v0(
        discover_one(&second_path, "proof-owner"),
        Some(&proof),
    )
    .unwrap_err();
    assert!(format!("{error}").contains("does not belong to catalog source"));
}
