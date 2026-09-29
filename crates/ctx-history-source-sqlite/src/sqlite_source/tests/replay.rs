use super::*;
use sha2::{Digest, Sha256};

#[test]
fn recovered_wal_copy_certifies_matching_commit_and_subsequent_no_copy_replay() {
    let temp = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    let authority = retain_parent_in_data_root(data.path(), temp.path());
    for expected in ["from-wal", "committed edit"] {
        if expected == "committed edit" {
            writer
                .execute("UPDATE messages SET body=?1", [expected])
                .unwrap();
        }
        // Explicit full-family policy exercises the non-Linux/root fallback
        // on every platform; it does not select the Linux-only live reader.
        let snapshot = authority
            .open_stable_snapshot(OsStr::new("provider.sqlite"))
            .unwrap();
        assert_eq!(
            snapshot.strategy(),
            SqliteSourceSnapshotStrategy::CopiedFamily
        );
        assert_eq!(read_values(&snapshot), [expected]);
        assert!(snapshot.admitted_revision_is_replay_safe());
        let revision = *snapshot.evidence().physical_revision();
        snapshot.finish().unwrap();
        let counters = authority.snapshot_counters();
        for _ in 0..2 {
            let fence = authority
                .observe_replay_fence(OsStr::new("provider.sqlite"))
                .unwrap();
            assert_eq!(*fence.revision(), revision);
            fence.revalidate().unwrap();
        }
        assert_eq!(authority.snapshot_counters(), counters);
    }
}

#[test]
fn physical_replay_fence_unchanged_is_zero_copy() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "first");
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    let fence = authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    let revision = *fence.revision();

    fence.revalidate().unwrap();
    assert_eq!(authority.snapshot_counters(), Default::default());
    assert_eq!(staging_entries(data_root.path()), 0);
    assert_eq!(*fence.revision(), revision);
    assert_eq!(
        *authority
            .observe_replay_fence(OsStr::new("provider.sqlite"))
            .unwrap()
            .revision(),
        revision,
    );
}

#[test]
fn physical_replay_revision_changes_on_move_and_old_fence_rejects_it() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = crate::test_support_paths::tempdir().unwrap();
    let original = temp.path().join("original");
    let moved = temp.path().join("moved");
    fs::create_dir(&original).unwrap();
    fs::create_dir(&moved).unwrap();
    create_database(&original.join("provider.sqlite"), "first");
    let original_authority = retain_parent_in_data_root(data_root.path(), &original);
    let original_fence = original_authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    let revision = *original_fence.revision();

    fs::rename(
        original.join("provider.sqlite"),
        moved.join("provider.sqlite"),
    )
    .unwrap();
    assert!(matches!(
        original_fence.revalidate(),
        Err(SqliteSourceAccessError::SourceChanged)
    ));

    let moved_authority = retain_parent_in_data_root(data_root.path(), &moved);
    let moved_fence = moved_authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    assert_ne!(*moved_fence.revision(), revision);
    moved_fence.revalidate().unwrap();
    let snapshot = moved_authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    assert_eq!(read_values(&snapshot), ["first"]);
    assert_eq!(
        snapshot.evidence().physical_revision(),
        moved_fence.revision()
    );
    snapshot.finish().unwrap();
}

#[test]
fn snapshot_replay_recertifies_legacy_tokens_and_metadata_only_changes() {
    use std::time::Duration;

    let temp = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    create_database(&database, "same content");
    let before = fs::read(&database).unwrap();
    let legacy = legacy_content_revision(&database);
    let authority = retain_parent_in_data_root(data.path(), temp.path());
    let snapshot = authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    let revision = *snapshot.evidence().physical_revision();
    assert_ne!(
        revision, legacy,
        "old checkpoints must not authorize new replay"
    );
    snapshot.finish().unwrap();
    let fence = authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    assert_eq!(*fence.revision(), revision);
    fence.revalidate().unwrap();

    // Change only native write metadata, with no timing or sleep dependency.
    let modified = fs::metadata(&database).unwrap().modified().unwrap();
    File::options()
        .write(true)
        .open(&database)
        .unwrap()
        .set_modified(modified + Duration::from_secs(2))
        .unwrap();
    assert_eq!(fs::read(&database).unwrap(), before);
    assert_eq!(legacy_content_revision(&database), legacy);
    let next = authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    assert_ne!(*next.revision(), revision);
    assert!(fence.revalidate().is_err());
    next.revalidate().unwrap();
    assert_eq!(
        next.revision(),
        authority
            .observe_replay_fence(OsStr::new("provider.sqlite"))
            .unwrap()
            .revision(),
    );
}

#[cfg(target_os = "linux")]
#[test]
fn persisted_replay_revision_detects_committed_wal_frame_reuse() {
    let temp = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    writer
        .execute_batch(
            "BEGIN;
         WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<130)
         INSERT INTO messages SELECT printf('%0300d', x) FROM n;
         COMMIT;",
        )
        .unwrap();
    let authority = retain_parent_in_data_root(data.path(), temp.path());
    let edit = "UPDATE messages SET body='edited' WHERE rowid=1;
                DELETE FROM messages WHERE rowid=2;";
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    writer.execute_batch(edit).unwrap();
    writer.cache_flush().unwrap();
    let family =
        SqliteSourceFamily::open(&authority, OsStr::new("provider.sqlite"), || {}).unwrap();
    let mut before_commit = family.capture_revision_evidence().unwrap();
    let prior = authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    let legacy = legacy_content_revision(&database);
    let snapshot = authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    assert!(snapshot.admitted_revision_is_replay_safe());
    assert_eq!(snapshot.evidence().physical_revision(), prior.revision());
    assert_eq!(read_values(&snapshot).len(), 131);
    snapshot.finish().unwrap();
    writer.execute_batch("ROLLBACK; BEGIN IMMEDIATE").unwrap();
    writer.execute_batch(edit).unwrap();
    writer.execute_batch("COMMIT").unwrap();

    assert_eq!(
        legacy_content_revision(&database),
        legacy,
        "fixture must reproduce the old persisted-token collision"
    );
    let next = authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    // Model equal timestamps exactly as the existing main-database regression:
    // retain the old commit/byte evidence but normalize every native DB/WAL
    // state field to its current value. Only committed-view evidence can differ.
    before_commit.database = family.database.capture_state().unwrap();
    before_commit.wal = Some(family.wal.as_ref().unwrap().capture_state().unwrap());
    assert_ne!(before_commit.revision_token(), *next.revision());
    assert!(family
        .revalidate_revision(&before_commit)
        .unwrap_err()
        .is_source_changed());
    assert_ne!(next.revision(), prior.revision());
    assert!(prior.revalidate().is_err());
    let snapshot = authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    assert!(snapshot.admitted_revision_is_replay_safe());
    assert_eq!(snapshot.evidence().physical_revision(), next.revision());
    let values = read_values(&snapshot);
    assert_eq!(values.len(), 130);
    assert_eq!(values[0], "edited");
    snapshot.finish().unwrap();
    next.revalidate().unwrap();
}

// Frozen released algorithm, used only to prove migration and the WAL collision.
fn legacy_content_revision(database: &Path) -> [u8; 32] {
    fn component(path: &Path) -> (u64, [u8; 32]) {
        let bytes = fs::read(path).unwrap();
        let length = bytes.len() as u64;
        let edge = bytes.len().min(64);
        let mut hash = Sha256::new();
        hash.update(length.to_le_bytes());
        hash.update(&bytes[..edge]);
        hash.update(&bytes[bytes.len() - edge..]);
        (length, hash.finalize().into())
    }
    let (length, token) = component(database);
    let mut hash = Sha256::new();
    hash.update(b"ctx-stock-sqlite-snapshot-v2\0content-revision\0");
    hash.update(length.to_le_bytes());
    hash.update(token);
    let wal = database.with_file_name("provider.sqlite-wal");
    if wal.exists() && fs::metadata(&wal).unwrap().len() != 0 {
        let (length, token) = component(&wal);
        hash.update([1]);
        hash.update(length.to_le_bytes());
        hash.update(token);
    } else {
        hash.update([0]);
    }
    hash.finalize().into()
}

#[cfg(target_os = "linux")]
#[test]
fn physical_replay_fence_rejects_committed_wal_mutation() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let data_root = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    let authority = retain_parent_in_data_root(data_root.path(), temp.path());
    let fence = authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();

    writer
        .execute("INSERT INTO messages (body) VALUES ('later')", [])
        .unwrap();

    assert!(matches!(
        fence.revalidate(),
        Err(SqliteSourceAccessError::SourceChanged)
    ));
    assert_eq!(authority.snapshot_counters(), Default::default());
    assert_eq!(staging_entries(data_root.path()), 0);
}

#[test]
fn physical_replay_fence_rejects_database_leaf_and_parent_replacement() {
    let data_root = crate::test_support_paths::tempdir().unwrap();
    let leaf_root = crate::test_support_paths::tempdir().unwrap();
    let database = leaf_root.path().join("provider.sqlite");
    create_database(&database, "first");
    let authority = retain_parent_in_data_root(data_root.path(), leaf_root.path());
    let fence = authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    fs::rename(&database, leaf_root.path().join("retired.sqlite")).unwrap();
    create_database(&database, "first");
    assert!(matches!(
        fence.revalidate(),
        Err(SqliteSourceAccessError::SourceChanged)
    ));

    let parent_root = crate::test_support_paths::tempdir().unwrap();
    let parent = parent_root.path().join("source");
    fs::create_dir(&parent).unwrap();
    create_database(&parent.join("provider.sqlite"), "first");
    let authority = retain_parent_in_data_root(data_root.path(), &parent);
    let fence = authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    fs::rename(&parent, parent_root.path().join("retired-source")).unwrap();
    fs::create_dir(&parent).unwrap();
    create_database(&parent.join("provider.sqlite"), "first");
    assert!(matches!(
        fence.revalidate(),
        Err(SqliteSourceAccessError::SourceChanged)
    ));
}
