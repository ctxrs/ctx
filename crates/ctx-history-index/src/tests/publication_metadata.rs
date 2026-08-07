use std::{cell::Cell, panic::AssertUnwindSafe, sync::Arc};

use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};
use tantivy::{
    collector::Count, indexer::NoMergePolicy, query::TermQuery, schema::IndexRecordOption,
};

use super::*;

fn staged_replacement(
    root: &Path,
    source: &SourceKey,
    revision: u8,
    body: &str,
) -> GenerationWriter {
    let mut writer = GenerationWriter::open(root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(document(source, 1, body)).unwrap();
    writer
        .certify_source(appendable_certificate(source, revision, 1, 10))
        .unwrap();
    writer
}

fn publish_with_metadata(
    root: &Path,
    source: &SourceKey,
    revision: u8,
    body: &str,
    metadata: &[u8],
) -> PublishedGeneration {
    staged_replacement(root, source, revision, body)
        .commit_with_publication_metadata(|_| true, |_| Ok(metadata.to_vec()))
        .unwrap()
}

fn active_payload(root: &Path) -> String {
    let pointer = load_active_generation_pointer(root).unwrap().unwrap();
    open_slot_index(root, pointer.active())
        .unwrap()
        .load_metas()
        .unwrap()
        .payload
        .unwrap()
}

fn rewrite_active_payload(root: &Path, payload: &str) -> PathBuf {
    let pointer = load_active_generation_pointer(root).unwrap().unwrap();
    let path = crate::publication::slot_path(root, pointer.active());
    let index = open_slot_index(root, pointer.active()).unwrap();
    let mut writer = index
        .writer_with_num_threads::<TantivyDocument>(1, INDEX_MEMORY_MIN_PER_THREAD)
        .unwrap();
    writer.set_merge_policy(Box::<NoMergePolicy>::default());
    let mut prepared = writer.prepare_commit().unwrap();
    prepared.set_payload(payload);
    prepared.commit().unwrap();
    writer.wait_merging_threads().unwrap();
    path
}

fn raw_term_count(root: &Path, term_text: &str) -> usize {
    let pointer = load_active_generation_pointer(root).unwrap().unwrap();
    let index = open_slot_index(root, pointer.active()).unwrap();
    let reader = index.reader().unwrap();
    let searcher = reader.searcher();
    let body = required_field(&index.schema(), "body_search").unwrap();
    searcher
        .search(
            &TermQuery::new(
                Term::from_field_text(body, term_text),
                IndexRecordOption::Basic,
            ),
            &Count,
        )
        .unwrap()
}

#[test]
fn metadata_factory_runs_inside_the_terminal_authority_fence_without_reopen() {
    let temp = tempdir().unwrap();
    let source = source("publication-metadata-ordering.jsonl");
    let inventory = complete_inventory(&source, 1, vec![source.clone()]);
    let mut writer = staged_replacement(temp.path(), &source, 1, "metadata ordering");
    writer
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    let source_revalidated = Cell::new(false);
    let inventory_revalidated = Cell::new(false);
    let metadata_built = Cell::new(false);
    let metadata = b"{not-domain-json:\xff\x00wrong-generation}".to_vec();

    crate::publication::reset_verification_activity();
    crate::reader::reset_verified_index_reopen_count();
    crate::reader::reset_verified_index_publication_construction_count();
    let published = writer
        .commit_with_complete_inventory_revalidation_and_publication_metadata(
            |target| {
                assert!(metadata_built.get());
                assert!(matches!(target, RevalidationTarget::Source(_)));
                source_revalidated.set(true);
                true
            },
            |current| {
                assert!(metadata_built.get());
                assert!(source_revalidated.get());
                assert_eq!(current, &inventory);
                inventory_revalidated.set(true);
                true
            },
            |context| {
                assert!(!source_revalidated.get());
                assert!(!inventory_revalidated.get());
                assert_eq!(context.generation_id(), context.manifest().generation_id()?);
                assert_eq!(context.manifest().sources.len(), 1);
                metadata_built.set(true);
                Ok(metadata.clone())
            },
        )
        .unwrap();

    assert_eq!(published.disposition(), PublicationDisposition::Published);
    assert!(metadata_built.get());
    assert!(source_revalidated.get());
    assert!(inventory_revalidated.get());
    assert_eq!(
        published.receipt().generation_id,
        published.verified_index().generation_id()
    );
    assert_eq!(
        published.verified_index().publication_metadata(),
        Some(metadata.as_slice())
    );
    assert_eq!(crate::publication::verification_activity(), (1, 1));
    assert_eq!(crate::reader::verified_index_reopen_count(), 0);
    assert_eq!(
        crate::reader::verified_index_publication_construction_count(),
        1
    );
    assert!(Arc::ptr_eq(
        &published.receipt().shared_manifest(),
        &published.verified_index().manifest
    ));

    let expected_payload = format!(
        "{{\"version\":2,\"generation_id\":\"{}\",\"publication_metadata\":\"{}\"}}",
        published.receipt().generation_id,
        STANDARD_NO_PAD.encode(&metadata)
    );
    assert_eq!(active_payload(temp.path()), expected_payload);
    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let active_path = crate::publication::slot_path(temp.path(), pointer.active());
    let active_index = open_slot_index(temp.path(), pointer.active()).unwrap();
    assert_eq!(
        physical_integrity_digest(&active_index, &active_path, Some(&pointer)).unwrap(),
        pointer.active().physical_integrity_digest()
    );

    let reopened = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_eq!(reopened.generation_id(), published.receipt().generation_id);
    assert_eq!(reopened.publication_metadata(), Some(metadata.as_slice()));
}

#[test]
fn receipt_only_commit_does_not_construct_a_return_pin() {
    let temp = tempdir().unwrap();
    let source = source("publication-metadata-receipt-only.jsonl");

    crate::publication::reset_verification_activity();
    crate::reader::reset_verified_index_reopen_count();
    crate::reader::reset_verified_index_publication_construction_count();
    let receipt = staged_replacement(temp.path(), &source, 1, "receipt only")
        .commit(|_| true)
        .unwrap();

    assert!(!receipt.generation_id.is_empty());
    assert_eq!(crate::publication::verification_activity(), (1, 1));
    assert_eq!(crate::reader::verified_index_reopen_count(), 0);
    assert_eq!(
        crate::reader::verified_index_publication_construction_count(),
        0
    );
}

#[test]
fn publication_metadata_accepts_exact_bound_and_rejects_one_over_before_commit() {
    let exact = tempdir().unwrap();
    let exact_source = source("publication-metadata-exact-bound.jsonl");
    let exact_metadata = vec![0x5a; MAX_PUBLICATION_METADATA_BYTES];
    let published = publish_with_metadata(
        exact.path(),
        &exact_source,
        1,
        "exact metadata bound",
        &exact_metadata,
    );
    assert_eq!(
        published
            .verified_index()
            .publication_metadata()
            .unwrap()
            .len(),
        MAX_PUBLICATION_METADATA_BYTES
    );
    let mut oversized_metas = published
        .verified_index()
        .searcher
        .index()
        .load_metas()
        .unwrap();
    oversized_metas.payload = Some(format!(
        "{{\"version\":2,\"generation_id\":\"{}\",\"publication_metadata\":\"{}\"}}",
        published.receipt().generation_id,
        STANDARD_NO_PAD.encode(vec![0x5a; MAX_PUBLICATION_METADATA_BYTES + 1])
    ));
    assert!(matches!(
        payload_generation_id(&oversized_metas),
        Err(IndexError::PublicationMetadataTooLarge { actual, maximum })
            if actual == MAX_PUBLICATION_METADATA_BYTES + 1
                && maximum == MAX_PUBLICATION_METADATA_BYTES
    ));

    let oversized = tempdir().unwrap();
    let oversized_source = source("publication-metadata-one-over.jsonl");
    let pointer_path = oversized.path().join("active-generation.json");
    let error = staged_replacement(oversized.path(), &oversized_source, 1, "oversized metadata")
        .commit_with_publication_metadata(
            |_| true,
            |_| Ok(vec![0x5a; MAX_PUBLICATION_METADATA_BYTES + 1]),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        IndexError::PublicationMetadataTooLarge { actual, maximum }
            if actual == MAX_PUBLICATION_METADATA_BYTES + 1
                && maximum == MAX_PUBLICATION_METADATA_BYTES
    ));
    assert!(!pointer_path.exists());
    assert!(fs::read_dir(oversized.path().join(MANIFEST_DIRECTORY))
        .unwrap()
        .next()
        .is_none());
}

#[test]
fn exact_reuse_skips_factory_and_returns_old_metadata_as_reused() {
    let temp = tempdir().unwrap();
    let source = source("publication-metadata-noop.jsonl");
    let original_metadata = b"request-one";
    let initial =
        publish_with_metadata(temp.path(), &source, 1, "unchanged body", original_metadata);
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();
    let payload_before = active_payload(temp.path());
    let inventory = complete_inventory(&source, 1, vec![source.clone()]);
    let mut replay = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let constructions = replay.index_writer_constructions.clone();
    replay
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    stage_exact_replay(&mut replay, &source);
    let factory_called = Cell::new(false);

    crate::publication::reset_verification_activity();
    crate::reader::reset_verified_index_reopen_count();
    crate::reader::reset_verified_index_publication_construction_count();
    let reused = replay
        .commit_with_complete_inventory_revalidation_and_publication_metadata(
            |_| true,
            |current| current == &inventory,
            |_| {
                factory_called.set(true);
                Ok(b"request-two".to_vec())
            },
        )
        .unwrap();

    assert!(!factory_called.get());
    assert_eq!(reused.disposition(), PublicationDisposition::Reused);
    assert_eq!(
        reused.receipt().generation_id,
        initial.receipt().generation_id
    );
    assert_eq!(
        reused.verified_index().publication_metadata(),
        Some(original_metadata.as_slice())
    );
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(active_payload(temp.path()), payload_before);
    assert_eq!(constructions.load(Ordering::SeqCst), 0);
    assert_eq!(crate::publication::verification_activity(), (0, 0));
    assert_eq!(crate::publication::hashed_artifact_bytes(), 0);
    assert_eq!(crate::reader::verified_index_reopen_count(), 0);
    assert_eq!(
        crate::reader::verified_index_publication_construction_count(),
        1
    );
}

#[test]
fn active_and_retained_generation_expose_their_own_opaque_metadata() {
    let temp = tempdir().unwrap();
    let source = source("publication-metadata-retained.jsonl");
    let malformed_and_mismatched = b"\x00\xff{generation:not-the-core-generation}";
    let previous = publish_with_metadata(
        temp.path(),
        &source,
        1,
        "previous metadata generation",
        malformed_and_mismatched,
    );
    let active_metadata = b"active opaque bytes";
    let active = publish_with_metadata(
        temp.path(),
        &source,
        2,
        "active metadata generation",
        active_metadata,
    );

    assert_eq!(
        active.verified_index().publication_metadata(),
        Some(active_metadata.as_slice())
    );
    let retained =
        VerifiedIndex::open_pinned_generation(temp.path(), &previous.receipt().generation_id)
            .unwrap();
    assert_eq!(
        retained.publication_metadata(),
        Some(malformed_and_mismatched.as_slice())
    );
    assert_eq!(retained.count_term("previous").unwrap(), 1);
    assert_eq!(retained.count_term("active").unwrap(), 0);
}

#[test]
fn retained_generation_peer_is_limited_to_the_current_pointer_pair() {
    let temp = tempdir().unwrap();
    let source = source("retained-generation-peer.jsonl");
    let first = publish_with_metadata(temp.path(), &source, 1, "first peer", b"first");
    assert!(VerifiedIndex::open_retained_generation_peer(
        temp.path(),
        &first.receipt().generation_id,
    )
    .unwrap()
    .is_none());

    let second = publish_with_metadata(temp.path(), &source, 2, "second peer", b"second");
    let previous =
        VerifiedIndex::open_retained_generation_peer(temp.path(), &second.receipt().generation_id)
            .unwrap()
            .unwrap();
    assert_eq!(previous.generation_id(), first.receipt().generation_id);
    let active =
        VerifiedIndex::open_retained_generation_peer(temp.path(), &first.receipt().generation_id)
            .unwrap()
            .unwrap();
    assert_eq!(active.generation_id(), second.receipt().generation_id);

    let third = publish_with_metadata(temp.path(), &source, 3, "third peer", b"third");
    let error = match VerifiedIndex::open_retained_generation_peer(
        temp.path(),
        &first.receipt().generation_id,
    ) {
        Ok(_) => panic!("expired generation unexpectedly retained a peer"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        IndexError::PinnedGenerationNotRetained { .. }
    ));
    let active =
        VerifiedIndex::open_retained_generation_peer(temp.path(), &second.receipt().generation_id)
            .unwrap()
            .unwrap();
    assert_eq!(active.generation_id(), third.receipt().generation_id);
}

#[test]
fn one_durable_lease_retains_an_exact_old_generation_without_changing_peer_slots() {
    let temp = tempdir().unwrap();
    let source = source("generation-retention-lease.jsonl");
    let first = publish_with_metadata(temp.path(), &source, 1, "leased first", b"first");
    let lease = acquire_generation_retention_lease(
        temp.path(),
        &first.receipt().generation_id,
        "pro_core_finalization",
        &"a".repeat(64),
    )
    .unwrap();

    let second = publish_with_metadata(temp.path(), &source, 2, "second", b"second");
    let third = publish_with_metadata(temp.path(), &source, 3, "third", b"third");
    let fourth = publish_with_metadata(temp.path(), &source, 4, "fourth", b"fourth");

    assert_eq!(lease.generation_id(), first.receipt().generation_id);
    assert_eq!(
        load_generation_retention_lease(temp.path()).unwrap(),
        Some(lease.clone())
    );
    let leased =
        VerifiedIndex::open_pinned_generation(temp.path(), &first.receipt().generation_id).unwrap();
    assert_eq!(leased.count_term("leased").unwrap(), 1);
    assert_eq!(leased.count_term("fourth").unwrap(), 0);
    let peer =
        VerifiedIndex::open_retained_generation_peer(temp.path(), &fourth.receipt().generation_id)
            .unwrap()
            .unwrap();
    assert_eq!(peer.generation_id(), third.receipt().generation_id);
    assert!(matches!(
        VerifiedIndex::open_retained_generation_peer(temp.path(), &first.receipt().generation_id,),
        Err(IndexError::PinnedGenerationNotRetained { .. })
    ));
    assert_eq!(generation_directories(temp.path()).len(), 3);
    assert_ne!(
        second.receipt().generation_id,
        third.receipt().generation_id
    );

    assert!(release_generation_retention_lease(temp.path(), &lease).unwrap());
    let fifth = publish_with_metadata(temp.path(), &source, 5, "fifth", b"fifth");
    assert_ne!(
        fifth.receipt().generation_id,
        fourth.receipt().generation_id
    );
    assert!(matches!(
        VerifiedIndex::open_pinned_generation(temp.path(), &first.receipt().generation_id),
        Err(IndexError::PinnedGenerationNotRetained { .. })
    ));
    assert_eq!(generation_directories(temp.path()).len(), 2);
}

#[test]
fn generation_retention_lease_is_single_owner_private_and_fail_closed() {
    let temp = tempdir().unwrap();
    let source = source("generation-retention-lease-bound.jsonl");
    let first = publish_with_metadata(temp.path(), &source, 1, "first", b"first");
    let lease = acquire_generation_retention_lease(
        temp.path(),
        &first.receipt().generation_id,
        "pro_core_finalization",
        &"a".repeat(64),
    )
    .unwrap();
    let replay = acquire_generation_retention_lease(
        temp.path(),
        &first.receipt().generation_id,
        "pro_core_finalization",
        &"a".repeat(64),
    )
    .unwrap();
    assert_eq!(replay, lease);
    assert!(matches!(
        acquire_generation_retention_lease(
            temp.path(),
            &first.receipt().generation_id,
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
        VerifiedIndex::open_pinned_generation(temp.path(), &first.receipt().generation_id),
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

#[test]
fn metadata_does_not_change_logical_generation_identity() {
    let first_root = tempdir().unwrap();
    let second_root = tempdir().unwrap();
    let source = source("publication-metadata-logical-identity.jsonl");
    let first = publish_with_metadata(first_root.path(), &source, 1, "same body", b"first");
    let second = publish_with_metadata(second_root.path(), &source, 1, "same body", b"second");

    assert_eq!(
        first.receipt().generation_id,
        second.receipt().generation_id
    );
    assert_eq!(
        serde_json::to_vec(first.receipt().manifest()).unwrap(),
        serde_json::to_vec(second.receipt().manifest()).unwrap()
    );
    assert_ne!(
        active_payload(first_root.path()),
        active_payload(second_root.path())
    );
    for root in [first_root.path(), second_root.path()] {
        let pointer = load_active_generation_pointer(root).unwrap().unwrap();
        let path = crate::publication::slot_path(root, pointer.active());
        let index = open_slot_index(root, pointer.active()).unwrap();
        assert_eq!(
            physical_integrity_digest(&index, &path, Some(&pointer)).unwrap(),
            pointer.active().physical_integrity_digest()
        );
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CrashStage {
    BeforeCommit,
    AfterCommit,
    BeforePointer,
    AfterPointer,
}

fn assert_metadata_crash_stage(stage: CrashStage) {
    let temp = tempdir().unwrap();
    let source = source("publication-metadata-crash.jsonl");
    let old = publish_with_metadata(temp.path(), &source, 1, "old body", b"old-metadata");
    let old_generation = old.receipt().generation_id.clone();
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();
    let mut candidate = staged_replacement(temp.path(), &source, 2, "new body");
    let crash = Box::new(|_: &Path| panic!("simulated process death"));
    match stage {
        CrashStage::BeforeCommit => candidate.before_candidate_commit = Some(crash),
        CrashStage::AfterCommit => candidate.after_candidate_commit = Some(crash),
        CrashStage::BeforePointer => candidate.before_pointer_publication = Some(crash),
        CrashStage::AfterPointer => candidate.after_pointer_switch = Some(crash),
    }

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _ =
            candidate.commit_with_publication_metadata(|_| true, |_| Ok(b"new-metadata".to_vec()));
    }));
    assert!(result.is_err());

    if stage == CrashStage::AfterPointer {
        let active = VerifiedIndex::open_pinned(temp.path()).unwrap();
        assert_ne!(active.generation_id(), old_generation);
        assert_eq!(
            active.publication_metadata(),
            Some(b"new-metadata".as_slice())
        );
        let retained = VerifiedIndex::open_pinned_generation(temp.path(), &old_generation).unwrap();
        assert_eq!(
            retained.publication_metadata(),
            Some(b"old-metadata".as_slice())
        );
        let reopened = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        assert_eq!(
            reopened.base_manifest().unwrap().generation_id().unwrap(),
            active.generation_id()
        );
        return;
    }

    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    let active = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_eq!(active.generation_id(), old_generation);
    assert_eq!(
        active.publication_metadata(),
        Some(b"old-metadata".as_slice())
    );

    let retried = publish_with_metadata(temp.path(), &source, 2, "new body", b"new-metadata");
    assert_eq!(retried.disposition(), PublicationDisposition::Published);
    assert_eq!(
        retried.verified_index().publication_metadata(),
        Some(b"new-metadata".as_slice())
    );
    let retained = VerifiedIndex::open_pinned_generation(temp.path(), &old_generation).unwrap();
    assert_eq!(
        retained.publication_metadata(),
        Some(b"old-metadata".as_slice())
    );
}

#[test]
fn metadata_publication_is_crash_safe_at_commit_and_pointer_boundaries() {
    for stage in [
        CrashStage::BeforeCommit,
        CrashStage::AfterCommit,
        CrashStage::BeforePointer,
        CrashStage::AfterPointer,
    ] {
        assert_metadata_crash_stage(stage);
    }
}

#[test]
fn payload_v1_refresh_rebuilds_and_failed_rebuild_preserves_old_data() {
    let temp = tempdir().unwrap();
    let source = source("publication-metadata-payload-v1.jsonl");
    let initial = publish_with_metadata(temp.path(), &source, 1, "old payload body", b"old");
    let old_generation = initial.receipt().generation_id.clone();
    let pointer_path = temp.path().join("active-generation.json");
    let pointer_before = fs::read(&pointer_path).unwrap();
    let version_one = format!("{{\"version\":1,\"generation_id\":\"{old_generation}\"}}");
    let old_generation_path = rewrite_active_payload(temp.path(), &version_one);
    assert!(matches!(
        VerifiedIndex::open_pinned(temp.path()),
        Err(IndexError::UnsupportedCommitPayload(1))
    ));

    let factory_called = Cell::new(false);
    let failed = staged_replacement(temp.path(), &source, 2, "failed rebuild body")
        .commit_with_publication_metadata(
            |_| false,
            |_| {
                factory_called.set(true);
                Ok(b"must-not-publish".to_vec())
            },
        )
        .unwrap_err();
    assert!(matches!(failed, IndexError::SourceInvalidated(_)));
    assert!(
        factory_called.get(),
        "owner observation must precede the rejecting terminal fence"
    );
    assert_eq!(fs::read(&pointer_path).unwrap(), pointer_before);
    assert!(old_generation_path.is_dir());
    assert_eq!(raw_term_count(temp.path(), "old"), 1);
    assert_eq!(raw_term_count(temp.path(), "failed"), 0);

    let rebuilt = publish_with_metadata(
        temp.path(),
        &source,
        2,
        "successful rebuild body",
        b"rebuilt",
    );
    assert_eq!(rebuilt.disposition(), PublicationDisposition::Published);
    assert_ne!(rebuilt.receipt().generation_id, old_generation);
    assert_eq!(
        rebuilt.verified_index().publication_metadata(),
        Some(b"rebuilt".as_slice())
    );
    assert_eq!(
        rebuilt.verified_index().count_term("successful").unwrap(),
        1
    );
    assert!(!old_generation_path.exists());
    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    assert!(pointer.previous().is_none());
}

#[test]
fn noncanonical_payload_is_rejected_but_raw_metadata_is_not_interpreted() {
    let temp = tempdir().unwrap();
    let source = source("publication-metadata-canonical.jsonl");
    let published = publish_with_metadata(temp.path(), &source, 1, "canonical body", b"{bad-json");
    assert_eq!(
        published.verified_index().publication_metadata(),
        Some(b"{bad-json".as_slice())
    );
    let canonical = active_payload(temp.path());
    let noncanonical = canonical.replacen("{\"version\"", "{ \"version\"", 1);
    rewrite_active_payload(temp.path(), &noncanonical);
    assert!(matches!(
        VerifiedIndex::open_pinned(temp.path()),
        Err(IndexError::NonCanonicalCommitPayload)
    ));

    let mut malformed_metas = published
        .verified_index()
        .searcher
        .index()
        .load_metas()
        .unwrap();
    malformed_metas.payload = Some(format!(
        "{{\"version\":2,\"generation_id\":\"{}\",\"publication_metadata\":\"!!\"}}",
        published.receipt().generation_id
    ));
    assert!(matches!(
        payload_generation_id(&malformed_metas),
        Err(IndexError::InvalidPublicationMetadataEncoding)
    ));
}
