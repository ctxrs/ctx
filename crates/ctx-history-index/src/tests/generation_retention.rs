use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
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

fn publish_manifest_base(root: &Path, source: &SourceKey) -> PathBuf {
    // Later flat deltas still require this manifest, even after its physical
    // generation is obsolete. The leased delta itself must be reclaimable.
    let base = publish(root, source, 0, "retained manifest base");
    crate::publication::manifest_path(root, &base.generation_id)
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
    let base_manifest = publish_manifest_base(temp.path(), &source);
    let first = publish(temp.path(), &source, 1, "leased first");
    let first_slot = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap()
        .active()
        .clone();
    let first_manifest = crate::publication::manifest_path(temp.path(), &first.generation_id);
    let first_certification =
        crate::publication::certification_file_for_active(temp.path()).unwrap();
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
    assert!(first_manifest.is_file());
    assert!(first_certification.is_file());
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
    assert!(!crate::publication::slot_path(temp.path(), &first_slot).exists());
    assert!(!first_manifest.exists());
    assert!(!first_certification.exists());
    assert!(base_manifest.is_file());
    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    for generation_id in [
        pointer.active().generation_id(),
        pointer.previous().unwrap().generation_id(),
    ] {
        assert!(crate::publication::manifest_path(temp.path(), generation_id).is_file());
        assert!(VerifiedIndex::open_pinned_generation(temp.path(), generation_id).is_ok());
    }
    assert!(matches!(
        VerifiedIndex::open_pinned_generation(temp.path(), &first.generation_id),
        Err(IndexError::PinnedGenerationNotRetained { .. })
    ));
    assert_eq!(generation_directories(temp.path()).len(), 2);
}

#[test]
fn durable_release_stays_successful_when_best_effort_directory_gc_fails() {
    use crate::publication::{ReclamationStage, ReclamationTestHookGuard};

    let temp = tempdir().unwrap();
    let source = source("generation-retention-release-fault.jsonl");
    let base_manifest = publish_manifest_base(temp.path(), &source);
    let first = publish(temp.path(), &source, 1, "fault retained first");
    let first_slot = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap()
        .active()
        .clone();
    let first_directory = crate::publication::slot_path(temp.path(), &first_slot);
    let first_manifest = crate::publication::manifest_path(temp.path(), &first.generation_id);
    let first_certification =
        crate::publication::certification_file_for_active(temp.path()).unwrap();
    let lease = acquire_generation_retention_lease(
        temp.path(),
        &first.generation_id,
        "pro_core_finalization",
        &"d".repeat(64),
    )
    .unwrap();

    publish(temp.path(), &source, 2, "fault second");
    publish(temp.path(), &source, 3, "fault third");
    publish(temp.path(), &source, 4, "fault fourth");

    let root = temp.path().to_path_buf();
    let injected_directory = first_directory.clone();
    let reached_after_release = Arc::new(AtomicBool::new(false));
    let reached_after_release_for_hook = Arc::clone(&reached_after_release);
    let hook = ReclamationTestHookGuard::set(move |stage, path| {
        if stage == ReclamationStage::AfterCandidateRetained && path == injected_directory {
            assert!(load_generation_retention_lease(&root).unwrap().is_none());
            reached_after_release_for_hook.store(true, Ordering::SeqCst);
            return Err(ctx_history_index_generation::GenerationError::ConcurrentGenerationChange);
        }
        Ok(())
    });

    assert!(release_generation_retention_lease(temp.path(), &lease).unwrap());
    assert!(reached_after_release.load(Ordering::SeqCst));
    assert!(load_generation_retention_lease(temp.path())
        .unwrap()
        .is_none());
    assert!(first_directory.is_dir());
    assert!(!first_manifest.exists());
    assert!(!first_certification.exists());
    assert!(base_manifest.is_file());
    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    assert!(
        VerifiedIndex::open_pinned_generation(temp.path(), pointer.active().generation_id())
            .is_ok()
    );
    assert!(VerifiedIndex::open_pinned_generation(
        temp.path(),
        pointer.previous().unwrap().generation_id()
    )
    .is_ok());

    drop(hook);
    publish(temp.path(), &source, 5, "fault fifth");
    assert!(!first_directory.exists());
}

const RELEASE_CRASH_ROOT: &str = "CTX_RETENTION_RELEASE_CRASH_TEST_ROOT";
const RELEASE_CRASH_EXIT: i32 = 86;

#[test]
fn durable_release_crash_child() {
    let Ok(root) = std::env::var(RELEASE_CRASH_ROOT) else {
        return;
    };
    let root = PathBuf::from(root);
    let lease = load_generation_retention_lease(&root).unwrap().unwrap();
    let root_for_hook = root.clone();
    let _hook = crate::publication::ReclamationTestHookGuard::set(move |_, _| {
        assert!(load_generation_retention_lease(&root_for_hook)
            .unwrap()
            .is_none());
        std::process::exit(RELEASE_CRASH_EXIT);
    });
    release_generation_retention_lease(&root, &lease).unwrap();
    panic!("durable release crash checkpoint was not reached");
}

#[test]
fn crash_after_durable_release_preserves_authority_and_next_writer_reclaims() {
    let temp = tempdir().unwrap();
    let source = source("generation-retention-release-crash.jsonl");
    let base_manifest = publish_manifest_base(temp.path(), &source);
    let first = publish(temp.path(), &source, 1, "crash first");
    let first_certification =
        crate::publication::certification_file_for_active(temp.path()).unwrap();
    let first_manifest = crate::publication::manifest_path(temp.path(), &first.generation_id);
    let lease = acquire_generation_retention_lease(
        temp.path(),
        &first.generation_id,
        "pro_core_finalization",
        &"f".repeat(64),
    )
    .unwrap();
    publish(temp.path(), &source, 2, "crash second");
    let third = publish(temp.path(), &source, 3, "crash third");
    let fourth = publish(temp.path(), &source, 4, "crash fourth");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tests::generation_retention::durable_release_crash_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(RELEASE_CRASH_ROOT, temp.path())
        .status()
        .unwrap();

    assert_eq!(status.code(), Some(RELEASE_CRASH_EXIT));
    assert!(load_generation_retention_lease(temp.path())
        .unwrap()
        .is_none());
    assert!(!release_generation_retention_lease(temp.path(), &lease).unwrap());
    assert_eq!(generation_directories(temp.path()).len(), 3);
    assert!(first_manifest.is_file());
    assert!(first_certification.is_file());
    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    assert_eq!(pointer.active().generation_id(), fourth.generation_id);
    assert_eq!(
        pointer.previous().unwrap().generation_id(),
        third.generation_id
    );
    for (id, term) in [
        (&fourth.generation_id, "fourth"),
        (&third.generation_id, "third"),
    ] {
        assert_eq!(
            VerifiedIndex::open_pinned_generation(temp.path(), id)
                .unwrap()
                .count_term(term)
                .unwrap(),
            1
        );
    }
    publish(temp.path(), &source, 5, "crash fifth");
    assert_eq!(generation_directories(temp.path()).len(), 2);
    assert!(!first_manifest.exists());
    assert!(!first_certification.exists());
    assert!(base_manifest.is_file());
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

#[test]
fn stale_owner_cannot_release_a_remaining_durable_lease() {
    let temp = tempdir().unwrap();
    let source = source("generation-retention-replaced-owner.jsonl");
    let base_manifest = publish_manifest_base(temp.path(), &source);
    let first = publish(temp.path(), &source, 1, "first owner");
    let first_manifest = crate::publication::manifest_path(temp.path(), &first.generation_id);
    let first_certification =
        crate::publication::certification_file_for_active(temp.path()).unwrap();
    let stale = acquire_generation_retention_lease(
        temp.path(),
        &first.generation_id,
        "pro_core_finalization",
        &"a".repeat(64),
    )
    .unwrap();
    assert!(release_generation_retention_lease(temp.path(), &stale).unwrap());
    assert_eq!(generation_directories(temp.path()).len(), 2);
    assert!(first_manifest.is_file());
    assert!(first_certification.is_file());
    assert!(VerifiedIndex::open_pinned(temp.path()).is_ok());
    let remaining = acquire_generation_retention_lease(
        temp.path(),
        &first.generation_id,
        "pro_core_finalization",
        &"b".repeat(64),
    )
    .unwrap();
    publish(temp.path(), &source, 2, "second owner");
    publish(temp.path(), &source, 3, "third owner");
    publish(temp.path(), &source, 4, "fourth owner");
    assert!(matches!(
        release_generation_retention_lease(temp.path(), &stale),
        Err(IndexError::GenerationRetentionLeaseOwnerMismatch)
    ));
    assert_eq!(
        load_generation_retention_lease(temp.path()).unwrap(),
        Some(remaining.clone())
    );
    assert_eq!(generation_directories(temp.path()).len(), 3);
    assert!(first_manifest.is_file());
    assert!(first_certification.is_file());
    assert_eq!(
        VerifiedIndex::open_pinned_generation(temp.path(), &first.generation_id)
            .unwrap()
            .count_term("first")
            .unwrap(),
        1
    );
    assert!(release_generation_retention_lease(temp.path(), &remaining).unwrap());
    assert_eq!(generation_directories(temp.path()).len(), 2);
    assert!(!first_manifest.exists());
    assert!(!first_certification.exists());
    assert!(base_manifest.is_file());
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
        let own_blocks = if seen.insert(identity) {
            metadata.blocks().saturating_mul(512)
        } else {
            0
        };
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
