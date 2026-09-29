use super::*;

fn hardlink_clones() -> CloneTestHookGuard {
    CloneTestHookGuard::set(
        CloneTestOptions {
            force_reflink_fallback: true,
            ..CloneTestOptions::default()
        },
        |_, _| Ok(()),
    )
}

fn staged_append(root: &Path, source: &SourceKey, revision: u8) -> GenerationWriter {
    let mut writer = GenerationWriter::open(root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = writer.begin_source_append(source.clone()).unwrap().clone();
    let frontier = base.frontier().unwrap();
    writer
        .add_core_record(document(
            source,
            u64::from(revision),
            "candidate append body",
        ))
        .unwrap();
    writer
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(
                    source,
                    revision,
                    u64::from(revision),
                    u64::from(revision) * 10,
                ),
                frontier.certified_prefix_bytes(),
                *frontier.certified_prefix_digest(),
            )
            .unwrap(),
        )
        .unwrap();
    writer
}

fn assert_metadata_only_open(root: &Path) {
    crate::publication::reset_verification_activity();
    let mut reader = VerifiedIndex::open_pinned_with_retained_peer(root).unwrap();
    let peer = reader.take_retained_generation_peer_for_reader().unwrap();
    drop((reader, peer));
    assert_eq!(crate::publication::hashed_artifact_bytes(), 0);
}

#[test]
fn obsolete_reader_completion_preserves_alias_authority_until_activation() {
    for release in [
        "after_preflight",
        "before_certification",
        "after_activation",
    ] {
        let (temp, source, _) = published_fixture("reader-completion.jsonl");
        let first_path = active_generation_path(temp.path());
        let reader = VerifiedIndex::open_pinned(temp.path()).unwrap();
        let guard = hardlink_clones();
        append_record(temp.path(), &source, 2).unwrap();
        append_record(temp.path(), &source, 3).unwrap();
        assert!(first_path.is_dir());

        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        let mut reader = Some(reader);
        if release == "after_preflight" {
            drop(reader.take());
        } else if release == "before_certification" {
            let reader = reader.take();
            writer.before_pointer_publication = Some(Box::new(move |_| drop(reader)));
        }
        let base = writer.begin_source_append(source.clone()).unwrap().clone();
        writer
            .add_core_record(document(&source, 4, "new body"))
            .unwrap();
        writer
            .certify_source_append(
                CertifiedSourceAppend::certify(
                    &base,
                    appendable_certificate(&source, 4, 4, 40),
                    30,
                    [3; 32],
                )
                .unwrap(),
            )
            .unwrap();
        let receipt = writer.commit(|_| true).unwrap();
        assert!(crate::publication::candidate_clone_metrics().retained_hardlinked_files > 0);
        assert_eq!(
            VerifiedIndex::open_pinned(temp.path())
                .unwrap()
                .generation_id(),
            receipt.generation_id
        );
        assert_metadata_only_open(temp.path());
        if release == "after_activation" {
            assert!(first_path.is_dir());
            drop(reader);
            drop(GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap());
        }
        assert!(!first_path.exists());
        drop(guard);
    }
}

#[test]
fn managed_clone_and_reclaim_refresh_all_reader_retained_certificates() {
    let (temp, source, _) = published_fixture("multiple-retained-readers.jsonl");
    let first_path = active_generation_path(temp.path());
    let first = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let guard = hardlink_clones();
    append_record(temp.path(), &source, 2).unwrap();
    let second_path = active_generation_path(temp.path());
    let second = VerifiedIndex::open_pinned(temp.path()).unwrap();
    append_record(temp.path(), &source, 3).unwrap();

    let writer = staged_append(temp.path(), &source, 4);
    // Readers arriving after the managed clone, while its candidate is still
    // unpublished, must not hash the unchanged active or previous payload.
    assert_metadata_only_open(temp.path());
    writer.commit(|_| true).unwrap();
    assert_metadata_only_open(temp.path());
    assert!(first_path.is_dir());
    assert!(second_path.is_dir());

    drop(second);
    let writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    assert!(!second_path.exists());
    assert!(first_path.is_dir());
    assert_metadata_only_open(temp.path());
    drop(writer);

    drop(first);
    drop(GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap());
    assert!(!first_path.exists());
    assert_metadata_only_open(temp.path());
    drop(guard);
}

#[test]
fn unowned_alias_added_after_preflight_cannot_be_certified() {
    let (temp, source, _) = published_fixture("unowned-alias-during-publication.jsonl");
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();
    let store = active_store_path(temp.path());
    let guard = hardlink_clones();
    let mut writer = staged_append(temp.path(), &source, 2);
    let rogue = temp
        .path()
        .join(INDEX_GENERATIONS_DIRECTORY)
        .join(format!("generation-{}", "d".repeat(32)));
    writer.before_pointer_switch = Some(Box::new(move |_| {
        fs::create_dir(&rogue).unwrap();
        fs::hard_link(&store, rogue.join(store.file_name().unwrap())).unwrap();
    }));
    assert!(matches!(
        writer.commit(|_| true),
        Err(IndexError::ChecksumMismatch)
    ));
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    drop(guard);
}

#[test]
fn reader_arriving_between_candidate_links_waits_then_hashes_zero_bytes() {
    use ctx_history_index_generation::{PublicationIoProbe, PublicationIoProbeGuard};
    use std::sync::{mpsc, Mutex};
    use std::time::Duration;

    for snapshot in [false, true] {
        let (temp, source, _) = published_fixture("reader-during-clone.jsonl");
        let root = temp.path().to_path_buf();
        let reader = Arc::new(Mutex::new(None));
        let reader_hook = Arc::clone(&reader);
        let mut started = false;
        let guard = CloneTestHookGuard::set(
            CloneTestOptions {
                force_reflink_fallback: true,
                ..CloneTestOptions::default()
            },
            move |stage, relative| {
                if stage != CloneStage::AfterFile
                    || relative.extension().is_none_or(|ext| ext != "store")
                    || started
                {
                    return Ok(());
                }
                started = true;
                let root = root.clone();
                let (waiting_tx, waiting_rx) = mpsc::channel();
                let (finished_tx, finished_rx) = mpsc::channel();
                *reader_hook.lock().unwrap() = Some(std::thread::spawn(move || {
                    let mut waiting_tx = Some(waiting_tx);
                    let _probe = PublicationIoProbeGuard::set(move |event| {
                        if event == PublicationIoProbe::CertificationWait {
                            if let Some(tx) = waiting_tx.take() {
                                tx.send(()).unwrap();
                            }
                        }
                        Ok(())
                    });
                    crate::publication::reset_verification_activity();
                    if snapshot {
                        let pointer = load_active_generation_pointer(&root).unwrap().unwrap();
                        let index = open_slot_index(&root, pointer.active()).unwrap();
                        ctx_history_index_generation::verify_physical_integrity_read_only(
                            &root,
                            pointer.active(),
                            &index,
                        )
                        .unwrap();
                    } else {
                        let reader = VerifiedIndex::open_pinned(&root).unwrap();
                        assert_eq!(reader.count_term("body").unwrap(), 1);
                    }
                    finished_tx.send(()).ok();
                    crate::publication::hashed_artifact_bytes()
                }));
                // This handshake observes actual lock contention, not a sleep
                // hoping the reader gets scheduled inside the clone window.
                waiting_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                assert!(finished_rx.try_recv().is_err());
                Ok(())
            },
        );
        let writer = staged_append(temp.path(), &source, 2);
        let hashed_bytes = reader.lock().unwrap().take().unwrap().join().unwrap();
        assert_eq!(hashed_bytes, 0, "managed links caused a payload rehash");
        writer.commit(|_| true).unwrap();
        drop(guard);
    }
}

#[test]
fn exact_active_and_previous_readers_finish_while_candidate_copy_is_paused() {
    use ctx_history_index_generation::{PublicationIoProbe, PublicationIoProbeGuard};
    use std::sync::mpsc;
    use std::time::Duration;

    let (temp, source, _) = published_fixture("readers-during-copy.jsonl");
    append_record(temp.path(), &source, 2).unwrap();
    let root = temp.path().to_path_buf();
    let checked = std::rc::Rc::new(std::cell::Cell::new(false));
    let hook_checked = std::rc::Rc::clone(&checked);
    let guard = CloneTestHookGuard::set(
        CloneTestOptions {
            force_reflink_fallback: true,
            force_hardlink_fallback: true,
            ..CloneTestOptions::default()
        },
        move |stage, relative| {
            if hook_checked.get()
                || stage != CloneStage::BeforeCopy
                || relative
                    .extension()
                    .is_none_or(|extension| extension != "store")
            {
                return Ok(());
            }
            hook_checked.set(true);
            let root = root.clone();
            let (finished_tx, finished_rx) = mpsc::channel();
            let reader = std::thread::spawn(move || {
                let _probe = PublicationIoProbeGuard::set(|event| {
                    assert_ne!(event, PublicationIoProbe::CertificationWait);
                    Ok(())
                });
                // Both pinned readers must finish before the copy resumes.
                assert_metadata_only_open(&root);
                let pointer = load_active_generation_pointer(&root).unwrap().unwrap();
                assert!(pointer.previous().is_some());
                crate::publication::reset_verification_activity();
                for slot in std::iter::once(pointer.active()).chain(pointer.previous()) {
                    let index = open_slot_index(&root, slot).unwrap();
                    ctx_history_index_generation::verify_physical_integrity_read_only(
                        &root, slot, &index,
                    )
                    .unwrap();
                }
                assert_eq!(crate::publication::hashed_artifact_bytes(), 0);
                finished_tx.send(()).unwrap();
            });
            // The writer cannot leave BeforeCopy until both read APIs finish;
            // this proves independence from copy duration without a large file.
            let finished = finished_rx.recv_timeout(Duration::from_secs(5));
            reader.join().unwrap();
            finished.unwrap();
            Ok(())
        },
    );
    let writer = staged_append(temp.path(), &source, 3);
    let metrics = crate::publication::candidate_clone_metrics();
    assert_eq!(metrics.retained_hardlinked_files, 0);
    assert!(metrics.retained_copied_bytes > 0);
    assert!(checked.get());
    writer.commit(|_| true).unwrap();
    drop(guard);
}

#[test]
fn external_alias_with_older_reader_is_corruption_then_recertifies_after_removal() {
    use std::os::unix::fs::MetadataExt as _;

    let (temp, source, _) = published_fixture("external-alias-recovery.jsonl");
    let old_reader = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let guard = hardlink_clones();
    append_record(temp.path(), &source, 2).unwrap();
    let receipt = append_record(temp.path(), &source, 3).unwrap();
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();
    let store = active_store_path(temp.path());
    let links_before = fs::metadata(&store).unwrap().nlink();
    let external = temp.path().join("external-copy");
    fs::hard_link(&store, &external).unwrap();
    assert_eq!(fs::metadata(&store).unwrap().nlink(), links_before + 1);
    crate::publication::reset_verification_activity();
    assert!(matches!(
        VerifiedIndex::open_pinned(temp.path()),
        Err(IndexError::ChecksumMismatch)
    ));
    assert_eq!(crate::publication::hashed_artifact_bytes(), 0);

    fs::remove_file(external).unwrap();
    assert_eq!(fs::metadata(&store).unwrap().nlink(), links_before);
    let reopened = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_eq!(reopened.generation_id(), receipt.generation_id);
    assert_eq!(reopened.count_term("body").unwrap(), 3);
    assert!(crate::publication::hashed_artifact_bytes() > 0);
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    crate::publication::reset_verification_activity();
    drop(VerifiedIndex::open_pinned(temp.path()).unwrap());
    assert_eq!(crate::publication::hashed_artifact_bytes(), 0);
    drop((old_reader, guard));
}
