use super::*;
use std::sync::mpsc;
use std::time::Duration;

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
