use notify::event::{DataChange, Flag};

use super::*;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct TestPayload(Option<WatchWatermark>);

impl CoalescingWakePayload for TestPayload {
    fn is_empty(&self) -> bool {
        self.0.is_none()
    }

    fn merge(&mut self, other: Self) {
        if let Some(watermark) = other.0 {
            self.0 = Some(self.0.map_or(watermark, |current| current.max(watermark)));
        }
    }
}

#[test]
fn notify_events_are_normalized_without_product_policy() {
    let path = PathBuf::from("/tmp/history.jsonl");
    let access = normalize_native_watch_event(Ok(
        Event::new(EventKind::Access(AccessKind::Read)).add_path(path.clone())
    ))
    .unwrap();
    assert_eq!(access.paths, vec![path.clone()]);
    assert_eq!(access.ignored_kind(), Some(NativeWatchIgnore::Access));
    assert!(!access.needs_rescan());
    assert!(!access.requires_rearm());

    let access_time = normalize_native_watch_event(Ok(Event::new(EventKind::Modify(
        ModifyKind::Metadata(MetadataKind::AccessTime),
    ))
    .add_path(path.clone())))
    .unwrap();
    assert_eq!(
        access_time.ignored_kind(),
        Some(NativeWatchIgnore::AccessTime)
    );

    let write_close = normalize_native_watch_event(Ok(Event::new(EventKind::Access(
        AccessKind::Close(AccessMode::Write),
    ))
    .add_path(path.clone())))
    .unwrap();
    assert_eq!(write_close.ignored_kind(), None);

    let rename = normalize_native_watch_event(Ok(Event::new(EventKind::Modify(ModifyKind::Name(
        notify::event::RenameMode::Both,
    )))
    .add_path(path.clone())))
    .unwrap();
    assert!(rename.requires_rearm());

    let rescan = normalize_native_watch_event(Ok(Event::new(EventKind::Modify(ModifyKind::Data(
        DataChange::Content,
    )))
    .add_path(path)
    .set_flag(Flag::Rescan)))
    .unwrap();
    assert!(rescan.needs_rescan());
}

#[test]
fn pressure_retains_only_the_maximum_lost_watermark() {
    let ingress = RawWatchIngress::default();
    ingress.record_loss(WatchWatermark::new(9, 4));
    ingress.record_loss(WatchWatermark::new(9, 2));
    ingress.record_loss(WatchWatermark::new(9, 7));
    assert_eq!(ingress.take_loss(9), Some(WatchWatermark::new(9, 7)));
    assert_eq!(ingress.take_loss(9), None);
}

#[test]
fn worker_drain_is_count_bounded_when_input_never_reaches_empty() {
    let (sender, receiver) = mpsc::channel();
    for sequence in 1..=WATCH_EVENT_QUEUE_CAPACITY as u64 + 2 {
        sender
            .send(WatchMessage::Event {
                event: Ok(NativeWatchEvent::ordinary(vec![PathBuf::from(format!(
                    "/tmp/{sequence}.jsonl"
                ))])),
                watermark: WatchWatermark::new(11, sequence),
            })
            .unwrap();
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let classified = Arc::clone(&seen);
    let classify_event: EventClassifier<TestPayload> = Arc::new(move |_, watermark| {
        classified.lock().unwrap().push(watermark.sequence);
        TestPayload::default()
    });
    let reconciliation: ReconciliationFactory<TestPayload> =
        Arc::new(|watermark| TestPayload(Some(watermark)));
    let observe_payload: ObservePayload<TestPayload> = Arc::new(|_| {});
    let mut relevant = TestPayload::default();
    assert!(!observe_pending_raw_events(
        None,
        &receiver,
        &RawWatchIngress::default(),
        11,
        &classify_event,
        &reconciliation,
        &observe_payload,
        &mut relevant,
    ));
    assert_eq!(seen.lock().unwrap().len(), WATCH_EVENT_QUEUE_CAPACITY);
    let WatchMessage::Event { watermark, .. } = receiver.try_recv().unwrap() else {
        panic!("bounded drain left a non-event message");
    };
    assert_eq!(
        watermark,
        WatchWatermark::new(11, WATCH_EVENT_QUEUE_CAPACITY as u64 + 1)
    );
}

#[test]
fn pressure_marker_never_discards_already_dequeued_events() {
    let (sender, receiver) = mpsc::channel();
    for sequence in 1..=3 {
        sender
            .send(WatchMessage::Event {
                event: Ok(NativeWatchEvent::ordinary(vec![PathBuf::from(
                    "/tmp/event",
                )])),
                watermark: WatchWatermark::new(12, sequence),
            })
            .unwrap();
    }
    let ingress = RawWatchIngress::default();
    ingress.record_loss(WatchWatermark::new(12, 9));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let classified = Arc::clone(&seen);
    let classify_event: EventClassifier<TestPayload> = Arc::new(move |_, watermark| {
        classified.lock().unwrap().push(watermark.sequence);
        TestPayload(Some(watermark))
    });
    let reconciliation: ReconciliationFactory<TestPayload> =
        Arc::new(|watermark| TestPayload(Some(watermark)));
    let observe_payload: ObservePayload<TestPayload> = Arc::new(|_| {});
    let mut relevant = TestPayload::default();
    assert!(!observe_pending_raw_events(
        None,
        &receiver,
        &ingress,
        12,
        &classify_event,
        &reconciliation,
        &observe_payload,
        &mut relevant,
    ));
    assert_eq!(seen.lock().unwrap().as_slice(), &[1, 2, 3]);
    assert_eq!(relevant.0, Some(WatchWatermark::new(12, 9)));
}

#[test]
fn full_and_disconnected_callback_channels_fence_synchronously() {
    let (sender, receiver) = mpsc::sync_channel(1);
    sender.send(WatchMessage::DrainIngress).unwrap();
    let ingress = RawWatchIngress::default();
    let accepting = AtomicBool::new(true);
    let sequence = AtomicU64::new(0);
    let observed = Arc::new(Mutex::new(Vec::new()));
    let fence_observed = Arc::clone(&observed);
    let fence: OverflowFence = Arc::new(move |watermark| {
        fence_observed.lock().unwrap().push(watermark);
    });
    let ignore: IgnoreEvent = Arc::new(|_| false);

    forward_native_watch_event(
        &sender,
        &ingress,
        &accepting,
        17,
        &sequence,
        &ignore,
        &fence,
        Ok(NativeWatchEvent::ordinary(vec![PathBuf::from("/tmp/full")])),
    );
    assert_eq!(
        observed.lock().unwrap().as_slice(),
        &[WatchWatermark::new(17, 1)]
    );
    assert_eq!(ingress.take_loss(17), Some(WatchWatermark::new(17, 1)));

    drop(receiver);
    forward_native_watch_event(
        &sender,
        &ingress,
        &accepting,
        17,
        &sequence,
        &ignore,
        &fence,
        Ok(NativeWatchEvent::ordinary(vec![PathBuf::from(
            "/tmp/disconnected",
        )])),
    );
    assert_eq!(
        observed.lock().unwrap().as_slice(),
        &[WatchWatermark::new(17, 1), WatchWatermark::new(17, 2)]
    );
    assert_eq!(ingress.disconnects.load(Ordering::Acquire), 1);
}

#[test]
fn worker_observes_payloads_before_debounce_and_empty_events_extend_activity() {
    let (classified_tx, classified_rx) = mpsc::channel();
    let (observed_tx, observed_rx) = mpsc::channel();
    let (signal_tx, signal_rx) = mpsc::channel();
    let watcher = NativeFileWatcher::start(
        "ctx-watch-activity-test",
        Arc::new(|_| false),
        Arc::new(move |event, watermark| {
            classified_tx.send(watermark).unwrap();
            TestPayload(
                event
                    .is_ok_and(|event| event.paths != [PathBuf::from("/tmp/empty")])
                    .then_some(watermark),
            )
        }),
        Arc::new(|_| {}),
        Arc::new(|watermark| TestPayload(Some(watermark))),
        Arc::new(move |payload| observed_tx.send(payload.clone()).unwrap()),
        Arc::new(move |payload| signal_tx.send(payload).unwrap()),
    )
    .unwrap();
    let send = |path| {
        forward_native_watch_event(
            &watcher.sender,
            &watcher.ingress,
            &watcher.accepting_events,
            watcher.watcher_epoch,
            &watcher.callback_sequence,
            &watcher.ignore_event,
            &watcher.overflow_fence,
            Ok(NativeWatchEvent::ordinary(vec![PathBuf::from(path)])),
        )
    };

    send("/tmp/first");
    assert_eq!(
        classified_rx.recv().unwrap(),
        WatchWatermark::new(watcher.watcher_epoch, 1)
    );
    assert_eq!(observed_rx.recv().unwrap().0.unwrap().sequence, 1);
    thread::sleep(Duration::from_millis(100));
    send("/tmp/empty");
    assert_eq!(classified_rx.recv().unwrap().sequence, 2);
    assert!(observed_rx.try_recv().is_err());
    thread::sleep(Duration::from_millis(175));
    assert!(
        signal_rx.try_recv().is_err(),
        "empty activity did not extend debounce"
    );
    send("/tmp/later");
    assert_eq!(classified_rx.recv().unwrap().sequence, 3);
    assert_eq!(observed_rx.recv().unwrap().0.unwrap().sequence, 3);
    assert_eq!(
        signal_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .0
            .unwrap()
            .sequence,
        3
    );
}

#[test]
fn worker_exit_requires_replacement() {
    let watcher = NativeFileWatcher::start(
        "ctx-watch-worker-death-test",
        Arc::new(|_| false),
        Arc::new(|_, _| TestPayload::default()),
        Arc::new(|_| {}),
        Arc::new(|watermark| TestPayload(Some(watermark))),
        Arc::new(|_| {}),
        Arc::new(|_| {}),
    )
    .unwrap();
    watcher.sender.send(WatchMessage::Stop).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while !watcher.worker_failed() {
        assert!(Instant::now() < deadline, "worker did not stop");
        thread::yield_now();
    }

    assert!(watcher.replacement_required(false));
}
