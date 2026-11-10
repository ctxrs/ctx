use super::*;

fn open(root: &Path) -> GenerationWriter {
    GenerationWriter::open(root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap()
}

#[test]
fn erasure_preserves_siblings_and_excludes_future_source_replacements() {
    let temp = tempdir().unwrap();
    let source = source("multi-session.jsonl");
    let erased = document_for_session(&source, "erase", 1, "secret-body-marker");
    let kept = document_for_session(&source, "keep", 2, "retained-body-marker");
    let mut first = open(temp.path());
    first.begin_source(source.clone()).unwrap();
    first.add_core_record(erased.clone()).unwrap();
    first.add_core_record(kept.clone()).unwrap();
    first.certify_source(certificate(&source, 1, 2)).unwrap();
    first.commit(|_| true).unwrap();
    fs::write(
        temp.path()
            .join("retained/records/.ctx-retained-interrupted"),
        b"secret-body-marker",
    )
    .unwrap();
    let receipt = open(temp.path())
        .erase_session(erased.session_id.as_uuid())
        .unwrap();
    assert_eq!(receipt.indexed_documents, 1);
    finish_session_erasure(temp.path()).unwrap();
    for entry in fs::read_dir(temp.path().join("retained/records")).unwrap() {
        assert!(!fs::read_to_string(entry.unwrap().path())
            .unwrap()
            .contains("secret-body-marker"));
    }
    {
        let index = VerifiedIndex::open(temp.path()).unwrap();
        assert!(index
            .core_record_by_id(erased.event_id.as_uuid())
            .unwrap()
            .is_none());
        assert_eq!(
            index.core_record_by_id(kept.event_id.as_uuid()).unwrap(),
            Some(kept.clone())
        );
    }
    for _ in 0..2 {
        let mut refresh = open(temp.path());
        refresh.begin_source(source.clone()).unwrap();
        refresh.add_core_record(erased.clone()).unwrap();
        refresh.add_core_record(kept.clone()).unwrap();
        refresh.certify_source(certificate(&source, 1, 2)).unwrap();
        assert_eq!(refresh.commit(|_| true).unwrap().indexed_documents, 1);
    }
    for entry in fs::read_dir(temp.path().join("retained/records")).unwrap() {
        assert!(!fs::read_to_string(entry.unwrap().path())
            .unwrap()
            .contains("secret-body-marker"));
    }
}

#[test]
fn portable_checkpoint_restores_missing_records_after_schema_rebuild() {
    let temp = tempdir().unwrap();
    let source = source("aged-out.jsonl");
    let record = document(&source, 1, "retained across schema upgrades");
    let mut first = open(temp.path());
    first.begin_source(source.clone()).unwrap();
    first.add_core_record(record.clone()).unwrap();
    first.certify_source(certificate(&source, 1, 1)).unwrap();
    first.commit(|_| true).unwrap();
    let inventory = complete_inventory(&source, 2, Vec::new());
    let proof = CertifiedSourceDeletion::from_inventory(source.clone(), &inventory).unwrap();
    let mut missing = open(temp.path());
    missing.retain_missing_source(proof, inventory).unwrap();
    missing.commit(|_| true).unwrap();
    let routes = VerifiedIndex::open(temp.path())
        .unwrap()
        .manifest()
        .source_routes()
        .to_vec();
    assert_eq!(routes[0].missing_sources(), std::slice::from_ref(&source));
    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let metadata_path = temp
        .path()
        .join(INDEX_GENERATIONS_DIRECTORY)
        .join(pointer.active().directory())
        .join("meta.json");
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
    metadata["schema"] = serde_json::to_value(tantivy::schema::Schema::builder().build()).unwrap();
    with_temporarily_writable(&metadata_path, || {
        fs::write(&metadata_path, serde_json::to_vec(&metadata).unwrap())
    })
    .unwrap();
    assert!(matches!(
        VerifiedIndex::open(temp.path()),
        Err(IndexError::SchemaMismatch(_))
    ));
    drop(open(temp.path()));
    let restored = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(
        restored
            .core_record_by_id(record.event_id.as_uuid())
            .unwrap(),
        Some(record)
    );
    assert_eq!(restored.document_count(), 1);
    assert_eq!(restored.manifest().source_routes(), routes);
}

#[test]
fn erased_session_append_frontier_survives_rebuild_and_new_records() {
    let temp = tempdir().unwrap();
    let source = source("append.jsonl");
    let erased = document_for_session(&source, "erase", 1, "erased");
    let mut writer = open(temp.path());
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(erased.clone()).unwrap();
    writer
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    writer.commit(|_| true).unwrap();
    open(temp.path())
        .erase_session(erased.session_id.as_uuid())
        .unwrap();
    let kept = document_for_session(&source, "keep", 3, "kept");
    let mut writer = open(temp.path());
    let base = writer.begin_source_append(source.clone()).unwrap().clone();
    writer
        .add_core_record(document_for_session(&source, "erase", 2, "still erased"))
        .unwrap();
    writer.add_core_record(kept.clone()).unwrap();
    writer
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(&source, 2, 3, 30),
                10,
                [1; 32],
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(writer.commit(|_| true).unwrap().indexed_documents, 1);
    let restored = open(temp.path());
    restored.rebuild_retained().unwrap();
    let index = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(index.document_count(), 1);
    assert_eq!(
        index.core_record_by_id(kept.event_id.as_uuid()).unwrap(),
        Some(kept)
    );
    assert!(index
        .session_by_id(erased.session_id.as_uuid())
        .unwrap()
        .is_none());
}

fn seed_checkpoint(root: &Path) -> CoreRecord {
    let source = source("checkpoint.jsonl");
    let record = document(&source, 1, "checkpoint secret");
    let mut writer = open(root);
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(record.clone()).unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();
    record
}

#[test]
fn interrupted_erasure_resumes_from_durable_exclusion() {
    let temp = tempdir().unwrap();
    let record = seed_checkpoint(temp.path());
    fs::write(
        temp.path().join("retained/erased-sessions.json"),
        serde_json::to_vec(&vec![record.session_id.as_uuid()]).unwrap(),
    )
    .unwrap();
    drop(open(temp.path()));
    finish_session_erasure(temp.path()).unwrap();
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().document_count(),
        0
    );
}

#[test]
fn erasure_reports_readers_until_old_bytes_can_be_reclaimed() {
    let temp = tempdir().unwrap();
    let record = seed_checkpoint(temp.path());
    let reader = VerifiedIndex::open_pinned(temp.path()).unwrap();
    open(temp.path())
        .erase_session(record.session_id.as_uuid())
        .unwrap();
    assert!(finish_session_erasure(temp.path()).is_err());
    assert!(reader
        .core_record_by_id(record.event_id.as_uuid())
        .unwrap()
        .is_some());
    drop(reader);
    finish_session_erasure(temp.path()).unwrap();
    assert_eq!(
        fs::read_dir(temp.path().join(INDEX_GENERATIONS_DIRECTORY))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn corrupt_portable_records_do_not_replace_active_history() {
    let temp = tempdir().unwrap();
    seed_checkpoint(temp.path());
    let pointer = fs::read(temp.path().join("active-generation.json")).unwrap();
    let blob = fs::read_dir(temp.path().join("retained/records"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let changed = fs::read_to_string(&blob)
        .unwrap()
        .replace("checkpoint secret", "altered secret");
    serde_json::from_str::<CoreRecord>(&changed)
        .unwrap()
        .validate_contract()
        .unwrap();
    fs::write(&blob, changed).unwrap();
    assert!(
        matches!(open(temp.path()).rebuild_retained(), Err(IndexError::RetainedHistory(message)) if message.contains("checksum"))
    );
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer
    );
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().document_count(),
        1
    );
}

#[test]
fn unsupported_checkpoint_preserves_active_history() {
    let temp = tempdir().unwrap();
    seed_checkpoint(temp.path());
    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let path = temp
        .path()
        .join("retained")
        .join(format!("{}.json", pointer.active().generation_id()));
    let mut checkpoint: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    checkpoint["core_contract"] = serde_json::json!("future-contract");
    fs::write(&path, serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    assert!(GenerationWriter::open(temp.path(), WriterOptions::default()).is_err());
    assert_eq!(
        VerifiedIndex::open(temp.path()).unwrap().document_count(),
        1
    );
}

#[test]
fn checkpoints_reuse_manifest_deltas_and_unchanged_record_blobs() {
    let temp = tempdir().unwrap();
    let sources: Vec<_> = (0..32)
        .map(|i| source(&format!("retained-{i}.jsonl")))
        .collect();
    let mut writer = open(temp.path());
    for source in &sources {
        writer.begin_source(source.clone()).unwrap();
        writer
            .add_core_record(document(source, 1, "original"))
            .unwrap();
        writer.certify_source(certificate(source, 1, 1)).unwrap();
    }
    let first = writer.commit(|_| true).unwrap();
    let blobs = temp.path().join("retained/records");
    let original_blobs: Vec<_> = fs::read_dir(&blobs)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    for path in &original_blobs {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
            .unwrap();
    }
    for revision in 2..=3 {
        let mut writer = open(temp.path());
        writer.begin_source(sources[0].clone()).unwrap();
        writer
            .add_core_record(document(&sources[0], 1, &format!("revision {revision}")))
            .unwrap();
        writer
            .certify_source(certificate(&sources[0], revision, 1))
            .unwrap();
        let receipt = writer.commit(|_| true).unwrap();
        let metadata = fs::read(
            temp.path()
                .join("retained")
                .join(format!("{}.json", receipt.generation_id)),
        )
        .unwrap();
        assert!(
            metadata.len() < 512,
            "checkpoint must not duplicate corpus metadata"
        );
        let manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(manifest_path(temp.path(), &receipt.generation_id)).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["storage_format"], "ctx-manifest-flat-delta-v1");
        assert_eq!(manifest["base_generation_id"], first.generation_id);
        assert!(manifest_path(temp.path(), &first.generation_id).exists());
        for path in original_blobs.iter().filter(|path| path.exists()) {
            assert_eq!(
                fs::metadata(path).unwrap().modified().unwrap(),
                std::time::UNIX_EPOCH
            );
        }
    }
    assert_eq!(fs::read_dir(&blobs).unwrap().count(), sources.len() + 1);
    let before = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let expected: Vec<_> = sources
        .iter()
        .map(|source| {
            before
                .core_source_event_page(source, None, 64)
                .unwrap()
                .items
        })
        .collect();
    drop(before);
    open(temp.path()).rebuild_retained().unwrap();
    let after = VerifiedIndex::open_pinned(temp.path()).unwrap();
    for records in expected {
        for record in records {
            assert_eq!(
                after
                    .core_record_by_id(record.core_record.event_id.as_uuid())
                    .unwrap(),
                Some(record.core_record)
            );
        }
    }
}

#[test]
fn self_contained_checkpoint_rebuild_migrates_to_shared_manifest() {
    let temp = tempdir().unwrap();
    let record = seed_checkpoint(temp.path());
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let metadata = temp
        .path()
        .join("retained")
        .join(format!("{}.json", index.generation_id()));
    let mut snapshot: serde_json::Value =
        serde_json::from_slice(&fs::read(&metadata).unwrap()).unwrap();
    let blob = fs::read_dir(temp.path().join("retained/records"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let digest = ctx_history_index_format::sha256_hex(&fs::read(&blob).unwrap());
    fs::rename(&blob, blob.with_file_name(format!("{digest}.jsonl"))).unwrap();
    snapshot["version"] = serde_json::json!(1);
    snapshot["manifest"] = serde_json::to_value(index.manifest()).unwrap();
    snapshot["records"] =
        serde_json::to_value(BTreeMap::from([(source_token(&record.source), digest)])).unwrap();
    fs::write(metadata, serde_json::to_vec(&snapshot).unwrap()).unwrap();
    drop(index);
    open(temp.path()).rebuild_retained().unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_eq!(
        index.core_record_by_id(record.event_id.as_uuid()).unwrap(),
        Some(record)
    );
    let metadata = temp
        .path()
        .join("retained")
        .join(format!("{}.json", index.generation_id()));
    let snapshot: serde_json::Value = serde_json::from_slice(&fs::read(metadata).unwrap()).unwrap();
    assert_eq!(snapshot["version"], 2);
    assert!(snapshot.get("manifest").is_none());
    assert!(snapshot.get("records").is_none());
}
