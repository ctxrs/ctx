use super::*;
use crate::materializer::{MaterializationPhase, MaterializationProgress};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

fn publish_progress(root: &Path, progress: &MaterializationProgress) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    ctx_history_platform::platform_security::restrict_private_file(file.path())?;
    file.write_all(&serde_json::to_vec(progress)?)?;
    file.persist(root.join("attribution-progress.json"))?;
    Ok(())
}

fn receive_progress(
    observed: &mpsc::Receiver<MaterializationProgress>,
    expected: impl Fn(&MaterializationProgress) -> bool,
) -> Result<MaterializationProgress> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(mpsc::RecvTimeoutError::Timeout)?;
        let progress = observed.recv_timeout(remaining)?;
        if expected(&progress) {
            return Ok(progress);
        }
    }
}

#[test]
fn waiting_observer_relays_writer_counters_and_torn_snapshot_without_cancelling_writer() {
    let fixture = Fixture::new();
    let generation = fixture.publish(1, &[], None);
    let snapshot =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &generation).unwrap();
    let mut owner = fixture.materializer();
    let sidecar = fixture.graph_root.join("attribution-progress.json");
    let initial = MaterializationProgress {
        phase: MaterializationPhase::Indexing,
        core_generation_id: Some(generation.clone()),
        completed_sources: Some(2),
        total_sources: Some(4),
        applied_changes: Some(100),
        elapsed_millis: 2000,
    };
    publish_progress(&fixture.graph_root, &initial).unwrap();
    let cancelled = AtomicBool::new(false);
    let fail_on_unavailable = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let (updates, observed) = mpsc::channel();
        let cancelled = &cancelled;
        let fail_on_unavailable = &fail_on_unavailable;
        let fixture = &fixture;
        let snapshot = &snapshot;
        let waiter = scope.spawn(move || {
            crate::catch_up_with_progress(
                &fixture.data_root,
                snapshot,
                &|| cancelled.load(Ordering::SeqCst),
                &|progress| {
                    updates.send(progress.clone()).unwrap();
                    if progress.phase == MaterializationPhase::SnapshotUnavailable
                        && fail_on_unavailable.load(Ordering::SeqCst)
                    {
                        anyhow::bail!("authored observer output failure");
                    }
                    Ok(())
                },
            )
        });
        let observations = (|| -> Result<_> {
            receive_progress(&observed, |progress| progress == &initial)?;
            let mut next = initial.clone();
            next.completed_sources = Some(3);
            next.applied_changes = Some(150);
            publish_progress(&fixture.graph_root, &next)?;
            receive_progress(&observed, |progress| progress == &next)?;
            // Atomic replacement can transiently make a snapshot unavailable.
            // Arm the deliberate failure only after observing the replacement.
            fail_on_unavailable.store(true, Ordering::SeqCst);
            fs::write(&sidecar, b"{")?;
            receive_progress(&observed, |progress| {
                progress.phase == MaterializationPhase::SnapshotUnavailable
            })
        })();
        // Always unblock the waiter before propagating an I/O error or assertion.
        cancelled.store(true, Ordering::SeqCst);
        let result = waiter.join().unwrap();
        let torn = observations.unwrap();
        assert_eq!(torn.phase, MaterializationPhase::SnapshotUnavailable);
        assert!(torn.completed_sources.is_none());
        assert!(torn.total_sources.is_none());
        assert!(torn.applied_changes.is_none());
        assert_eq!(
            result.unwrap_err().to_string(),
            "authored observer output failure"
        );
    });
    assert_eq!(
        sync(&fixture, &generation, &mut owner).core_generation_id,
        generation
    );
    drop(owner);
    assert!(
        crate::materialization_progress(&fixture.data_root)
            .unwrap()
            .is_none()
    );
}

#[test]
fn observed_writer_completion_is_terminal_only_after_requested_generation_synchronizes() {
    for advance_core in [false, true] {
        let fixture = Fixture::new();
        let source = source("completion");
        let generation =
            fixture.publish(1, &[(source.clone(), vec![(1, "first".to_owned())])], None);
        let mut owner = fixture.materializer();
        sync(&fixture, &generation, &mut owner);
        let requested = if advance_core {
            fixture.publish(2, &[(source, vec![(1, "second".to_owned())])], None)
        } else {
            generation.clone()
        };
        let snapshot =
            crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &requested).unwrap();
        let owner_progress = crate::materialization_progress(&fixture.data_root)
            .unwrap()
            .unwrap();
        assert_eq!(owner_progress.phase, MaterializationPhase::Complete);
        assert_eq!(owner_progress.applied_changes, Some(1));
        std::thread::scope(|scope| {
            let (updates, observed) = mpsc::channel();
            let data_root = &fixture.data_root;
            let snapshot = &snapshot;
            let waiter = scope.spawn(move || {
                crate::catch_up_with_progress(data_root, snapshot, &|| false, &|progress| {
                    updates.send(progress.clone())?;
                    Ok(())
                })
            });
            // The completed owner's lease remains held until this observation.
            let before_release = observed.recv_timeout(Duration::from_secs(5));
            drop(owner);
            let outcome = waiter.join().unwrap().unwrap();
            let waiting = before_release.unwrap();
            assert_eq!(waiting.phase, MaterializationPhase::WaitingForWriter);
            assert_eq!(waiting.core_generation_id, Some(generation.clone()));
            assert_eq!(waiting.completed_sources, owner_progress.completed_sources);
            assert_eq!(waiting.total_sources, owner_progress.total_sources);
            assert_eq!(waiting.applied_changes, owner_progress.applied_changes);
            let remaining = observed.into_iter().collect::<Vec<_>>();
            let complete = remaining
                .iter()
                .filter(|update| update.phase == MaterializationPhase::Complete)
                .collect::<Vec<_>>();
            assert_eq!(complete.len(), 1);
            assert_eq!(
                complete[0].core_generation_id.as_deref(),
                Some(requested.as_str())
            );
            assert_eq!(
                remaining.last().unwrap().phase,
                MaterializationPhase::Complete
            );
            let CoreMaterializationSyncOutcome::Finished { receipt, did_work } = outcome;
            assert_eq!(receipt.core_generation_id, requested);
            assert_eq!(did_work, advance_core);
        });
    }
}

#[test]
fn cancelled_wait_does_not_report_another_generations_completion() {
    let fixture = Fixture::new();
    let generation = fixture.publish(1, &[], None);
    let mut owner = fixture.materializer();
    sync(&fixture, &generation, &mut owner);
    let requested = fixture.publish(2, &[(source("new"), vec![(1, "second".to_owned())])], None);
    let snapshot =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &requested).unwrap();
    let cancelled = AtomicBool::new(false);
    let updates = Mutex::new(Vec::new());
    let error = crate::catch_up_with_progress(
        &fixture.data_root,
        &snapshot,
        &|| cancelled.load(Ordering::SeqCst),
        &|progress| {
            updates.lock().unwrap().push(progress.clone());
            cancelled.store(true, Ordering::SeqCst);
            Ok(())
        },
    )
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<crate::materializer::SegmentMaterializerError>(),
        Some(crate::materializer::SegmentMaterializerError::Cancelled)
    ));
    let updates = updates.into_inner().unwrap();
    assert!(!updates.is_empty());
    assert!(
        updates
            .iter()
            .all(|update| update.phase != MaterializationPhase::Complete)
    );
    assert_eq!(owner.progress().phase, MaterializationPhase::Complete);
    assert_eq!(
        owner.progress().core_generation_id.as_deref(),
        Some(generation.as_str())
    );
}

#[test]
fn command_completion_waits_beyond_two_seconds_then_reuses_the_published_generation() {
    let fixture = Fixture::new();
    let generation = fixture.publish(1, &[], None);
    let snapshot =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &generation).unwrap();
    let mut owner = fixture.materializer();
    std::thread::scope(|scope| {
        let (started, waiting) = mpsc::channel();
        let (done, result) = mpsc::channel();
        let data_root = &fixture.data_root;
        let snapshot = &snapshot;
        scope.spawn(move || {
            started.send(()).unwrap();
            done.send(crate::catch_up(data_root, snapshot, &|| false))
                .unwrap();
        });
        waiting.recv().unwrap();
        assert!(matches!(
            result.recv_timeout(Duration::from_millis(2100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        let completed = sync(&fixture, &generation, &mut owner);
        drop(owner);
        let CoreMaterializationSyncOutcome::Finished { receipt, did_work } = result
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(receipt, completed);
        assert!(!did_work);
    });
}

#[test]
fn waiting_command_cancellation_does_not_cancel_or_modify_the_writer() {
    let fixture = Fixture::new();
    let generation = fixture.publish(1, &[], None);
    let snapshot =
        crate::catch_up::open_exact_core_snapshot(&fixture.data_root, &generation).unwrap();
    let mut owner = fixture.materializer();
    let cancelled = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let (done, result) = mpsc::channel();
        let data_root = &fixture.data_root;
        let snapshot = &snapshot;
        let cancel_flag = &cancelled;
        scope.spawn(move || {
            done.send(crate::catch_up(data_root, snapshot, &|| {
                cancel_flag.load(Ordering::SeqCst)
            }))
            .unwrap();
        });
        assert!(matches!(
            result.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        cancelled.store(true, Ordering::SeqCst);
        let outcome = match result.recv_timeout(Duration::from_secs(1)) {
            Ok(outcome) => outcome,
            Err(error) => {
                // Release the writer before scope unwinding joins the waiter.
                drop(owner);
                panic!("cancelled waiter did not finish: {error}");
            }
        };
        let error = outcome.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<crate::materializer::SegmentMaterializerError>(),
            Some(crate::materializer::SegmentMaterializerError::Cancelled)
        ));
        assert_eq!(
            sync(&fixture, &generation, &mut owner).core_generation_id,
            generation
        );
    });
}
