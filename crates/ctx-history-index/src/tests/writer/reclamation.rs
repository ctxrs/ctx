#[test]
fn writer_open_reclaims_unreferenced_and_quarantined_manifests() {
    let temp = tempdir().unwrap();
    let source = source("manifest-retention.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "visible generation"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    let receipt = initial.commit(|_| true).unwrap();

    let directory = temp.path().join(MANIFEST_DIRECTORY);
    let stale_generation = "11".repeat(32);
    let stale = directory.join(format!("{stale_generation}.json"));
    let quarantine = directory.join(format!(".{stale_generation}.corrupt-test"));
    let obsolete_integrity =
        directory.join("generation-0123456789abcdef0123456789abcdef.integrity.json");
    let uppercase_integrity =
        directory.join("generation-FEDCBA9876543210FEDCBA9876543210.integrity.json");
    let sha256_integrity = directory.join(format!(
        "generation-{}.integrity.json",
        receipt.generation_id
    ));
    let unrelated = directory.join("operator-note.txt");
    fs::write(&stale, b"orphaned precommit manifest").unwrap();
    fs::write(&quarantine, b"quarantined collision").unwrap();
    fs::write(&obsolete_integrity, b"obsolete receipt-era integrity state").unwrap();
    fs::write(
        &uppercase_integrity,
        b"not an actual historical writer shape",
    )
    .unwrap();
    fs::write(&sha256_integrity, b"not a legacy UUID receipt").unwrap();
    fs::write(&unrelated, b"not managed by ctx manifest retention").unwrap();
    assert_ne!(
        fs::read(&obsolete_integrity).unwrap(),
        fs::read(&uppercase_integrity).unwrap()
    );

    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert_eq!(
        writer.base_manifest().unwrap().generation_id().unwrap(),
        receipt.generation_id
    );
    assert!(!stale.exists());
    assert!(!quarantine.exists());
    assert!(!obsolete_integrity.exists());
    assert!(uppercase_integrity.exists());
    assert!(sha256_integrity.exists());
    assert!(unrelated.exists());
    assert!(manifest_path(temp.path(), &receipt.generation_id).exists());
}

#[cfg(unix)]
#[test]
fn writer_open_repairs_legacy_flat_delta_ancestor_before_materialization() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempdir().unwrap();
    let source = source("legacy-flat-delta-ancestor.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "base generation"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    let base = initial.commit(|_| true).unwrap();
    let pinned_base = VerifiedIndex::open(temp.path()).unwrap();

    let mut successor = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    successor.begin_source(source.clone()).unwrap();
    successor
        .add_core_record(document(&source, 2, "delta generation"))
        .unwrap();
    successor
        .certify_source(certificate(&source, 2, 1))
        .unwrap();
    let delta = successor.commit(|_| true).unwrap();
    let delta_bytes = fs::read(manifest_path(temp.path(), &delta.generation_id)).unwrap();
    let delta_json: serde_json::Value = serde_json::from_slice(&delta_bytes).unwrap();
    assert_eq!(delta_json["storage_format"], "ctx-manifest-flat-delta-v1");
    assert_eq!(delta_json["base_generation_id"], base.generation_id);

    let base_manifest = manifest_path(temp.path(), &base.generation_id);
    fs::set_permissions(&base_manifest, fs::Permissions::from_mode(0o664)).unwrap();
    let reopened = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();

    assert_eq!(
        fs::metadata(&base_manifest).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        reopened.base_generation_id(),
        Some(delta.generation_id.as_str())
    );
    assert_eq!(pinned_base.generation_id(), base.generation_id);
}

#[cfg(unix)]
#[test]
fn permission_repair_through_noncanonical_root_evicts_canonical_cache_entry() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir_in(".").unwrap();
    let root = PathBuf::from(temp.path().file_name().unwrap());
    assert!(root.is_relative());
    let canonical_root = root.canonicalize().unwrap();
    assert_eq!(canonical_root, temp.path().canonicalize().unwrap());
    assert_ne!(root, canonical_root);
    let source = source("noncanonical-repair-root.jsonl");
    let mut initial = GenerationWriter::open(&root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "cached generation"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    let receipt = initial.commit(|_| true).unwrap();
    let pinned = VerifiedIndex::open(&root).unwrap();

    let manifest = manifest_path(&root, &receipt.generation_id);
    fs::set_permissions(&manifest, fs::Permissions::from_mode(0o664)).unwrap();
    ensure_generation_control_state_private(&root).unwrap();

    let reopened = VerifiedIndex::open(&root).unwrap();
    assert_eq!(reopened.generation_id(), receipt.generation_id);
    assert_eq!(pinned.generation_id(), receipt.generation_id);
    assert_eq!(
        fs::metadata(&manifest).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn writer_exposes_the_base_manifest_captured_under_its_lock() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert!(first.base_manifest().is_none());
    first.begin_source(source.clone()).unwrap();
    first.add_core_record(document(&source, 1, "base")).unwrap();
    first.certify_source(certificate(&source, 1, 1)).unwrap();
    let receipt = first.commit(|_| true).unwrap();

    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = writer.base_manifest().unwrap();
    assert_eq!(base.generation_id().unwrap(), receipt.generation_id);
    assert_eq!(base.sources.len(), 1);
    assert_eq!(base.sources[0].observation().source(), &source);

    let error = match GenerationWriter::open(temp.path(), WriterOptions::default()) {
        Ok(_) => panic!("competing writer unexpectedly acquired the writer lock"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        IndexError::Tantivy(tantivy::TantivyError::LockFailure(_, _))
    ));
    assert_eq!(
        writer.base_manifest().unwrap().generation_id().unwrap(),
        receipt.generation_id
    );
}

#[test]
fn orphaned_inactive_generation_is_reclaimed_before_exact_noop_without_index_writer() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "stable generation"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    let initial_receipt = initial.commit(|_| true).unwrap();
    let pinned = VerifiedIndex::open(temp.path()).unwrap();

    let orphan_directory = temp
        .path()
        .join(INDEX_GENERATIONS_DIRECTORY)
        .join("generation-00000000000000000000000000000000");
    fs::create_dir(&orphan_directory).unwrap();
    let orphan_path = orphan_directory.join("abandoned.store");
    fs::write(&orphan_path, b"abandoned candidate segment").unwrap();
    assert!(orphan_path.is_file());

    let mut replay = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert!(
        !orphan_path.exists(),
        "preflight recovery left an orphaned managed file"
    );
    let constructions = std::sync::Arc::clone(&replay.index_writer_constructions);
    let inventory = complete_inventory(&source, 1, vec![source.clone()]);
    replay
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    stage_exact_replay(&mut replay, &source);
    let receipt = replay
        .commit_with_complete_inventory_revalidation(|_| true, |current| current == &inventory)
        .unwrap();

    assert_eq!(
        constructions.load(Ordering::SeqCst),
        0,
        "orphan recovery plus exact replay must not construct IndexWriter"
    );
    assert_eq!(receipt.generation_id, initial_receipt.generation_id);
    assert_eq!(receipt.opstamp, initial_receipt.opstamp);
    assert_eq!(pinned.generation_id(), initial_receipt.generation_id);
    assert_eq!(pinned.count_term("stable").unwrap(), 1);
}

#[test]
fn inactive_generation_reclamation_revalidates_retained_identity_before_deletion() {
    use crate::publication::{ReclamationStage, ReclamationTestHookGuard};

    let temp = tempdir().unwrap();
    let source = source("reclamation-race.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "stable authority"))
        .unwrap();
    initial.certify_source(certificate(&source, 1, 1)).unwrap();
    initial.commit(|_| true).unwrap();

    let orphan = temp
        .path()
        .join(INDEX_GENERATIONS_DIRECTORY)
        .join("generation-00000000000000000000000000000000");
    let displaced = temp.path().join("retained-reclamation-candidate");
    fs::create_dir(&orphan).unwrap();
    fs::write(orphan.join("original-sentinel"), b"retained object").unwrap();
    let orphan_for_hook = orphan.clone();
    let displaced_for_hook = displaced.clone();
    let mut replaced = false;
    let hook = ReclamationTestHookGuard::set(move |stage, path| {
        if stage == ReclamationStage::AfterCandidateRetained && path == orphan_for_hook && !replaced
        {
            fs::rename(&orphan_for_hook, &displaced_for_hook)?;
            fs::create_dir(&orphan_for_hook)?;
            fs::write(
                orphan_for_hook.join("replacement-sentinel"),
                b"must survive reclamation",
            )?;
            replaced = true;
        }
        Ok(())
    });

    assert!(matches!(
        GenerationWriter::open(temp.path(), WriterOptions::default()),
        Err(IndexError::ConcurrentGenerationChange)
    ));
    assert_eq!(
        fs::read(orphan.join("replacement-sentinel")).unwrap(),
        b"must survive reclamation"
    );
    assert_eq!(
        VerifiedIndex::open(temp.path())
            .unwrap()
            .count_term("authority")
            .unwrap(),
        1
    );

    drop(hook);
    fs::remove_dir_all(&orphan).unwrap();
    fs::rename(&displaced, &orphan).unwrap();
    drop(
        GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap(),
    );
    assert!(!orphan.exists());
}

#[test]
fn post_publication_mutation_fails_exact_noop_then_forces_fresh_rebuild() {
    let temp = tempdir().unwrap();
    let source = source("scrub-rebuild.jsonl");
    let certificate = appendable_certificate(&source, 1, 1, 10);
    let inventory = complete_inventory(&source, 1, vec![source.clone()]);

    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "stable searchable body"))
        .unwrap();
    initial.certify_source(certificate.clone()).unwrap();
    let initial_receipt = initial.commit(|_| true).unwrap();
    let original_generation_path = active_generation_path(temp.path());

    let mut noop = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert!(noop.base_manifest().is_some());

    // An omissive managed-file receipt must not hide corruption in an active
    // segment component referenced by meta.json.
    omit_managed_and_corrupt_body_projection(&original_generation_path);
    noop.certify_complete_inventory(inventory.clone()).unwrap();
    stage_exact_replay(&mut noop, &source);
    assert!(matches!(
        noop.commit_with_complete_inventory_revalidation(
            |_| true,
            |current| current == &inventory,
        ),
        Err(IndexError::ActiveGenerationNeedsRebuild { generation_id, .. })
            if generation_id == initial_receipt.generation_id
    ));
    assert_eq!(
        active_generation_path(temp.path()),
        original_generation_path
    );
    assert!(temp
        .path()
        .join("active-generation-rebuild-required.json")
        .is_file());

    let mut rebuild = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert!(
        rebuild.base_manifest().is_none(),
        "a marked physical generation must not be exposed as reusable base state"
    );
    rebuild
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    rebuild.begin_source(source.clone()).unwrap();
    rebuild
        .add_core_record(document(&source, 1, "stable searchable body"))
        .unwrap();
    rebuild.certify_source(certificate.clone()).unwrap();
    let rebuilt = rebuild
        .commit_with_complete_inventory_revalidation(|_| true, |current| current == &inventory)
        .unwrap();

    let rebuilt_generation_path = active_generation_path(temp.path());
    assert_ne!(rebuilt_generation_path, original_generation_path);
    assert!(!original_generation_path.exists());
    assert_eq!(rebuilt.generation_id, initial_receipt.generation_id);
    assert_eq!(
        VerifiedIndex::open(temp.path())
            .unwrap()
            .count_term("searchable")
            .unwrap(),
        1
    );

    let mut second_noop = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let constructions = Arc::clone(&second_noop.index_writer_constructions);
    second_noop
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    stage_exact_replay(&mut second_noop, &source);
    second_noop
        .commit_with_complete_inventory_revalidation(|_| true, |current| current == &inventory)
        .unwrap();
    assert_eq!(constructions.load(Ordering::SeqCst), 0);
}

#[test]
fn abandoned_publication_reclamation_does_not_construct_index_writer() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "base"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    initial.commit(|_| true).unwrap();

    let root_abandoned = temp
        .path()
        .join(".ctx-tantivy-atomic-0123456789abcdef0123456789abcdef.tmp");
    let manifest_abandoned = temp
        .path()
        .join(MANIFEST_DIRECTORY)
        .join(".ctx-tantivy-atomic-fedcba9876543210fedcba9876543210.tmp");
    fs::write(&root_abandoned, b"abandoned root publication").unwrap();
    fs::write(&manifest_abandoned, b"abandoned manifest publication").unwrap();

    let mut replay = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert!(!root_abandoned.exists());
    assert!(!manifest_abandoned.exists());
    let constructions = std::sync::Arc::clone(&replay.index_writer_constructions);
    let inventory = complete_inventory(&source, 1, vec![source.clone()]);
    replay
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    stage_exact_replay(&mut replay, &source);
    replay
        .commit_with_complete_inventory_revalidation(|_| true, |current| current == &inventory)
        .unwrap();
    assert_eq!(
        constructions.load(Ordering::SeqCst),
        0,
        "preflight reclamation must not construct Tantivy IndexWriter"
    );
}

#[test]
fn root_writer_lock_closes_the_lazy_writer_handoff_gap() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "base"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    initial.commit(|_| true).unwrap();

    let mut stale = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = stale.begin_source_append(source.clone()).unwrap().clone();
    let competing_root = temp.path().to_path_buf();
    stale.before_writer_handoff = Some(Box::new(move || {
        let error = match GenerationWriter::open(&competing_root, WriterOptions::default()) {
            Ok(_) => panic!("competing writer acquired the root publication lock"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            IndexError::Tantivy(tantivy::TantivyError::LockFailure(_, _))
        ));
    }));

    stale
        .add_core_record(document(&source, 2, "serialized delta"))
        .unwrap();
    stale
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
    stale.commit(|_| true).unwrap();

    let current = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(current.count_term("serialized").unwrap(), 1);
    assert_eq!(current.document_count(), 2);
}

#[test]
fn lazy_writer_handoff_retries_a_short_lived_inherited_lock() {
    let temp = tempdir().unwrap();
    let source = source("inherited-lock.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "base"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    initial.commit(|_| true).unwrap();

    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = append.begin_source_append(source.clone()).unwrap().clone();
    let release_thread = Arc::new(std::sync::Mutex::new(None));
    let release_thread_for_hook = Arc::clone(&release_thread);
    let root = temp.path().to_path_buf();
    append.before_writer_handoff = Some(Box::new(move || {
        let directory = DurableMmapDirectory::open(&root).unwrap();
        let inherited = directory.acquire_lock(&INDEX_WRITER_LOCK).unwrap();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(40));
            drop(inherited);
        });
        *release_thread_for_hook.lock().unwrap() = Some(thread);
    }));

    append
        .add_core_record(document(&source, 2, "delta after inherited lock"))
        .unwrap();
    if let Some(thread) = release_thread.lock().unwrap().take() {
        thread.join().unwrap();
    }
    let proof = CertifiedSourceAppend::certify(
        &base,
        appendable_certificate(&source, 2, 2, 20),
        10,
        [1; 32],
    )
    .unwrap();
    append.certify_source_append(proof).unwrap();
    append.commit(|_| true).unwrap();

    let verified = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(verified.count_term("inherited").unwrap(), 1);
    assert_eq!(verified.document_count(), 2);
}

#[test]
fn writer_rejects_a_nonempty_payloadless_index() {
    let temp = tempdir().unwrap();
    let source = source("session.jsonl");
    let mut first = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    first.begin_source(source.clone()).unwrap();
    first.add_core_record(document(&source, 1, "body")).unwrap();
    first.certify_source(certificate(&source, 1, 1)).unwrap();
    first.commit(|_| true).unwrap();

    let directory = DurableMmapDirectory::open(active_generation_path(temp.path())).unwrap();
    let index = Index::open(directory.clone()).unwrap();
    let mut metas = index.load_metas().unwrap();
    metas.payload = None;
    directory
        .atomic_write(Path::new("meta.json"), &serde_json::to_vec(&metas).unwrap())
        .unwrap();

    let error = match GenerationWriter::open(temp.path(), WriterOptions::default()) {
        Ok(_) => panic!("nonempty payloadless index unexpectedly opened for writing"),
        Err(error) => error,
    };
    assert!(matches!(error, IndexError::UnboundIndexState));
}
