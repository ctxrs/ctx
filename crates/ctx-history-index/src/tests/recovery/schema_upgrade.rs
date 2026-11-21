use super::*;

fn copy_fixture(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(to, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_fixture(&entry.path(), &target);
        } else {
            assert!(entry.path().is_file());
            fs::copy(entry.path(), &target).unwrap();
            // Git preserves bytes, not the published artifact's readonly seal.
            let mut permissions = fs::metadata(&target).unwrap().permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(0o600);
            }
            if matches!(
                target.extension().and_then(|ext| ext.to_str()),
                Some("fast" | "fieldnorm" | "idx" | "pos" | "store" | "term")
            ) {
                permissions.set_readonly(true);
            }
            fs::set_permissions(&target, permissions).unwrap();
        }
    }
}

fn generation_directories(root: &Path) -> HashSet<PathBuf> {
    fs::read_dir(root.join(INDEX_GENERATIONS_DIRECTORY))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect()
}

#[test]
fn main_schema_22_stays_searchable_through_interrupted_rebuild_and_publication() {
    let temp = tempdir().unwrap();
    copy_fixture(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/schema-22/index"),
        temp.path(),
    );
    let pointer_path = temp.path().join("active-generation.json");
    let before_pointer = fs::read(&pointer_path).unwrap();
    // This is the actual predecessor writer's output, not a current index
    // relabeled as schema 22. The authored input and producer pin accompany it.
    let old = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(old.manifest().lexical_schema_version, 22);
    assert_eq!(old.count_term("schema22upgrademarker").unwrap(), 1);
    let (searcher, _) = open_unverified_generation(temp.path());
    let address = searcher
        .search(&AllQuery, &DocSetCollector)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let record = decoded_stored_core(&searcher, address);
    let certificate = old.manifest().sources[0].clone();

    let active_path = active_generation_path(temp.path());
    for attempt in 0..5 {
        let mut interrupted = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        assert!(interrupted.base_manifest().is_none());
        interrupted.begin_source(record.source.clone()).unwrap();
        interrupted.add_core_record(record.clone()).unwrap();
        interrupted.certify_source(certificate.clone()).unwrap();
        let candidate_path = temp
            .path()
            .join(INDEX_GENERATIONS_DIRECTORY)
            .join(interrupted.candidate_directory_name.as_ref().unwrap());
        assert_eq!(
            generation_directories(temp.path()),
            HashSet::from([active_path.clone(), candidate_path.clone()]),
            "abandoned candidates accumulated on attempt {attempt}"
        );
        if attempt == 0 {
            assert!(matches!(
                GenerationWriter::open(temp.path(), WriterOptions::default()),
                Err(IndexError::Tantivy(tantivy::TantivyError::LockFailure(
                    _,
                    _
                )))
            ));
            assert!(
                candidate_path.is_dir(),
                "a live writer still owns its candidate"
            );
        }
        let served = VerifiedIndex::open_pinned(temp.path()).unwrap();
        assert_eq!(served.generation_id(), old.generation_id());
        assert_eq!(served.count_term("schema22upgrademarker").unwrap(), 1);
        drop(interrupted);
        assert!(
            candidate_path.is_dir(),
            "interruption must leave a candidate to reclaim"
        );
        assert_eq!(fs::read(&pointer_path).unwrap(), before_pointer);
        assert_eq!(
            VerifiedIndex::open(temp.path()).unwrap().generation_id(),
            old.generation_id()
        );
    }

    let mut restarted = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert!(restarted.base_manifest().is_none());
    restarted.begin_source(record.source.clone()).unwrap();
    restarted.add_core_record(record.clone()).unwrap();
    restarted.certify_source(certificate).unwrap();
    let root = temp.path().to_path_buf();
    restarted.before_pointer_switch = Some(Box::new(move |_| {
        let still_old = VerifiedIndex::open_pinned(&root).unwrap();
        assert_eq!(still_old.manifest().lexical_schema_version, 22);
        assert_eq!(still_old.count_term("schema22upgrademarker").unwrap(), 1);
    }));
    restarted.commit(|_| true).unwrap();
    let current = VerifiedIndex::open(temp.path()).unwrap();
    assert_eq!(
        current.manifest().lexical_schema_version,
        LEXICAL_SCHEMA_VERSION
    );
    assert_eq!(current.count_term("schema22upgrademarker").unwrap(), 1);
    assert_eq!(
        current
            .core_record_by_id(record.event_id.as_uuid())
            .unwrap(),
        Some(record.clone())
    );
    assert_eq!(old.count_term("schema22upgrademarker").unwrap(), 1);
    let old_generation_id = old.generation_id().to_owned();
    drop(old);

    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert!(writer
        .base_event_identity_lookup()
        .contains_call_result(record.session_id, &TypedKey::utf8("call-1").unwrap())
        .unwrap());
    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    assert_eq!(
        pointer.previous().unwrap().generation_id(),
        old_generation_id
    );
    assert_eq!(generation_directories(temp.path()).len(), 2);
    assert!(
        active_path.is_dir(),
        "the readable schema-22 predecessor is retained"
    );
}

#[test]
fn rebuild_reclaims_candidates_and_retires_unleased_previous_while_honoring_readers() {
    let temp = tempdir().unwrap();
    let source = source("retained-rebuild.jsonl");
    let mut reader = None;
    let mut reader_path = PathBuf::new();
    for revision in 1..=2 {
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer
            .add_core_record(document(&source, 1, "retained marker"))
            .unwrap();
        writer
            .certify_source(certificate(&source, revision, 1))
            .unwrap();
        writer.commit(|_| true).unwrap();
        if revision == 1 {
            reader = Some(VerifiedIndex::open_pinned(temp.path()).unwrap());
            reader_path = active_generation_path(temp.path());
        }
    }
    let previous = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let previous_path = active_generation_path(temp.path());
    // Combine genuine published slots with a schema-22 active slot. The first
    // publication is now outside the pointer pair and survives only by lease.
    copy_fixture(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/schema-22/index"),
        temp.path(),
    );
    let active = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let pointer =
        ActiveGenerationPointer::new(active.active().clone(), Some(previous.active().clone()))
            .unwrap();
    publish_active_generation_pointer(temp.path(), &pointer).unwrap();
    let pointer_bytes = fs::read(temp.path().join("active-generation.json")).unwrap();
    let active_path = active_generation_path(temp.path());

    // While a live reader holds `previous`, rebuild must keep `previous` in the
    // pointer and on disk alongside abandoned-candidate reclamation.
    let previous_reader =
        VerifiedIndex::open_pinned_generation(temp.path(), previous.active().generation_id())
            .unwrap();
    for _ in 0..5 {
        let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        assert!(writer.base_manifest().is_none());
        let candidate_path = temp
            .path()
            .join(INDEX_GENERATIONS_DIRECTORY)
            .join(writer.candidate_directory_name.as_ref().unwrap());
        assert_eq!(
            generation_directories(temp.path()),
            HashSet::from([
                active_path.clone(),
                previous_path.clone(),
                reader_path.clone(),
                candidate_path,
            ])
        );
        assert_eq!(reader.as_ref().unwrap().count_term("retained").unwrap(), 1);
        assert_eq!(previous_reader.count_term("retained").unwrap(), 1);
        assert_eq!(
            VerifiedIndex::open_pinned(temp.path())
                .unwrap()
                .count_term("schema22upgrademarker")
                .unwrap(),
            1
        );
        assert_eq!(
            fs::read(temp.path().join("active-generation.json")).unwrap(),
            pointer_bytes
        );
        drop(writer);
    }

    // Once `previous` has no live reader, opening the upgrade writer retires
    // `previous` and reclaims its directory while keeping `active` and any
    // externally leased reader generation intact.
    drop(previous_reader);
    {
        let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        let candidate_path = temp
            .path()
            .join(INDEX_GENERATIONS_DIRECTORY)
            .join(writer.candidate_directory_name.as_ref().unwrap());
        assert!(!previous_path.exists());
        assert_eq!(
            generation_directories(temp.path()),
            HashSet::from([active_path.clone(), reader_path.clone(), candidate_path])
        );
        let updated_pointer = load_active_generation_pointer(temp.path())
            .unwrap()
            .unwrap();
        assert_eq!(updated_pointer.active(), active.active());
        assert!(updated_pointer.previous().is_none());
        assert_eq!(
            VerifiedIndex::open_pinned(temp.path())
                .unwrap()
                .count_term("schema22upgrademarker")
                .unwrap(),
            1
        );
        assert_eq!(reader.as_ref().unwrap().count_term("retained").unwrap(), 1);
    }

    // Once the external reader drops too, peak on-disk generations during the
    // rebuild are bounded to two (`active` + `candidate`), and committing the
    // upgraded generation retains `active` as `previous` (still two dirs).
    drop(reader);
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert!(
        !reader_path.exists(),
        "released readers no longer retain obsolete slots"
    );
    assert_eq!(generation_directories(temp.path()).len(), 2);
    assert!(active_path.is_dir());
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "upgraded schema 23 marker"))
        .unwrap();
    writer.certify_source(certificate(&source, 3, 1)).unwrap();
    let receipt = writer.commit(|_| true).unwrap();
    let final_pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    assert_eq!(
        final_pointer.active().generation_id(),
        receipt.generation_id
    );
    assert_eq!(
        final_pointer.previous().unwrap().generation_id(),
        active.active().generation_id()
    );
    assert_eq!(generation_directories(temp.path()).len(), 2);
}

#[test]
fn upgrade_rebuild_preserves_previous_under_retention_lease_until_released() {
    let temp = tempdir().unwrap();
    let source = source("leased-previous-rebuild.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "leased previous marker"))
        .unwrap();
    writer.certify_source(certificate(&source, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let previous = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let previous_path = active_generation_path(temp.path());
    copy_fixture(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/schema-22/index"),
        temp.path(),
    );
    let active = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let active_path = active_generation_path(temp.path());
    let pointer =
        ActiveGenerationPointer::new(active.active().clone(), Some(previous.active().clone()))
            .unwrap();
    publish_active_generation_pointer(temp.path(), &pointer).unwrap();

    let owner_id = "ab".repeat(32);
    let lease = acquire_generation_retention_lease(
        temp.path(),
        previous.active().generation_id(),
        "semantic_projection",
        &owner_id,
    )
    .unwrap();

    {
        let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        let candidate_path = temp
            .path()
            .join(INDEX_GENERATIONS_DIRECTORY)
            .join(writer.candidate_directory_name.as_ref().unwrap());
        assert_eq!(
            generation_directories(temp.path()),
            HashSet::from([active_path.clone(), previous_path.clone(), candidate_path,])
        );
        assert_eq!(
            load_active_generation_pointer(temp.path())
                .unwrap()
                .unwrap()
                .previous()
                .unwrap()
                .generation_id(),
            previous.active().generation_id()
        );
    }

    release_generation_retention_lease(temp.path(), &lease).unwrap();

    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let candidate_path = temp
        .path()
        .join(INDEX_GENERATIONS_DIRECTORY)
        .join(writer.candidate_directory_name.as_ref().unwrap());
    assert!(!previous_path.exists());
    assert_eq!(
        generation_directories(temp.path()),
        HashSet::from([active_path, candidate_path])
    );
    assert!(load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap()
        .previous()
        .is_none());
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .count_term("schema22upgrademarker")
            .unwrap(),
        1
    );
    drop(writer);
}

#[test]
fn settings_upgrade_retires_unleased_previous_directory_while_preserving_flat_delta_base_manifest()
{
    let temp = tempdir().unwrap();
    let source = source("flat-delta-upgrade.jsonl");
    for (revision, body) in [(1, "delta baseline"), (2, "delta updated")] {
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer.add_core_record(document(&source, 1, body)).unwrap();
        writer
            .certify_source(appendable_certificate(&source, revision, 1, 10))
            .unwrap();
        writer.commit(|_| true).unwrap();
    }

    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let previous = pointer.previous().unwrap().clone();
    let active = pointer.active().clone();
    let active_manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(manifest_path(temp.path(), active.generation_id())).unwrap(),
    )
    .unwrap();
    assert_eq!(
        active_manifest["storage_format"],
        "ctx-manifest-flat-delta-v1"
    );
    assert_eq!(
        active_manifest["base_generation_id"],
        previous.generation_id()
    );

    // Hold a live reader on `active`, which pins `previous`'s manifest id key
    // for flat-delta resolution but does NOT pin `previous`'s index directory.
    let active_reader = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let active_path = active_generation_path(temp.path());
    let previous_path = temp
        .path()
        .join(INDEX_GENERATIONS_DIRECTORY)
        .join(previous.directory());

    // Induce an index-settings upgrade mismatch on `active`.
    let meta_path = active_path.join("meta.json");
    let mut meta: serde_json::Value =
        serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
    meta["index_settings"]["docstore_compression"] =
        serde_json::Value::String("zstd(compression_level=1)".to_owned());
    meta["index_settings"]["docstore_blocksize"] = serde_json::Value::from(64 * 1024);
    fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();

    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert!(writer.base_manifest().is_none());
    let candidate_path = temp
        .path()
        .join(INDEX_GENERATIONS_DIRECTORY)
        .join(writer.candidate_directory_name.as_ref().unwrap());
    assert!(
        !previous_path.exists(),
        "unleased previous directory must be reclaimed before full settings rebuild"
    );
    assert_eq!(
        generation_directories(temp.path()),
        HashSet::from([active_path, candidate_path])
    );
    assert!(
        manifest_path(temp.path(), previous.generation_id()).is_file(),
        "flat-delta base manifest must survive directory retirement"
    );
    assert_eq!(active_reader.count_term("updated").unwrap(), 1);
    drop(writer);
    drop(active_reader);
}

#[test]
fn rebuild_marker_retires_unleased_previous_directory_and_preserves_leased_previous_and_delta_base()
{
    let temp = tempdir().unwrap();
    let source = source("rebuild-marker-upgrade.jsonl");
    for (revision, body) in [(1, "marker baseline"), (2, "marker updated")] {
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        writer.begin_source(source.clone()).unwrap();
        writer.add_core_record(document(&source, 1, body)).unwrap();
        writer
            .certify_source(appendable_certificate(&source, revision, 1, 10))
            .unwrap();
        writer.commit(|_| true).unwrap();
    }

    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let previous = pointer.previous().unwrap().clone();
    let active = pointer.active().clone();
    let active_path = active_generation_path(temp.path());
    let previous_path = temp
        .path()
        .join(INDEX_GENERATIONS_DIRECTORY)
        .join(previous.directory());

    crate::writer_support::mark_active_generation_for_rebuild(temp.path(), &active).unwrap();

    // While a durable retention lease pins `previous`, rebuild opening must preserve it.
    let owner_id = "cd".repeat(32);
    let lease = acquire_generation_retention_lease(
        temp.path(),
        previous.generation_id(),
        "semantic_projection",
        &owner_id,
    )
    .unwrap();
    {
        let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        assert!(writer.base_manifest().is_none());
        let candidate_path = temp
            .path()
            .join(INDEX_GENERATIONS_DIRECTORY)
            .join(writer.candidate_directory_name.as_ref().unwrap());
        assert!(previous_path.is_dir());
        assert_eq!(
            generation_directories(temp.path()),
            HashSet::from([active_path.clone(), previous_path.clone(), candidate_path])
        );
    }

    release_generation_retention_lease(temp.path(), &lease).unwrap();

    // Once `previous` is unleased, rebuild opening retires and reclaims its
    // directory while preserving `active` and `previous`'s flat-delta base manifest.
    let active_reader = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert!(writer.base_manifest().is_none());
    let candidate_path = temp
        .path()
        .join(INDEX_GENERATIONS_DIRECTORY)
        .join(writer.candidate_directory_name.as_ref().unwrap());
    assert!(
        !previous_path.exists(),
        "unleased previous directory must be reclaimed when active-generation-rebuild-required.json is present"
    );
    assert_eq!(
        generation_directories(temp.path()),
        HashSet::from([active_path, candidate_path])
    );
    assert!(load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap()
        .previous()
        .is_none());
    assert!(
        manifest_path(temp.path(), previous.generation_id()).is_file(),
        "flat-delta base manifest must survive rebuild-marker retirement"
    );
    assert_eq!(active_reader.count_term("updated").unwrap(), 1);
    drop(writer);
    drop(active_reader);
}

#[test]
fn active_generation_requires_rebuild_classifies_schema_22_and_rebuild_marker() {
    let schema_22_dir = tempdir().unwrap();
    copy_fixture(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/schema-22/index"),
        schema_22_dir.path(),
    );
    let schema_22 = VerifiedIndex::open_pinned(schema_22_dir.path()).unwrap();
    assert_eq!(schema_22.manifest().lexical_schema_version, 22);
    assert!(active_generation_requires_rebuild(schema_22_dir.path(), &schema_22).unwrap());
    drop(schema_22);

    let current_dir = tempdir().unwrap();
    let src = source("rebuild-classifier.jsonl");
    let mut writer = GenerationWriter::open(current_dir.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(src.clone()).unwrap();
    writer
        .add_core_record(document(&src, 1, "clean schema 23 record"))
        .unwrap();
    writer.certify_source(certificate(&src, 1, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    let first = VerifiedIndex::open_pinned(current_dir.path()).unwrap();
    assert!(!active_generation_requires_rebuild(current_dir.path(), &first).unwrap());

    let pointer = load_active_generation_pointer(current_dir.path())
        .unwrap()
        .unwrap();
    writer_support::mark_active_generation_for_rebuild(current_dir.path(), pointer.active())
        .unwrap();
    assert!(active_generation_requires_rebuild(current_dir.path(), &first).unwrap());
    drop(first);

    // Publishing a fresh generation clears the marker for the active generation;
    // writing a stale marker for the previous generation must not mark the new active one.
    let mut writer = GenerationWriter::open(current_dir.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert!(writer.base_manifest().is_none());
    writer.begin_source(src.clone()).unwrap();
    writer
        .add_core_record(document(&src, 1, "rebuilt schema 23 record"))
        .unwrap();
    writer.certify_source(certificate(&src, 2, 1)).unwrap();
    writer.commit(|_| true).unwrap();

    writer_support::mark_active_generation_for_rebuild(current_dir.path(), pointer.active())
        .unwrap();
    let second = VerifiedIndex::open_pinned(current_dir.path()).unwrap();
    assert_ne!(second.generation_id(), pointer.active().generation_id());
    assert!(!active_generation_requires_rebuild(current_dir.path(), &second).unwrap());
}
