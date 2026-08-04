use super::*;

#[test]
fn identical_staging_revalidates_active_checksum_after_terminal_callback() {
    let temp = tempdir().unwrap();
    let source = source("identical-terminal-corruption.jsonl");
    let certificate = certificate(&source, 1, 1);
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "stable logical row"))
        .unwrap();
    initial.certify_source(certificate.clone()).unwrap();
    let baseline = initial.commit(|_| true).unwrap();
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();
    let active_path = active_generation_path(temp.path());

    let mut staged = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    staged.begin_source(source.clone()).unwrap();
    staged
        .add_core_record(document(&source, 1, "stable logical row"))
        .unwrap();
    staged.certify_source(certificate.clone()).unwrap();
    let mut corrupted = false;
    let error = staged
        .commit(|target| {
            assert!(matches!(
                target,
                RevalidationTarget::Source(current) if current == &certificate
            ));
            omit_managed_and_corrupt_body_projection(&active_path);
            corrupted = true;
            true
        })
        .unwrap_err();

    assert!(corrupted);
    assert!(matches!(
        error,
        IndexError::ActiveGenerationNeedsRebuild { generation_id, .. }
            if generation_id == baseline.generation_id
    ));
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert!(temp
        .path()
        .join("active-generation-rebuild-required.json")
        .is_file());
}

#[test]
fn publication_activity_keeps_cold_scrub_and_uses_incremental_identity_audit() {
    const RETAINED_DOCUMENTS: u64 = 32;

    let cold = tempdir().unwrap();
    let cold_source = source("cold-activity.jsonl");
    let mut initial = GenerationWriter::open(cold.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(cold_source.clone()).unwrap();
    for sequence in 1..=RETAINED_DOCUMENTS {
        initial
            .add_core_record(document(&cold_source, sequence, "cold body"))
            .unwrap();
    }
    initial
        .certify_source(appendable_certificate(
            &cold_source,
            1,
            RETAINED_DOCUMENTS,
            RETAINED_DOCUMENTS * 10,
        ))
        .unwrap();
    crate::publication::reset_verification_activity();
    initial.commit(|_| true).unwrap();
    assert_eq!(crate::publication::verification_activity(), (1, 1));

    crate::publication::reset_verification_activity();
    let mut noop = GenerationWriter::open(cold.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let constructions = Arc::clone(&noop.index_writer_constructions);
    let inventory = complete_inventory(&cold_source, 1, vec![cold_source.clone()]);
    noop.certify_complete_inventory(inventory.clone()).unwrap();
    stage_exact_replay(&mut noop, &cold_source);
    noop.commit_with_complete_inventory_revalidation(|_| true, |current| current == &inventory)
        .unwrap();
    assert_eq!(crate::publication::verification_activity(), (0, 0));
    assert_eq!(crate::publication::hashed_artifact_bytes(), 0);
    assert_eq!(constructions.load(Ordering::SeqCst), 0);

    crate::publication::reset_verification_activity();
    let mut append = GenerationWriter::open(cold.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = append
        .begin_source_append(cold_source.clone())
        .unwrap()
        .clone();
    append
        .add_core_record(document(
            &cold_source,
            RETAINED_DOCUMENTS + 1,
            "incremental body",
        ))
        .unwrap();
    append
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(
                    &cold_source,
                    2,
                    RETAINED_DOCUMENTS + 1,
                    (RETAINED_DOCUMENTS + 1) * 10,
                ),
                RETAINED_DOCUMENTS * 10,
                [1; 32],
            )
            .unwrap(),
        )
        .unwrap();
    append.commit(|_| true).unwrap();
    assert_eq!(crate::publication::verification_activity(), (1, 0));
    assert_eq!(
        crate::publication::candidate_identity_verification_activity(),
        (2, 5),
        "one changed identity must sample the retained session without replaying its records"
    );
}

#[test]
fn committed_visible_error_reconciliation_uses_incremental_identity_audit() {
    let temp = tempdir().unwrap();
    let source = source("committed-visible-reconciliation.jsonl");
    let mut initial = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    initial.begin_source(source.clone()).unwrap();
    initial
        .add_core_record(document(&source, 1, "reconciliation baseline"))
        .unwrap();
    initial
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    let baseline = initial.commit(|_| true).unwrap();

    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = append.begin_source_append(source.clone()).unwrap().clone();
    append
        .add_core_record(document(&source, 2, "reconciled append"))
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
    append.return_commit_error_after_visibility = true;

    crate::publication::reset_verification_activity();
    let receipt = append.commit(|_| true).unwrap();
    assert_eq!(crate::publication::verification_activity(), (1, 0));
    assert_eq!(
        crate::publication::candidate_identity_verification_activity(),
        (2, 5)
    );
    assert_ne!(receipt.generation_id, baseline.generation_id);
    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    assert_eq!(pointer.active().generation_id(), receipt.generation_id);
    assert_eq!(
        pointer.previous().map(GenerationSlot::generation_id),
        Some(baseline.generation_id.as_str())
    );
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().generation_id(),
        receipt.generation_id
    );
}
