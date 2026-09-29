use super::*;
use ctx_history_index_generation::{ManagedUnlinkStage, ManagedUnlinkTestGuard};
use std::os::unix::fs::MetadataExt as _;

fn hardlink_clones() -> CloneTestHookGuard {
    CloneTestHookGuard::set(
        CloneTestOptions {
            force_reflink_fallback: true,
            ..CloneTestOptions::default()
        },
        |_, _| Ok(()),
    )
}

fn assert_metadata_only_readers(root: &Path, active_docs: u64, previous_docs: u64) {
    crate::publication::reset_verification_activity();
    let mut active = VerifiedIndex::open_pinned_with_retained_peer(root).unwrap();
    assert_eq!(active.document_count(), active_docs);
    let previous = active
        .take_retained_generation_peer_for_reader()
        .unwrap()
        .unwrap();
    assert_eq!(previous.document_count(), previous_docs);
    assert_eq!(crate::publication::hashed_artifact_bytes(), 0);
}

#[test]
fn candidate_merge_unlinks_preserve_published_certifications() {
    let (temp, source, _) = published_fixture("merge-unlink.jsonl");
    let oldest_pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let oldest = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let _clones = hardlink_clones();
    const SEGMENTS: u8 = 8;
    for revision in 2..=SEGMENTS {
        append_record(temp.path(), &source, revision).unwrap();
    }
    let published = active_generation_path(temp.path());
    let published_store = active_store_path(temp.path());

    use ctx_history_index_generation::{PublicationIoProbe, PublicationIoProbeGuard};
    use std::sync::{atomic::AtomicUsize, mpsc, Mutex};
    use std::time::Duration;
    let snapshots = Arc::new(AtomicUsize::new(0));
    let unlinks = Arc::new(AtomicUsize::new(0));
    let reader = Arc::new(Mutex::new(None));
    let (snapshot_count, unlink_count, reader_handle) = (
        Arc::clone(&snapshots),
        Arc::clone(&unlinks),
        Arc::clone(&reader),
    );
    let root = temp.path().to_owned();
    let _probe = ManagedUnlinkTestGuard::set(move |stage, _| {
        if stage == ManagedUnlinkStage::ArtifactSnapshot {
            snapshot_count.fetch_add(1, Ordering::SeqCst);
        }
        if stage != ManagedUnlinkStage::AfterUnlink
            || unlink_count.fetch_add(1, Ordering::SeqCst) != 0
        {
            return;
        }
        let root = root.clone();
        let (waiting_tx, waiting_rx) = mpsc::channel();
        *reader_handle.lock().unwrap() = Some(std::thread::spawn(move || {
            let mut waiting_tx = Some(waiting_tx);
            let _probe = PublicationIoProbeGuard::set(move |event| {
                if event == PublicationIoProbe::CertificationWait {
                    if let Some(tx) = waiting_tx.take() {
                        tx.send(()).unwrap();
                    }
                }
                Ok(())
            });
            assert_metadata_only_readers(&root, u64::from(SEGMENTS), u64::from(SEGMENTS - 1));
        }));
        // Observe actual contention while the unlink is visible but its
        // certificate has not yet been refreshed, without a scheduling sleep.
        waiting_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    });

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    // Construct the real candidate and production merge policy. Force its
    // inherited segments through Tantivy's actual merge and GC before commit.
    writer.writer_mut().unwrap();
    let candidate = temp
        .path()
        .join(INDEX_GENERATIONS_DIRECTORY)
        .join(writer.candidate_directory_name.as_ref().unwrap());
    let linked_store = candidate.join(published_store.file_name().unwrap());
    let before = fs::metadata(&published_store).unwrap();
    assert_eq!(before.ino(), fs::metadata(&linked_store).unwrap().ino());
    let segments = writer.index.searchable_segment_ids().unwrap();
    assert!(
        segments.len() >= 2,
        "fixture must really merge inherited segments"
    );
    writer
        .writer_mut()
        .unwrap()
        .merge(&segments)
        .wait()
        .unwrap();
    writer
        .writer_mut()
        .unwrap()
        .garbage_collect_files()
        .wait()
        .unwrap();
    assert!(
        !linked_store.exists(),
        "GC must unlink an inherited shared file"
    );
    let after = fs::metadata(&published_store).unwrap();
    assert_eq!(after.ino(), before.ino());
    assert_eq!(after.nlink() + 1, before.nlink());
    assert_eq!(active_generation_path(temp.path()), published);
    reader.lock().unwrap().take().unwrap().join().unwrap();
    assert_metadata_only_readers(temp.path(), u64::from(SEGMENTS), u64::from(SEGMENTS - 1));
    let deleted = unlinks.load(Ordering::SeqCst);
    let checked = snapshots.load(Ordering::SeqCst);
    assert!(deleted >= usize::from(SEGMENTS), "exercise a many-file GC");
    assert!(checked > deleted);
    assert!(checked <= deleted * 7, "only one candidate and two snapshots per retained certificate: {checked} snapshots / {deleted} unlinks");
    eprintln!(
        "managed unlink metadata: {checked} artifact snapshots / {deleted} shared-file unlinks"
    );

    // The same owned unlink must preserve a generation kept by an older reader.
    let old_index = open_slot_index(temp.path(), oldest_pointer.active()).unwrap();
    let check_oldest = || {
        crate::publication::reset_verification_activity();
        ctx_history_index_generation::verify_physical_integrity_read_only(
            temp.path(),
            oldest_pointer.active(),
            &old_index,
        )
        .unwrap();
        assert_eq!(crate::publication::hashed_artifact_bytes(), 0);
    };
    check_oldest();
    assert_eq!(oldest.document_count(), 1);

    let base = writer.begin_source_append(source.clone()).unwrap().clone();
    let next = SEGMENTS + 1;
    writer
        .add_core_record(document(
            &source,
            u64::from(next),
            "merged candidate append",
        ))
        .unwrap();
    let frontier = base.frontier().unwrap();
    writer
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(&source, next, u64::from(next), u64::from(next) * 10),
                frontier.certified_prefix_bytes(),
                *frontier.certified_prefix_digest(),
            )
            .unwrap(),
        )
        .unwrap();
    writer.commit(|_| true).unwrap();
    assert_metadata_only_readers(temp.path(), u64::from(next), u64::from(SEGMENTS));
    check_oldest();
}

fn tamper_preserving_length_mode_and_mtime(path: &Path) {
    let mut bytes = fs::read(path).unwrap();
    bytes[0] ^= 0x5a;
    with_temporarily_writable(path, || overwrite_same_size_and_restore_mtime(path, &bytes))
        .unwrap();
}

#[test]
fn candidate_unlink_does_not_rebind_preexisting_tamper_or_external_alias() {
    use tantivy::directory::Directory as _;
    for alteration in ["target_tamper", "other_tamper", "external_alias"] {
        let (temp, _, _) = published_fixture("unlink-integrity.jsonl");
        let _clones = hardlink_clones();
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        writer.writer_mut().unwrap();
        let store = active_store_path(temp.path());
        let target = store.file_name().unwrap();
        match alteration {
            "external_alias" => fs::hard_link(&store, temp.path().join("unowned.store")).unwrap(),
            "target_tamper" => {
                tamper_preserving_length_mode_and_mtime(&store);
            }
            _ => {
                let other = fs::read_dir(active_generation_path(temp.path()))
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .find(|path| path.extension().is_some_and(|ext| ext == "term"))
                    .unwrap();
                tamper_preserving_length_mode_and_mtime(&other);
            }
        }
        // Directly exercise the same directory deletion callback as GC. The
        // candidate is discarded; corrupt input must never become a cache hit.
        writer.index.directory().delete(Path::new(target)).unwrap();
        let pointer = load_active_generation_pointer(temp.path())
            .unwrap()
            .unwrap();
        let index = open_slot_index(temp.path(), pointer.active()).unwrap();
        assert!(matches!(
            ctx_history_index_generation::verify_physical_integrity_read_only(
                temp.path(),
                pointer.active(),
                &index,
            ),
            Err(ctx_history_index_generation::GenerationError::ChecksumMismatch)
        ));
    }
}
