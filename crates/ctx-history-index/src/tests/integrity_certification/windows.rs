use super::*;

#[cfg(windows)]
#[test]
fn windows_clone_proof_capture_blocks_destination_substitution() {
    let (temp, source, _) = published_fixture("windows-proof-substitution.jsonl");
    let root = temp.path().to_owned();
    let active = active_generation_path(temp.path());
    let attempted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let blocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed_attempt = Arc::clone(&attempted);
    let observed_block = Arc::clone(&blocked);
    let _portable = PortableCloneTestGuard::set(
        PortableCloneTestOptions::default(),
        move |stage, relative| {
            if stage != PortableCloneStage::AfterCopy
                || relative
                    .extension()
                    .is_none_or(|extension| extension != "store")
                || observed_attempt.swap(true, Ordering::SeqCst)
            {
                return Ok(());
            }
            let target = fs::read_dir(root.join(INDEX_GENERATIONS_DIRECTORY))
                .unwrap()
                .map(|entry| entry.unwrap().path().join(relative))
                .find(|path| path.is_file() && !path.starts_with(&active))
                .expect("portable candidate must contain the copied proof target");
            let replacement = target.with_extension("ctx-proof-substitution");
            let mut bytes = fs::read(&target).unwrap();
            bytes[0] ^= 0x5a;
            let mut replacement_file = fs::File::create(&replacement).unwrap();
            replacement_file.write_all(&bytes).unwrap();
            replacement_file.sync_all().unwrap();
            drop(replacement_file);
            let delete_error = fs::OpenOptions::new()
                .access_mode(DELETE)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .open(&target)
                .unwrap_err();
            assert_eq!(delete_error.raw_os_error(), Some(32));
            let error = durable_atomic_replace_file(&replacement, &target).unwrap_err();
            assert_eq!(error.raw_os_error(), Some(5));
            fs::remove_file(replacement).unwrap();
            observed_block.store(true, Ordering::SeqCst);
            Ok(())
        },
    );

    let successor = append_one_record(temp.path(), &source).unwrap();
    assert!(attempted.load(Ordering::SeqCst));
    assert!(blocked.load(Ordering::SeqCst));
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .generation_id(),
        successor.generation_id
    );
}

#[cfg(windows)]
#[test]
fn windows_retained_writer_blocks_candidate_seal_before_publication() {
    for target in ["segment", "meta.json", ".managed.json"] {
        let (temp, source, baseline) =
            published_fixture(&format!("windows-retained-writer-{target}.jsonl"));
        let target_name = if target == "segment" {
            active_store_path(temp.path())
                .file_name()
                .unwrap()
                .to_owned()
        } else {
            target.into()
        };
        let retained_writer = Arc::new(Mutex::new(None));
        let captured_writer = Arc::clone(&retained_writer);

        let mut append = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        let base = append.begin_source_append(source.clone()).unwrap().clone();
        append.before_pointer_switch = Some(Box::new(move |candidate_path| {
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .open(candidate_path.join(target_name))
                .unwrap();
            *captured_writer.lock().unwrap() = Some(file);
        }));
        append
            .add_core_record(document(&source, 2, "candidate append body"))
            .unwrap();
        append
            .certify_source_append(
                CertifiedSourceAppend::certify(
                    &base,
                    appendable_certificate(&source, 2, 2, 20),
                    10,
                    [1; 32],
                )
                .unwrap(),
            )
            .unwrap();
        let error = append.commit(|_| true).unwrap_err();
        assert!(
            matches!(&error, IndexError::Io(error) if error.raw_os_error() == Some(32)),
            "retained writer for {target} must block the terminal seal: {error}"
        );

        let mut retained_writer = retained_writer.lock().unwrap().take().unwrap();
        retained_writer.seek(SeekFrom::Start(0)).unwrap();
        retained_writer.write_all(b"x").unwrap();
        retained_writer.sync_all().unwrap();
        assert_eq!(
            VerifiedIndex::open_pinned(temp.path())
                .unwrap()
                .generation_id(),
            baseline.generation_id
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_unproven_readonly_file_blocks_commit_before_pointer_publication() {
    let (temp, source, baseline) = published_fixture("windows-unproven-readonly.jsonl");
    let pointer_publication_attempted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed_attempt = Arc::clone(&pointer_publication_attempted);

    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = append.begin_source_append(source.clone()).unwrap().clone();
    append.after_candidate_commit = Some(Box::new(|candidate_path| {
        let path = candidate_path.join("unproven.store");
        let file = fs::File::create(path).unwrap();
        file.sync_all().unwrap();
        let mut permissions = file.metadata().unwrap().permissions();
        permissions.set_readonly(true);
        file.set_permissions(permissions).unwrap();
    }));
    append.before_pointer_publication = Some(Box::new(move |_| {
        observed_attempt.store(true, Ordering::SeqCst);
    }));
    append
        .add_core_record(document(&source, 2, "candidate append body"))
        .unwrap();
    append
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(&source, 2, 2, 20),
                10,
                [1; 32],
            )
            .unwrap(),
        )
        .unwrap();
    let error = append.commit(|_| true).unwrap_err();
    assert!(matches!(
        error,
        IndexError::Io(ref error) if error.kind() == std::io::ErrorKind::PermissionDenied
    ));
    assert!(!pointer_publication_attempted.load(Ordering::SeqCst));
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .generation_id(),
        baseline.generation_id
    );
}

#[cfg(windows)]
#[test]
fn windows_validation_failure_after_terminal_seal_keeps_predecessor() {
    let (temp, source, baseline) = published_fixture("windows-terminal-validation.jsonl");
    let root = temp.path().to_owned();
    let baseline_manifest = manifest_path(&root, &baseline.generation_id);

    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = append.begin_source_append(source.clone()).unwrap().clone();
    append.before_pointer_publication = Some(Box::new(move |_| {
        let candidate_manifest = fs::read_dir(root.join(MANIFEST_DIRECTORY))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.is_file() && path != &baseline_manifest)
            .expect("candidate manifest must be durable before pointer publication");
        let bytes = mismatched_same_size_bytes(&candidate_manifest);
        fs::write(&candidate_manifest, bytes).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(candidate_manifest)
            .unwrap()
            .sync_all()
            .unwrap();
    }));
    append
        .add_core_record(document(&source, 2, "candidate append body"))
        .unwrap();
    append
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(&source, 2, 2, 20),
                10,
                [1; 32],
            )
            .unwrap(),
        )
        .unwrap();
    let error = append.commit(|_| true).unwrap_err();
    assert!(
        matches!(&error, IndexError::ManifestDigestMismatch { .. }),
        "terminal manifest mutation must fail with ManifestDigestMismatch, got {error:?}"
    );
    let predecessor = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_eq!(predecessor.generation_id(), baseline.generation_id);
    assert_eq!(predecessor.count_term("body").unwrap(), 1);
}

#[cfg(windows)]
#[test]
fn windows_successor_seals_allowlisted_files_before_publication() {
    let (temp, source, baseline) = published_fixture("windows-readonly-successor.jsonl");
    let held_reader = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let predecessor_store = active_store_path(temp.path());
    let retained_name = predecessor_store.file_name().unwrap().to_owned();
    assert!(
        fs::metadata(&predecessor_store)
            .unwrap()
            .permissions()
            .readonly(),
        "predecessor segment must be sealed read-only"
    );

    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = append.begin_source_append(source.clone()).unwrap().clone();
    let clone_was_writable = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writable_before_terminal = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed_clone = Arc::clone(&clone_was_writable);
    let observed_terminal = Arc::clone(&writable_before_terminal);
    append.after_candidate_commit = Some(Box::new(move |candidate_path| {
        for candidate_file in publication_allowlisted_files(candidate_path) {
            assert!(candidate_file.is_file());
            assert!(
                !fs::metadata(candidate_file)
                    .unwrap()
                    .permissions()
                    .readonly(),
                "successor candidate files must remain writable through final sync"
            );
        }
        observed_clone.store(true, Ordering::SeqCst);
    }));
    append.before_pointer_publication = Some(Box::new(move |candidate_path| {
        for candidate_file in publication_allowlisted_files(candidate_path) {
            assert!(candidate_file.is_file());
            assert!(
                !fs::metadata(candidate_file)
                    .unwrap()
                    .permissions()
                    .readonly(),
                "candidate files must remain writable until the terminal atomic closure"
            );
        }
        observed_terminal.store(true, Ordering::SeqCst);
    }));
    append
        .add_core_record(document(&source, 2, "candidate append body"))
        .unwrap();
    append
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(&source, 2, 2, 20),
                10,
                [1; 32],
            )
            .unwrap(),
        )
        .unwrap();
    let successor = append.commit(|_| true).unwrap();
    assert!(clone_was_writable.load(Ordering::SeqCst));
    assert!(writable_before_terminal.load(Ordering::SeqCst));
    assert_ne!(successor.generation_id, baseline.generation_id);
    let successor_path = active_generation_path(temp.path());
    let retained = successor_path.join(retained_name);
    assert!(
        retained.is_file(),
        "successor must retain predecessor segment"
    );
    assert!(
        fs::metadata(retained).unwrap().permissions().readonly(),
        "retained successor segment must preserve its read-only seal"
    );
    for active_file in publication_allowlisted_files(&successor_path) {
        assert!(
            fs::metadata(&active_file).unwrap().permissions().readonly(),
            "active successor file must remain sealed: {}",
            active_file.display()
        );
    }
    assert_certification_is_bounded(
        &crate::publication::certification_file_for_active(temp.path()).unwrap(),
    );
    assert_eq!(held_reader.count_term("body").unwrap(), 1);
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .count_term("body")
            .unwrap(),
        2
    );
}
