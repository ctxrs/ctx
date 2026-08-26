use super::*;

#[test]
fn native_clone_enforces_the_entry_cap_during_enumeration() {
    let predecessor = GoldenPredecessor::copy();
    fill_generation_past_republish_entry_cap(&active_generation_path(predecessor.root()));
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::CurrentRepublishFileLimit {
            actual: 4_097,
            maximum: 4_096
        }
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn forced_copy_fallback_is_bounded_instrumented_and_migrates() {
    let predecessor = GoldenPredecessor::copy();
    let hook = CloneTestHookGuard::set(
        CloneTestOptions {
            force_copy: true,
            force_reflink_fallback: false,
            force_hardlink_fallback: false,
            available_bytes: None,
            rechecked_available_bytes: None,
        },
        |_, _| Ok(()),
    );

    let writer = open_republish_writer(predecessor.root())
        .unwrap()
        .into_writer()
        .unwrap();
    let metrics: CloneMetrics = hook.metrics();
    assert!(metrics.planned_files > 2);
    assert_eq!(metrics.linked_files, 0);
    assert_eq!(metrics.copied_files, metrics.planned_files);
    assert_eq!(metrics.copied_bytes, metrics.logical_bytes);
    assert!(metrics.required_headroom > metrics.logical_bytes);
    assert!(metrics.available_bytes >= metrics.required_headroom);
    assert!(writer.base_manifest().is_some());
    assert_eq!(
        VerifiedIndex::open(predecessor.root())
            .unwrap()
            .count_term("evidence")
            .unwrap(),
        3
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn forced_copy_write_failures_preserve_base_pointer_and_queries() {
    for raw_error in [libc::ENOSPC, libc::EIO] {
        let predecessor = GoldenPredecessor::copy();
        let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
        let fault = CloneTestHookGuard::set(
            CloneTestOptions {
                force_copy: true,
                force_reflink_fallback: false,
                force_hardlink_fallback: false,
                available_bytes: None,
                rechecked_available_bytes: None,
            },
            move |stage, _| {
                if stage == CloneStage::BeforeCopy {
                    return Err(io::Error::from_raw_os_error(raw_error).into());
                }
                Ok(())
            },
        );

        assert!(matches!(
            open_writer_error(predecessor.root()),
            IndexError::Io(ref error) if error.raw_os_error() == Some(raw_error)
        ));
        assert_eq!(
            fs::read(predecessor.root().join("active-generation.json")).unwrap(),
            pointer_before
        );
        assert_eq!(
            VerifiedIndex::open(predecessor.root())
                .unwrap()
                .count_term("evidence")
                .unwrap(),
            3
        );
        drop(fault);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn native_copy_detects_growth_without_writing_past_authenticated_length() {
    use std::io::Write as _;

    let predecessor = GoldenPredecessor::copy();
    let source_generation = active_generation_path(predecessor.root());
    let source_file = fs::read_dir(&source_generation)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().and_then(|extension| extension.to_str()) == Some("store"))
        .unwrap();
    let source_name = source_file.file_name().unwrap().to_owned();
    let original_bytes = fs::metadata(&source_file).unwrap().len();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let source_for_hook = source_file.clone();
    let mut grew = false;
    let guard = CloneTestHookGuard::set(
        CloneTestOptions {
            force_copy: true,
            force_reflink_fallback: false,
            force_hardlink_fallback: false,
            available_bytes: None,
            rechecked_available_bytes: None,
        },
        move |stage, relative| {
            if stage == CloneStage::BeforeCopy && relative == Path::new(&source_name) && !grew {
                with_temporarily_writable(&source_for_hook, || {
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(&source_for_hook)?
                        .write_all(b"growth-after-authentication")
                })?;
                grew = true;
            }
            Ok(())
        },
    );

    let error = open_writer_error(predecessor.root());
    assert!(
        matches!(
            error,
            IndexError::CurrentRepublishSourceTopology("source file grew while cloning")
                | IndexError::ConcurrentGenerationChange
                | IndexError::ChecksumMismatch
        ),
        "{error:?}"
    );
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    drop(guard);
    with_temporarily_writable(&source_file, || {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&source_file)?
            .set_len(original_bytes)
    })
    .unwrap();
    assert_eq!(
        VerifiedIndex::open(predecessor.root())
            .unwrap()
            .count_term("evidence")
            .unwrap(),
        3
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn native_candidate_replacement_is_rejected_and_cleanup_preserves_replacement() {
    for replacement_stage in [
        RepublishStage::AfterCandidateCreation,
        RepublishStage::BeforeCandidateCommit,
        RepublishStage::BeforeCandidateSync,
        RepublishStage::BeforeCandidateVerification,
        RepublishStage::BeforePointerPublication,
    ] {
        let predecessor = GoldenPredecessor::copy();
        let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
        let replacement = std::rc::Rc::new(std::cell::RefCell::new(None::<(PathBuf, PathBuf)>));
        let replacement_for_hook = std::rc::Rc::clone(&replacement);
        let mut replaced = false;
        let hook = RepublishTestHookGuard::set(move |stage, path| {
            if stage == replacement_stage && !replaced {
                let candidate = path.unwrap().to_path_buf();
                let displaced = candidate.with_file_name(format!(
                    "{}-authenticated-orphan",
                    candidate.file_name().unwrap().to_string_lossy()
                ));
                fs::rename(&candidate, &displaced)?;
                fs::create_dir(&candidate)?;
                fs::write(
                    candidate.join("replacement-sentinel"),
                    b"must survive native cleanup",
                )?;
                *replacement_for_hook.borrow_mut() = Some((candidate, displaced));
                replaced = true;
            }
            Ok(())
        });

        assert!(matches!(
            open_writer_error(predecessor.root()),
            IndexError::CurrentRepublishSourceTopology(
                "active generation directory changed during republish"
            )
        ));
        assert_eq!(
            fs::read(predecessor.root().join("active-generation.json")).unwrap(),
            pointer_before,
            "pointer changed at {replacement_stage:?}"
        );
        let (replacement, displaced) = replacement.borrow().clone().unwrap();
        assert_eq!(
            fs::read(replacement.join("replacement-sentinel")).unwrap(),
            b"must survive native cleanup",
            "replacement was mutated at {replacement_stage:?}"
        );
        fs::remove_dir_all(displaced).unwrap();
        assert_eq!(
            VerifiedIndex::open(predecessor.root())
                .unwrap()
                .count_term("evidence")
                .unwrap(),
            3
        );
        drop(hook);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn insufficient_clone_headroom_is_rejected_before_writes() {
    let predecessor = GoldenPredecessor::copy();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let fault = CloneTestHookGuard::set(
        CloneTestOptions {
            force_copy: false,
            force_reflink_fallback: false,
            force_hardlink_fallback: false,
            available_bytes: Some(0),
            rechecked_available_bytes: None,
        },
        |_, _| panic!("headroom rejection must precede clone work"),
    );

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::CurrentRepublishInsufficientHeadroom {
            available: 0,
            required
        } if required > 0
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(
        VerifiedIndex::open(predecessor.root())
            .unwrap()
            .count_term("evidence")
            .unwrap(),
        3
    );
    drop(fault);
}

#[test]
fn pointer_directory_fsync_error_returns_committed_visible_status_and_restarts_current() {
    let predecessor = GoldenPredecessor::copy();
    let fault = AtomicWriteTestHookGuard::set(|stage, target| {
        if stage == AtomicWriteStage::AfterReplaceBeforeDirectorySync
            && target.file_name().and_then(|name| name.to_str()) == Some("active-generation.json")
        {
            return Err(io::Error::from_raw_os_error(5));
        }
        Ok(())
    });

    let outcome = open_republish_writer(predecessor.root()).unwrap();
    let recovery = outcome
        .committed_republish_recovery()
        .expect("visible pointer durability uncertainty must be reported as committed")
        .clone();
    let writer = outcome.into_writer().unwrap();
    assert_eq!(
        recovery.generation_id(),
        writer.base_manifest().unwrap().generation_id().unwrap()
    );
    assert_eq!(recovery.generation_id(), predecessor.generation_id());
    drop(writer);
    drop(fault);

    let current = VerifiedIndex::open(predecessor.root()).unwrap();
    assert_eq!(current.count_term("evidence").unwrap(), 3);
    let restarted = GenerationWriter::open(predecessor.root(), WriterOptions::default()).unwrap();
    assert!(matches!(restarted, GenerationWriterOpenOutcome::Ready(_)));
    drop(restarted.into_writer().unwrap());
}

#[test]
fn malformed_pointer_reload_after_visibility_is_repaired_as_committed_outcome() {
    let predecessor = GoldenPredecessor::copy();
    let failed_once = std::rc::Rc::new(std::cell::Cell::new(false));
    let atomic_failed_once = std::rc::Rc::clone(&failed_once);
    let atomic_fault = AtomicWriteTestHookGuard::set(move |stage, target| {
        if stage == AtomicWriteStage::AfterReplaceBeforeDirectorySync
            && target.file_name().and_then(|name| name.to_str()) == Some("active-generation.json")
            && !atomic_failed_once.replace(true)
        {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        Ok(())
    });
    let reconciliation_fault = PointerReconciliationTestHookGuard::set(|root| {
        fs::write(root.join("active-generation.json"), b"{malformed-pointer")?;
        load_active_generation_pointer(root)
    });

    let outcome = open_republish_writer(predecessor.root())
        .expect("visible republish must not be reported as an ordinary error");
    assert!(matches!(
        &outcome,
        RepublishWriterOpenOutcome::CommittedVisible { .. }
    ));
    drop(outcome.into_writer().unwrap());
    drop(reconciliation_fault);
    drop(atomic_fault);
}

#[test]
fn unreadable_pointer_reconciliation_and_failed_repair_require_committed_recovery() {
    let predecessor = GoldenPredecessor::copy();
    let pointer_visible = std::rc::Rc::new(std::cell::Cell::new(false));
    let atomic_pointer_visible = std::rc::Rc::clone(&pointer_visible);
    let atomic_fault = AtomicWriteTestHookGuard::set(move |stage, target| {
        if target.file_name().and_then(|name| name.to_str()) != Some("active-generation.json") {
            return Ok(());
        }
        if stage == AtomicWriteStage::AfterReplaceBeforeDirectorySync
            && !atomic_pointer_visible.replace(true)
        {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        if stage == AtomicWriteStage::BeforeTemporaryWrite && atomic_pointer_visible.get() {
            return Err(io::Error::from_raw_os_error(libc::ENOSPC));
        }
        Ok(())
    });
    let reconciliation_fault = PointerReconciliationTestHookGuard::set(|_| {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected unreadable replacement pointer",
        )
        .into())
    });

    let outcome = open_republish_writer(predecessor.root())
        .expect("postvisibility unknown state must be a committed outcome");
    let recovery = match outcome {
        RepublishWriterOpenOutcome::CommittedRecoveryRequired { recovery } => recovery,
        _ => panic!("failed pointer repair unexpectedly produced a usable writer"),
    };
    assert_eq!(recovery.generation_id(), predecessor.generation_id());
    assert!(recovery.detail().contains("pointer reload failed"));
    assert!(recovery.detail().contains("pointer repair failed"));
    drop(reconciliation_fault);
    drop(atomic_fault);
    let restarted = GenerationWriter::open(predecessor.root(), WriterOptions::default()).unwrap();
    assert!(matches!(restarted, GenerationWriterOpenOutcome::Ready(_)));
}

#[test]
fn atomic_pointer_rename_failure_is_previsibility_and_preserves_predecessor_bytes() {
    let predecessor = GoldenPredecessor::copy();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let fault = AtomicWriteTestHookGuard::set(|stage, target| {
        if stage == AtomicWriteStage::BeforeReplace
            && target.file_name().and_then(|name| name.to_str()) == Some("active-generation.json")
        {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        Ok(())
    });

    assert!(matches!(
        open_writer_error(predecessor.root()),
        IndexError::Io(ref error) if error.raw_os_error() == Some(libc::EIO)
    ));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    drop(fault);
}

#[test]
fn post_publication_cleanup_failure_is_best_effort_and_restart_keeps_current_authority() {
    let predecessor = GoldenPredecessor::copy();
    let fault = RepublishTestHookGuard::set(|stage, _| {
        if stage == RepublishStage::PostPublicationCleanup {
            return Err(io::Error::other("injected post-publication cleanup failure").into());
        }
        Ok(())
    });

    let outcome = open_republish_writer(predecessor.root()).unwrap();
    assert!(outcome.committed_republish_recovery().is_none());
    let writer = outcome.into_writer().unwrap();
    drop(writer);
    drop(fault);
    let current_id = VerifiedIndex::active_generation_id(predecessor.root())
        .unwrap()
        .unwrap();
    assert_eq!(current_id, predecessor.generation_id());
    drop(
        GenerationWriter::open(predecessor.root(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap(),
    );
    assert_eq!(
        VerifiedIndex::active_generation_id(predecessor.root())
            .unwrap()
            .unwrap(),
        current_id
    );
}

pub(super) fn subprocess_paths(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    (
        root.join("republish-child.marker"),
        root.join("republish-child.continue"),
        root.join("republish-child.result"),
    )
}

fn pause_subprocess(marker: &Path, continue_path: &Path, witness: &str) -> io::Result<()> {
    fs::write(marker, witness)?;
    while !continue_path.exists() {
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn is_candidate_certification_target(target: &Path) -> bool {
    target
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        == Some("integrity-certifications")
        && target
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".physical-certification.json"))
}

fn publish_subprocess_successor(
    root: &Path,
    report_progress: impl FnMut(PublicationStage) -> Result<()>,
) -> Result<PublishedGeneration> {
    let source = source("golden-predecessor.jsonl");
    let mut writer = GenerationWriter::open(root, WriterOptions::default())?
        .into_writer()
        .map_err(|_| IndexError::WriterInvariant("unexpected committed recovery"))?;
    writer.begin_source(source.clone())?;
    for sequence in 1..=4 {
        writer.add_core_record(document(
            &source,
            sequence,
            &format!("certified candidate evidence {sequence}"),
        ))?;
    }
    writer.certify_source(certificate(&source, 2, 4))?;
    writer.commit_with_complete_inventory_revalidation_and_publication_metadata_and_progress(
        |_| true,
        |_| true,
        |_| Ok(PUBLICATION_METADATA.to_vec()),
        report_progress,
    )
}

#[test]
fn candidate_certification_publication_subprocess_worker() {
    let Ok(mode) = env::var(SUBPROCESS_MODE_ENV) else {
        return;
    };
    let root = PathBuf::from(env::var_os(SUBPROCESS_ROOT_ENV).unwrap());
    let marker = PathBuf::from(env::var_os(SUBPROCESS_MARKER_ENV).unwrap());
    let continue_path = PathBuf::from(env::var_os(SUBPROCESS_CONTINUE_ENV).unwrap());
    let result = PathBuf::from(env::var_os(SUBPROCESS_RESULT_ENV).unwrap());
    let pause_after_certification = mode == "pause-after-candidate-certification";
    let progress_marker = marker.clone();
    let progress_continue_path = continue_path.clone();
    let atomic_guard = if mode == "pause-candidate-certification" {
        Some(AtomicWriteTestHookGuard::set(move |stage, target| {
            if stage == AtomicWriteStage::AfterTemporarySyncBeforeReplace
                && is_candidate_certification_target(target)
            {
                pause_subprocess(
                    &marker,
                    &continue_path,
                    "candidate-certification-temp-synced",
                )?;
            }
            Ok(())
        }))
    } else if mode == "fail-candidate-certification-write" {
        Some(AtomicWriteTestHookGuard::set(|stage, target| {
            if stage == AtomicWriteStage::BeforeTemporaryWrite
                && is_candidate_certification_target(target)
            {
                return Err(io::Error::from_raw_os_error(libc::ENOSPC));
            }
            Ok(())
        }))
    } else if pause_after_certification {
        None
    } else {
        panic!("unknown candidate certification child mode {mode}");
    };
    let detail = match publish_subprocess_successor(&root, move |stage| {
        if pause_after_certification && stage == PublicationStage::Activation {
            pause_subprocess(
                &progress_marker,
                &progress_continue_path,
                "candidate-certified-and-reopened",
            )?;
        }
        Ok(())
    }) {
        Ok(published) => format!(
            "PUBLISHED {} {}",
            published.receipt().generation_id,
            published.verified_index().generation_id()
        ),
        Err(error) => format!("ERROR {error:?}\n{error}"),
    };
    fs::write(result, detail).unwrap();
    drop(atomic_guard);
}

#[test]
fn candidate_certification_reader_subprocess_worker() {
    let Some(root) = env::var_os("CTX_CANDIDATE_CERTIFICATION_READER_ROOT").map(PathBuf::from)
    else {
        return;
    };
    let detail = match VerifiedIndex::open_pinned(&root) {
        Ok(index) => format!(
            "READER {} {}",
            index.generation_id(),
            index.document_count()
        ),
        Err(error) => format!("ERROR {error:?}\n{error}"),
    };
    fs::write(root.join("candidate-certification-reader.result"), detail).unwrap();
}

#[test]
fn certified_candidate_reader_subprocess_worker() {
    let Some(root) = env::var_os("CTX_CERTIFIED_CANDIDATE_READER_ROOT").map(PathBuf::from) else {
        return;
    };
    let result = root.join("certified-candidate-reader.result");
    let detail = (|| -> Result<String> {
        let pointer = load_active_generation_pointer(&root)?
            .ok_or(IndexError::MissingActiveGenerationPointer)?;
        let predecessor_fence =
            ctx_history_index_generation::ActiveGenerationPointerFence::capture(
                &root,
                Some(&pointer),
            )?;
        let certification_directory = root.join("integrity-certifications");
        let mut candidate_slot = None;
        for entry in fs::read_dir(certification_directory)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_slice(&fs::read(entry.path())?)?;
            let Some(slot) = value.get("slot") else {
                continue;
            };
            let slot: GenerationSlot = serde_json::from_value(slot.clone())?;
            if slot.generation_id() != pointer.active().generation_id()
                && candidate_slot.replace(slot).is_some()
            {
                return Err(IndexError::WriterInvariant(
                    "multiple inactive certified candidates",
                ));
            }
        }
        let slot = candidate_slot.ok_or(IndexError::WriterInvariant(
            "inactive certified candidate missing",
        ))?;
        let index = VerifiedIndex::open_certified_candidate_before_activation(
            &root,
            &predecessor_fence,
            &slot,
        )?;
        Ok(format!(
            "CANDIDATE {} {}",
            index.generation_id(),
            index.document_count()
        ))
    })()
    .unwrap_or_else(|error| format!("ERROR {error:?}\n{error}"));
    fs::write(result, detail).unwrap();
}

#[test]
fn predecessor_republish_subprocess_worker() {
    let Ok(mode) = env::var(SUBPROCESS_MODE_ENV) else {
        return;
    };
    let root = PathBuf::from(env::var_os(SUBPROCESS_ROOT_ENV).unwrap());
    let marker = PathBuf::from(env::var_os(SUBPROCESS_MARKER_ENV).unwrap());
    let continue_path = PathBuf::from(env::var_os(SUBPROCESS_CONTINUE_ENV).unwrap());
    let result = PathBuf::from(env::var_os(SUBPROCESS_RESULT_ENV).unwrap());

    let mut republish_guard = None;
    let mut atomic_guard = None;
    if let Some(stage_name) = mode.strip_prefix("pause-republish:") {
        let stage_name = stage_name.to_owned();
        let marker = marker.clone();
        let continue_path = continue_path.clone();
        republish_guard = Some(RepublishTestHookGuard::set(move |stage, _| {
            if format!("{stage:?}") == stage_name {
                pause_subprocess(&marker, &continue_path, &stage_name)?;
            }
            Ok(())
        }));
    } else if mode == "pause-after-pointer-temp-sync" {
        let marker = marker.clone();
        let continue_path = continue_path.clone();
        atomic_guard = Some(AtomicWriteTestHookGuard::set(move |stage, target| {
            if stage == AtomicWriteStage::AfterTemporarySyncBeforeReplace
                && target.file_name().and_then(|name| name.to_str())
                    == Some("active-generation.json")
            {
                pause_subprocess(&marker, &continue_path, "pointer-temp-synced")?;
            }
            Ok(())
        }));
    } else if mode == "pause-after-pointer-replace" {
        let marker = marker.clone();
        let continue_path = continue_path.clone();
        atomic_guard = Some(AtomicWriteTestHookGuard::set(move |stage, target| {
            if stage == AtomicWriteStage::AfterReplaceBeforeDirectorySync
                && target.file_name().and_then(|name| name.to_str())
                    == Some("active-generation.json")
            {
                pause_subprocess(&marker, &continue_path, "pointer-replaced")?;
            }
            Ok(())
        }));
    } else if mode == "fail-pointer-directory-sync" {
        atomic_guard = Some(AtomicWriteTestHookGuard::set(|stage, target| {
            if stage == AtomicWriteStage::AfterReplaceBeforeDirectorySync
                && target.file_name().and_then(|name| name.to_str())
                    == Some("active-generation.json")
            {
                return Err(io::Error::from_raw_os_error(5));
            }
            Ok(())
        }));
    } else if mode == "fail-pointer-write-enospc" {
        atomic_guard = Some(AtomicWriteTestHookGuard::set(|stage, target| {
            if stage == AtomicWriteStage::BeforeTemporaryWrite
                && target.file_name().and_then(|name| name.to_str())
                    == Some("active-generation.json")
            {
                return Err(io::Error::from_raw_os_error(28));
            }
            Ok(())
        }));
    } else {
        panic!("unknown predecessor republish child mode {mode}");
    }

    let detail = match open_republish_writer(&root) {
        Ok(outcome) => {
            let recovered = outcome.committed_republish_recovery().is_some();
            let generation_id = outcome
                .into_writer()
                .unwrap()
                .base_manifest()
                .unwrap()
                .generation_id()
                .unwrap()
                .to_owned();
            format!("COMMITTED {generation_id} {recovered}")
        }
        Err(error) => format!("ERROR {error:?}\n{error}"),
    };
    fs::write(result, detail).unwrap();
    drop(atomic_guard);
    drop(republish_guard);
}

pub(super) fn spawn_republish_subprocess(root: &Path, mode: &str) -> Child {
    let (marker, continue_path, result) = subprocess_paths(root);
    for path in [&marker, &continue_path, &result] {
        let _ = fs::remove_file(path);
    }
    Command::new(env::current_exe().unwrap())
        .arg("--exact")
        .arg("tests::republish::additional::predecessor_republish_subprocess_worker")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(SUBPROCESS_MODE_ENV, mode)
        .env(SUBPROCESS_ROOT_ENV, root)
        .env(SUBPROCESS_MARKER_ENV, marker)
        .env(SUBPROCESS_CONTINUE_ENV, continue_path)
        .env(SUBPROCESS_RESULT_ENV, result)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap()
}

fn spawn_candidate_certification_subprocess(root: &Path, mode: &str) -> Child {
    let (marker, continue_path, result) = subprocess_paths(root);
    for path in [&marker, &continue_path, &result] {
        let _ = fs::remove_file(path);
    }
    Command::new(env::current_exe().unwrap())
        .arg("--exact")
        .arg("tests::republish::additional::candidate_certification_publication_subprocess_worker")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(SUBPROCESS_MODE_ENV, mode)
        .env(SUBPROCESS_ROOT_ENV, root)
        .env(SUBPROCESS_MARKER_ENV, marker)
        .env(SUBPROCESS_CONTINUE_ENV, continue_path)
        .env(SUBPROCESS_RESULT_ENV, result)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap()
}

fn fresh_reader_subprocess(root: &Path) -> String {
    let result = root.join("candidate-certification-reader.result");
    let _ = fs::remove_file(&result);
    let mut child = Command::new(env::current_exe().unwrap())
        .arg("--exact")
        .arg("tests::republish::additional::candidate_certification_reader_subprocess_worker")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env("CTX_CANDIDATE_CERTIFICATION_READER_ROOT", root)
        .spawn()
        .unwrap();
    let status = wait_for_subprocess_exit(&mut child);
    assert!(status.success());
    fs::read_to_string(result).unwrap()
}

fn fresh_certified_candidate_subprocess(root: &Path) -> String {
    let result = root.join("certified-candidate-reader.result");
    let _ = fs::remove_file(&result);
    let mut child = Command::new(env::current_exe().unwrap())
        .arg("--exact")
        .arg("tests::republish::additional::certified_candidate_reader_subprocess_worker")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env("CTX_CERTIFIED_CANDIDATE_READER_ROOT", root)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let status = wait_for_subprocess_exit(&mut child);
    assert!(status.success());
    fs::read_to_string(result).unwrap()
}

fn wait_for_subprocess_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + SUBPROCESS_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    let status = child.wait().unwrap();
    panic!("timed out waiting for subprocess exit: {status}");
}

pub(super) fn wait_for_subprocess_marker(child: &mut Child, marker: &Path) {
    let deadline = Instant::now() + SUBPROCESS_TIMEOUT;
    while Instant::now() < deadline {
        if marker.exists() {
            return;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("republish child exited before checkpoint: {status}");
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for republish child checkpoint");
}

fn kill_republish_subprocess(child: &mut Child) {
    child.kill().unwrap();
    assert!(!wait_for_subprocess_exit(child).success());
}

#[test]
fn subprocess_process_death_around_commit_sync_and_pointer_rename_recovers_correct_authority() {
    for (mode, successor_visible) in [
        ("pause-republish:AfterCandidateCommit", false),
        ("pause-republish:AfterCandidateSync", false),
        ("pause-after-pointer-temp-sync", false),
        ("pause-after-pointer-replace", true),
    ] {
        let predecessor = GoldenPredecessor::copy();
        let held_reader = VerifiedIndex::open(predecessor.root()).unwrap();
        let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
        let (marker, _, _) = subprocess_paths(predecessor.root());
        let mut child = spawn_republish_subprocess(predecessor.root(), mode);
        wait_for_subprocess_marker(&mut child, &marker);
        kill_republish_subprocess(&mut child);

        let pointer_after = fs::read(predecessor.root().join("active-generation.json")).unwrap();
        assert_eq!(pointer_after != pointer_before, successor_visible, "{mode}");
        assert_eq!(held_reader.count_term("evidence").unwrap(), 3);
        VerifiedIndex::open(predecessor.root()).unwrap();

        drop(
            open_republish_writer(predecessor.root())
                .unwrap()
                .into_writer()
                .unwrap(),
        );
        let after_restart = VerifiedIndex::open(predecessor.root()).unwrap();
        assert_eq!(after_restart.count_term("evidence").unwrap(), 3);
    }
}

#[test]
fn delayed_candidate_certification_io_keeps_predecessor_visible_to_fresh_process() {
    let predecessor = GoldenPredecessor::copy();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let (marker, continue_path, _) = subprocess_paths(predecessor.root());
    let mut child = spawn_candidate_certification_subprocess(
        predecessor.root(),
        "pause-candidate-certification",
    );
    wait_for_subprocess_marker(&mut child, &marker);
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        "candidate-certification-temp-synced"
    );
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(
        fresh_reader_subprocess(predecessor.root()),
        format!("READER {} 3", predecessor.generation_id())
    );

    fs::write(continue_path, b"release").unwrap();
    assert!(wait_for_subprocess_exit(&mut child).success());
}

#[test]
fn candidate_certification_is_durable_and_reopenable_before_pointer_activation() {
    let predecessor = GoldenPredecessor::copy();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let (marker, continue_path, result) = subprocess_paths(predecessor.root());
    let mut child = spawn_candidate_certification_subprocess(
        predecessor.root(),
        "pause-after-candidate-certification",
    );
    wait_for_subprocess_marker(&mut child, &marker);
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        "candidate-certified-and-reopened"
    );
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    let fresh_candidate = fresh_certified_candidate_subprocess(predecessor.root());
    let mut candidate_fields = fresh_candidate.split_whitespace();
    assert_eq!(
        candidate_fields.next(),
        Some("CANDIDATE"),
        "{fresh_candidate}"
    );
    let candidate_generation_id = candidate_fields.next().unwrap().to_owned();
    assert_eq!(candidate_fields.next(), Some("4"), "{fresh_candidate}");
    assert_eq!(candidate_fields.next(), None, "{fresh_candidate}");
    assert_eq!(
        fresh_reader_subprocess(predecessor.root()),
        format!("READER {} 3", predecessor.generation_id())
    );

    fs::write(continue_path, b"release").unwrap();
    assert!(wait_for_subprocess_exit(&mut child).success());
    let published = fs::read_to_string(result).unwrap();
    let mut fields = published.split_whitespace();
    assert_eq!(fields.next(), Some("PUBLISHED"), "{published}");
    let receipt_generation_id = fields.next().unwrap();
    let reopened_generation_id = fields.next().unwrap();
    assert_eq!(fields.next(), None, "{published}");
    assert_eq!(receipt_generation_id, reopened_generation_id);
    assert_eq!(receipt_generation_id, candidate_generation_id);
    assert_ne!(receipt_generation_id, predecessor.generation_id());
    assert_eq!(
        fresh_reader_subprocess(predecessor.root()),
        format!("READER {receipt_generation_id} 4")
    );
}

#[test]
fn process_death_after_candidate_certification_preserves_predecessor_authority() {
    let predecessor = GoldenPredecessor::copy();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let (marker, _, _) = subprocess_paths(predecessor.root());
    let mut child = spawn_candidate_certification_subprocess(
        predecessor.root(),
        "pause-after-candidate-certification",
    );
    wait_for_subprocess_marker(&mut child, &marker);
    assert!(fresh_certified_candidate_subprocess(predecessor.root()).starts_with("CANDIDATE "));
    kill_republish_subprocess(&mut child);
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(
        fresh_reader_subprocess(predecessor.root()),
        format!("READER {} 3", predecessor.generation_id())
    );
    drop(
        GenerationWriter::open(predecessor.root(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap(),
    );
}

#[test]
fn candidate_certification_write_failure_cannot_publish() {
    let predecessor = GoldenPredecessor::copy();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let mut child = spawn_candidate_certification_subprocess(
        predecessor.root(),
        "fail-candidate-certification-write",
    );
    assert!(wait_for_subprocess_exit(&mut child).success());
    let (_, _, result) = subprocess_paths(predecessor.root());
    assert!(fs::read_to_string(result).unwrap().starts_with("ERROR"));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(
        fresh_reader_subprocess(predecessor.root()),
        format!("READER {} 3", predecessor.generation_id())
    );
}

#[test]
fn subprocess_pointer_enospc_is_prepublication_failure_and_retry_migrates() {
    let predecessor = GoldenPredecessor::copy();
    let pointer_before = fs::read(predecessor.root().join("active-generation.json")).unwrap();
    let mut child = spawn_republish_subprocess(predecessor.root(), "fail-pointer-write-enospc");
    assert!(wait_for_subprocess_exit(&mut child).success());
    let (_, _, result) = subprocess_paths(predecessor.root());
    assert!(fs::read_to_string(result).unwrap().starts_with("ERROR"));
    assert_eq!(
        fs::read(predecessor.root().join("active-generation.json")).unwrap(),
        pointer_before
    );
    drop(
        open_republish_writer(predecessor.root())
            .unwrap()
            .into_writer()
            .unwrap(),
    );
}

#[test]
fn subprocess_post_rename_fsync_failure_is_committed_and_restart_reads_successor() {
    let predecessor = GoldenPredecessor::copy();
    let mut child = spawn_republish_subprocess(predecessor.root(), "fail-pointer-directory-sync");
    assert!(wait_for_subprocess_exit(&mut child).success());
    let (_, _, result) = subprocess_paths(predecessor.root());
    let result = fs::read_to_string(result).unwrap();
    assert!(result.starts_with("COMMITTED "), "{result}");
    assert!(result.ends_with(" true"), "{result}");
    let current = VerifiedIndex::open(predecessor.root()).unwrap();
    drop(
        GenerationWriter::open(predecessor.root(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap(),
    );
    assert_eq!(
        VerifiedIndex::active_generation_id(predecessor.root())
            .unwrap()
            .as_deref(),
        Some(current.generation_id())
    );
}
