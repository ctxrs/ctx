use std::{
    fs::{self, OpenOptions},
    io::Write,
};

use super::*;
use ctx_history_source_io::open_provider_source_file_mapped as open_provider_source_file;

fn drain(reader: &mut JsonlReader<CaptureError>) -> Result<Vec<Vec<u8>>> {
    let mut records = Vec::new();
    while reader
        .visit_page(&mut |record| -> Result<()> {
            records.push(record.bytes().to_vec());
            Ok(())
        })?
        .is_some()
    {}
    Ok(records)
}

fn semantic_identity(source_path: &Path, revision: &str) -> JsonlSourceIdentity {
    JsonlSourceIdentity::new(
        "test",
        revision,
        "semantic-pass-binding-policy-v1",
        [9; 32],
        source_path.to_owned(),
    )
}

fn finish_semantic_pass(
    reader: &mut JsonlReader<CaptureError>,
) -> Result<Vec<JsonlPhysicalRecord>> {
    let mut records = Vec::new();
    while let Some(record) = reader.next_execution_record()? {
        records.push(record);
    }
    Ok(records)
}

#[test]
fn readers_opened_from_one_retained_source_drain_independently() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("source.jsonl");
    fs::write(
        &source_path,
        b"{\"message\":\"first\"}\n{\"message\":\"second\"}\n",
    )
    .unwrap();
    let source_file = Arc::new(open_provider_source_file(&source_path).unwrap());
    let identity = JsonlSourceIdentity::new(
        "test",
        "independent-reader-v1",
        "independent-reader-policy-v1",
        [7; 32],
        source_path,
    );
    let mut first =
        JsonlReader::open(identity.clone(), Arc::clone(&source_file), None, None).unwrap();
    let mut second = JsonlReader::open(identity, source_file, None, None).unwrap();
    let expected = vec![
        br#"{"message":"first"}"#.to_vec(),
        br#"{"message":"second"}"#.to_vec(),
    ];

    assert_eq!(drain(&mut first).unwrap(), expected);
    assert_eq!(drain(&mut second).unwrap(), expected);
}

#[test]
fn unchanged_standard_zstd_source_reuses_its_checkpoint_without_physical_resume() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("source.jsonl.zst");
    let encoded = zstd::stream::encode_all(
        std::io::Cursor::new(b"{\"message\":\"first\"}\n{\"message\":\"second\"}\n"),
        1,
    )
    .unwrap();
    fs::write(&source_path, encoded).unwrap();
    let identity = JsonlSourceIdentity::new(
        "test",
        "standard-zstd-unchanged-v1",
        "standard-zstd-unchanged-policy-v1",
        [8; 32],
        source_path.clone(),
    );
    let source_file = Arc::new(open_provider_source_file(&source_path).unwrap());
    let mut first = JsonlReader::open_with_record_framing_and_encoding(
        identity.clone(),
        source_file,
        None,
        None,
        JsonlPhysicalEncoding::StandardZstdJsonl,
        JsonlRecordFraming::ordinary(),
    )
    .unwrap();
    assert_eq!(drain(&mut first).unwrap().len(), 2);
    let checkpoint = first.outcome().unwrap().checkpoint().clone();

    let source_file = Arc::new(open_provider_source_file(&source_path).unwrap());
    let mut unchanged = JsonlReader::open_with_record_framing_and_encoding(
        identity,
        source_file,
        Some(&checkpoint),
        None,
        JsonlPhysicalEncoding::StandardZstdJsonl,
        JsonlRecordFraming::ordinary(),
    )
    .unwrap();
    assert_eq!(unchanged.source_change(), JsonlSourceChange::Unchanged);
    assert!(drain(&mut unchanged).unwrap().is_empty());
    assert_eq!(unchanged.outcome().unwrap().checkpoint(), &checkpoint);
}

#[test]
fn semantic_projection_rejects_same_length_rewrite_after_preflight() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("source.jsonl");
    let admitted = b"{\"message\":\"authority-a\"}\n{\"message\":\"stable-z\"}\n";
    let rewritten = b"{\"message\":\"projected-b\"}\n{\"message\":\"stable-z\"}\n";
    assert_eq!(admitted.len(), rewritten.len());
    fs::write(&source_path, admitted).unwrap();
    let source_file = Arc::new(open_provider_source_file(&source_path).unwrap());
    let mut reader = JsonlReader::open_semantic_with_record_framing(
        semantic_identity(&source_path, "semantic-rewrite-v1"),
        source_file,
        None,
        JsonlSemanticPreflightMode::AdmittedEof(None),
        None,
        JsonlRecordFraming::ordinary(),
        None,
    )
    .unwrap();

    let initial = reader.execution_position().unwrap();
    finish_semantic_pass(&mut reader).unwrap();
    let hook_path = source_path.clone();
    set_after_jsonl_semantic_preflight_hook(source_path, move || {
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(hook_path)
            .unwrap();
        file.write_all(rewritten).unwrap();
        file.sync_all().unwrap();
    });
    assert!(reader
        .settle_semantic_preflight(initial, true, false)
        .unwrap());

    let mut projected = Vec::new();
    let error = loop {
        match reader.next_execution_record() {
            Ok(Some(record)) => {
                projected.push(reader.execution_record_bytes(record).unwrap().to_vec());
            }
            Ok(None) => {
                panic!("rewritten projection unexpectedly satisfied the preflight seal")
            }
            Err(error) => break error,
        }
    };
    assert!(matches!(error, CaptureError::SourceChangedDuringCapture));
    assert_eq!(
        projected,
        vec![
            br#"{"message":"projected-b"}"#.to_vec(),
            br#"{"message":"stable-z"}"#.to_vec()
        ]
    );
    assert!(reader.outcome().is_none());
}

#[test]
fn semantic_binding_preserves_incomplete_tail_completion_ordinal() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("source.jsonl");
    fs::write(&source_path, b"first\npartial").unwrap();
    let identity = semantic_identity(&source_path, "semantic-tail-v1");
    let source_file = Arc::new(open_provider_source_file(&source_path).unwrap());
    let mut first = JsonlReader::open_semantic_with_record_framing(
        identity.clone(),
        source_file,
        None,
        JsonlSemanticPreflightMode::AdmittedEof(None),
        None,
        JsonlRecordFraming::ordinary(),
        None,
    )
    .unwrap();

    let initial = first.execution_position().unwrap();
    finish_semantic_pass(&mut first).unwrap();
    let hook_path = source_path.clone();
    set_after_jsonl_semantic_preflight_hook(source_path.clone(), move || {
        let mut file = OpenOptions::new().append(true).open(hook_path).unwrap();
        file.write_all(b"-done\n").unwrap();
        file.sync_all().unwrap();
    });
    assert!(first
        .settle_semantic_preflight(initial, true, false)
        .unwrap());
    let first_records = finish_semantic_pass(&mut first).unwrap();
    assert_eq!(first_records.len(), 2);
    assert_eq!(first_records[1].physical_ordinal, 1);
    assert!(!first_records[1].complete);
    assert_eq!(
        first
            .outcome()
            .unwrap()
            .checkpoint()
            .next_physical_ordinal(),
        1
    );
    let checkpoint = first.outcome().unwrap().checkpoint().clone();
    let admitted_eof_sha256 = first.admitted_eof_sha256().unwrap().unwrap();
    drop(first);

    let source_file = Arc::new(open_provider_source_file(&source_path).unwrap());
    let mut resumed = JsonlReader::open_semantic_with_record_framing(
        identity,
        source_file,
        Some(&checkpoint),
        JsonlSemanticPreflightMode::AdmittedEof(Some(admitted_eof_sha256)),
        None,
        JsonlRecordFraming::ordinary(),
        None,
    )
    .unwrap();
    assert_eq!(
        resumed.execution_certified_prefix_end(),
        Some(checkpoint.complete_prefix_end())
    );
    let preflight_start = resumed.execution_position().unwrap();
    finish_semantic_pass(&mut resumed).unwrap();
    assert!(resumed
        .settle_semantic_preflight(preflight_start, true, false)
        .unwrap());
    let completed = resumed.next_execution_record().unwrap().unwrap();
    assert_eq!(completed.physical_ordinal, 1);
    assert!(completed.complete);
    assert_eq!(
        resumed.execution_record_bytes(completed).unwrap(),
        b"partial-done"
    );
    assert!(resumed.next_execution_record().unwrap().is_none());
    assert_eq!(
        resumed
            .outcome()
            .unwrap()
            .checkpoint()
            .next_physical_ordinal(),
        2
    );
}

fn open_direct_tail_reader(
    source_path: &Path,
    previous: Option<&JsonlCheckpoint>,
    direct_append: bool,
    bind_admitted_eof: bool,
    logical_eof: Option<u64>,
    framing: JsonlRecordFraming,
) -> JsonlReader<CaptureError> {
    JsonlReader::open_semantic_with_record_framing_and_encoding_direct_and_resources(
        semantic_identity(source_path, "direct-tail-v1"),
        Arc::new(open_provider_source_file(source_path).unwrap()),
        previous,
        if bind_admitted_eof {
            JsonlSemanticPreflightMode::AdmittedEof(
                previous.and_then(JsonlCheckpoint::admitted_eof_sha256),
            )
        } else {
            JsonlSemanticPreflightMode::CompletePrefix
        },
        None,
        JsonlPhysicalEncoding::RawJsonl,
        framing,
        None,
        direct_append,
        logical_eof,
        None,
    )
    .unwrap()
}

fn assert_tail_checkpoint(checkpoint: &JsonlCheckpoint, admitted: &[u8]) {
    let complete_end = admitted
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |last| last + 1);
    assert!(checkpoint.is_internally_consistent());
    assert_eq!(checkpoint.admitted_length(), admitted.len() as u64);
    assert_eq!(checkpoint.complete_prefix_end(), complete_end as u64);
    assert_eq!(
        checkpoint.next_physical_ordinal(),
        admitted.iter().filter(|byte| **byte == b'\n').count() as u64
    );
    assert_eq!(checkpoint.terminal(), complete_end == admitted.len());
    assert_eq!(
        checkpoint.admitted_eof_sha256().unwrap(),
        <[u8; 32]>::from(Sha256::digest(admitted))
    );
    let frontier = checkpoint.restore_complete_prefix_eof_hasher().unwrap();
    assert_eq!(frontier.bytes_hashed(), complete_end as u64);
    assert_eq!(
        frontier.digest(),
        <[u8; 32]>::from(Sha256::digest(&admitted[..complete_end]))
    );
    let mut complete = Sha256::new();
    complete.update(b"ctx-direct-jsonl-nativepath-prefix-v1\0");
    complete.update(&admitted[..complete_end]);
    assert_eq!(
        *checkpoint.complete_prefix_sha256(),
        <[u8; 32]>::from(complete.finalize())
    );
}

fn assert_tail_pass(
    reader: &mut JsonlReader<CaptureError>,
    admitted: &[u8],
    start: u64,
    first_ordinal: u64,
) -> u64 {
    let mut offset = start;
    let mut ordinal = first_ordinal;
    for wire in admitted[start as usize..].split_inclusive(|byte| *byte == b'\n') {
        let record = reader.next_execution_record().unwrap().unwrap();
        let complete = wire.ends_with(b"\n");
        assert_eq!(record.byte_start, offset);
        assert_eq!(record.byte_end_exclusive, offset + wire.len() as u64);
        assert_eq!(record.physical_ordinal, ordinal);
        assert_eq!(record.complete, complete);
        assert_eq!(record.sha256, <[u8; 32]>::from(Sha256::digest(wire)));
        assert_eq!(
            reader.execution_record_bytes(record).unwrap(),
            wire.strip_suffix(b"\n").unwrap_or(wire)
        );
        offset += wire.len() as u64;
        ordinal += u64::from(complete);
    }
    assert!(reader.next_execution_record().unwrap().is_none());
    assert_tail_checkpoint(reader.outcome().unwrap().checkpoint(), admitted);
    offset - start
}

#[test]
fn direct_append_reconsiders_only_deferred_tail_and_suffix() {
    for (bind_admitted_eof, prefix_records) in [(false, 4097), (true, 4097), (true, 0)] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let source_path = temp.path().join("source.jsonl");
        let mut contents = b"{\"message\":\"prefix\"}\n".repeat(prefix_records);
        contents.extend_from_slice(b"{\"message\":\"part");
        // Cross a BufReader refill with the old EOF inside the deferred record,
        // and keep the nonempty raw frontier off a SHA block boundary.
        contents.extend_from_slice(&b"x".repeat(8 * 1024 + 17));
        fs::write(&source_path, &contents).unwrap();
        let mut cold = open_direct_tail_reader(
            &source_path,
            None,
            false,
            bind_admitted_eof,
            None,
            JsonlRecordFraming::ordinary(),
        );
        assert_tail_pass(&mut cold, &contents, 0, 0);
        let mut checkpoint = cold.outcome().unwrap().checkpoint().clone();
        drop(cold);

        for suffix in [
            b"ial".as_slice(),
            b"ly",
            b" done\"}\n{\"message\":\"next\"}\n{\"message\":\"later",
            b" done\"}\n",
            b"{\"message\":\"final\"}\n",
        ] {
            OpenOptions::new()
                .append(true)
                .open(&source_path)
                .unwrap()
                .write_all(suffix)
                .unwrap();
            contents.extend_from_slice(suffix);
            let prefix_reads = track_jsonl_prefix_hash_bytes(source_path.clone());
            let mut resumed = open_direct_tail_reader(
                &source_path,
                Some(&checkpoint),
                true,
                bind_admitted_eof,
                None,
                JsonlRecordFraming::ordinary(),
            );
            assert_eq!(resumed.source_change(), JsonlSourceChange::Append);
            assert!(resumed.execution_is_direct_append_resume());
            let start = checkpoint.complete_prefix_end();
            assert_eq!(resumed.execution_offset().unwrap(), start);
            let initial = resumed.execution_position().unwrap();
            let preflight_bytes = assert_tail_pass(
                &mut resumed,
                &contents,
                start,
                checkpoint.next_physical_ordinal(),
            );
            if bind_admitted_eof && !checkpoint.terminal() {
                assert_eq!(
                    resumed
                        .physical
                        .as_ref()
                        .unwrap()
                        .digest()
                        .authenticated_prefix_sha256(),
                    Some((checkpoint.admitted_eof_sha256().unwrap(), 0))
                );
            }
            assert!(resumed
                .settle_semantic_preflight(initial, true, false)
                .unwrap());
            let projection_bytes = assert_tail_pass(
                &mut resumed,
                &contents,
                start,
                checkpoint.next_physical_ordinal(),
            );
            assert_eq!(preflight_bytes, contents.len() as u64 - start);
            assert_eq!(projection_bytes, preflight_bytes);
            assert_eq!(
                prefix_reads.bytes(),
                0,
                "complete prefix must never be reread"
            );
            checkpoint = resumed.outcome().unwrap().checkpoint().clone();
            drop(resumed);
            drop(prefix_reads);

            let no_op_reads = track_jsonl_prefix_hash_bytes(source_path.clone());
            let mut unchanged = open_direct_tail_reader(
                &source_path,
                Some(&checkpoint),
                true,
                bind_admitted_eof,
                None,
                JsonlRecordFraming::ordinary(),
            );
            assert_eq!(unchanged.source_change(), JsonlSourceChange::Unchanged);
            assert!(finish_semantic_pass(&mut unchanged).unwrap().is_empty());
            assert_eq!(unchanged.outcome().unwrap().checkpoint(), &checkpoint);
            assert_eq!(no_op_reads.bytes(), 0);
        }
    }
}

#[test]
fn direct_append_incomplete_tail_page_rollback_matches_cold_scan() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("source.jsonl");
    let prefix = b"{\"message\":\"first\"}\n";
    let large_record = |fill| {
        let mut record = b"{\"message\":\"".to_vec();
        record.resize(PAGE_MAX_BYTES / 2 + 17, fill);
        record.extend_from_slice(b"\"}\n");
        record
    };
    let completed_tail = large_record(b'a');
    let next_record = large_record(b'b');
    let new_tail = b"{\"message\":\"new unfinished tail";
    let old = [
        prefix.as_slice(),
        &completed_tail[..completed_tail.len() / 2],
    ]
    .concat();
    fs::write(&source_path, &old).unwrap();
    let mut initial = open_direct_tail_reader(
        &source_path,
        None,
        false,
        true,
        None,
        JsonlRecordFraming::ordinary(),
    );
    finish_semantic_pass(&mut initial).unwrap();
    let previous = initial.outcome().unwrap().checkpoint().clone();
    assert!(!previous.terminal());
    assert_eq!(previous.complete_prefix_end(), prefix.len() as u64);
    drop(initial);

    let contents = [
        prefix.as_slice(),
        completed_tail.as_slice(),
        next_record.as_slice(),
        new_tail.as_slice(),
    ]
    .concat();
    fs::write(&source_path, &contents).unwrap();
    let prefix_reads = track_jsonl_prefix_hash_bytes(source_path.clone());
    let mut resumed = open_direct_tail_reader(
        &source_path,
        Some(&previous),
        true,
        true,
        None,
        JsonlRecordFraming::ordinary(),
    );
    assert!(resumed.execution_is_direct_append_resume());
    let start = resumed.execution_position().unwrap();
    finish_semantic_pass(&mut resumed).unwrap();
    assert!(resumed
        .settle_semantic_preflight(start, true, false)
        .unwrap());
    assert_eq!(
        resumed
            .physical
            .as_ref()
            .unwrap()
            .digest()
            .authenticated_prefix_sha256(),
        Some((
            <[u8; 32]>::from(Sha256::digest(prefix)),
            (old.len() - prefix.len()) as u64
        ))
    );

    // Both records fit individually, but the second is read speculatively and
    // rolled back when its bytes would push this page over the aggregate bound.
    assert!(completed_tail.len() < PAGE_MAX_BYTES);
    assert!(next_record.len() < PAGE_MAX_BYTES);
    assert!(completed_tail.len() + next_record.len() > PAGE_MAX_BYTES);
    let mut projected = Vec::new();
    assert!(resumed
        .visit_page(&mut |record| -> Result<()> {
            projected.push(record.evidence());
            Ok(())
        })
        .unwrap()
        .is_some());
    assert_eq!(projected.len(), 1);
    assert_eq!(
        projected[0].physical_ordinal(),
        previous.next_physical_ordinal()
    );
    let page_end = (prefix.len() + completed_tail.len()) as u64;
    assert_eq!(resumed.execution_offset().unwrap(), page_end);
    let physical = resumed.physical.as_ref().unwrap();
    assert_eq!(physical.complete_prefix_end(), page_end);
    assert_eq!(
        physical.next_physical_ordinal(),
        previous.next_physical_ordinal() + 1
    );
    let full = physical.digest().full_hasher().unwrap();
    assert_eq!(full.bytes_hashed(), page_end);
    assert_eq!(
        full.digest(),
        <[u8; 32]>::from(Sha256::digest(&contents[..page_end as usize]))
    );
    assert_eq!(
        physical.digest().authenticated_prefix_sha256(),
        Some((previous.admitted_eof_sha256().unwrap(), 0))
    );
    while resumed
        .visit_page(&mut |record| -> Result<()> {
            projected.push(record.evidence());
            Ok(())
        })
        .unwrap()
        .is_some()
    {}
    let resumed_checkpoint = resumed.outcome().unwrap().checkpoint().clone();
    assert_tail_checkpoint(&resumed_checkpoint, &contents);
    assert_eq!(prefix_reads.bytes(), 0);
    drop(prefix_reads);

    let mut cold = open_direct_tail_reader(
        &source_path,
        None,
        false,
        true,
        None,
        JsonlRecordFraming::ordinary(),
    );
    let cold_suffix = finish_semantic_pass(&mut cold)
        .unwrap()
        .into_iter()
        .filter(|record| {
            record.complete && record.physical_ordinal >= previous.next_physical_ordinal()
        })
        .map(|record| {
            JsonlRecordEvidence::new(
                record.physical_ordinal,
                record.byte_start,
                record.byte_end_exclusive,
                record.sha256,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(projected, cold_suffix);
    assert_eq!(&resumed_checkpoint, cold.outcome().unwrap().checkpoint());
}

#[test]
fn direct_append_rejects_changed_deferred_bytes_before_projection() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("source.jsonl");
    let prefix = b"{\"message\":\"prefix\"}\n".repeat(4096);
    let old = [prefix.as_slice(), b"{\"message\":\"old"].concat();
    fs::write(&source_path, &old).unwrap();
    let mut cold = open_direct_tail_reader(
        &source_path,
        None,
        false,
        true,
        None,
        JsonlRecordFraming::ordinary(),
    );
    finish_semantic_pass(&mut cold).unwrap();
    let checkpoint = cold.outcome().unwrap().checkpoint().clone();
    drop(cold);

    let rewritten = [prefix.as_slice(), b"{\"message\":\"new done\"}\n"].concat();
    fs::write(&source_path, &rewritten).unwrap();
    let prefix_reads = track_jsonl_prefix_hash_bytes(source_path.clone());
    let mut resumed = open_direct_tail_reader(
        &source_path,
        Some(&checkpoint),
        true,
        true,
        None,
        JsonlRecordFraming::ordinary(),
    );
    assert!(resumed.execution_is_direct_append_resume());
    let initial = resumed.execution_position().unwrap();
    assert_tail_pass(&mut resumed, &rewritten, prefix.len() as u64, 4096);
    assert!(!resumed
        .settle_semantic_preflight(initial, true, false)
        .unwrap());
    assert_eq!(prefix_reads.bytes(), 0);
    drop(resumed);

    let mut replacement = open_direct_tail_reader(
        &source_path,
        None,
        false,
        true,
        None,
        JsonlRecordFraming::ordinary(),
    );
    assert_tail_pass(&mut replacement, &rewritten, 0, 0);
}

#[test]
fn direct_append_tail_rewrite_after_preflight_breaks_the_pass_binding() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("source.jsonl");
    let old = b"{\"message\":\"first\"}\n{\"message\":\"part";
    fs::write(&source_path, old).unwrap();
    let mut cold = open_direct_tail_reader(
        &source_path,
        None,
        false,
        true,
        None,
        JsonlRecordFraming::ordinary(),
    );
    finish_semantic_pass(&mut cold).unwrap();
    let checkpoint = cold.outcome().unwrap().checkpoint().clone();
    drop(cold);
    let admitted = [old.as_slice(), b"ial done\"}\n"].concat();
    fs::write(&source_path, &admitted).unwrap();
    let mut resumed = open_direct_tail_reader(
        &source_path,
        Some(&checkpoint),
        true,
        true,
        None,
        JsonlRecordFraming::ordinary(),
    );
    let initial = resumed.execution_position().unwrap();
    finish_semantic_pass(&mut resumed).unwrap();
    let mut rewritten = admitted;
    rewritten[old.len() - 1] ^= 1;
    let hook_path = source_path.clone();
    set_after_jsonl_semantic_preflight_hook(source_path, move || {
        fs::write(hook_path, rewritten).unwrap();
    });
    assert!(resumed
        .settle_semantic_preflight(initial, true, false)
        .unwrap());
    let error = finish_semantic_pass(&mut resumed).unwrap_err();
    assert!(matches!(error, CaptureError::SourceChangedDuringCapture));
    assert!(resumed.outcome().is_none());
}

#[test]
fn direct_append_missing_or_corrupt_frontier_state_replaces_conservatively() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("source.jsonl");
    let old = b"{\"message\":\"prefix\"}\n{\"message\":\"partial";
    fs::write(&source_path, old).unwrap();
    let mut cold = open_direct_tail_reader(
        &source_path,
        None,
        false,
        true,
        None,
        JsonlRecordFraming::ordinary(),
    );
    finish_semantic_pass(&mut cold).unwrap();
    let checkpoint = cold.outcome().unwrap().checkpoint().clone();
    drop(cold);
    let grown = [old.as_slice(), b" done\"}\n"].concat();
    fs::write(&source_path, &grown).unwrap();

    for damage in [
        "legacy",
        "version",
        "digest",
        "bound",
        "domain-state",
        "eof-state",
    ] {
        let mut value = serde_json::to_value(&checkpoint).unwrap();
        match damage {
            "legacy" => {
                value
                    .as_object_mut()
                    .unwrap()
                    .remove("complete_prefix_eof_state");
            }
            "version" => {
                value["complete_prefix_eof_state"]["state"]["version"] = serde_json::json!(999)
            }
            "digest" => {
                let word = value["complete_prefix_eof_state"]["state"]["state"][0]
                    .as_u64()
                    .unwrap();
                value["complete_prefix_eof_state"]["state"]["state"][0] =
                    serde_json::json!(word ^ 1);
            }
            "bound" => {
                value["complete_prefix_eof_state"]["state"]["bytes_hashed"] =
                    serde_json::json!(checkpoint.admitted_length())
            }
            "domain-state" => {
                value
                    .as_object_mut()
                    .unwrap()
                    .remove("complete_prefix_sha256_state");
            }
            "eof-state" => value["admitted_eof_sha256_state"]["version"] = serde_json::json!(999),
            _ => unreachable!(),
        }
        let damaged: JsonlCheckpoint = serde_json::from_value(value).unwrap();
        assert_eq!(
            damaged.is_internally_consistent(),
            matches!(damage, "legacy" | "domain-state")
        );
        let mut replacement = open_direct_tail_reader(
            &source_path,
            Some(&damaged),
            true,
            true,
            None,
            JsonlRecordFraming::ordinary(),
        );
        assert_eq!(
            replacement.source_change(),
            JsonlSourceChange::Replace,
            "{damage}"
        );
        assert!(!replacement.execution_is_direct_append_resume());
        assert_tail_pass(&mut replacement, &grown, 0, 0);
    }
}

#[test]
fn direct_append_tail_truncation_and_path_replacement_do_not_resume() {
    for replace_path in [false, true] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let source_path = temp.path().join("source.jsonl");
        let old = b"{\"message\":\"prefix\"}\n{\"message\":\"a long unfinished message";
        fs::write(&source_path, old).unwrap();
        let mut cold = open_direct_tail_reader(
            &source_path,
            None,
            false,
            true,
            None,
            JsonlRecordFraming::ordinary(),
        );
        finish_semantic_pass(&mut cold).unwrap();
        let checkpoint = cold.outcome().unwrap().checkpoint().clone();
        drop(cold);
        let new = if replace_path {
            fs::rename(&source_path, temp.path().join("retained.jsonl")).unwrap();
            b"{\"message\":\"replacement with a different identity and longer length\"}\n"
                .as_slice()
        } else {
            b"{\"message\":\"prefix\"}\n{\"message\":\"short\"}\n".as_slice()
        };
        fs::write(&source_path, new).unwrap();
        let mut replacement = open_direct_tail_reader(
            &source_path,
            Some(&checkpoint),
            true,
            true,
            None,
            JsonlRecordFraming::ordinary(),
        );
        assert_eq!(replacement.source_change(), JsonlSourceChange::Replace);
        assert!(!replacement.execution_is_direct_append_resume());
        assert_tail_pass(&mut replacement, new, 0, 0);
    }
}

#[test]
fn direct_append_tail_continuation_respects_advancing_logical_eof() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("source.jsonl");
    let contents =
        b"{\"message\":\"first\"}\n{\"message\":\"partial done\"}\n{\"message\":\"uncommitted\"}\n";
    fs::write(&source_path, contents).unwrap();
    let initial_eof = b"{\"message\":\"first\"}\n{\"message\":\"part".len();
    let mut cold = open_direct_tail_reader(
        &source_path,
        None,
        false,
        true,
        Some(initial_eof as u64),
        JsonlRecordFraming::ordinary(),
    );
    assert_tail_pass(&mut cold, &contents[..initial_eof], 0, 0);
    let mut checkpoint = cold.outcome().unwrap().checkpoint().clone();
    drop(cold);
    let completed_eof = contents
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'\n')
        .nth(1)
        .unwrap()
        .0
        + 1;
    for eof in [initial_eof + 2, completed_eof] {
        let prefix_reads = track_jsonl_prefix_hash_bytes(source_path.clone());
        let mut resumed = open_direct_tail_reader(
            &source_path,
            Some(&checkpoint),
            true,
            true,
            Some(eof as u64),
            JsonlRecordFraming::ordinary(),
        );
        assert_eq!(resumed.source_change(), JsonlSourceChange::Append);
        assert!(resumed.execution_is_direct_append_resume());
        let initial = resumed.execution_position().unwrap();
        assert_tail_pass(
            &mut resumed,
            &contents[..eof],
            checkpoint.complete_prefix_end(),
            1,
        );
        assert!(resumed
            .settle_semantic_preflight(initial, true, false)
            .unwrap());
        assert_tail_pass(
            &mut resumed,
            &contents[..eof],
            checkpoint.complete_prefix_end(),
            1,
        );
        checkpoint = resumed.outcome().unwrap().checkpoint().clone();
        assert_eq!(
            checkpoint.source_observation().length(),
            contents.len() as u64
        );
        assert_eq!(prefix_reads.bytes(), 0);
    }
    assert_eq!(checkpoint.next_physical_ordinal(), 2);
    assert_eq!(checkpoint.complete_prefix_end(), completed_eof as u64);
}

#[test]
fn exhaustive_tail_preflight_rejects_rewritten_complete_prefix() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("source.jsonl");
    let old = b"{\"message\":\"before\"}\n{\"message\":\"partial";
    fs::write(&source_path, old).unwrap();
    let mut cold = open_direct_tail_reader(
        &source_path,
        None,
        false,
        true,
        None,
        JsonlRecordFraming::ordinary(),
    );
    finish_semantic_pass(&mut cold).unwrap();
    let checkpoint = cold.outcome().unwrap().checkpoint().clone();
    drop(cold);
    let replacement = b"{\"message\":\"after!\"}\n{\"message\":\"partial done\"}\n";
    fs::write(&source_path, replacement).unwrap();
    let mut exhaustive = open_direct_tail_reader(
        &source_path,
        Some(&checkpoint),
        false,
        true,
        None,
        JsonlRecordFraming::ordinary(),
    );
    assert!(!exhaustive.execution_is_direct_append_resume());
    assert_eq!(exhaustive.execution_offset().unwrap(), 0);
    let initial = exhaustive.execution_position().unwrap();
    assert_tail_pass(&mut exhaustive, replacement, 0, 0);
    assert!(!exhaustive
        .settle_semantic_preflight(initial, true, false)
        .unwrap());
}

#[test]
fn direct_append_completed_tail_retains_terminal_nul_framing() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("source.jsonl");
    let old = b"first\npartial";
    fs::write(&source_path, old).unwrap();
    let framing = JsonlRecordFraming::terminal_nul_padded(1024);
    let mut cold = open_direct_tail_reader(&source_path, None, false, true, None, framing);
    finish_semantic_pass(&mut cold).unwrap();
    let checkpoint = cold.outcome().unwrap().checkpoint().clone();
    drop(cold);
    let contents = [old.as_slice(), b" done\n\0\0\0"].concat();
    fs::write(&source_path, &contents).unwrap();
    let prefix_reads = track_jsonl_prefix_hash_bytes(source_path.clone());
    let mut resumed =
        open_direct_tail_reader(&source_path, Some(&checkpoint), true, true, None, framing);
    let initial = resumed.execution_position().unwrap();
    let preflight = finish_semantic_pass(&mut resumed).unwrap();
    assert!(resumed
        .settle_semantic_preflight(initial, true, false)
        .unwrap());
    let projection = finish_semantic_pass(&mut resumed).unwrap();
    assert_eq!(preflight, projection);
    assert_eq!(projection.len(), 2);
    assert_eq!(projection[0].physical_ordinal, 1);
    assert_eq!(projection[1].physical_ordinal, 2);
    assert!(projection[1].terminal_nul_padding);
    assert!(resumed.complete_prefix_ends_with_terminal_nul_padding());
    let checkpoint = resumed.outcome().unwrap().checkpoint();
    assert!(checkpoint.terminal());
    assert_eq!(checkpoint.complete_prefix_end(), contents.len() as u64);
    assert_eq!(
        checkpoint.admitted_eof_sha256().unwrap(),
        <[u8; 32]>::from(Sha256::digest(&contents))
    );
    assert_eq!(prefix_reads.bytes(), 0);
}
