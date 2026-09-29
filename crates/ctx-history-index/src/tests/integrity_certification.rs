use super::*;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
};

#[cfg(unix)]
use std::{
    fs::{File, FileTimes, OpenOptions},
    io::{Read, Seek, Write},
};

#[cfg(windows)]
use std::{
    io::{Seek, SeekFrom, Write},
    os::windows::fs::OpenOptionsExt,
    sync::Mutex,
};

#[cfg(windows)]
const DELETE: u32 = 0x0001_0000;
#[cfg(windows)]
const FILE_SHARE_READ: u32 = 0x0000_0001;
#[cfg(windows)]
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
#[cfg(windows)]
const FILE_SHARE_DELETE: u32 = 0x0000_0004;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::publication::{CloneStage, CloneTestHookGuard, CloneTestOptions};
use crate::publication::{PortableCloneStage, PortableCloneTestGuard, PortableCloneTestOptions};

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
    assert!(
        fs::metadata(active_store_path(temp.path()))
            .unwrap()
            .permissions()
            .readonly(),
        "certified immutable segment artifacts must be sealed read-only"
    );
    (temp, source, receipt)
}

fn append_one_record(root: &Path, source: &SourceKey) -> Result<CommitReceipt> {
    append_record(root, source, 2)
}

fn append_record(root: &Path, source: &SourceKey, revision: u8) -> Result<CommitReceipt> {
    let mut writer = GenerationWriter::open(root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = writer.begin_source_append(source.clone())?.clone();
    let frontier = base.frontier().unwrap();
    writer.add_core_record(document(
        source,
        u64::from(revision),
        "candidate append body",
    ))?;
    writer.certify_source_append(
        CertifiedSourceAppend::certify(
            &base,
            appendable_certificate(
                source,
                revision,
                u64::from(revision),
                u64::from(revision) * 10,
            ),
            frontier.certified_prefix_bytes(),
            *frontier.certified_prefix_digest(),
        )
        .unwrap(),
    )?;
    writer.commit(|_| true)
}

#[cfg(windows)]
fn publication_allowlisted_files(generation_path: &Path) -> Vec<PathBuf> {
    let mut relative: Vec<PathBuf> =
        serde_json::from_slice(&fs::read(generation_path.join(".managed.json")).unwrap()).unwrap();
    relative.push(PathBuf::from(".managed.json"));
    relative.sort();
    relative.dedup();
    relative
        .into_iter()
        .map(|path| generation_path.join(path))
        .collect()
}

fn mismatched_same_size_managed_bytes(path: &Path) -> Vec<u8> {
    let mut bytes = fs::read(path).unwrap();
    let offset = bytes
        .windows(b"meta.json".len())
        .position(|window| window == b"meta.json")
        .expect("managed topology must contain meta.json");
    bytes[offset] = b'n';
    bytes
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn mismatched_same_size_bytes(path: &Path) -> Vec<u8> {
    let mut bytes = fs::read(path).unwrap();
    bytes[0] ^= 0x5a;
    bytes
}

fn overwrite_same_size_and_restore_mtime(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    let metadata = fs::metadata(path)?;
    assert_eq!(metadata.len(), bytes.len() as u64);
    let modified = metadata.modified()?;
    let mut file = fs::OpenOptions::new().write(true).open(path)?;
    file.write_all(bytes)?;
    file.set_times(std::fs::FileTimes::new().set_modified(modified))?;
    file.sync_all()
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

#[cfg(unix)]
#[test]
fn identical_replacement_preserves_certification_when_discarding_shared_files() {
    for corrupt_shared_artifact in [false, true] {
        let (temp, source, initial) = published_fixture("identical-replacement.jsonl");
        let active_path = active_generation_path(temp.path());
        let store_path = active_store_path(temp.path());
        let meta_bytes = fs::metadata(active_path.join("meta.json")).unwrap().len();
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        // This test owns disposal of a real candidate sharing the base's files.
        // Force ordinary replacement; differential no-op coverage is separate.
        writer.replacement_memory_bytes = 0;
        writer.begin_source(source.clone()).unwrap();
        writer
            .add_core_record(document(&source, 1, "certified generation body"))
            .unwrap();
        writer
            .certify_source(appendable_certificate(&source, 1, 1, 10))
            .unwrap();

        // Exercise shared-inode cleanup even where the native clone uses reflinks.
        // Prime the ordinary candidate proof before measuring the no-op commit.
        let candidate = temp
            .path()
            .join(INDEX_GENERATIONS_DIRECTORY)
            .join(writer.candidate_directory_name.as_ref().unwrap());
        for entry in fs::read_dir(&active_path).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') || name == "meta.json" {
                continue;
            }
            let candidate_file = candidate.join(&name);
            assert!(candidate_file.is_file());
            fs::remove_file(&candidate_file).unwrap();
            fs::hard_link(entry.path(), candidate_file).unwrap();
        }
        crate::publication::prime_candidate_physical_proof(
            &writer.index,
            &candidate,
            writer.active_pointer.as_ref(),
            writer.candidate_physical_proof.as_mut().unwrap(),
        )
        .unwrap();
        crate::publication::reset_verification_activity();
        let outcome = writer.commit(|_| {
            if corrupt_shared_artifact {
                let mut bytes = fs::read(&store_path).unwrap();
                bytes[0] ^= 0x5a;
                with_temporarily_writable(&store_path, || {
                    overwrite_same_size_and_restore_mtime(&store_path, &bytes)
                })
                .unwrap();
            }
            true
        });
        if corrupt_shared_artifact {
            assert!(matches!(
                outcome,
                Err(IndexError::ActiveGenerationNeedsRebuild { .. })
            ));
            assert_eq!(
                load_active_generation_pointer(temp.path())
                    .unwrap()
                    .unwrap()
                    .active()
                    .generation_id(),
                initial.generation_id
            );
            continue;
        }
        let reused = outcome.unwrap();

        assert_eq!(reused.generation_id, initial.generation_id);
        assert!(!candidate.exists());
        assert_eq!(
            VerifiedIndex::open_pinned(temp.path())
                .unwrap()
                .document_count(),
            1
        );
        assert_eq!(crate::publication::hashed_artifact_bytes(), meta_bytes);
    }
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
fn physical_audit_uses_explicit_topology_without_decoding_the_pointer_file() {
    let (temp, _, _) = published_fixture("explicit-topology-authority.jsonl");
    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let generation_path = crate::publication::slot_path(temp.path(), pointer.active());
    let index = open_slot_index(temp.path(), pointer.active()).unwrap();
    let unsupported_pointer = serde_json::json!({
        "version": 1,
        "active": {
            "generation_id": pointer.active().generation_id(),
            "directory": pointer.active().directory(),
        },
        "previous": null,
    });
    fs::write(
        temp.path().join("active-generation.json"),
        serde_json::to_vec(&unsupported_pointer).unwrap(),
    )
    .unwrap();

    assert!(matches!(
        load_active_generation_pointer(temp.path()),
        Err(IndexError::UnsupportedActiveGenerationPointer(1))
    ));
    for topology_authority in [None, Some(&pointer)] {
        assert_eq!(
            physical_integrity_digest(&index, &generation_path, topology_authority).unwrap(),
            pointer.active().physical_integrity_digest()
        );
    }
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

#[cfg(target_os = "linux")]
#[test]
fn append_reports_nonreflink_fallback_without_faking_hardlink_availability() {
    let (temp, source, _) = published_fixture("append-nonreflink-fallback.jsonl");
    crate::publication::reset_candidate_clone_metrics();
    let guard = CloneTestHookGuard::set(
        CloneTestOptions {
            force_reflink_fallback: true,
            ..CloneTestOptions::default()
        },
        |_, _| Ok(()),
    );

    append_one_record(temp.path(), &source).unwrap();
    let metrics = crate::publication::candidate_clone_metrics();
    drop(guard);
    assert_eq!(metrics.retained_reflinked_files, 0);
    if metrics.retained_hardlinked_files > 0 {
        assert_eq!(metrics.retained_copied_files, 0);
        assert_eq!(metrics.retained_copied_bytes, 0);
    } else {
        assert_eq!(metrics.retained_hardlinked_files, 0);
        assert!(metrics.retained_copied_files > 0);
        assert!(metrics.retained_copied_bytes > 0);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn managed_three_generation_reclamation_preserves_retained_certifications() {
    let (temp, source, _) = published_fixture("managed-reclamation-certification.jsonl");
    let first_directory = active_generation_path(temp.path());
    let guard = CloneTestHookGuard::set(
        CloneTestOptions {
            force_reflink_fallback: true,
            ..CloneTestOptions::default()
        },
        |_, _| Ok(()),
    );

    let second = append_record(temp.path(), &source, 2).unwrap();
    append_record(temp.path(), &source, 3).unwrap();
    let metrics = crate::publication::candidate_clone_metrics();
    drop(guard);

    assert!(metrics.retained_hardlinked_files > 0);
    assert!(
        !first_directory.exists(),
        "the third publication must reclaim its unretained first generation"
    );
    crate::publication::reset_verification_activity();
    let mut active = VerifiedIndex::open_pinned_with_retained_peer(temp.path()).unwrap();
    let previous = active
        .take_retained_generation_peer_for_reader()
        .unwrap()
        .expect("the active generation did not retain its predecessor");
    assert_eq!(previous.generation_id(), second.generation_id);
    drop((previous, active));
    assert_eq!(crate::publication::verification_activity().0, 0);
    assert_eq!(crate::publication::hashed_artifact_bytes(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn reclamation_does_not_rebind_a_restored_metadata_mutation() {
    let (temp, source, _) = published_fixture("managed-reclamation-mutation.jsonl");
    let first_directory = active_generation_path(temp.path());
    let guard = CloneTestHookGuard::set(
        CloneTestOptions {
            force_reflink_fallback: true,
            ..CloneTestOptions::default()
        },
        |_, _| Ok(()),
    );
    append_record(temp.path(), &source, 2).unwrap();

    let previous = active_generation_path(temp.path());
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = writer.begin_source_append(source.clone()).unwrap().clone();
    let frontier = base.frontier().unwrap();
    writer.after_pointer_switch = Some(Box::new(move |candidate| {
        let shared = fs::read_dir(candidate)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "store")
                    && previous.join(path.file_name().unwrap()).is_file()
                    && std::os::unix::fs::MetadataExt::ino(&fs::metadata(path).unwrap())
                        == std::os::unix::fs::MetadataExt::ino(
                            &fs::metadata(previous.join(path.file_name().unwrap())).unwrap(),
                        )
            })
            .expect("third generation must retain one hard-linked segment");
        let bytes = mismatched_same_size_bytes(&shared);
        with_temporarily_writable(&shared, || {
            overwrite_same_size_and_restore_mtime(&shared, &bytes)
        })
        .unwrap();
    }));
    writer
        .add_core_record(document(&source, 3, "candidate append body"))
        .unwrap();
    writer
        .certify_source_append(
            CertifiedSourceAppend::certify(
                &base,
                appendable_certificate(&source, 3, 3, 30),
                frontier.certified_prefix_bytes(),
                *frontier.certified_prefix_digest(),
            )
            .unwrap(),
        )
        .unwrap();
    writer.commit(|_| true).unwrap();
    drop(guard);

    assert!(!first_directory.exists());
    crate::publication::reset_verification_activity();
    assert!(matches!(
        VerifiedIndex::open_pinned(temp.path()),
        Err(IndexError::ChecksumMismatch)
    ));
    assert_eq!(crate::publication::verification_activity().0, 1);
    assert!(crate::publication::hashed_artifact_bytes() > 0);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn append_reports_copy_fallback_when_reflink_and_hardlink_are_forced_off() {
    let (temp, source, _) = published_fixture("append-copy-fallback.jsonl");
    crate::publication::reset_candidate_clone_metrics();
    let guard = CloneTestHookGuard::set(
        CloneTestOptions {
            force_reflink_fallback: true,
            force_hardlink_fallback: true,
            ..CloneTestOptions::default()
        },
        |_, _| Ok(()),
    );

    append_one_record(temp.path(), &source).unwrap();
    let metrics = crate::publication::candidate_clone_metrics();
    drop(guard);
    assert_eq!(metrics.retained_reflinked_files, 0);
    assert_eq!(metrics.retained_hardlinked_files, 0);
    assert!(metrics.retained_copied_files > 0);
    assert!(metrics.retained_copied_bytes > 0);
}

#[cfg(target_os = "linux")]
#[test]
fn hardlink_fallback_rejects_same_size_restored_mtime_mutation_before_link() {
    let (temp, source, _) = published_fixture("hardlink-prelink-mutation.jsonl");
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();
    let source_file = active_store_path(temp.path());
    let source_name = source_file.file_name().unwrap().to_owned();
    let mutation = mismatched_same_size_bytes(&source_file);
    let source_for_hook = source_file.clone();
    let mut mutated = false;
    let guard = CloneTestHookGuard::set(
        CloneTestOptions {
            force_reflink_fallback: true,
            ..CloneTestOptions::default()
        },
        move |stage, relative| {
            if stage == CloneStage::BeforeHardlink
                && relative == Path::new(&source_name)
                && !mutated
            {
                with_temporarily_writable(&source_for_hook, || {
                    overwrite_same_size_and_restore_mtime(&source_for_hook, &mutation)
                })?;
                mutated = true;
            }
            Ok(())
        },
    );

    let error = append_one_record(temp.path(), &source).unwrap_err();
    drop(guard);
    assert!(
        matches!(
            error,
            IndexError::ConcurrentGenerationChange | IndexError::ChecksumMismatch
        ),
        "{error:?}"
    );
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn unix_candidate_copies_authenticated_managed_plan_bytes_after_same_size_mutation() {
    let (temp, source, _) = published_fixture("managed-plan-unix.jsonl");
    let base_managed = active_generation_path(temp.path()).join(".managed.json");
    let mutation = mismatched_same_size_managed_bytes(&base_managed);
    let hook_path = base_managed.clone();
    let mut mutated = false;
    let guard = CloneTestHookGuard::set(CloneTestOptions::default(), move |stage, relative| {
        if stage == CloneStage::BeforeFile && relative == Path::new(".managed.json") && !mutated {
            overwrite_same_size_and_restore_mtime(&hook_path, &mutation)?;
            mutated = true;
        }
        Ok(())
    });

    append_one_record(temp.path(), &source).unwrap();
    drop(guard);
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .count_term("body")
            .unwrap(),
        2
    );
}

#[test]
fn portable_candidate_copies_authenticated_managed_plan_bytes_after_same_size_mutation() {
    let (temp, source, _) = published_fixture("managed-plan-portable.jsonl");
    let base_managed = active_generation_path(temp.path()).join(".managed.json");
    let mutation = mismatched_same_size_managed_bytes(&base_managed);
    let hook_path = base_managed.clone();
    let mut mutated = false;
    let guard = PortableCloneTestGuard::set(
        PortableCloneTestOptions::default(),
        move |stage, relative| {
            if stage == PortableCloneStage::BeforeCopy
                && relative == Path::new(".managed.json")
                && !mutated
            {
                overwrite_same_size_and_restore_mtime(&hook_path, &mutation)?;
                mutated = true;
            }
            Ok(())
        },
    );

    append_one_record(temp.path(), &source).unwrap();
    drop(guard);
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .count_term("body")
            .unwrap(),
        2
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn copy_fallback_rechecks_corpus_and_writer_headroom_before_copying() {
    let (temp, source, baseline) = published_fixture("copy-admission-recheck.jsonl");
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();
    let generation = active_generation_path(temp.path());
    let writer_output_headroom =
        (WriterOptions::default().memory_bytes as u64).saturating_add(16 * 1024 * 1024);
    let rechecked_available_bytes = writer_output_headroom
        .saturating_add(fs::metadata(generation.join("meta.json")).unwrap().len());
    let guard = CloneTestHookGuard::set(
        CloneTestOptions {
            force_reflink_fallback: true,
            force_hardlink_fallback: true,
            available_bytes: Some(u64::MAX),
            rechecked_available_bytes: Some(rechecked_available_bytes),
            ..CloneTestOptions::default()
        },
        |stage, relative| {
            if stage == CloneStage::BeforeCopy && relative != Path::new(".managed.json") {
                panic!("copy fallback began before its terminal disk recheck");
            }
            Ok(())
        },
    );

    let error = append_one_record(temp.path(), &source).unwrap_err();
    let metrics = guard.metrics();
    drop(guard);
    assert!(matches!(
        error,
        IndexError::CurrentRepublishInsufficientHeadroom {
            available,
            required
        } if available == rechecked_available_bytes && required > available
    ));
    assert!(metrics.required_headroom < metrics.logical_bytes + writer_output_headroom);
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .generation_id(),
        baseline.generation_id
    );
}

#[test]
fn portable_copy_rechecks_corpus_and_writer_headroom_before_copying() {
    let (temp, source, baseline) = published_fixture("portable-copy-admission-recheck.jsonl");
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();
    let guard = PortableCloneTestGuard::set(
        PortableCloneTestOptions {
            available_bytes: Some(u64::MAX),
            rechecked_available_bytes: Some(0),
        },
        |stage, _| {
            if stage == PortableCloneStage::BeforeCopy {
                panic!("portable copy began before its terminal disk recheck");
            }
            Ok(())
        },
    );

    let error = append_one_record(temp.path(), &source).unwrap_err();
    let metrics = guard.metrics();
    drop(guard);
    assert!(matches!(
        error,
        IndexError::CurrentRepublishInsufficientHeadroom {
            available: 0,
            required
        } if required > metrics.logical_bytes
    ));
    assert_eq!(
        metrics.required_headroom,
        metrics.logical_bytes + WriterOptions::default().memory_bytes as u64 + 16 * 1024 * 1024
    );
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .generation_id(),
        baseline.generation_id
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn append_rejects_source_directory_swap_after_authenticated_source_open() {
    let (temp, source, _) = published_fixture("append-source-directory-swap.jsonl");
    let held_reader = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let active = active_generation_path(temp.path());
    let displaced = temp.path().join("append-displaced-active-generation");
    let hook_active = active.clone();
    let hook_displaced = displaced.clone();
    let mut swapped = false;
    let guard = CloneTestHookGuard::set(CloneTestOptions::default(), move |stage, _| {
        if stage == CloneStage::AfterSourceOpen && !swapped {
            fs::rename(&hook_active, &hook_displaced)?;
            fs::create_dir(&hook_active)?;
            swapped = true;
        }
        Ok(())
    });

    let error = append_one_record(temp.path(), &source).unwrap_err();
    assert!(
        matches!(
            error,
            IndexError::CurrentRepublishSourceTopology(_)
                | IndexError::ConcurrentGenerationChange
                | IndexError::ChecksumMismatch
        ),
        "{error:?}"
    );
    assert_eq!(held_reader.generation_id().len(), 64);
    drop(guard);
    fs::remove_dir(&active).unwrap();
    fs::rename(displaced, active).unwrap();
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .count_term("body")
            .unwrap(),
        1
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn append_copy_fallback_rejects_source_growth_after_authenticated_open() {
    use std::io::Write as _;

    let (temp, source, _) = published_fixture("append-copy-growth-rejection.jsonl");
    let source_file = active_store_path(temp.path());
    let source_name = source_file.file_name().unwrap().to_owned();
    let original_bytes = fs::metadata(&source_file).unwrap().len();
    let source_for_hook = source_file.clone();
    let mut grew = false;
    let guard = CloneTestHookGuard::set(
        CloneTestOptions {
            force_reflink_fallback: true,
            force_hardlink_fallback: true,
            ..CloneTestOptions::default()
        },
        move |stage, relative| {
            if stage == CloneStage::AfterSourceOpen && relative == Path::new(&source_name) && !grew
            {
                with_temporarily_writable(&source_for_hook, || {
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(&source_for_hook)?
                        .write_all(b"growth-after-authenticated-open")
                })?;
                grew = true;
            }
            Ok(())
        },
    );

    let error = append_one_record(temp.path(), &source).unwrap_err();
    assert!(
        matches!(
            error,
            IndexError::CurrentRepublishSourceTopology("source file grew while cloning")
                | IndexError::ConcurrentGenerationChange
                | IndexError::ChecksumMismatch
        ),
        "{error:?}"
    );
    drop(guard);
    with_temporarily_writable(&source_file, || {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&source_file)?
            .set_len(original_bytes)
    })
    .unwrap();
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

#[cfg(unix)]
fn rewrite_active_store_with_valid_crc(root: &Path) -> PathBuf {
    const FOOTER_MAGIC: u32 = 1337;

    let path = active_store_path(root);
    let mut bytes = fs::read(&path).unwrap();
    assert!(bytes.len() > 8);
    let trailer = bytes.len() - 8;
    let footer_len = u32::from_le_bytes(bytes[trailer..trailer + 4].try_into().unwrap()) as usize;
    assert_eq!(
        u32::from_le_bytes(bytes[trailer + 4..].try_into().unwrap()),
        FOOTER_MAGIC
    );
    let footer_start = trailer.checked_sub(footer_len).unwrap();
    assert!(footer_start > 0);
    let mut footer: serde_json::Value =
        serde_json::from_slice(&bytes[footer_start..trailer]).unwrap();
    bytes[footer_start / 2] ^= 0x5a;
    footer["crc"] = serde_json::Value::from(crc32fast::hash(&bytes[..footer_start]));
    let footer = serde_json::to_vec(&footer).unwrap();
    bytes.truncate(footer_start);
    bytes.extend_from_slice(&footer);
    bytes.extend_from_slice(&u32::try_from(footer.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(&FOOTER_MAGIC.to_le_bytes());
    with_temporarily_writable(&path, || {
        fs::write(&path, bytes)?;
        File::open(&path)?.sync_all()
    })
    .unwrap();
    path
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

mod headroom;

#[cfg(target_os = "linux")]
mod lifecycle;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod managed_unlinks;
mod validation;
#[cfg(windows)]
mod windows;
