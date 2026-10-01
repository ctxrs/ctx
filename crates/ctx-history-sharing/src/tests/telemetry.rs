use super::*;
use std::time::Duration;

fn observer() -> (SharingObserver, Arc<Mutex<Vec<SharingObservation>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let capture = events.clone();
    (
        Arc::new(move |event| capture.lock().unwrap().push(event)),
        events,
    )
}

#[test]
fn disabled_and_paused_ticks_never_invent_progress_or_network_work() {
    let root = tempdir().unwrap();
    let (observe, events) = observer();
    let collector = Collector::new(root.path().join("data"), root.path().join("sharing"))
        .with_observer(Some(observe));
    assert_eq!(collector.tick(), TickOutcome::Disabled);
    assert!(!root.path().join("data").exists());
    assert!(!root.path().join("sharing").exists());
    assert!(matches!(
        events.lock().unwrap().as_slice(),
        [SharingObservation::Tick {
            outcome: SharingTick::Disabled,
            failure: None,
            progress_after_failure: false,
            ..
        }]
    ));
}

#[test]
fn failed_upload_backoff_progress_and_receipt_recovery_have_separate_facts() {
    let root = tempdir().unwrap();
    let (server, remote) = recovery::accepting_remote();
    let (store, collector, _) = make_ready(root.path(), server.endpoint());
    let pending = recovery::read_pending(&store.pending_paths().unwrap()[0]);
    let (observe, events) = observer();
    let collector = collector.with_observer(Some(observe));
    remote.lock().unwrap().failure = Some(503);
    assert_eq!(collector.tick(), TickOutcome::Failed(Error::Unavailable));
    let retried = recovery::read_pending(&store.pending_paths().unwrap()[0]);
    assert_eq!(retried.failures, 1);
    assert!(events.lock().unwrap().iter().any(|event| matches!(event, SharingObservation::Retry { phase: SharingPhase::BeginUpload, failure: SharingFailure::Unavailable, attempts: 1, delay } if *delay > Duration::ZERO)));
    // Paused is not recovery, and policy pause performs no network request.
    store.pause(true).unwrap();
    let before = server.requests().len();
    assert_eq!(collector.tick(), TickOutcome::Paused);
    assert_eq!(server.requests().len(), before);
    store.pause(false).unwrap();
    remote.lock().unwrap().failure = None;
    expire_backoff(&store);
    assert_eq!(collector.tick(), TickOutcome::Progress); // staging admission
    assert_eq!(collector.tick(), TickOutcome::Progress); // one chunk
    remote.lock().unwrap().drop_publish_ack = true;
    assert_eq!(collector.tick(), TickOutcome::Failed(Error::Unavailable));
    assert!(!events
        .lock()
        .unwrap()
        .iter()
        .any(|event| matches!(event, SharingObservation::Accepted { .. })));
    expire_backoff(&store);
    assert_eq!(collector.tick(), TickOutcome::Progress); // receipt and local checkpoint
    let events = events.lock().unwrap();
    assert!(events.iter().any(|event| matches!(event, SharingObservation::Transfer { bytes } if *bytes == pending.member.bytes)));
    assert!(events.iter().any(|event| matches!(event, SharingObservation::Accepted { bytes, records, recovered_receipt: true } if *bytes == pending.member.bytes && *records == pending.member.records)));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                SharingObservation::Tick {
                    progress_after_failure: true,
                    ..
                }
            ))
            .count(),
        2
    );
    let safe = format!("{events:?}");
    for secret in [
        TOKEN,
        ORIGINAL_TEXT,
        COLLECTION,
        &pending.operation.idempotency_key,
    ] {
        assert!(!safe.contains(secret));
    }
}

#[test]
fn actual_worker_ticks_publish_and_stop_through_optional_observer() {
    let root = tempdir().unwrap();
    let data = root.path().join("data");
    let record = seed(&data);
    let (server, _) = recovery::accepting_remote();
    let store = SharingStore::new(data.join("sharing/team"));
    connect(&store, server.endpoint());
    store
        .set_policy(policy(capture::hex(&record.source.identity().digest())))
        .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = SharingWorker::start_with_observer(
        data,
        store.root().into(),
        Some(Arc::new(move |fact| {
            let _ = sender.send(fact);
        })),
    )
    .unwrap()
    .unwrap();
    let mut events = Vec::new();
    loop {
        let fact = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("worker publication completion");
        events.push(fact);
        if matches!(fact, SharingObservation::Accepted { .. }) {
            break;
        }
    }
    drop(worker);
    events.extend(receiver.try_iter());
    assert_eq!(events.first(), Some(&SharingObservation::WorkerStarted));
    assert_eq!(events.last(), Some(&SharingObservation::WorkerStopped));
    assert!(events.iter().any(|fact| matches!(
        fact,
        SharingObservation::Selection {
            decision: SelectionDecision::Selected,
            count: 1,
            complete: true
        }
    )));
    assert!(events
        .iter()
        .any(|fact| matches!(fact, SharingObservation::Queued { records: 1, .. })));
    assert!(events.iter().any(|fact| matches!(
        fact,
        SharingObservation::Tick {
            outcome: SharingTick::Progress,
            ..
        }
    )));
    assert_eq!(store.status().unwrap().pending, 0);
}
