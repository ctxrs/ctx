use std::{
    fs::{self, OpenOptions},
    io::Write,
    sync::Arc,
};

use super::jsonl::*;
use crate::common::io::open_provider_source_file;

fn opened(path: &std::path::Path) -> Arc<crate::common::io::OpenedProviderSourceFile> {
    Arc::new(open_provider_source_file(path).unwrap())
}

fn identity(path: &std::path::Path) -> JsonlSourceIdentity {
    JsonlSourceIdentity::new(
        "test",
        "parser-v1",
        "policy-v1",
        [7; 32],
        path.to_path_buf(),
    )
}

type DrainedRecords = Vec<(u64, u64, Vec<u8>)>;
type DrainOutcome = (JsonlSourceChange, DrainedRecords, JsonlCheckpoint);

fn drain(path: &std::path::Path, previous: Option<&JsonlCheckpoint>) -> DrainOutcome {
    let source = opened(path);
    let mut reader = JsonlReader::open(identity(path), source, previous, None).unwrap();
    let change = reader.source_change();
    let mut records = Vec::new();
    while reader
        .visit_page(&mut |record| -> crate::Result<()> {
            let evidence = record.evidence();
            records.push((
                evidence.physical_ordinal(),
                evidence.byte_start(),
                record.bytes().to_vec(),
            ));
            Ok(())
        })
        .unwrap()
        .is_some()
    {}
    let checkpoint = reader.outcome().unwrap().checkpoint().clone();
    (change, records, checkpoint)
}

#[test]
fn physical_lifecycle_matrix_is_fail_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("events.jsonl");
    fs::write(&path, b"{\"n\":1}\n{\"n\":2}\n").unwrap();

    let (change, cold, cold_checkpoint) = drain(&path, None);
    assert_eq!(change, JsonlSourceChange::Cold);
    assert_eq!(cold.len(), 2);

    let (change, unchanged, unchanged_checkpoint) = drain(&path, Some(&cold_checkpoint));
    assert_eq!(change, JsonlSourceChange::Unchanged);
    assert!(unchanged.is_empty());
    assert_eq!(unchanged_checkpoint, cold_checkpoint);

    fs::write(&path, b"{\"n\":9}\n{\"n\":2}\n{\"n\":3}\n").unwrap();
    let (change, replacement, _) = drain(&path, Some(&cold_checkpoint));
    assert_eq!(change, JsonlSourceChange::Replace);
    assert_eq!(replacement.len(), 3);

    fs::write(&path, b"{\"n\":1}\n{\"n\":2}\n").unwrap();
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"n\":3}\n")
        .unwrap();
    let (change, appended, append_checkpoint) = drain(&path, Some(&cold_checkpoint));
    assert_eq!(change, JsonlSourceChange::Append);
    assert_eq!(
        appended,
        vec![(
            2,
            cold_checkpoint.complete_prefix_end(),
            b"{\"n\":3}".to_vec()
        )]
    );

    fs::write(&path, b"{\"n\":9}\n").unwrap();
    let (change, replacement, _) = drain(&path, Some(&append_checkpoint));
    assert_eq!(change, JsonlSourceChange::Replace);
    assert_eq!(replacement.len(), 1);
}

#[cfg(unix)]
#[test]
fn metadata_churn_with_exact_equal_length_prefix_is_logically_unchanged() {
    use std::os::unix::fs::PermissionsExt;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("events.jsonl");
    fs::write(&path, b"{\"n\":1}\n{\"n\":2}\n").unwrap();
    let (_, _, checkpoint) = drain(&path, None);

    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(permissions.mode() ^ 0o100);
    fs::set_permissions(&path, permissions).unwrap();

    let (change, records, current) = drain(&path, Some(&checkpoint));
    assert_eq!(change, JsonlSourceChange::Unchanged);
    assert!(records.is_empty());
    assert_eq!(current, checkpoint);
}

#[test]
fn incomplete_tail_is_reread_from_its_start() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("events.jsonl");
    fs::write(&path, b"{\"n\":1}\n{\"n\":").unwrap();
    let (_, records, partial) = drain(&path, None);
    assert_eq!(records.len(), 1);
    assert!(!partial.terminal());

    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"2}\n")
        .unwrap();
    let (change, records, complete) = drain(&path, Some(&partial));
    assert_eq!(change, JsonlSourceChange::Append);
    assert_eq!(
        records,
        vec![(1, partial.complete_prefix_end(), b"{\"n\":2}".to_vec())]
    );
    assert!(complete.terminal());
}

#[test]
fn parser_policy_descriptor_and_path_mismatch_force_replacement() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("events.jsonl");
    fs::write(&path, b"{\"n\":1}\n").unwrap();
    let (_, _, checkpoint) = drain(&path, None);

    for mismatched in [
        JsonlSourceIdentity::new("other", "parser-v1", "policy-v1", [7; 32], &path),
        JsonlSourceIdentity::new("test", "parser-v2", "policy-v1", [7; 32], &path),
        JsonlSourceIdentity::new("test", "parser-v1", "policy-v2", [7; 32], &path),
        JsonlSourceIdentity::new("test", "parser-v1", "policy-v1", [8; 32], &path),
        JsonlSourceIdentity::new(
            "test",
            "parser-v1",
            "policy-v1",
            [7; 32],
            path.with_file_name("other.jsonl"),
        ),
    ] {
        let source = opened(&path);
        let reader = JsonlReader::open(mismatched, source, Some(&checkpoint), None).unwrap();
        assert_eq!(reader.source_change(), JsonlSourceChange::Replace);
    }
}
