use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
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

#[test]
fn active_source_family_contract_documented_storage_families_cover_routed_provider_formats() {
    let policy = include_str!("../../../../../../docs/provider-import-policy.md");
    let storage_families = policy
        .split_once("## Storage Families")
        .and_then(|(_, remainder)| {
            remainder
                .split_once("## Active-Writer Lifecycle Contract")
                .map(|(section, _)| section)
        })
        .expect("provider policy contains the storage-family table");
    let mut documented_formats = Vec::new();
    let mut mimocode_family = None;
    for row in storage_families.lines().filter(|line| {
        line.starts_with("| ") && !line.starts_with("| Provider ") && !line.starts_with("| ---")
    }) {
        let columns = row.split('|').collect::<Vec<_>>();
        assert_eq!(columns.len(), 6, "malformed storage-family row: {row}");
        let formats = columns[2]
            .split('`')
            .enumerate()
            .filter_map(|(index, value)| (index % 2 == 1).then_some(value.to_owned()))
            .collect::<Vec<_>>();
        assert!(
            !formats.is_empty(),
            "storage-family row has no format: {row}"
        );
        if formats.iter().any(|format| format == "mimocode_sqlite") {
            mimocode_family = Some(columns[3].trim().to_owned());
        }
        documented_formats.extend(formats);
    }

    let documented = documented_formats.iter().cloned().collect::<BTreeSet<_>>();
    assert_eq!(
        documented.len(),
        documented_formats.len(),
        "the storage-family table repeats a source format"
    );

    let routes = crate::provider::source_backed::source_backed_route_inventory();
    let routed = routes
        .iter()
        .filter(|route| route.provider != ctx_history_core::CaptureProvider::Custom)
        .filter(|route| {
            route.automatic
                || !routes
                    .iter()
                    .any(|candidate| candidate.provider == route.provider && candidate.automatic)
        })
        .map(|route| route.source_format.to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        documented, routed,
        "the policy taxonomy must describe every routed provider format exactly once"
    );
    assert_eq!(mimocode_family.as_deref(), Some("SQLite message store"));
}

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

    reset_jsonl_prefix_hash_bytes();
    let (change, unchanged, unchanged_checkpoint) = drain(&path, Some(&cold_checkpoint));
    assert_eq!(change, JsonlSourceChange::Unchanged);
    assert!(unchanged.is_empty());
    assert_eq!(unchanged_checkpoint, cold_checkpoint);
    assert_eq!(
        jsonl_prefix_hash_bytes(),
        0,
        "exact no-op must not rehash the certified source prefix"
    );

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
fn jsonl_page_bytes_are_a_rollover_target_not_a_record_limit() {
    const PAGE_TARGET_BYTES: usize = 8 * 1024 * 1024;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("large-events.jsonl");
    let large_record = vec![b'x'; PAGE_TARGET_BYTES + 1024];
    let mut contents = large_record.clone();
    contents.extend_from_slice(b"\nsmall\n");
    fs::write(&path, contents).unwrap();

    let source = opened(&path);
    let mut reader = JsonlReader::open(identity(&path), source, None, None).unwrap();
    let mut first_page = Vec::new();
    assert!(reader
        .visit_page(&mut |record| -> crate::Result<()> {
            first_page.push(record.bytes().len());
            Ok(())
        })
        .unwrap()
        .is_some());
    assert_eq!(first_page, vec![large_record.len()]);

    let mut second_page = Vec::new();
    assert!(reader
        .visit_page(&mut |record| -> crate::Result<()> {
            second_page.push(record.bytes().to_vec());
            Ok(())
        })
        .unwrap()
        .is_some());
    assert_eq!(second_page, vec![b"small".to_vec()]);
    assert!(reader
        .visit_page(&mut |_record| -> crate::Result<()> { Ok(()) })
        .unwrap()
        .is_none());
}

#[test]
fn jsonl_record_above_the_sixteen_mib_contract_is_rejected() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("oversized-events.jsonl");
    let mut contents = vec![b'x'; crate::MAX_PROVIDER_JSONL_LINE_BYTES + 1];
    contents.push(b'\n');
    fs::write(&path, contents).unwrap();

    let source = opened(&path);
    let mut reader = JsonlReader::open(identity(&path), source, None, None).unwrap();
    let error = reader
        .visit_page(&mut |_record| -> crate::Result<()> { Ok(()) })
        .unwrap_err();
    assert!(error.to_string().contains("JSONL record limit"));
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

    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("events.jsonl");
    let initial = b"{\"n\":1}\n{\"n\":2}\n";
    fs::write(&path, initial).unwrap();
    let (_, _, checkpoint) = drain(&path, None);
    let source = opened(&path);
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"n\":3}\n")
        .unwrap();
    let rewrite_path = path.clone();
    let rewrite_ran = Arc::new(AtomicBool::new(false));
    let observed_rewrite = Arc::clone(&rewrite_ran);
    set_after_jsonl_prefix_hash_hook(move || {
        OpenOptions::new()
            .write(true)
            .open(rewrite_path)
            .unwrap()
            .write_all(b"{\"n\":9}\n")
            .unwrap();
        observed_rewrite.store(true, Ordering::SeqCst);
    });
    let revalidation = revalidate_frozen_prefix(
        &path,
        source.as_ref(),
        checkpoint.source_observation(),
        checkpoint.complete_prefix_end(),
        *checkpoint.complete_prefix_sha256(),
    );
    assert!(rewrite_ran.load(Ordering::SeqCst), "test hook did not run");
    assert!(
        revalidation.is_err(),
        "a rewrite between prefix hashing and terminal observation must fail closed"
    );
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
