use super::tests::{body, event_id, read_current, test_outbox, ENDPOINT, NOW, ROOT};
use super::*;

#[test]
fn every_reason_survives_restart_partial_recovery_and_final_drain() {
    for reason in [
        AnalyticsDeliveryFailureReason::RequestDns,
        AnalyticsDeliveryFailureReason::RequestConnect,
        AnalyticsDeliveryFailureReason::RequestTimeout,
        AnalyticsDeliveryFailureReason::RequestIo,
        AnalyticsDeliveryFailureReason::ResponseStatus408,
        AnalyticsDeliveryFailureReason::ResponseBodyTimeout,
        AnalyticsDeliveryFailureReason::ResponseBodyIo,
        AnalyticsDeliveryFailureReason::FileOpen,
        AnalyticsDeliveryFailureReason::FileWrite,
        AnalyticsDeliveryFailureReason::FileFlush,
        AnalyticsDeliveryFailureReason::OutboxCorrupt,
        AnalyticsDeliveryFailureReason::OutboxExpired,
        AnalyticsDeliveryFailureReason::OutboxCapacity,
        AnalyticsDeliveryFailureReason::OutboxClock,
        AnalyticsDeliveryFailureReason::OutboxOversized,
    ] {
        let class = if reason.permits(AnalyticsDeliveryFailureClass::Transport) {
            AnalyticsDeliveryFailureClass::Transport
        } else {
            AnalyticsDeliveryFailureClass::LocalIo
        };
        let (_dir, path, outbox) = test_outbox();
        for index in 1..=2 {
            outbox
                .append_at(ENDPOINT, &body(&event_id(index)), NOW)
                .unwrap();
        }
        let first = outbox.snapshot_at(ENDPOINT, NOW).unwrap().remove(0);
        let original_payload = first.payload.clone();
        outbox
            .reconcile_at(
                &[(
                    first,
                    DeliveryDisposition::Retry {
                        class,
                        reason: Some(reason),
                        retry_after: None,
                    },
                )],
                NOW,
            )
            .unwrap();
        let durable = read_current(&path);
        assert_eq!(durable.entries[0].payload, original_payload);
        assert_eq!(durable.root(ROOT).last_failure_reason, Some(reason));
        drop(outbox);
        let outbox = AnalyticsOutbox::open_at(path.clone(), ROOT, NOW).unwrap();
        assert!(outbox.pending_observation_at(NOW).unwrap().is_none());
        let second = outbox.snapshot_at(ENDPOINT, NOW).unwrap().remove(0);
        outbox
            .reconcile_at(&[(second, DeliveryDisposition::Accepted)], NOW)
            .unwrap();
        let partial = outbox.pending_observation_at(NOW).unwrap().unwrap();
        assert_eq!(partial.event.failure_reason, Some(reason));
        assert_eq!(partial.event.queued, CountBucket::One);
        outbox
            .queue_observation_at(ENDPOINT, &body(&event_id(3)), &partial, NOW)
            .unwrap();
        let health = outbox.snapshot_at(ENDPOINT, NOW).unwrap().remove(0);
        // A failed observation is not an ordinary failure and must not overwrite counters.
        outbox
            .reconcile_at(
                &[(
                    health,
                    DeliveryDisposition::Retry {
                        class,
                        reason: Some(reason),
                        retry_after: None,
                    },
                )],
                NOW,
            )
            .unwrap();
        assert_eq!(read_current(&path).root(ROOT).last_failure_reason, None);
        drop(outbox);
        let later = NOW + RETRY_MAX_SECONDS as i64 + 1;
        let outbox = AnalyticsOutbox::open_at(path.clone(), ROOT, later).unwrap();
        let attempts = outbox
            .snapshot_at(ENDPOINT, later)
            .unwrap()
            .into_iter()
            .map(|entry| (entry, DeliveryDisposition::Accepted))
            .collect::<Vec<_>>();
        outbox.reconcile_at(&attempts, later).unwrap();
        drop(outbox);
        let outbox = AnalyticsOutbox::open_at(path.clone(), ROOT, later).unwrap();
        let recovered = outbox.pending_observation_at(later).unwrap().unwrap();
        assert_eq!(
            recovered.event.outcome(),
            crate::analytics::Outcome::Success
        );
        assert_eq!(recovered.event.failure_reason, None);
        outbox
            .queue_observation_at(ENDPOINT, &body(&event_id(4)), &recovered, later)
            .unwrap();
        let last = outbox.snapshot_at(ENDPOINT, later).unwrap().remove(0);
        outbox
            .reconcile_at(&[(last, DeliveryDisposition::Accepted)], later)
            .unwrap();
        assert!(outbox.pending_observation_at(later).unwrap().is_none());
        assert!(read_current(&path).roots.is_empty());
    }
}

#[test]
fn local_maintenance_reasons_are_produced_by_the_actual_branches() {
    use AnalyticsDeliveryFailureReason as Reason;
    for reason in [
        Reason::OutboxCorrupt,
        Reason::OutboxExpired,
        Reason::OutboxCapacity,
        Reason::OutboxClock,
        Reason::OutboxOversized,
    ] {
        let (_dir, path, outbox) = test_outbox();
        match reason {
            Reason::OutboxCorrupt => write_private_file_durably(&path, b"not-json").unwrap(),
            Reason::OutboxExpired => outbox
                .append_at(
                    ENDPOINT,
                    &body(&event_id(1)),
                    NOW - OUTBOX_MAX_AGE_SECONDS - 1,
                )
                .unwrap(),
            Reason::OutboxCapacity => {
                for index in 0..=OUTBOX_MAX_ENTRIES {
                    outbox
                        .append_at(ENDPOINT, &body(&event_id(index)), NOW)
                        .unwrap();
                }
            }
            Reason::OutboxClock => {
                outbox
                    .append_at(ENDPOINT, &body(&event_id(1)), NOW)
                    .unwrap();
                let mut state = read_current(&path);
                state.entries[0].queued_at_epoch_seconds = i64::MAX;
                outbox.persist(&state).unwrap();
            }
            Reason::OutboxOversized => {
                let body = serde_json::to_vec(
                    &serde_json::json!({"padding": "x".repeat(OUTBOX_MAX_BODY_BYTES)}),
                )
                .unwrap();
                assert!(outbox.append_at(ENDPOINT, &body, NOW).is_err());
            }
            _ => unreachable!(),
        }
        drop(outbox);
        let outbox = AnalyticsOutbox::open_at(path.clone(), ROOT, NOW).unwrap();
        assert_eq!(
            read_current(&path).root(ROOT).last_failure_reason,
            Some(reason)
        );
        assert!(outbox.pending_observation_at(NOW).unwrap().is_none());
        outbox
            .append_at(ENDPOINT, &body(&event_id(500)), NOW)
            .unwrap();
        let accepted = outbox
            .snapshot_at(ENDPOINT, NOW)
            .unwrap()
            .into_iter()
            .map(|entry| (entry, DeliveryDisposition::Accepted))
            .collect::<Vec<_>>();
        outbox.reconcile_at(&accepted, NOW).unwrap();
        assert_eq!(
            outbox
                .pending_observation_at(NOW)
                .unwrap()
                .unwrap()
                .event
                .failure_reason,
            Some(reason)
        );
    }
}

#[test]
fn unknown_optional_reason_does_not_discard_payloads_and_new_failure_wins() {
    let (_dir, path, outbox) = test_outbox();
    outbox
        .append_at(ENDPOINT, &body(&event_id(1)), NOW)
        .unwrap();
    for value in [
        serde_json::json!("future"),
        serde_json::json!("/private/token"),
        serde_json::json!(12),
        serde_json::json!({}),
    ] {
        let first = outbox
            .snapshot_at(ENDPOINT, NOW + RETRY_MAX_SECONDS as i64 + 1)
            .unwrap()
            .remove(0);
        outbox
            .reconcile_at(
                &[(
                    first,
                    DeliveryDisposition::Retry {
                        class: AnalyticsDeliveryFailureClass::Transport,
                        reason: Some(AnalyticsDeliveryFailureReason::RequestTimeout),
                        retry_after: None,
                    },
                )],
                NOW,
            )
            .unwrap();
        let sidecar = path.with_extension("reasons.json");
        let mut metadata: Value = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
        metadata["roots"][ROOT]["reason"] = value;
        write_private_file_durably(&sidecar, &serde_json::to_vec(&metadata).unwrap()).unwrap();
        let reopened = AnalyticsOutbox::open_at(path.clone(), ROOT, NOW).unwrap();
        assert_eq!(
            reopened
                .snapshot_at(ENDPOINT, NOW + RETRY_MAX_SECONDS as i64 + 1)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(read_current(&path).root(ROOT).last_failure_reason, None);
    }
    let mut root = RootDeliveryState::default();
    root.record_failure(
        AnalyticsDeliveryFailureClass::Transport,
        Some(AnalyticsDeliveryFailureReason::RequestTimeout),
    );
    root.record_failure(AnalyticsDeliveryFailureClass::Server, None);
    assert_eq!(root.last_failure_reason, None);
    assert_eq!(root.failure_sequence, 2);
}
