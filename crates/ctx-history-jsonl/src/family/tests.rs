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
