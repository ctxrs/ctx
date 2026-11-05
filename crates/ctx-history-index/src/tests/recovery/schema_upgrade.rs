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
fn rebuild_reclaims_candidates_but_keeps_previous_and_live_reader_generations() {
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
        assert_eq!(
            VerifiedIndex::open_pinned(temp.path())
                .unwrap()
                .count_term("schema22upgrademarker")
                .unwrap(),
            1
        );
        assert!(VerifiedIndex::open_pinned_generation(
            temp.path(),
            previous.active().generation_id()
        )
        .is_ok());
        assert_eq!(
            fs::read(temp.path().join("active-generation.json")).unwrap(),
            pointer_bytes
        );
        drop(writer);
    }
    drop(reader);
    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert!(
        !reader_path.exists(),
        "released readers no longer retain obsolete slots"
    );
    assert_eq!(generation_directories(temp.path()).len(), 3);
    assert!(previous_path.is_dir());
    drop(writer);
}
