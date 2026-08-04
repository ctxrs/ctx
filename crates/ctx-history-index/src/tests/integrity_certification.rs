use super::*;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
};

#[cfg(unix)]
use std::{
    fs::{FileTimes, OpenOptions},
    io::{Read, Seek, Write},
};

fn published_fixture(name: &str) -> (TempDir, SourceKey, CommitReceipt) {
    let temp = tempdir().unwrap();
    let source = source(name);
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "certified generation body"))
        .unwrap();
    writer
        .certify_source(appendable_certificate(&source, 1, 1, 10))
        .unwrap();
    let receipt = writer.commit(|_| true).unwrap();
    assert_certification_is_bounded(
        &crate::publication::certification_file_for_active(temp.path()).unwrap(),
    );
    (temp, source, receipt)
}

#[test]
fn repeated_open_and_exact_noop_read_zero_artifact_bodies() {
    crate::publication::reset_verification_activity();
    let (temp, source, receipt) = published_fixture("certified-replay.jsonl");
    assert_eq!(crate::publication::verification_activity().0, 1);
    assert!(crate::publication::hashed_artifact_bytes() > 0);

    crate::publication::reset_verification_activity();
    for _ in 0..3 {
        let reopened = VerifiedIndex::open_pinned(temp.path()).unwrap();
        assert_eq!(reopened.generation_id(), receipt.generation_id);
    }
    assert_eq!(crate::publication::verification_activity().0, 0);
    assert_eq!(crate::publication::hashed_artifact_bytes(), 0);

    let mut noop = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let constructions = Arc::clone(&noop.index_writer_constructions);
    let inventory = complete_inventory(&source, 1, vec![source.clone()]);
    noop.certify_complete_inventory(inventory.clone()).unwrap();
    stage_exact_replay(&mut noop, &source);
    noop.commit_with_complete_inventory_revalidation(|_| true, |current| current == &inventory)
        .unwrap();
    assert_eq!(constructions.load(Ordering::SeqCst), 0);
    assert_eq!(crate::publication::verification_activity().0, 0);
    assert_eq!(crate::publication::hashed_artifact_bytes(), 0);
}

#[test]
fn explicit_scrub_forces_one_full_hash_and_refreshes_reusable_authority() {
    let (temp, _, _) = published_fixture("explicit-integrity-scrub.jsonl");
    crate::publication::reset_verification_activity();
    drop(VerifiedIndex::scrub(temp.path()).unwrap());
    assert_eq!(crate::publication::verification_activity().0, 1);
    assert!(crate::publication::hashed_artifact_bytes() > 0);

    crate::publication::reset_verification_activity();
    drop(VerifiedIndex::open_pinned(temp.path()).unwrap());
    assert_eq!(crate::publication::verification_activity().0, 0);
    assert_eq!(crate::publication::hashed_artifact_bytes(), 0);
}

#[test]
fn each_new_generation_hashes_once_and_is_immediately_restart_reusable() {
    let (temp, source, baseline) = published_fixture("one-hash-generation.jsonl");
    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = append.begin_source_append(source.clone()).unwrap().clone();
    append
        .add_core_record(document(&source, 2, "new generation suffix"))
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

    crate::publication::reset_verification_activity();
    let appended = append.commit(|_| true).unwrap();
    assert_ne!(appended.generation_id, baseline.generation_id);
    assert_eq!(crate::publication::verification_activity().0, 1);
    assert!(crate::publication::hashed_artifact_bytes() > 0);

    crate::publication::reset_verification_activity();
    drop(VerifiedIndex::open_pinned(temp.path()).unwrap());
    drop(VerifiedIndex::open_pinned(temp.path()).unwrap());
    assert_eq!(crate::publication::verification_activity().0, 0);
    assert_eq!(crate::publication::hashed_artifact_bytes(), 0);
}

#[test]
fn unsafe_external_hardlink_fails_closed() {
    let (hardlinked, _, _) = published_fixture("unsafe-artifact-hardlink.jsonl");
    let hardlinked_store = active_store_path(hardlinked.path());
    fs::hard_link(
        &hardlinked_store,
        hardlinked.path().join("external-store-hardlink"),
    )
    .unwrap();
    crate::publication::reset_verification_activity();
    assert!(matches!(
        VerifiedIndex::open_pinned(hardlinked.path()),
        Err(IndexError::ChecksumMismatch)
    ));
    assert_eq!(crate::publication::hashed_artifact_bytes(), 0);
}

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
fn pointer_manifest_and_artifact_identity_replacement_force_rehash() {
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
        assert_eq!(crate::publication::verification_activity().0, 1);
        assert!(crate::publication::hashed_artifact_bytes() > 0);
    }
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
    let mut store = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&store_path)
        .unwrap();
    let mut byte = [0_u8; 1];
    store.read_exact(&mut byte).unwrap();
    store.seek(std::io::SeekFrom::Start(0)).unwrap();
    byte[0] ^= 0x5a;
    store.write_all(&byte).unwrap();
    store
        .set_times(FileTimes::new().set_modified(modified))
        .unwrap();
    store.sync_all().unwrap();
    drop(store);
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

fn active_store_path(root: &Path) -> PathBuf {
    fs::read_dir(active_generation_path(root))
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "store")
        })
        .unwrap()
}

fn replace_with_same_bytes(path: &Path) {
    let replacement = path.with_extension("ctx-identity-replacement");
    fs::write(&replacement, fs::read(path).unwrap()).unwrap();
    fs::File::open(&replacement).unwrap().sync_all().unwrap();
    durable_atomic_replace_file(&replacement, path).unwrap();
}

fn assert_certification_is_bounded(path: &Path) {
    let bytes = fs::read(path).unwrap();
    assert!(bytes.len() <= crate::publication::MAX_CERTIFICATION_BYTES);
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        value["artifacts"].as_array().unwrap().len() <= crate::publication::MAX_CERTIFIED_ARTIFACTS
    );
}
