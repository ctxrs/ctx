use super::*;

#[test]
fn missing_and_corrupt_certification_force_one_rehash_then_reuse() {
    for corrupt in [false, true] {
        let (temp, _, _) = published_fixture(if corrupt {
            "corrupt-certification.jsonl"
        } else {
            "missing-certification.jsonl"
        });
        let certification = crate::publication::certification_file_for_active(temp.path()).unwrap();
        if corrupt {
            fs::write(&certification, b"{corrupt certification").unwrap();
        } else {
            fs::remove_file(&certification).unwrap();
        }

        crate::publication::reset_verification_activity();
        drop(VerifiedIndex::open_pinned(temp.path()).unwrap());
        assert_eq!(crate::publication::verification_activity().0, 1);
        assert!(crate::publication::hashed_artifact_bytes() > 0);
        assert!(certification.is_file());

        crate::publication::reset_verification_activity();
        drop(VerifiedIndex::open_pinned(temp.path()).unwrap());
        assert_eq!(crate::publication::verification_activity().0, 0);
        assert_eq!(crate::publication::hashed_artifact_bytes(), 0);
    }
}

#[test]
fn legacy_pointer_bound_certifications_rehash_once_into_generation_bound_format() {
    for legacy_version in [3, 4] {
        let (temp, _, _) =
            published_fixture(&format!("legacy-v{legacy_version}-certification.jsonl"));
        let pointer = load_active_generation_pointer(temp.path())
            .unwrap()
            .unwrap();
        let certification = crate::publication::certification_file_for_active(temp.path()).unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&certification).unwrap()).unwrap();
        value["version"] = serde_json::json!(legacy_version);
        value["pointer"] = serde_json::to_value(&pointer).unwrap();
        value["pointer_identity"] = value["manifest_identity"].clone();
        fs::write(&certification, serde_json::to_vec(&value).unwrap()).unwrap();

        crate::publication::reset_verification_activity();
        drop(VerifiedIndex::open_pinned(temp.path()).unwrap());
        assert_eq!(crate::publication::verification_activity().0, 1);
        assert!(crate::publication::hashed_artifact_bytes() > 0);

        let upgraded: serde_json::Value =
            serde_json::from_slice(&fs::read(&certification).unwrap()).unwrap();
        assert_eq!(upgraded["version"], 5);
        assert!(upgraded.get("pointer").is_none());
        assert!(upgraded.get("pointer_identity").is_none());
        crate::publication::reset_verification_activity();
        drop(VerifiedIndex::open_pinned(temp.path()).unwrap());
        assert_eq!(crate::publication::verification_activity().0, 0);
        assert_eq!(crate::publication::hashed_artifact_bytes(), 0);
    }
}

#[test]
fn oversized_and_overcount_certifications_fallback_without_unbounded_decode() {
    for overcount in [false, true] {
        let (temp, _, _) = published_fixture(if overcount {
            "overcount-certification.jsonl"
        } else {
            "oversized-certification.jsonl"
        });
        let certification = crate::publication::certification_file_for_active(temp.path()).unwrap();
        if overcount {
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&certification).unwrap()).unwrap();
            let artifacts = value
                .get_mut("artifacts")
                .and_then(serde_json::Value::as_array_mut)
                .unwrap();
            let artifact = artifacts.first().unwrap().clone();
            artifacts.clear();
            artifacts.resize(crate::publication::MAX_CERTIFIED_ARTIFACTS + 1, artifact);
            let bytes = serde_json::to_vec(&value).unwrap();
            assert!(bytes.len() <= crate::publication::MAX_CERTIFICATION_BYTES);
            fs::write(&certification, bytes).unwrap();
        } else {
            let file = fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&certification)
                .unwrap();
            file.set_len(
                u64::try_from(crate::publication::MAX_CERTIFICATION_BYTES)
                    .unwrap()
                    .saturating_add(1),
            )
            .unwrap();
        }

        crate::publication::reset_verification_activity();
        drop(VerifiedIndex::open_pinned(temp.path()).unwrap());
        assert_eq!(crate::publication::verification_activity().0, 1);
        assert!(crate::publication::hashed_artifact_bytes() > 0);
        assert_certification_is_bounded(&certification);

        crate::publication::reset_verification_activity();
        drop(VerifiedIndex::open_pinned(temp.path()).unwrap());
        assert_eq!(crate::publication::verification_activity().0, 0);
        assert_eq!(crate::publication::hashed_artifact_bytes(), 0);
    }
}

#[test]
fn generation_certification_ignores_pointer_inode_but_rehashes_generation_replacements() {
    let replacements = ["pointer", "manifest", "artifact"];
    for replacement in replacements {
        let (temp, _, _) = published_fixture(&format!("{replacement}-replacement.jsonl"));
        let pointer = load_active_generation_pointer(temp.path())
            .unwrap()
            .unwrap();
        let path = match replacement {
            "pointer" => temp.path().join("active-generation.json"),
            "manifest" => manifest_path(temp.path(), pointer.active().generation_id()),
            "artifact" => active_store_path(temp.path()),
            _ => unreachable!(),
        };
        replace_with_same_bytes(&path);

        crate::publication::reset_verification_activity();
        drop(VerifiedIndex::open_pinned(temp.path()).unwrap());
        if replacement == "pointer" {
            assert_eq!(crate::publication::verification_activity().0, 0);
            assert_eq!(crate::publication::hashed_artifact_bytes(), 0);
        } else {
            assert_eq!(crate::publication::verification_activity().0, 1);
            assert!(crate::publication::hashed_artifact_bytes() > 0);
        }
    }
}

#[test]
fn certification_sha_authority_must_recompute_to_the_exact_slot_digest() {
    let (temp, _, _) = published_fixture("certificate-digest-binding.jsonl");
    let certification = crate::publication::certification_file_for_active(temp.path()).unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&certification).unwrap()).unwrap();
    let sha = value
        .get_mut("artifacts")
        .and_then(serde_json::Value::as_array_mut)
        .and_then(|artifacts| artifacts.first_mut())
        .and_then(|artifact| artifact.get_mut("sha256"))
        .and_then(serde_json::Value::as_array_mut)
        .unwrap();
    let first = sha.first_mut().unwrap();
    *first = serde_json::Value::from(first.as_u64().unwrap() ^ 0x5a);
    fs::write(&certification, serde_json::to_vec(&value).unwrap()).unwrap();

    crate::publication::reset_verification_activity();
    drop(VerifiedIndex::open_pinned(temp.path()).unwrap());
    assert_eq!(crate::publication::verification_activity().0, 1);
    assert!(crate::publication::hashed_artifact_bytes() > 0);
}

#[cfg(unix)]
#[test]
fn same_size_restored_mtime_mutation_and_symlink_fail_closed() {
    use std::os::unix::fs::{symlink, MetadataExt as _};

    let (mutated, _, _) = published_fixture("same-metadata-mutation.jsonl");
    let store_path = active_store_path(mutated.path());
    let before = fs::metadata(&store_path).unwrap();
    let before_ctime = (before.ctime(), before.ctime_nsec());
    let modified = before.modified().unwrap();
    with_temporarily_writable(&store_path, || {
        let mut store = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&store_path)?;
        let mut byte = [0_u8; 1];
        store.read_exact(&mut byte)?;
        store.seek(std::io::SeekFrom::Start(0))?;
        byte[0] ^= 0x5a;
        store.write_all(&byte)?;
        store.set_times(FileTimes::new().set_modified(modified))?;
        store.sync_all()
    })
    .unwrap();
    let after = fs::metadata(&store_path).unwrap();
    assert_eq!(after.len(), before.len());
    assert_eq!(after.modified().unwrap(), modified);
    assert_ne!((after.ctime(), after.ctime_nsec()), before_ctime);

    crate::publication::reset_verification_activity();
    assert!(matches!(
        VerifiedIndex::open_pinned(mutated.path()),
        Err(IndexError::ChecksumMismatch)
    ));
    assert_eq!(crate::publication::verification_activity().0, 1);
    assert!(crate::publication::hashed_artifact_bytes() > 0);

    let (linked, _, _) = published_fixture("unsafe-artifact-link.jsonl");
    let linked_store = active_store_path(linked.path());
    let target = linked.path().join("external-store-copy");
    fs::copy(&linked_store, &target).unwrap();
    fs::remove_file(&linked_store).unwrap();
    symlink(&target, &linked_store).unwrap();
    crate::publication::reset_verification_activity();
    assert!(matches!(
        VerifiedIndex::open_pinned(linked.path()),
        Err(IndexError::ChecksumMismatch) | Err(IndexError::Tantivy(_))
    ));
    assert_eq!(crate::publication::hashed_artifact_bytes(), 0);
}

#[cfg(unix)]
#[test]
fn crc_valid_active_segment_mutation_is_rejected_before_pinning() {
    let (temp, _, _) = published_fixture("crc-valid-before-pinning.jsonl");
    rewrite_active_store_with_valid_crc(temp.path());

    let error = match GenerationWriter::open(temp.path(), WriterOptions::default()) {
        Ok(_) => panic!("mutated active segment unexpectedly became a writer base"),
        Err(error) => error,
    };
    assert!(matches!(error, IndexError::ChecksumMismatch));
    assert!(!temp
        .path()
        .join("active-generation-rebuild-required.json")
        .exists());
}

#[cfg(unix)]
#[test]
fn crc_valid_retained_segment_mutation_cannot_be_rebound_by_mutating_publication() {
    let (temp, source, baseline) = published_fixture("crc-valid-retained-base.jsonl");
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();
    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    append.begin_source_append(source.clone()).unwrap();

    rewrite_active_store_with_valid_crc(temp.path());
    let error = append
        .add_core_record(document(
            &source,
            2,
            "candidate must not rebind altered base bytes",
        ))
        .unwrap_err();
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
        .exists());
}
