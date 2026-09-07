use super::*;
use crate::tests::{document, source};
use ctx_history_index_generation::{PortableCloneTestGuard, PortableCloneTestOptions};
use tempfile::tempdir;

fn worker_error() -> IndexError {
    tantivy::TantivyError::ErrorInThread("index worker failed".to_owned()).into()
}

#[test]
fn low_space_diagnostic_preserves_the_original_cause_and_probe_failures() {
    let temp = tempdir().unwrap();
    for available in [0, 16 * 1024 * 1024 - 1, 16 * 1024 * 1024, u64::MAX] {
        let _probe = PortableCloneTestGuard::set(
            PortableCloneTestOptions {
                available_bytes: Some(available),
                ..Default::default()
            },
            |_, _| Ok(()),
        );
        let error = observe_candidate_failure(temp.path(), worker_error());
        if available < 16 * 1024 * 1024 {
            assert!(matches!(
                &error,
                IndexError::CandidateFailureWithLowSpace { available: actual, cause }
                    if *actual == available && matches!(**cause, IndexError::Tantivy(_))
            ));
            assert!(error.to_string().contains("bytes observed free"));
            let cause = std::error::Error::source(&error).unwrap();
            assert_eq!(cause.to_string(), worker_error().to_string());
            assert!(!error.to_string().contains("ENOSPC"));
        } else {
            assert!(matches!(error, IndexError::Tantivy(_)));
        }
        assert!(matches!(
            observe_candidate_failure(&temp.path().join("missing"), worker_error()),
            IndexError::Tantivy(_)
        ));
        assert!(matches!(
            observe_candidate_failure(temp.path(), IndexError::ConcurrentGenerationChange),
            IndexError::ConcurrentGenerationChange
        ));
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn native_probe_observes_space_without_restricting_a_successful_cold_commit() {
    use ctx_history_index_generation::{CloneTestHookGuard, CloneTestOptions};
    let temp = tempdir().unwrap();
    let _probe = CloneTestHookGuard::set(
        CloneTestOptions {
            available_bytes: Some(0),
            ..Default::default()
        },
        |_, _| Ok(()),
    );
    assert!(matches!(
        observe_candidate_failure(temp.path(), worker_error()),
        IndexError::CandidateFailureWithLowSpace { available: 0, .. }
    ));
    let source = source("cold-commit.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(&source, 1, "retained content"))
        .unwrap();
    writer
        .certify_source(crate::tests::certificate(&source, 1, 1))
        .unwrap();
    writer.commit(|_| true).unwrap();
    assert_eq!(
        VerifiedIndex::open(temp.path())
            .unwrap()
            .manifest()
            .indexed_documents,
        1
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn unsuccessful_worker_join_preserves_the_candidate() {
    use ctx_history_index_generation::{CloneStage, CloneTestHookGuard, CloneTestOptions};
    use std::os::unix::fs::PermissionsExt;
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    if unsafe { geteuid() } == 0 {
        eprintln!("permission fault requires a non-root test process");
        return;
    }
    let temp = tempdir().unwrap();
    let source = source("worker-failure.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.writer_mut().unwrap();
    let candidate = writer.candidate_path().unwrap();
    let permissions = fs::metadata(&candidate).unwrap().permissions();
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o500)).unwrap();
    assert_eq!(
        fs::write(candidate.join("permission-check"), b"")
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let _hook = CloneTestHookGuard::set(CloneTestOptions::default(), |stage, _| {
        assert_ne!(
            stage,
            CloneStage::BeforeCleanup,
            "failed join reached discard"
        );
        Ok(())
    });
    // The queued document must flush when the original writer is joined. Its
    // directory cannot accept the segment, so completion cannot be established.
    writer
        .add_core_record(document(&source, 1, "cannot flush"))
        .unwrap();
    writer.discard_after_manifest_failure();
    assert!(
        candidate.is_dir(),
        "failed join authorized candidate removal"
    );
    fs::set_permissions(&candidate, permissions).unwrap();
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn manifest_cleanup_holds_root_lock_and_respects_candidate_binding() {
    use ctx_history_index_generation::{CloneStage, CloneTestHookGuard, CloneTestOptions};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use tantivy::directory::{error::LockError, Directory, Lock};
    let temp = tempdir().unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.writer_mut().unwrap();
    let candidate = writer.candidate_path().unwrap();
    let moved = temp.path().join("moved-candidate");
    let root = temp.path().to_owned();
    let called = Arc::new(AtomicBool::new(false));
    let reached = Arc::clone(&called);
    let _hook = CloneTestHookGuard::set(CloneTestOptions::default(), move |stage, name| {
        if stage == CloneStage::BeforeCleanup {
            let directory = DurableMmapDirectory::open(&root).unwrap();
            assert!(matches!(
                directory.acquire_lock(&Lock {
                    filepath: GENERATION_WRITER_LOCK_FILE.into(),
                    is_blocking: false,
                }),
                Err(LockError::LockBusy)
            ));
            let path = root.join(INDEX_GENERATIONS_DIRECTORY).join(name);
            fs::rename(&path, root.join("moved-candidate")).unwrap();
            fs::create_dir(&path).unwrap();
            fs::write(path.join("foreign"), b"preserve").unwrap();
            reached.store(true, Ordering::SeqCst);
        }
        Ok(())
    });
    writer.discard_after_manifest_failure();
    assert!(called.load(Ordering::SeqCst));
    assert_eq!(fs::read(candidate.join("foreign")).unwrap(), b"preserve");
    assert!(moved.join("meta.json").is_file());
}
