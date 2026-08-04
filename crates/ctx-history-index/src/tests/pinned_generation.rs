use super::*;

fn publish_pinned_test_generation(
    root: &Path,
    source: &SourceKey,
    revision: u8,
    body: &str,
) -> CommitReceipt {
    let mut writer = GenerationWriter::open(root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(document(source, 1, body)).unwrap();
    writer
        .certify_source(certificate(source, revision, 1))
        .unwrap();
    writer.commit(|_| true).unwrap()
}

fn pinned_generation_open_error(root: &Path, expected_generation_id: &str) -> IndexError {
    match VerifiedIndex::open_pinned_generation(root, expected_generation_id) {
        Ok(_) => panic!("requested generation unexpectedly opened"),
        Err(error) => error,
    }
}

#[test]
fn pinned_generation_opens_the_exact_active_generation() {
    let temp = tempdir().unwrap();
    let source = source("pinned-active.jsonl");
    let active = publish_pinned_test_generation(temp.path(), &source, 1, "active evidence");

    let index = VerifiedIndex::open_pinned_generation(temp.path(), &active.generation_id).unwrap();
    assert_eq!(index.generation_id(), active.generation_id);
    assert_eq!(index.count_term("active").unwrap(), 1);
}

#[test]
fn pinned_generation_opens_the_exact_retained_previous_generation() {
    let temp = tempdir().unwrap();
    let source = source("pinned-previous.jsonl");
    let previous = publish_pinned_test_generation(temp.path(), &source, 1, "previous evidence");
    let active = publish_pinned_test_generation(temp.path(), &source, 2, "active replacement");
    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let retained = pointer.previous().unwrap();
    assert_eq!(retained.generation_id(), previous.generation_id);
    assert_ne!(
        retained.physical_integrity_digest(),
        pointer.active().physical_integrity_digest()
    );

    let index =
        VerifiedIndex::open_pinned_generation(temp.path(), &previous.generation_id).unwrap();
    assert_eq!(index.generation_id(), previous.generation_id);
    assert_ne!(index.generation_id(), active.generation_id);
    assert_eq!(index.count_term("previous").unwrap(), 1);
    assert_eq!(index.count_term("replacement").unwrap(), 0);
}

#[test]
fn pinned_generation_reports_not_retained_after_pointer_rotation() {
    let temp = tempdir().unwrap();
    let source = source("pinned-rotated.jsonl");
    let rotated = publish_pinned_test_generation(temp.path(), &source, 1, "rotated evidence");
    let previous = publish_pinned_test_generation(temp.path(), &source, 2, "previous evidence");
    let active = publish_pinned_test_generation(temp.path(), &source, 3, "active evidence");

    let error = pinned_generation_open_error(temp.path(), &rotated.generation_id);
    assert!(matches!(
        error,
        IndexError::PinnedGenerationNotRetained {
            expected_generation_id,
            active_generation_id,
            previous_generation_id: Some(previous_generation_id),
        } if expected_generation_id == rotated.generation_id
            && active_generation_id == active.generation_id
            && previous_generation_id == previous.generation_id
    ));
}

#[test]
fn pinned_generation_rejects_malformed_expected_ids_before_resolution() {
    let temp = tempdir().unwrap();
    for malformed in [
        "a".repeat(63),
        "A".repeat(64),
        format!("{}g", "a".repeat(63)),
    ] {
        assert!(matches!(
            VerifiedIndex::open_pinned_generation(temp.path(), &malformed),
            Err(IndexError::InvalidGenerationId)
        ));
    }
}

#[test]
fn pinned_generation_never_falls_back_to_an_unrelated_active_generation() {
    let temp = tempdir().unwrap();
    let source = source("pinned-no-fallback.jsonl");
    let active = publish_pinned_test_generation(temp.path(), &source, 1, "active evidence");
    let unrelated = "f".repeat(64);
    assert_ne!(unrelated, active.generation_id);

    let error = pinned_generation_open_error(temp.path(), &unrelated);
    assert!(matches!(
        error,
        IndexError::PinnedGenerationNotRetained {
            expected_generation_id,
            active_generation_id,
            previous_generation_id: None,
        } if expected_generation_id == unrelated && active_generation_id == active.generation_id
    ));
}

#[test]
fn pinned_generation_rejects_a_pointer_payload_generation_mismatch() {
    let temp = tempdir().unwrap();
    let source = source("pinned-mismatch.jsonl");
    let actual = publish_pinned_test_generation(temp.path(), &source, 1, "actual evidence");
    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let expected = "e".repeat(64);
    assert_ne!(expected, actual.generation_id);
    let forged_active = GenerationSlot::new(
        expected.clone(),
        pointer.active().directory().to_owned(),
        pointer.active().physical_integrity_digest().to_owned(),
    )
    .unwrap();
    publish_active_generation_pointer(
        temp.path(),
        &ActiveGenerationPointer::new(forged_active, pointer.previous().cloned()).unwrap(),
    )
    .unwrap();

    let error = pinned_generation_open_error(temp.path(), &expected);
    assert!(matches!(
        error,
        IndexError::PinnedGenerationMismatch {
            expected_generation_id,
            actual_generation_id,
        } if expected_generation_id == expected && actual_generation_id == actual.generation_id
    ));
}

#[test]
fn pinned_generation_preserves_manifest_corruption_errors() {
    let temp = tempdir().unwrap();
    let source = source("pinned-corrupt-manifest.jsonl");
    let active = publish_pinned_test_generation(temp.path(), &source, 1, "active evidence");
    fs::write(
        manifest_path(temp.path(), &active.generation_id),
        b"corrupt",
    )
    .unwrap();

    assert!(matches!(
        VerifiedIndex::open_pinned_generation(temp.path(), &active.generation_id),
        Err(IndexError::ManifestDigestMismatch { .. })
    ));
}

#[test]
fn pinned_generation_retries_once_when_the_publication_pointer_changes() {
    let temp = tempdir().unwrap();
    let source = source("pinned-pointer-race.jsonl");
    publish_pinned_test_generation(temp.path(), &source, 1, "previous evidence");
    let before_publication = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let expected = publish_pinned_test_generation(temp.path(), &source, 2, "published evidence");
    let after_publication = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let mut pointer_reads = 0;

    let index = VerifiedIndex::open_pinned_generation_with_pointer_loader(
        temp.path(),
        &expected.generation_id,
        |_| {
            pointer_reads += 1;
            Ok(Some(if pointer_reads == 1 {
                before_publication.clone()
            } else {
                after_publication.clone()
            }))
        },
    )
    .unwrap();

    assert_eq!(
        pointer_reads, 3,
        "resolution did not perform one bounded retry"
    );
    assert_eq!(index.generation_id(), expected.generation_id);
    assert_eq!(index.count_term("published").unwrap(), 1);
}

#[test]
fn pinned_generation_fails_closed_after_a_second_pointer_change() {
    let temp = tempdir().unwrap();
    let source = source("pinned-second-pointer-race.jsonl");
    publish_pinned_test_generation(temp.path(), &source, 1, "first evidence");
    let first_pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let expected = publish_pinned_test_generation(temp.path(), &source, 2, "second evidence");
    let second_pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    publish_pinned_test_generation(temp.path(), &source, 3, "third evidence");
    let third_pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let mut pointer_reads = 0;

    let result = VerifiedIndex::open_pinned_generation_with_pointer_loader(
        temp.path(),
        &expected.generation_id,
        |_| {
            pointer_reads += 1;
            Ok(Some(match pointer_reads {
                1 => first_pointer.clone(),
                2 => second_pointer.clone(),
                _ => third_pointer.clone(),
            }))
        },
    );

    assert!(matches!(
        result,
        Err(IndexError::ConcurrentGenerationChange)
    ));
    assert_eq!(
        pointer_reads, 3,
        "resolution chased more than one publication change"
    );
}

#[test]
fn pinned_generation_real_publication_moves_active_to_previous_and_keeps_reads_valid() {
    let temp = tempdir().unwrap();
    let source = source("pinned-real-active-race.jsonl");
    let expected = publish_pinned_test_generation(temp.path(), &source, 1, "first evidence");
    let first_reader =
        VerifiedIndex::open_pinned_generation(temp.path(), &expected.generation_id).unwrap();
    let mut pointer_reads = 0;
    let mut replacement = None;
    let mut first_reader_valid_during_publication = false;

    let resolved = VerifiedIndex::open_pinned_generation_with_pointer_loader(
        temp.path(),
        &expected.generation_id,
        |root| {
            pointer_reads += 1;
            if pointer_reads == 2 {
                replacement = Some(publish_pinned_test_generation(
                    root,
                    &source,
                    2,
                    "second evidence",
                ));
                first_reader_valid_during_publication =
                    first_reader.count_term("first").unwrap() == 1;
            }
            load_active_generation_pointer(root)
        },
    )
    .unwrap();

    let replacement = replacement.expect("the publication hook did not commit a replacement");
    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    assert_eq!(
        pointer_reads, 3,
        "the real publication was not retried once"
    );
    assert_eq!(pointer.active().generation_id(), replacement.generation_id);
    assert_eq!(
        pointer.previous().unwrap().generation_id(),
        expected.generation_id
    );
    assert!(first_reader_valid_during_publication);
    assert_eq!(first_reader.count_term("first").unwrap(), 1);
    assert_eq!(first_reader.count_term("second").unwrap(), 0);
    assert_eq!(resolved.generation_id(), expected.generation_id);
    assert_eq!(resolved.count_term("first").unwrap(), 1);
    assert_eq!(resolved.count_term("second").unwrap(), 0);
}

#[test]
fn pinned_generation_real_publication_evicts_previous_and_fails_closed_with_valid_read() {
    let temp = tempdir().unwrap();
    let source = source("pinned-real-eviction-race.jsonl");
    let expected = publish_pinned_test_generation(temp.path(), &source, 1, "first evidence");
    #[cfg(not(windows))]
    let expected_path = active_generation_path(temp.path());
    let first_reader =
        VerifiedIndex::open_pinned_generation(temp.path(), &expected.generation_id).unwrap();
    let previous = publish_pinned_test_generation(temp.path(), &source, 2, "second evidence");
    let mut pointer_reads = 0;
    let mut replacement = None;
    let mut first_reader_valid_during_reclamation = false;

    let result = VerifiedIndex::open_pinned_generation_with_pointer_loader(
        temp.path(),
        &expected.generation_id,
        |root| {
            pointer_reads += 1;
            if pointer_reads == 2 {
                replacement = Some(publish_pinned_test_generation(
                    root,
                    &source,
                    3,
                    "third evidence",
                ));
                first_reader_valid_during_reclamation =
                    first_reader.count_term("first").unwrap() == 1;
            }
            load_active_generation_pointer(root)
        },
    );

    let replacement = replacement.expect("the publication hook did not commit a replacement");
    let error = match result {
        Ok(_) => panic!("the evicted previous generation unexpectedly opened"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        IndexError::PinnedGenerationNotRetained {
            expected_generation_id,
            active_generation_id,
            previous_generation_id: Some(previous_generation_id),
        } if expected_generation_id == expected.generation_id
            && active_generation_id == replacement.generation_id
            && previous_generation_id == previous.generation_id
    ));
    assert_eq!(
        pointer_reads, 3,
        "the eviction race did not stop after one retry"
    );
    #[cfg(not(windows))]
    assert!(
        !expected_path.exists(),
        "the evicted generation directory was not reclaimed"
    );
    assert!(first_reader_valid_during_reclamation);
    assert_eq!(first_reader.count_term("first").unwrap(), 1);
    assert_eq!(first_reader.count_term("second").unwrap(), 0);
    assert_eq!(first_reader.count_term("third").unwrap(), 0);
}

#[test]
fn open_previous_generation_decodes_core_body_after_real_reclamation() {
    const BODY: &str = "complete generation-owned body survives real reclamation";

    let temp = tempdir().unwrap();
    let source = source("pinned-real-reclaimed-core.jsonl");
    let expected = document(&source, 1, BODY);
    let first = publish_pinned_test_generation(temp.path(), &source, 1, BODY);
    #[cfg(not(windows))]
    let first_path = active_generation_path(temp.path());
    let first_reader =
        VerifiedIndex::open_pinned_generation(temp.path(), &first.generation_id).unwrap();

    let second = publish_pinned_test_generation(temp.path(), &source, 2, "second body");
    let pointer_with_first_retained = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    assert_eq!(
        pointer_with_first_retained
            .previous()
            .unwrap()
            .generation_id(),
        first.generation_id
    );

    let third = publish_pinned_test_generation(temp.path(), &source, 3, "third body");
    let pointer_after_reclamation = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    assert_eq!(
        pointer_after_reclamation.active().generation_id(),
        third.generation_id
    );
    assert_eq!(
        pointer_after_reclamation
            .previous()
            .unwrap()
            .generation_id(),
        second.generation_id
    );
    #[cfg(not(windows))]
    assert!(
        !first_path.exists(),
        "the evicted generation directory was not reclaimed"
    );

    let decoded = first_reader
        .core_event_by_id(expected.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(decoded.core_record.event_id, expected.event_id);
    assert_eq!(
        decoded.core_record.content.normalized_body.as_deref(),
        Some(BODY)
    );

    let error = pinned_generation_open_error(temp.path(), &first.generation_id);
    assert!(matches!(
        error,
        IndexError::PinnedGenerationNotRetained {
            expected_generation_id,
            active_generation_id,
            previous_generation_id: Some(previous_generation_id),
        } if expected_generation_id == first.generation_id
            && active_generation_id == third.generation_id
            && previous_generation_id == second.generation_id
    ));
}
