use super::*;
use ctx_history_index_generation::{
    slot_path, AtomicPublicationStage, PublicationIoProbe, PublicationIoProbeGuard,
};

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

struct PreviousDelta {
    temp: TempDir,
    original: SourceKey,
    added: SourceKey,
    base: String,
    pointer: ActiveGenerationPointer,
}

impl PreviousDelta {
    fn new() -> Self {
        let temp = tempdir().unwrap();
        let original = source("previous-recovery-original.jsonl");
        let added = source("previous-recovery-added.jsonl");
        let base = publish(temp.path(), &original, 1, "baseline").generation_id;
        let previous = publish(temp.path(), &original, 2, "retained");
        let descriptor: serde_json::Value = serde_json::from_slice(
            &fs::read(manifest_path(temp.path(), &previous.generation_id)).unwrap(),
        )
        .unwrap();
        assert_eq!(descriptor["storage_format"], "ctx-manifest-flat-delta-v1");
        assert_eq!(descriptor["base_generation_id"], base);

        // A new source forces a full manifest, independent of the old base.
        let active = publish(temp.path(), &added, 1, "healthy");
        let manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(manifest_path(temp.path(), &active.generation_id)).unwrap(),
        )
        .unwrap();
        assert!(manifest.get("storage_format").is_none());
        let pointer = load_active_generation_pointer(temp.path())
            .unwrap()
            .unwrap();
        assert_eq!(pointer.active().generation_id(), active.generation_id);
        assert_eq!(
            pointer.previous().unwrap().generation_id(),
            previous.generation_id
        );
        Self {
            temp,
            original,
            added,
            base,
            pointer,
        }
    }

    fn root(&self) -> &Path {
        self.temp.path()
    }

    fn remove_base(&self) {
        fs::remove_file(manifest_path(self.root(), &self.base)).unwrap();
    }

    fn assert_pointer_unchanged(&self) {
        assert_eq!(
            load_active_generation_pointer(self.root())
                .unwrap()
                .as_ref(),
            Some(&self.pointer)
        );
    }
}

fn active_files(root: &Path) -> std::collections::BTreeMap<std::ffi::OsString, Vec<u8>> {
    fs::read_dir(active_generation_path(root))
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.path().is_file())
        .map(|entry| (entry.file_name(), fs::read(entry.path()).unwrap()))
        .collect()
}

#[test]
fn previous_recovery_allows_exact_import_replay_and_paired_queries_without_rewriting_active() {
    let fixture = PreviousDelta::new();
    let root = fixture.root();
    let active = fixture.pointer.active();
    let previous = fixture.pointer.previous().unwrap();
    let manifest = fs::read(manifest_path(root, active.generation_id())).unwrap();
    let files = active_files(root);
    let reader = VerifiedIndex::open_pinned_with_retained_peer(root)
        .unwrap()
        .into_retained_generation_peer_for_reader()
        .unwrap()
        .unwrap();
    assert_eq!(reader.generation_id(), previous.generation_id());
    let event_id = document(&fixture.added, 1, "healthy").event_id.as_uuid();
    let event_before = VerifiedIndex::open_pinned(root)
        .unwrap()
        .core_event_by_id(event_id)
        .unwrap()
        .unwrap();

    fixture.remove_base();
    assert!(matches!(
        VerifiedIndex::open_pinned_with_retained_peer(root),
        Err(IndexError::MissingManifest(id)) if id == fixture.base
    ));
    fixture.assert_pointer_unchanged();
    assert_eq!(
        VerifiedIndex::open_pinned(root)
            .unwrap()
            .count_term("healthy")
            .unwrap(),
        1
    );

    // This is the writer entry point used by ordinary source-backed import.
    let mut writer = GenerationWriter::open_with_expected_base_generation(
        root,
        WriterOptions::default(),
        Some(active.generation_id()),
    )
    .unwrap()
    .into_writer()
    .unwrap();
    assert_eq!(writer.base_generation_id(), Some(active.generation_id()));
    let constructions = Arc::clone(&writer.index_writer_constructions);
    let certificates = [
        stage_exact_replay(&mut writer, &fixture.original),
        stage_exact_replay(&mut writer, &fixture.added),
    ];
    let inventory = complete_inventory(
        &fixture.original,
        3,
        vec![fixture.original.clone(), fixture.added.clone()],
    );
    writer
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    let receipt = writer.commit_with_complete_inventory_revalidation(
        |target| matches!(target, RevalidationTarget::Source(cert) if certificates.contains(cert)),
        |observed| observed == &inventory,
    ).unwrap();
    assert_eq!(receipt.generation_id, active.generation_id());
    assert_eq!(constructions.load(Ordering::SeqCst), 0);
    assert_eq!(
        load_active_generation_pointer(root).unwrap().unwrap(),
        ActiveGenerationPointer::new(active.clone(), None).unwrap()
    );
    assert_eq!(
        fs::read(manifest_path(root, active.generation_id())).unwrap(),
        manifest
    );
    assert_eq!(active_files(root), files);
    let mut current = VerifiedIndex::open_pinned_with_retained_peer(root).unwrap();
    assert_eq!(current.document_count(), 2);
    assert_eq!(
        current.core_event_by_id(event_id).unwrap().unwrap(),
        event_before
    );
    assert!(current
        .take_retained_generation_peer_for_reader()
        .unwrap()
        .is_none());
    // A retired compact reference must expire, never resolve to current data.
    assert!(matches!(
        VerifiedIndex::open_pinned_generation(root, previous.generation_id()),
        Err(IndexError::PinnedGenerationNotRetained { .. })
    ));
    assert_eq!(reader.count_term("retained").unwrap(), 1);
    assert!(slot_path(root, previous).exists());
    assert!(manifest_path(root, previous.generation_id()).exists());

    drop((reader, current));
    let successor = publish(root, &fixture.added, 2, "successor");
    let pointer = load_active_generation_pointer(root).unwrap().unwrap();
    assert_eq!(pointer.active().generation_id(), successor.generation_id);
    assert_eq!(pointer.previous(), Some(active));
    let current = VerifiedIndex::open_pinned_with_retained_peer(root).unwrap();
    assert_eq!(current.count_term("successor").unwrap(), 1);
    assert_eq!(current.count_term("retained").unwrap(), 1);
    assert!(!slot_path(root, previous).exists());
    assert!(!manifest_path(root, previous.generation_id()).exists());
}

#[test]
fn previous_recovery_keeps_durable_authority_and_requires_its_dependencies() {
    for retain_previous in [false, true] {
        let fixture = PreviousDelta::new();
        let target = if retain_previous {
            fixture.pointer.previous().unwrap()
        } else {
            fixture.pointer.active()
        };
        let lease = acquire_generation_retention_lease(
            fixture.root(),
            target.generation_id(),
            "pro_core_finalization",
            &"a".repeat(64),
        )
        .unwrap();
        fixture.remove_base();
        let opened = GenerationWriter::open(fixture.root(), WriterOptions::default());
        if retain_previous {
            assert!(matches!(opened, Err(IndexError::MissingManifest(id)) if id == fixture.base));
            fixture.assert_pointer_unchanged();
        } else {
            let writer = opened.unwrap().into_writer().unwrap();
            assert_eq!(writer.base_generation_id(), Some(target.generation_id()));
            assert!(load_active_generation_pointer(fixture.root())
                .unwrap()
                .unwrap()
                .previous()
                .is_none());
        }
        assert_eq!(
            load_generation_retention_lease(fixture.root()).unwrap(),
            Some(lease)
        );
        assert!(slot_path(fixture.root(), target).exists());
    }
}

#[test]
fn previous_recovery_does_not_mask_active_corruption_or_missing_shared_base() {
    let fixture = PreviousDelta::new();
    fixture.remove_base();
    corrupt_candidate_segment_store(
        &active_generation_path(fixture.root()),
        &HashSet::new(),
        false,
    );
    assert!(matches!(
        GenerationWriter::open(fixture.root(), WriterOptions::default()),
        Err(IndexError::ChecksumMismatch)
    ));
    fixture.assert_pointer_unchanged();

    let temp = tempdir().unwrap();
    let source = source("shared-recovery-base.jsonl");
    let base = publish(temp.path(), &source, 1, "base");
    publish(temp.path(), &source, 2, "previous");
    publish(temp.path(), &source, 3, "active");
    let pointer = load_active_generation_pointer(temp.path()).unwrap();
    fs::remove_file(manifest_path(temp.path(), &base.generation_id)).unwrap();
    assert!(
        matches!(GenerationWriter::open(temp.path(), WriterOptions::default()), Err(IndexError::MissingManifest(id)) if id == base.generation_id)
    );
    assert_eq!(
        load_active_generation_pointer(temp.path()).unwrap(),
        pointer
    );
}

#[test]
fn previous_recovery_rejects_other_previous_damage_and_stale_import_expectations() {
    for missing in [false, true] {
        let fixture = PreviousDelta::new();
        let path = manifest_path(
            fixture.root(),
            fixture.pointer.previous().unwrap().generation_id(),
        );
        if missing {
            fs::remove_file(path).unwrap();
        } else {
            let mut bytes = fs::read(&path).unwrap();
            bytes.push(b' ');
            fs::write(path, bytes).unwrap();
        }
        assert!(GenerationWriter::open(fixture.root(), WriterOptions::default()).is_err());
        fixture.assert_pointer_unchanged();
    }
    let fixture = PreviousDelta::new();
    fixture.remove_base();
    assert!(matches!(
        GenerationWriter::open_with_expected_base_generation(
            fixture.root(),
            WriterOptions::default(),
            Some(&fixture.base)
        ),
        Err(IndexError::ConcurrentGenerationChange)
    ));
    fixture.assert_pointer_unchanged();
}

#[test]
fn previous_recovery_pointer_failures_preserve_active_and_allow_restart() {
    for stage in [
        AtomicPublicationStage::Replacement,
        AtomicPublicationStage::Synchronization,
    ] {
        let fixture = PreviousDelta::new();
        fixture.remove_base();
        let files = active_files(fixture.root());
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hook_fired = Arc::clone(&fired);
        let probe = PublicationIoProbeGuard::set(move |event| {
            if event == PublicationIoProbe::ActivePointer(stage) {
                hook_fired.store(true, Ordering::SeqCst);
                return Err(std::io::Error::other(
                    "injected previous retirement failure",
                ));
            }
            Ok(())
        });
        let result = GenerationWriter::open(fixture.root(), WriterOptions::default());
        drop(probe);
        assert!(fired.load(Ordering::SeqCst));
        if stage == AtomicPublicationStage::Replacement {
            assert!(result.is_err());
            fixture.assert_pointer_unchanged();
        } else {
            assert!(
                matches!(result, Err(IndexError::CommittedGenerationNeedsRecovery { generation_id, .. }) if generation_id == fixture.pointer.active().generation_id())
            );
            let pointer = load_active_generation_pointer(fixture.root())
                .unwrap()
                .unwrap();
            assert_eq!(pointer.active(), fixture.pointer.active());
            assert!(pointer.previous().is_none());
        }
        assert_eq!(active_files(fixture.root()), files);
        drop(
            GenerationWriter::open(fixture.root(), WriterOptions::default())
                .unwrap()
                .into_writer()
                .unwrap(),
        );
        let current = VerifiedIndex::open_pinned_with_retained_peer(fixture.root()).unwrap();
        assert_eq!(
            current.generation_id(),
            fixture.pointer.active().generation_id()
        );
        assert_eq!(current.count_term("healthy").unwrap(), 1);
    }
}

#[test]
fn previous_recovery_revalidates_active_at_pointer_replacement() {
    for retain_active in [false, true] {
        let fixture = PreviousDelta::new();
        let _lease = retain_active.then(|| {
            acquire_generation_retention_lease(
                fixture.root(),
                fixture.pointer.active().generation_id(),
                "pro_core_finalization",
                &"a".repeat(64),
            )
            .unwrap()
        });
        fixture.remove_base();
        let active_manifest =
            manifest_path(fixture.root(), fixture.pointer.active().generation_id());
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hook_fired = Arc::clone(&fired);
        let probe = PublicationIoProbeGuard::set(move |event| {
            if event == PublicationIoProbe::ActivePointer(AtomicPublicationStage::Validation) {
                hook_fired.store(true, Ordering::SeqCst);
                // The initial active check has already passed; terminal validation
                // must still reject a manifest changed before pointer replacement.
                let mut bytes = fs::read(&active_manifest)?;
                bytes.push(b' ');
                fs::write(&active_manifest, bytes)?;
            }
            Ok(())
        });
        let result = GenerationWriter::open(fixture.root(), WriterOptions::default());
        drop(probe);
        assert!(fired.load(Ordering::SeqCst));
        let active_id = fixture.pointer.active().generation_id();
        assert!(matches!(
            ctx_history_index_generation::load_manifest_bytes(fixture.root(), active_id),
            Err(ctx_history_index_generation::GenerationError::ManifestDigestMismatch { expected, .. }) if expected == active_id
        ));
        // The certificate can reject the changed manifest before format loading;
        // retained-read authority paths differ across supported platforms.
        match result {
            Err(IndexError::ChecksumMismatch) => {}
            Err(IndexError::ManifestDigestMismatch { expected, .. }) => {
                assert_eq!(expected, active_id);
            }
            other => panic!("unexpected failure: {:?}", other.as_ref().err()),
        }
        fixture.assert_pointer_unchanged();
    }
}
