use super::*;
use crate::analytics_outbox::tests::{
    body, event_id, read_current, test_outbox, ENDPOINT, NOW, ROOT,
};

fn summary() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"events":[{
        "event_name":"runtime_observation", "operation":"sift_summary"
    }]}))
    .unwrap()
}

#[test]
fn contention_skips_optional_open_without_changing_the_queue() {
    let (_temp, path, outbox) = test_outbox();
    outbox
        .append_at(ENDPOINT, &body(&event_id(1)), NOW)
        .unwrap();
    let before = fs::read(&path).unwrap();
    let _lock = OutboxLock::acquire(&outbox.state_lock_path()).unwrap();
    assert!(AnalyticsOutbox::try_open(path.clone(), ROOT)
        .unwrap()
        .is_none());
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn optional_summary_does_not_evict_or_rewrite_full_queue_receipts() {
    let (_temp, path, outbox) = test_outbox();
    for i in 0..OUTBOX_MAX_ENTRIES {
        outbox
            .append_at(ENDPOINT, &body(&event_id(i)), NOW)
            .unwrap();
    }
    let before = fs::read(&path).unwrap();
    assert!(!outbox.append_summary_at(ENDPOINT, &summary(), NOW).unwrap());
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn ordinary_admission_discards_optional_summary_before_older_ordinary_receipts() {
    let (_temp, path, outbox) = test_outbox();
    outbox
        .append_at(ENDPOINT, &body(&event_id(1)), NOW)
        .unwrap();
    assert!(outbox.append_summary_at(ENDPOINT, &summary(), NOW).unwrap());
    let summary_bytes = fs::read(&path).unwrap();
    assert!(!outbox.append_summary_at(ENDPOINT, &summary(), NOW).unwrap());
    assert_eq!(fs::read(&path).unwrap(), summary_bytes);
    for i in 2..=OUTBOX_MAX_ENTRIES {
        outbox
            .append_at(ENDPOINT, &body(&event_id(i)), NOW)
            .unwrap();
    }
    let current = read_current(&path);
    assert_eq!(current.entries.len(), OUTBOX_MAX_ENTRIES);
    assert!(current.entries.iter().all(|entry| !is_summary(entry)));
    assert!(current.entries[0].payload.contains(&event_id(1)));
}

#[test]
fn malformed_or_mixed_batches_are_not_treated_as_replaceable_summaries() {
    let (_temp, _path, outbox) = test_outbox();
    assert!(outbox
        .append_summary_at(ENDPOINT, &body(&event_id(1)), NOW)
        .is_err());
    let mut mixed: Value = serde_json::from_slice(&summary()).unwrap();
    mixed["events"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "event_name":"operation_completed", "operation":"search"
        }));
    assert!(outbox
        .append_summary_at(ENDPOINT, &serde_json::to_vec(&mixed).unwrap(), NOW)
        .is_err());
}

#[test]
fn an_old_endpoint_summary_does_not_block_the_current_endpoint_window() {
    let (_temp, path, outbox) = test_outbox();
    assert!(outbox.append_summary_at(ENDPOINT, &summary(), NOW).unwrap());
    let old = read_current(&path).entries[0].clone();
    let current = "https://current.example/v1/telemetry";
    assert!(outbox.append_summary_at(current, &summary(), NOW).unwrap());
    let state = read_current(&path);
    assert_eq!(state.entries.len(), 2);
    assert_eq!(state.entries[0].entry_id, old.entry_id);
    assert_eq!(state.entries[0].payload, old.payload);
    assert_eq!(
        state.entries[0].endpoint_fingerprint,
        old.endpoint_fingerprint
    );
    assert_eq!(outbox.snapshot_at(current, NOW).unwrap().len(), 1);
    assert!(!outbox.append_summary_at(current, &summary(), NOW).unwrap());
}
#[test]
fn optional_purge_of_absent_owner_preserves_queue_and_failure_evidence() {
    use ctx_client_observability::analytics::AnalyticsDeliveryFailureReason;
    let (_dir, path, a) = test_outbox();
    let b = AnalyticsOutbox::open_at(path.clone(), "00000000-0000-4000-8000-000000000003", NOW)
        .unwrap();
    b.append_at(ENDPOINT, &body(&event_id(2)), NOW).unwrap();
    let snapshot = b.snapshot_at(ENDPOINT, NOW).unwrap().remove(0);
    b.reconcile_at(
        &[(
            snapshot,
            DeliveryDisposition::Retry {
                class: AnalyticsDeliveryFailureClass::Transport,
                reason: Some(AnalyticsDeliveryFailureReason::RequestConnect),
                retry_after: None,
            },
        )],
        NOW,
    )
    .unwrap();
    let bytes = fs::read(&path).unwrap();
    let reasons = fs::read(path.with_extension("reasons.json")).unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    for _ in 0..5 {
        AnalyticsOutbox::try_purge(&path, Some(ROOT)).unwrap();
        AnalyticsOutbox::try_purge(&path, None).unwrap();
    }
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(
        fs::read(path.with_extension("reasons.json")).unwrap(),
        reasons
    );
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    assert_eq!(
        read_current(&path).roots[&b.data_root_id].last_failure_reason,
        Some(AnalyticsDeliveryFailureReason::RequestConnect)
    );
    a.append_at(ENDPOINT, &body(&event_id(1)), NOW).unwrap();
    AnalyticsOutbox::try_purge(&path, Some(ROOT)).unwrap();
    assert!(a.snapshot_at(ENDPOINT, NOW).unwrap().is_empty());
    let after = fs::read(&path).unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    AnalyticsOutbox::try_purge(&path, Some(ROOT)).unwrap();
    assert_eq!(fs::read(&path).unwrap(), after);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    assert_eq!(read_current(&path).entries.len(), 1);
}
