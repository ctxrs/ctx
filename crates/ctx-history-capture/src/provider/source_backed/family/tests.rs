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
fn active_source_family_contract_jsonl_physical_lifecycle_matrix_is_fail_closed() {
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

#[test]
fn active_source_family_contract_jsonl_probe_and_scan_share_one_growing_source() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("events.jsonl");
    fs::write(&path, b"{\"n\":1}\n").unwrap();
    let source = opened(&path);
    let (_, probe) = probe_first_record(&path, &source, |_record| -> crate::Result<()> {
        OpenOptions::new()
            .append(true)
            .open(&path)?
            .write_all(b"{\"n\":2}\n")?;
        Ok(())
    })
    .unwrap();
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"n\":3}\n")
        .unwrap();

    let mut reader = JsonlReader::open(identity(&path), source, None, Some(probe)).unwrap();
    let mut records = Vec::new();
    while reader
        .visit_page(&mut |record| -> crate::Result<()> {
            records.push((
                record.evidence().physical_ordinal(),
                record.bytes().to_vec(),
            ));
            Ok(())
        })
        .unwrap()
        .is_some()
    {}
    assert_eq!(
        records,
        vec![(1, b"{\"n\":2}".to_vec()), (2, b"{\"n\":3}".to_vec())]
    );
    assert!(reader.outcome().unwrap().checkpoint().terminal());
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
fn active_source_family_contract_jsonl_incomplete_tail_resumes_from_complete_boundary() {
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
fn active_source_family_contract_jsonl_append_publishes_one_frozen_prefix() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("events.jsonl");
    let initial = (0..64)
        .map(|value| format!("{{\"n\":{value}}}\n"))
        .collect::<String>();
    fs::write(&path, &initial).unwrap();
    let source = opened(&path);
    let mut reader = JsonlReader::open(identity(&path), source, None, None).unwrap();
    let mut frozen = Vec::new();

    assert!(reader
        .visit_page(&mut |record| -> crate::Result<()> {
            frozen.push(record.bytes().to_vec());
            Ok(())
        })
        .unwrap()
        .is_some());
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"n\":64}\n")
        .unwrap();
    assert!(reader
        .visit_page(&mut |_record| -> crate::Result<()> { Ok(()) })
        .unwrap()
        .is_none());
    let checkpoint = reader.outcome().unwrap().checkpoint().clone();
    assert_eq!(frozen.len(), 64);
    assert_eq!(checkpoint.complete_prefix_end(), initial.len() as u64);

    let (change, appended, next) = drain(&path, Some(&checkpoint));
    assert_eq!(change, JsonlSourceChange::Append);
    assert_eq!(
        appended,
        vec![(64, initial.len() as u64, b"{\"n\":64}".to_vec())]
    );
    assert!(next.terminal());
}

#[test]
fn active_source_family_contract_jsonl_partial_tail_completion_is_deferred() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("events.jsonl");
    let complete = (0..64)
        .map(|value| format!("{{\"n\":{value}}}\n"))
        .collect::<String>();
    fs::write(&path, format!("{complete}{{\"n\":")).unwrap();
    let source = opened(&path);
    let mut reader = JsonlReader::open(identity(&path), source, None, None).unwrap();

    assert!(reader
        .visit_page(&mut |_record| -> crate::Result<()> { Ok(()) })
        .unwrap()
        .is_some());
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"64}\n")
        .unwrap();
    assert!(reader
        .visit_page(&mut |_record| -> crate::Result<()> { Ok(()) })
        .unwrap()
        .is_none());
    let checkpoint = reader.outcome().unwrap().checkpoint().clone();
    assert!(!checkpoint.terminal());
    assert_eq!(checkpoint.complete_prefix_end(), complete.len() as u64);

    let (change, appended, next) = drain(&path, Some(&checkpoint));
    assert_eq!(change, JsonlSourceChange::Append);
    assert_eq!(
        appended,
        vec![(64, complete.len() as u64, b"{\"n\":64}".to_vec())]
    );
    assert!(next.terminal());
}

#[test]
fn active_source_family_contract_jsonl_rewrite_truncate_and_replacement_fail_closed() {
    #[derive(Debug, Clone, Copy)]
    enum Mutation {
        Rewrite,
        Truncate,
        Replace,
    }

    for mutation in [Mutation::Rewrite, Mutation::Truncate, Mutation::Replace] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("events.jsonl");
        let moved = temp.path().join("events-old.jsonl");
        let initial = (0..64)
            .map(|value| format!("{{\"n\":{value:02}}}\n"))
            .collect::<String>();
        fs::write(&path, &initial).unwrap();
        let initial_modified = fs::metadata(&path).unwrap().modified().unwrap();
        let source = opened(&path);
        let mut reader = JsonlReader::open(identity(&path), source, None, None).unwrap();
        assert!(reader
            .visit_page(&mut |_record| -> crate::Result<()> { Ok(()) })
            .unwrap()
            .is_some());

        match mutation {
            Mutation::Rewrite => {
                let rewritten = initial.replacen("{\"n\":00}", "{\"n\":99}", 1);
                assert_eq!(rewritten.len(), initial.len());
                fs::write(&path, rewritten).unwrap();
                fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_times(
                        std::fs::FileTimes::new().set_modified(
                            initial_modified
                                .checked_add(std::time::Duration::from_secs(2))
                                .unwrap(),
                        ),
                    )
                    .unwrap();
            }
            Mutation::Truncate => fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len(0)
                .unwrap(),
            Mutation::Replace => {
                fs::rename(&path, &moved).unwrap();
                fs::write(&path, &initial).unwrap();
            }
        }

        assert!(
            reader
                .visit_page(&mut |_record| -> crate::Result<()> { Ok(()) })
                .is_err(),
            "{mutation:?} must invalidate the frozen scan"
        );
        assert!(reader.outcome().is_none());
    }
}

#[test]
fn active_source_family_contract_jsonl_hydration_tolerates_append_only() {
    use sha2::{Digest, Sha256};

    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("events.jsonl");
    let moved = temp.path().join("events-old.jsonl");
    let first = b"{\"message\":\"indexed\"}\n";
    fs::write(&path, first).unwrap();
    let source = opened(&path);
    let range = JsonlHydrationRange::new(0, first.len(), Sha256::digest(first).into()).unwrap();

    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"message\":\"active\"}\n")
        .unwrap();
    let hydrated = visit_verified_ranges(
        &path,
        &source,
        &[range],
        |_, bytes| -> crate::Result<Vec<u8>> { Ok(bytes.to_vec()) },
    )
    .unwrap();
    assert_eq!(hydrated, vec![first.to_vec()]);

    fs::rename(&path, &moved).unwrap();
    fs::write(&path, first).unwrap();
    assert!(visit_verified_ranges(
        &path,
        &source,
        &[range],
        |_, bytes| -> crate::Result<Vec<u8>> { Ok(bytes.to_vec()) }
    )
    .is_err());
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
