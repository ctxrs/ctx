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
    let mut writer = GenerationWriter::open(root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = writer.begin_source_append(source.clone())?.clone();
    writer.add_core_record(document(source, 2, "candidate append body"))?;
    writer.certify_source_append(
        CertifiedSourceAppend::certify(
            &base,
            appendable_certificate(source, 2, 2, 20),
            10,
            [1; 32],
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

#[cfg(windows)]
#[test]
fn windows_clone_proof_capture_blocks_destination_substitution() {
    let (temp, source, _) = published_fixture("windows-proof-substitution.jsonl");
    let root = temp.path().to_owned();
    let active = active_generation_path(temp.path());
    let attempted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let blocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed_attempt = Arc::clone(&attempted);
    let observed_block = Arc::clone(&blocked);
    let _portable = PortableCloneTestGuard::set(
        PortableCloneTestOptions::default(),
        move |stage, relative| {
            if stage != PortableCloneStage::AfterCopy
                || relative
                    .extension()
                    .is_none_or(|extension| extension != "store")
                || observed_attempt.swap(true, Ordering::SeqCst)
            {
                return Ok(());
            }
            let target = fs::read_dir(root.join(INDEX_GENERATIONS_DIRECTORY))
                .unwrap()
                .map(|entry| entry.unwrap().path().join(relative))
                .find(|path| path.is_file() && !path.starts_with(&active))
                .expect("portable candidate must contain the copied proof target");
            let replacement = target.with_extension("ctx-proof-substitution");
            let mut bytes = fs::read(&target).unwrap();
            bytes[0] ^= 0x5a;
            let mut replacement_file = fs::File::create(&replacement).unwrap();
            replacement_file.write_all(&bytes).unwrap();
            replacement_file.sync_all().unwrap();
            drop(replacement_file);
            let delete_error = fs::OpenOptions::new()
                .access_mode(DELETE)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .open(&target)
                .unwrap_err();
            assert_eq!(delete_error.raw_os_error(), Some(32));
            let error = durable_atomic_replace_file(&replacement, &target).unwrap_err();
            assert_eq!(error.raw_os_error(), Some(5));
            fs::remove_file(replacement).unwrap();
            observed_block.store(true, Ordering::SeqCst);
            Ok(())
        },
    );

    let successor = append_one_record(temp.path(), &source).unwrap();
    assert!(attempted.load(Ordering::SeqCst));
    assert!(blocked.load(Ordering::SeqCst));
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .generation_id(),
        successor.generation_id
    );
}

#[cfg(windows)]
#[test]
fn windows_retained_writer_blocks_candidate_seal_before_publication() {
    for target in ["segment", "meta.json", ".managed.json"] {
        let (temp, source, baseline) =
            published_fixture(&format!("windows-retained-writer-{target}.jsonl"));
        let target_name = if target == "segment" {
            active_store_path(temp.path())
                .file_name()
                .unwrap()
                .to_owned()
        } else {
            target.into()
        };
        let retained_writer = Arc::new(Mutex::new(None));
        let captured_writer = Arc::clone(&retained_writer);

        let mut append = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        let base = append.begin_source_append(source.clone()).unwrap().clone();
        append.before_pointer_switch = Some(Box::new(move |candidate_path| {
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .open(candidate_path.join(target_name))
                .unwrap();
            *captured_writer.lock().unwrap() = Some(file);
        }));
        append
            .add_core_record(document(&source, 2, "candidate append body"))
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
        let error = append.commit(|_| true).unwrap_err();
        assert!(
            matches!(&error, IndexError::Io(error) if error.raw_os_error() == Some(32)),
            "retained writer for {target} must block the terminal seal: {error}"
        );

        let mut retained_writer = retained_writer.lock().unwrap().take().unwrap();
        retained_writer.seek(SeekFrom::Start(0)).unwrap();
        retained_writer.write_all(b"x").unwrap();
        retained_writer.sync_all().unwrap();
        assert_eq!(
            VerifiedIndex::open_pinned(temp.path())
                .unwrap()
                .generation_id(),
            baseline.generation_id
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_unproven_readonly_file_blocks_commit_before_pointer_publication() {
    let (temp, source, baseline) = published_fixture("windows-unproven-readonly.jsonl");
    let pointer_publication_attempted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed_attempt = Arc::clone(&pointer_publication_attempted);

    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = append.begin_source_append(source.clone()).unwrap().clone();
    append.after_candidate_commit = Some(Box::new(|candidate_path| {
        let path = candidate_path.join("unproven.store");
        let file = fs::File::create(path).unwrap();
        file.sync_all().unwrap();
        let mut permissions = file.metadata().unwrap().permissions();
        permissions.set_readonly(true);
        file.set_permissions(permissions).unwrap();
    }));
    append.before_pointer_publication = Some(Box::new(move |_| {
        observed_attempt.store(true, Ordering::SeqCst);
    }));
    append
        .add_core_record(document(&source, 2, "candidate append body"))
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
    let error = append.commit(|_| true).unwrap_err();
    assert!(matches!(
        error,
        IndexError::Io(ref error) if error.kind() == std::io::ErrorKind::PermissionDenied
    ));
    assert!(!pointer_publication_attempted.load(Ordering::SeqCst));
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .generation_id(),
        baseline.generation_id
    );
}

#[cfg(windows)]
#[test]
fn windows_validation_failure_after_terminal_seal_keeps_predecessor() {
    let (temp, source, baseline) = published_fixture("windows-terminal-validation.jsonl");
    let root = temp.path().to_owned();
    let baseline_manifest = manifest_path(&root, &baseline.generation_id);

    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = append.begin_source_append(source.clone()).unwrap().clone();
    append.before_pointer_publication = Some(Box::new(move |_| {
        let candidate_manifest = fs::read_dir(root.join(MANIFEST_DIRECTORY))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.is_file() && path != &baseline_manifest)
            .expect("candidate manifest must be durable before pointer publication");
        let bytes = mismatched_same_size_bytes(&candidate_manifest);
        fs::write(&candidate_manifest, bytes).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(candidate_manifest)
            .unwrap()
            .sync_all()
            .unwrap();
    }));
    append
        .add_core_record(document(&source, 2, "candidate append body"))
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
    let error = append.commit(|_| true).unwrap_err();
    assert!(matches!(error, IndexError::ChecksumMismatch));
    let predecessor = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_eq!(predecessor.generation_id(), baseline.generation_id);
    assert_eq!(predecessor.count_term("body").unwrap(), 1);
}

#[cfg(windows)]
#[test]
fn windows_successor_seals_allowlisted_files_before_publication() {
    let (temp, source, baseline) = published_fixture("windows-readonly-successor.jsonl");
    let held_reader = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let predecessor_store = active_store_path(temp.path());
    let retained_name = predecessor_store.file_name().unwrap().to_owned();
    assert!(
        fs::metadata(&predecessor_store)
            .unwrap()
            .permissions()
            .readonly(),
        "predecessor segment must be sealed read-only"
    );

    let mut append = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let base = append.begin_source_append(source.clone()).unwrap().clone();
    let clone_was_writable = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writable_before_terminal = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed_clone = Arc::clone(&clone_was_writable);
    let observed_terminal = Arc::clone(&writable_before_terminal);
    append.after_candidate_commit = Some(Box::new(move |candidate_path| {
        for candidate_file in publication_allowlisted_files(candidate_path) {
            assert!(candidate_file.is_file());
            assert!(
                !fs::metadata(candidate_file)
                    .unwrap()
                    .permissions()
                    .readonly(),
                "successor candidate files must remain writable through final sync"
            );
        }
        observed_clone.store(true, Ordering::SeqCst);
    }));
    append.before_pointer_publication = Some(Box::new(move |candidate_path| {
        for candidate_file in publication_allowlisted_files(candidate_path) {
            assert!(candidate_file.is_file());
            assert!(
                !fs::metadata(candidate_file)
                    .unwrap()
                    .permissions()
                    .readonly(),
                "candidate files must remain writable until the terminal atomic closure"
            );
        }
        observed_terminal.store(true, Ordering::SeqCst);
    }));
    append
        .add_core_record(document(&source, 2, "candidate append body"))
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
    let successor = append.commit(|_| true).unwrap();
    assert!(clone_was_writable.load(Ordering::SeqCst));
    assert!(writable_before_terminal.load(Ordering::SeqCst));
    assert_ne!(successor.generation_id, baseline.generation_id);
    let successor_path = active_generation_path(temp.path());
    let retained = successor_path.join(retained_name);
    assert!(
        retained.is_file(),
        "successor must retain predecessor segment"
    );
    assert!(
        fs::metadata(retained).unwrap().permissions().readonly(),
        "retained successor segment must preserve its read-only seal"
    );
    for active_file in publication_allowlisted_files(&successor_path) {
        assert!(
            fs::metadata(&active_file).unwrap().permissions().readonly(),
            "active successor file must remain sealed: {}",
            active_file.display()
        );
    }
    assert_certification_is_bounded(
        &crate::publication::certification_file_for_active(temp.path()).unwrap(),
    );
    assert_eq!(held_reader.count_term("body").unwrap(), 1);
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .count_term("body")
            .unwrap(),
        2
    );
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
    let logical_bytes = fs::read_dir(&generation)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_file())
        .map(|entry| entry.metadata().unwrap().len())
        .sum::<u64>();
    let writer_output_headroom = logical_bytes
        .saturating_add(WriterOptions::default().memory_bytes as u64)
        .saturating_add(16 * 1024 * 1024);
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
    assert!(
        metrics.required_headroom
            >= metrics
                .logical_bytes
                .saturating_mul(2)
                .saturating_add(WriterOptions::default().memory_bytes as u64)
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
    assert!(
        metrics.required_headroom
            >= metrics
                .logical_bytes
                .saturating_mul(2)
                .saturating_add(WriterOptions::default().memory_bytes as u64)
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
