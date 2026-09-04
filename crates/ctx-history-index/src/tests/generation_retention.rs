use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use ctx_history_index_generation::acquire_retained_generation_read_lease;

use super::*;

fn publish(root: &Path, source: &SourceKey, revision: u8, body: &str) -> CommitReceipt {
    let mut writer = GenerationWriter::open(root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(document(source, 1, body)).unwrap();
    writer
        .certify_source(appendable_certificate(source, revision, 1, 10))
        .unwrap();
    writer.commit(|_| true).unwrap()
}

#[test]
fn retained_generation_peer_is_limited_to_the_current_pointer_pair() {
    let temp = tempdir().unwrap();
    let source = source("retained-generation-peer.jsonl");
    let first = publish(temp.path(), &source, 1, "first peer");
    let mut first_reader =
        VerifiedIndex::open_pinned_generation_with_retained_peer(temp.path(), &first.generation_id)
            .unwrap();
    assert!(first_reader
        .take_retained_generation_peer_for_reader()
        .unwrap()
        .is_none());
    drop(first_reader);

    let second = publish(temp.path(), &source, 2, "second peer");
    let mut second_reader = VerifiedIndex::open_pinned_generation_with_retained_peer(
        temp.path(),
        &second.generation_id,
    )
    .unwrap();
    let previous = second_reader
        .take_retained_generation_peer_for_reader()
        .unwrap()
        .unwrap();
    assert_eq!(previous.generation_id(), first.generation_id);
    let mut first_reader =
        VerifiedIndex::open_pinned_generation_with_retained_peer(temp.path(), &first.generation_id)
            .unwrap();
    let active = first_reader
        .take_retained_generation_peer_for_reader()
        .unwrap()
        .unwrap();
    assert_eq!(active.generation_id(), second.generation_id);
    drop((active, first_reader, previous, second_reader));

    let third = publish(temp.path(), &source, 3, "third peer");
    assert!(matches!(
        VerifiedIndex::open_pinned_generation(temp.path(), &first.generation_id),
        Err(IndexError::PinnedGenerationNotRetained { .. })
    ));
    let mut second_reader = VerifiedIndex::open_pinned_generation_with_retained_peer(
        temp.path(),
        &second.generation_id,
    )
    .unwrap();
    let active = second_reader
        .take_retained_generation_peer_for_reader()
        .unwrap()
        .unwrap();
    assert_eq!(active.generation_id(), third.generation_id);
}

#[test]
fn one_durable_lease_retains_an_exact_old_generation_without_changing_peer_slots() {
    let temp = tempdir().unwrap();
    let source = source("generation-retention-lease.jsonl");
    let first = publish(temp.path(), &source, 1, "leased first");
    let lease = acquire_generation_retention_lease(
        temp.path(),
        &first.generation_id,
        "pro_core_finalization",
        &"a".repeat(64),
    )
    .unwrap();

    let second = publish(temp.path(), &source, 2, "second");
    let third = publish(temp.path(), &source, 3, "third");
    let fourth = publish(temp.path(), &source, 4, "fourth");
    assert_eq!(lease.generation_id(), first.generation_id);
    assert_eq!(
        load_generation_retention_lease(temp.path()).unwrap(),
        Some(lease.clone())
    );

    let mut leased =
        VerifiedIndex::open_pinned_generation(temp.path(), &first.generation_id).unwrap();
    assert_eq!(leased.count_term("leased").unwrap(), 1);
    assert_eq!(leased.count_term("fourth").unwrap(), 0);
    assert!(leased
        .take_retained_generation_peer_for_reader()
        .unwrap()
        .is_none());
    let fourth_reader =
        VerifiedIndex::open_pinned_generation(temp.path(), &fourth.generation_id).unwrap();
    assert_eq!(generation_directories(temp.path()).len(), 3);
    assert_ne!(second.generation_id, third.generation_id);

    drop((fourth_reader, leased));
    let allocated_before_release = allocated_bytes(temp.path());
    let release_started = Instant::now();
    assert!(release_generation_retention_lease(temp.path(), &lease).unwrap());
    let release_elapsed = release_started.elapsed();
    let allocated_after_release = allocated_bytes(temp.path());
    eprintln!(
        "immediate durable-lease reclamation: allocated_bytes_before={allocated_before_release} allocated_bytes_after={allocated_after_release} elapsed_ms={}",
        release_elapsed.as_millis(),
    );
    #[cfg(unix)]
    assert!(allocated_after_release < allocated_before_release);
    assert_eq!(generation_directories(temp.path()).len(), 2);
    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    for generation_id in [
        pointer.active().generation_id(),
        pointer.previous().unwrap().generation_id(),
    ] {
        assert!(VerifiedIndex::open_pinned_generation(temp.path(), generation_id).is_ok());
    }
    assert!(matches!(
        VerifiedIndex::open_pinned_generation(temp.path(), &first.generation_id),
        Err(IndexError::PinnedGenerationNotRetained { .. })
    ));
    assert_eq!(generation_directories(temp.path()).len(), 2);
}

#[test]
fn candidate_certification_accepts_aliases_held_by_a_process_reader() {
    let temp = tempdir().unwrap();
    let first = publish(
        temp.path(),
        &source("process-reader-first.jsonl"),
        1,
        "first",
    );
    let lease = acquire_generation_retention_lease(
        temp.path(),
        &first.generation_id,
        "process_reader_test",
        &"c".repeat(64),
    )
    .unwrap();
    publish(
        temp.path(),
        &source("process-reader-second.jsonl"),
        2,
        "second",
    );
    publish(
        temp.path(),
        &source("process-reader-third.jsonl"),
        3,
        "third",
    );
    let process_lease = acquire_retained_generation_read_lease(temp.path(), &lease).unwrap();
    assert!(release_generation_retention_lease(temp.path(), &lease).unwrap());
    assert_eq!(generation_directories(temp.path()).len(), 3);

    publish(
        temp.path(),
        &source("process-reader-fourth.jsonl"),
        4,
        "fourth",
    );
    assert_eq!(generation_directories(temp.path()).len(), 3);
    drop(process_lease);
    publish(
        temp.path(),
        &source("process-reader-fifth.jsonl"),
        5,
        "fifth",
    );
    assert_eq!(generation_directories(temp.path()).len(), 2);
}

#[test]
fn ordinary_reader_does_not_retain_the_previous_generation() {
    let temp = tempdir().unwrap();
    let source = source("ordinary-reader-does-not-retain-previous.jsonl");
    let first = publish(temp.path(), &source, 1, "first ordinary reader");
    let second = publish(temp.path(), &source, 2, "second ordinary reader");
    let first_path = temp.path().join(INDEX_GENERATIONS_DIRECTORY).join(
        load_active_generation_pointer(temp.path())
            .unwrap()
            .unwrap()
            .previous()
            .unwrap()
            .directory(),
    );
    let mut ordinary = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert!(ordinary
        .take_retained_generation_peer_for_reader()
        .unwrap()
        .is_none());

    let third = publish(temp.path(), &source, 3, "third ordinary reader");

    assert_eq!(ordinary.generation_id(), second.generation_id);
    assert_eq!(ordinary.count_term("second").unwrap(), 1);
    assert!(!first_path.exists());
    assert!(matches!(
        VerifiedIndex::open_pinned_generation(temp.path(), &first.generation_id),
        Err(IndexError::PinnedGenerationNotRetained { .. })
    ));
    assert_eq!(ordinary.generation_id(), second.generation_id);
    assert_eq!(
        third.generation_id,
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .generation_id()
    );
}

#[test]
fn generation_retention_lease_is_single_owner_private_and_fail_closed() {
    let temp = tempdir().unwrap();
    let first = publish(
        temp.path(),
        &source("generation-retention-lease-bound.jsonl"),
        1,
        "first",
    );
    let lease = acquire_generation_retention_lease(
        temp.path(),
        &first.generation_id,
        "pro_core_finalization",
        &"a".repeat(64),
    )
    .unwrap();
    let replay = acquire_generation_retention_lease(
        temp.path(),
        &first.generation_id,
        "pro_core_finalization",
        &"a".repeat(64),
    )
    .unwrap();
    assert_eq!(replay, lease);
    assert!(matches!(
        acquire_generation_retention_lease(
            temp.path(),
            &first.generation_id,
            "foreign_consumer",
            &"b".repeat(64),
        ),
        Err(IndexError::GenerationRetentionLeaseConflict { .. })
    ));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(temp.path().join("generation-retention-lease.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o177, 0);
    }

    fs::write(
        temp.path().join("generation-retention-lease.json"),
        b"{not-canonical",
    )
    .unwrap();
    assert!(matches!(
        GenerationWriter::open(temp.path(), WriterOptions::default()),
        Err(IndexError::InvalidGenerationRetentionLease)
    ));
    assert!(matches!(
        VerifiedIndex::open_pinned_generation(temp.path(), &first.generation_id),
        Err(IndexError::InvalidGenerationRetentionLease)
    ));
}

fn generation_directories(root: &Path) -> Vec<PathBuf> {
    let mut directories = fs::read_dir(root.join(INDEX_GENERATIONS_DIRECTORY))
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            entry.file_type().ok()?.is_dir().then(|| entry.path())
        })
        .collect::<Vec<_>>();
    directories.sort();
    directories
}

#[cfg(unix)]
fn allocated_bytes(root: &Path) -> u64 {
    use std::{collections::HashSet, os::unix::fs::MetadataExt as _};

    fn walk(root: &Path, seen: &mut HashSet<(u64, u64)>) -> u64 {
        let metadata = fs::symlink_metadata(root).unwrap();
        let identity = (metadata.dev(), metadata.ino());
        let own_blocks = seen
            .insert(identity)
            .then_some(metadata.blocks().saturating_mul(512))
            .unwrap_or_default();
        if !metadata.is_dir() {
            return own_blocks;
        }
        own_blocks.saturating_add(
            fs::read_dir(root)
                .unwrap()
                .map(|entry| walk(&entry.unwrap().path(), seen))
                .sum(),
        )
    }

    walk(root, &mut HashSet::new())
}

#[cfg(not(unix))]
fn allocated_bytes(_root: &Path) -> u64 {
    0
}
