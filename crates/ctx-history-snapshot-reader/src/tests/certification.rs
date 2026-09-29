use super::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use ctx_history_index_generation::{
    acquire_generation_read_lease, active_index_files, hashed_artifact_bytes, open_slot_index,
    CloneTestHookGuard, CloneTestOptions, GenerationReadLease,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use ctx_history_index_query::{IndexError, VerifiedIndex};

#[test]
fn schema_fingerprint_certification_and_bounds_fail_with_typed_errors_without_hashing() {
    let fixture = Fixture::new();
    let source = source("typed-errors");
    let generation_id = publish(
        &fixture.index_root,
        1,
        &[(source.clone(), vec![record(&source, 1, "record")])],
    );

    let mut wrong_schema = SnapshotContract::current().unwrap();
    wrong_schema.schema.manifest_version += 1;
    assert!(matches!(
        CoreSnapshot::open(&fixture.data_root, &generation_id, &wrong_schema),
        Err(SnapshotError::SchemaMismatch { .. })
    ));
    let mut wrong_fingerprint = SnapshotContract::current().unwrap();
    wrong_fingerprint.core_record_fingerprint = "f".repeat(64);
    assert!(matches!(
        CoreSnapshot::open(&fixture.data_root, &generation_id, &wrong_fingerprint),
        Err(SnapshotError::FingerprintMismatch { .. })
    ));

    let snapshot = open(&fixture, &generation_id);
    assert!(matches!(
        snapshot.source_manifest_page(None, 0),
        Err(SnapshotError::Bounds(_))
    ));
    assert!(matches!(
        snapshot.source_manifest_page(None, MAX_SOURCE_MANIFEST_PAGE_ITEMS + 1),
        Err(SnapshotError::Bounds(_))
    ));
    assert!(matches!(
        snapshot.source_delta_page(&snapshot, None, 0),
        Err(SnapshotError::Bounds(_))
    ));
    assert!(matches!(
        snapshot.record_page(&source, None, 0, DEFAULT_SNAPSHOT_PAGE_BUDGET),
        Err(SnapshotError::Bounds(_))
    ));
    assert!(matches!(
        snapshot.record_page(
            &source,
            None,
            MAX_SOURCE_EVENT_PAGE_ITEMS + 1,
            DEFAULT_SNAPSHOT_PAGE_BUDGET
        ),
        Err(SnapshotError::Bounds(_))
    ));
    assert!(matches!(
        snapshot.record_page(&source, None, 1, SnapshotPageBudget::new(0, 1)),
        Err(SnapshotError::Bounds(_))
    ));
    drop(snapshot);

    fs::remove_file(certification_file_for_active(&fixture.index_root).unwrap()).unwrap();
    reset_physical_verification_activity();
    assert!(matches!(
        CoreSnapshot::open(
            &fixture.data_root,
            &generation_id,
            &SnapshotContract::current().unwrap()
        ),
        Err(SnapshotError::Corrupt(_))
    ));
    assert_eq!(
        checksum_walks(),
        0,
        "reader hashed an uncertified generation"
    );
}

#[test]
fn overlapping_readers_retain_a_real_generation_until_the_last_close() {
    let fixture = Fixture::new();
    let source = source("overlapping");
    let first_id = publish(
        &fixture.index_root,
        1,
        &[(source.clone(), vec![record(&source, 1, "first")])],
    );
    let first_slot = load_active_generation_pointer(&fixture.index_root)
        .unwrap()
        .unwrap()
        .active()
        .clone();
    let first_path = slot_path(&fixture.index_root, &first_slot);
    let first = open(&fixture, &first_id);
    let second = open(&fixture, &first_id);

    publish(
        &fixture.index_root,
        2,
        &[(source.clone(), vec![record(&source, 1, "second")])],
    );
    publish(
        &fixture.index_root,
        3,
        &[(source.clone(), vec![record(&source, 1, "third")])],
    );
    assert!(first_path.is_dir());
    drop(first);
    drop(
        GenerationWriter::open(&fixture.index_root, WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap(),
    );
    assert!(first_path.is_dir());
    drop(second);
    drop(
        GenerationWriter::open(&fixture.index_root, WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap(),
    );
    assert!(!first_path.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn active_snapshot_opens_while_an_older_reader_retains_reused_artifacts() {
    use std::os::unix::fs::MetadataExt as _;

    let _clone = CloneTestHookGuard::set(
        CloneTestOptions {
            force_reflink_fallback: true,
            ..CloneTestOptions::default()
        },
        |_, _| Ok(()),
    );
    let fixture = Fixture::new();
    let first_source = source("retained-alias-active-open-first");
    let first_id = publish(
        &fixture.index_root,
        1,
        &[(
            first_source.clone(),
            vec![record(&first_source, 1, "first")],
        )],
    );
    let first = open(&fixture, &first_id);
    let second_source = source("retained-alias-active-open-second");
    publish(
        &fixture.index_root,
        2,
        &[(
            second_source.clone(),
            vec![record(&second_source, 1, "second")],
        )],
    );
    let third_source = source("retained-alias-active-open-third");
    let active_id = publish(
        &fixture.index_root,
        3,
        &[(
            third_source.clone(),
            vec![record(&third_source, 1, "third")],
        )],
    );
    let active_slot = load_active_generation_pointer(&fixture.index_root)
        .unwrap()
        .unwrap()
        .active()
        .clone();
    let active_path = slot_path(&fixture.index_root, &active_slot);
    let active_index = open_slot_index(&fixture.index_root, &active_slot).unwrap();
    assert!(
        active_index_files(&active_index)
            .unwrap()
            .iter()
            .any(|path| {
                fs::metadata(active_path.join(path)).is_ok_and(|metadata| metadata.nlink() >= 3)
            }),
        "fixture did not retain a reused artifact across all three generations"
    );

    let active = open(&fixture, &active_id);
    assert_eq!(active.generation_id(), active_id);
    drop(active);
    drop(first);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn leased_generation_after_managed_publications() -> (Fixture, String, GenerationReadLease, PathBuf)
{
    use std::os::unix::fs::MetadataExt as _;

    let _clone = CloneTestHookGuard::set(
        CloneTestOptions {
            force_reflink_fallback: true,
            ..CloneTestOptions::default()
        },
        |_, _| Ok(()),
    );
    let fixture = Fixture::new();
    let source = source("pointer-churn");
    let first_id = publish(
        &fixture.index_root,
        1,
        &[(source.clone(), vec![record(&source, 1, "first")])],
    );
    let first_slot = load_active_generation_pointer(&fixture.index_root)
        .unwrap()
        .unwrap()
        .active()
        .clone();
    let first_path = slot_path(&fixture.index_root, &first_slot);
    let first_index = open_slot_index(&fixture.index_root, &first_slot).unwrap();
    let certified_metadata = active_index_files(&first_index)
        .unwrap()
        .into_iter()
        .map(|relative_path| {
            let metadata = fs::metadata(first_path.join(&relative_path)).unwrap();
            (
                relative_path,
                metadata.len(),
                metadata.mode(),
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.ctime(),
                metadata.ctime_nsec(),
                metadata.nlink(),
            )
        })
        .collect::<Vec<_>>();
    let lease = acquire_generation_read_lease(&fixture.index_root, &first_id).unwrap();
    publish(
        &fixture.index_root,
        2,
        &[(source.clone(), vec![record(&source, 1, "second")])],
    );
    publish(
        &fixture.index_root,
        3,
        &[(source.clone(), vec![record(&source, 1, "third")])],
    );
    assert!(certified_metadata.iter().any(
        |(relative_path, len, mode, mtime, mtime_nsec, ctime, ctime_nsec, nlink)| {
            let current = fs::metadata(first_path.join(relative_path)).unwrap();
            current.len() == *len
                && current.mode() == *mode
                && current.mtime() == *mtime
                && current.mtime_nsec() == *mtime_nsec
                && current.nlink() == *nlink
                && (current.ctime(), current.ctime_nsec()) != (*ctime, *ctime_nsec)
        }
    ));

    // These ctime changes come entirely from owned candidate links and GC.
    // The writer must preserve their immutable proof for an older held lease.
    (fixture, first_id, lease, first_path)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn assert_leased_original_without_hashing(lease: &GenerationReadLease, generation_id: &str) {
    reset_physical_verification_activity();
    let reader = lease
        .with_root_access(|root| VerifiedIndex::open_generation_read_lease(root, lease))
        .unwrap()
        .unwrap();
    assert_eq!(reader.generation_id(), generation_id);
    assert_eq!(reader.document_count(), 1);
    assert_eq!(reader.count_term("first").unwrap(), 1);
    assert_eq!(reader.count_term("third").unwrap(), 0);
    assert_eq!(checksum_walks(), 0);
    assert_eq!(hashed_artifact_bytes(), 0);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn leased_certified_generation_accepts_managed_net_zero_link_changes_without_hashing() {
    let (_fixture, generation_id, lease, _) = leased_generation_after_managed_publications();
    assert_leased_original_without_hashing(&lease, &generation_id);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn leased_certified_generation_rejects_unowned_changes_without_hashing() {
    use std::io::Write as _;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    for alteration in ["external_link", "net_zero_link_churn", "payload"] {
        let (fixture, generation_id, lease, first_path) =
            leased_generation_after_managed_publications();
        assert_leased_original_without_hashing(&lease, &generation_id);
        let artifact = fs::read_dir(&first_path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "store")
            })
            .unwrap();
        let before = fs::metadata(&artifact).unwrap();
        if alteration == "payload" {
            let mut bytes = fs::read(&artifact).unwrap();
            bytes[0] ^= 0x5a;
            fs::set_permissions(&artifact, fs::Permissions::from_mode(before.mode() | 0o200))
                .unwrap();
            let mut file = fs::OpenOptions::new().write(true).open(&artifact).unwrap();
            file.write_all(&bytes).unwrap();
            file.set_times(fs::FileTimes::new().set_modified(before.modified().unwrap()))
                .unwrap();
            file.sync_all().unwrap();
            drop(file);
            fs::set_permissions(&artifact, before.permissions()).unwrap();
        } else {
            // This alias is outside every generation and every retention owner.
            let alias = fixture._temp.path().join("unowned.store");
            fs::hard_link(&artifact, &alias).unwrap();
            if alteration == "net_zero_link_churn" {
                fs::remove_file(alias).unwrap();
            }
        }
        let after = fs::metadata(&artifact).unwrap();
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        assert_eq!(
            (after.len(), after.mode(), after.mtime(), after.mtime_nsec()),
            (
                before.len(),
                before.mode(),
                before.mtime(),
                before.mtime_nsec()
            )
        );
        assert_eq!(
            after.nlink(),
            before.nlink() + u64::from(alteration == "external_link")
        );
        assert_ne!(
            (after.ctime(), after.ctime_nsec()),
            (before.ctime(), before.ctime_nsec())
        );

        reset_physical_verification_activity();
        assert!(
            matches!(
                lease
                    .with_root_access(|root| {
                        VerifiedIndex::open_generation_read_lease(root, &lease)
                    })
                    .unwrap(),
                Err(IndexError::ChecksumMismatch)
            ),
            "{alteration}"
        );
        assert_eq!(checksum_walks(), 0, "{alteration} triggered a full audit");
        assert_eq!(
            hashed_artifact_bytes(),
            0,
            "{alteration} hashed payload bytes"
        );
    }
}
