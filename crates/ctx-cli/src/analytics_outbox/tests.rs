use std::{sync::mpsc, thread, time::Duration};

use ctx_client_observability::analytics::CountBucket;

use super::*;

const ENDPOINT: &str = "https://cli.ctx.rs/functions/v1/analytics";
const ROOT: &str = "00000000-0000-4000-8000-000000000002";
const OTHER_ROOT: &str = "00000000-0000-4000-8000-000000000003";
const NOW: i64 = 1_800_000_000;

fn body(event_id: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "client_profile_id": "00000000-0000-4000-8000-000000000001",
        "data_root_id": "00000000-0000-4000-8000-000000000002",
        "events": [{"event_id": event_id, "properties": {}}]
    }))
    .unwrap()
}

fn event_id(index: usize) -> String {
    format!("00000000-0000-4000-8000-{index:012}")
}

fn test_outbox() -> (tempfile::TempDir, PathBuf, AnalyticsOutbox) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox.json");
    let outbox = AnalyticsOutbox::open_at(path.clone(), ROOT, NOW).unwrap();
    (root, path, outbox)
}

fn read_current(path: &Path) -> OutboxState {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn retry(class: AnalyticsDeliveryFailureClass) -> DeliveryDisposition {
    DeliveryDisposition::Retry {
        class,
        retry_after: None,
    }
}

#[test]
fn native_smoke_fixture_matches_real_persisted_outbox() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../scripts/tests/native-candidate-outbox.json"
    ))
    .unwrap();
    let expected: OutboxState = serde_json::from_value(fixture.clone()).unwrap();
    validate_state(&expected).unwrap();
    assert_eq!(fixture["schema_version"], 3);
    assert_eq!(expected.entries.len(), 1);

    let (_root, path, outbox) = test_outbox();
    outbox
        .append_at(ENDPOINT, expected.entries[0].payload.as_bytes(), NOW)
        .unwrap();
    let mut actual: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let generated_id = actual["entries"][0]["entry_id"].as_str().unwrap();
    let generated_id = uuid::Uuid::parse_str(generated_id).unwrap();
    assert_eq!(generated_id.get_version_num(), 4);
    // Only the generated queue identity varies; compare the entire serialized
    // shape, consent owner, endpoint fingerprint, payload and initial counters.
    actual["entries"][0]["entry_id"] = fixture["entries"][0]["entry_id"].clone();
    assert_eq!(actual, fixture);
    for version in [1, 2] {
        let mut legacy = serde_json::to_value(&expected).unwrap();
        legacy["schema_version"] = version.into();
        assert!(validate_state(&serde_json::from_value(legacy).unwrap()).is_err());
    }
}

#[test]
fn legacy_shared_bodies_and_counters_are_discarded_without_guessing_ownership() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox.json");
    for version in [1, 2] {
        let mut entry = serde_json::json!({
            "schema_version": version,
            "endpoint_fingerprint": endpoint_fingerprint(ENDPOINT),
            "queued_at_epoch_seconds": NOW,
            "attempts": 3,
            "payload": String::from_utf8(body(&event_id(1))).unwrap(),
        });
        let mut state = serde_json::json!({
            "schema_version": version, "entries": [], "retry_attempts": 7,
            "dropped": 2, "failure_sequence": 4, "last_failure_class": "transport",
        });
        if version == 2 {
            entry["entry_id"] = event_id(1).into();
            entry["next_attempt_at_epoch_seconds"] = 0.into();
            entry["kind"] = "ordinary".into();
            state["observation_due"] = true.into();
        }
        state["entries"] = serde_json::json!([entry]);
        write_private_file_durably(&path, &serde_json::to_vec(&state).unwrap()).unwrap();
        let outbox = AnalyticsOutbox::open_at(path.clone(), ROOT, NOW).unwrap();
        assert!(outbox.snapshot_at(ENDPOINT, NOW).unwrap().is_empty());
        assert!(outbox.pending_observation_at(NOW).unwrap().is_none());
        assert!(read_current(&path).roots.is_empty());
        outbox
            .append_at(ENDPOINT, &body(&event_id(2)), NOW)
            .unwrap();
        let snapshot = outbox.snapshot_at(ENDPOINT, NOW).unwrap();
        outbox
            .reconcile_at(&[(snapshot[0].clone(), DeliveryDisposition::Accepted)], NOW)
            .unwrap();
        assert!(outbox.pending_observation_at(NOW).unwrap().is_none());

        write_private_file_durably(&path, &serde_json::to_vec(&state).unwrap()).unwrap();
        AnalyticsOutbox::purge(&path, None).unwrap();
        assert!(
            !path.exists(),
            "opt-out without an identity also discards legacy state"
        );
    }
}

#[test]
fn snapshot_releases_state_lock_and_uploader_lease_does_not_block_foreground_append() {
    let (_root, path, outbox) = test_outbox();
    outbox
        .append_at(ENDPOINT, &body(&event_id(1)), NOW)
        .unwrap();
    let _uploader = outbox.try_begin_upload().unwrap().unwrap();
    let snapshot = outbox.snapshot_at(ENDPOINT, NOW).unwrap();
    assert_eq!(snapshot.len(), 1);

    let (sent, received) = mpsc::channel();
    let writer = thread::spawn(move || {
        let concurrent = AnalyticsOutbox::open_at(path, ROOT, NOW).unwrap();
        concurrent
            .append_at(ENDPOINT, &body(&event_id(2)), NOW)
            .unwrap();
        sent.send(()).unwrap();
    });

    received
        .recv_timeout(Duration::from_secs(2))
        .expect("foreground writer was blocked by an in-flight upload");
    writer.join().unwrap();
    assert_eq!(outbox.snapshot_at(ENDPOINT, NOW).unwrap().len(), 2);
}

#[test]
fn uploader_mutex_is_device_global_and_nonblocking() {
    let (_root, path, first) = test_outbox();
    let second = AnalyticsOutbox::open_at(path, ROOT, NOW).unwrap();
    let lease = first.try_begin_upload().unwrap().unwrap();
    assert!(second.try_begin_upload().unwrap().is_none());
    drop(lease);
    assert!(second.try_begin_upload().unwrap().is_some());
}

#[test]
fn restart_replays_the_same_outbox_id_payload_and_event_id() {
    let (_root, path, outbox) = test_outbox();
    let original = body("c220e8ef-eb0b-43d9-89c8-c64806e87d93");
    outbox.append_at(ENDPOINT, &original, NOW).unwrap();
    let before = outbox.snapshot_at(ENDPOINT, NOW).unwrap().remove(0);
    drop(outbox);

    let reopened = AnalyticsOutbox::open_at(path, ROOT, NOW + 1).unwrap();
    let after = reopened.snapshot_at(ENDPOINT, NOW + 1).unwrap().remove(0);

    assert_eq!(after.entry_id, before.entry_id);
    assert_eq!(after.payload(), original);
    assert_eq!(after.payload, before.payload);
}

#[test]
fn crash_after_server_acceptance_replays_instead_of_guessing() {
    let (_root, path, outbox) = test_outbox();
    let original = body(&event_id(1));
    outbox.append_at(ENDPOINT, &original, NOW).unwrap();
    let accepted_but_unreconciled = outbox.snapshot_at(ENDPOINT, NOW).unwrap().remove(0);
    drop(outbox);

    let reopened = AnalyticsOutbox::open_at(path, ROOT, NOW + 1).unwrap();
    let replay = reopened.snapshot_at(ENDPOINT, NOW + 1).unwrap().remove(0);

    assert_eq!(replay.entry_id, accepted_but_unreconciled.entry_id);
    assert_eq!(replay.payload(), original);
}

#[test]
fn exact_id_reconciliation_preserves_a_concurrent_writer() {
    let (_root, _path, outbox) = test_outbox();
    let first_body = body(&event_id(1));
    let second_body = body(&event_id(2));
    outbox.append_at(ENDPOINT, &first_body, NOW).unwrap();
    let first = outbox.snapshot_at(ENDPOINT, NOW).unwrap().remove(0);
    outbox.append_at(ENDPOINT, &second_body, NOW).unwrap();

    outbox
        .reconcile_at(&[(first, DeliveryDisposition::Accepted)], NOW)
        .unwrap();

    let remaining = outbox.snapshot_at(ENDPOINT, NOW).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].payload(), second_body);
}

#[test]
fn retry_is_retained_with_backoff_while_permanent_rejection_is_dropped() {
    let (_root, path, outbox) = test_outbox();
    outbox
        .append_at(ENDPOINT, &body(&event_id(1)), NOW)
        .unwrap();
    outbox
        .append_at(ENDPOINT, &body(&event_id(2)), NOW)
        .unwrap();
    let snapshot = outbox.snapshot_at(ENDPOINT, NOW).unwrap();

    outbox
        .reconcile_at(
            &[
                (
                    snapshot[0].clone(),
                    retry(AnalyticsDeliveryFailureClass::Transport),
                ),
                (
                    snapshot[1].clone(),
                    DeliveryDisposition::Permanent {
                        class: AnalyticsDeliveryFailureClass::ClientRejection,
                    },
                ),
            ],
            NOW,
        )
        .unwrap();

    let state = read_current(&path);
    assert_eq!(state.entries.len(), 1);
    assert_eq!(state.entries[0].entry_id, snapshot[0].entry_id);
    assert_eq!(state.entries[0].attempts, 1);
    assert!(state.entries[0].next_attempt_at_epoch_seconds > NOW);
    assert_eq!(state.root(ROOT).retry_attempts, 1);
    assert_eq!(state.root(ROOT).dropped, 1);
    assert!(outbox.snapshot_at(ENDPOINT, NOW).unwrap().is_empty());
}

#[test]
fn exponential_backoff_and_retry_after_are_deterministic_and_capped() {
    let id = "c220e8ef-eb0b-43d9-89c8-c64806e87d93";
    let first = retry_delay(id, 1, None);
    assert_eq!(first, retry_delay(id, 1, None));
    assert!(first >= Duration::from_secs(RETRY_BASE_SECONDS));
    assert!(first <= Duration::from_secs(RETRY_BASE_SECONDS * 3 / 2));
    assert_eq!(
        retry_delay(id, u16::MAX, None),
        Duration::from_secs(RETRY_MAX_SECONDS)
    );
    assert_eq!(
        retry_delay(id, 1, Some(Duration::from_secs(RETRY_MAX_SECONDS * 10))),
        Duration::from_secs(RETRY_MAX_SECONDS)
    );
    assert!(retry_delay(id, 1, Some(Duration::from_secs(120))) >= Duration::from_secs(120));
}

#[test]
fn expired_entries_are_dropped_at_the_clock_seam() {
    let (_root, path, outbox) = test_outbox();
    outbox
        .append_at(ENDPOINT, &body(&event_id(1)), NOW)
        .unwrap();
    drop(outbox);

    let reopened =
        AnalyticsOutbox::open_at(path.clone(), ROOT, NOW + OUTBOX_MAX_AGE_SECONDS + 1).unwrap();
    let state = read_current(&path);

    assert!(reopened
        .snapshot_at(ENDPOINT, NOW + OUTBOX_MAX_AGE_SECONDS + 1)
        .unwrap()
        .is_empty());
    assert_eq!(state.root(ROOT).dropped, 1);
}

#[test]
fn corrupted_future_timestamps_are_bounded_and_cannot_evade_expiry() {
    let (_root, path, outbox) = test_outbox();
    let mut state = OutboxState::empty();
    state.entries.push(OutboxEntry {
        schema_version: OUTBOX_SCHEMA_VERSION,
        entry_id: uuid::Uuid::new_v4().to_string(),
        data_root_id: ROOT.to_owned(),
        endpoint_fingerprint: endpoint_fingerprint(ENDPOINT),
        queued_at_epoch_seconds: i64::MAX,
        attempts: 1,
        next_attempt_at_epoch_seconds: i64::MAX,
        kind: OutboxEntryKind::Ordinary,
        payload: String::from_utf8(body(&event_id(1))).unwrap(),
    });
    outbox.persist(&state).unwrap();
    drop(outbox);

    let normalized = AnalyticsOutbox::open_at(path.clone(), ROOT, NOW).unwrap();
    let state = read_current(&path);
    assert_eq!(state.entries[0].queued_at_epoch_seconds, NOW);
    assert_eq!(
        state.entries[0].next_attempt_at_epoch_seconds,
        NOW + RETRY_MAX_SECONDS as i64
    );
    assert_eq!(
        state.root(ROOT).last_failure_class,
        Some(AnalyticsDeliveryFailureClass::LocalIo)
    );
    drop(normalized);

    let expired =
        AnalyticsOutbox::open_at(path.clone(), ROOT, NOW + OUTBOX_MAX_AGE_SECONDS + 1).unwrap();
    assert!(expired
        .snapshot_at(ENDPOINT, NOW + OUTBOX_MAX_AGE_SECONDS + 1)
        .unwrap()
        .is_empty());
}

#[test]
fn corrupt_private_state_recovers_and_reports_one_safe_drop() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox.json");
    write_private_file_durably(&path, b"not-json").unwrap();

    let outbox = AnalyticsOutbox::open_at(path.clone(), ROOT, NOW).unwrap();
    let state = read_current(&path);

    assert!(state.entries.is_empty());
    assert_eq!(state.root(ROOT).dropped, 1);
    assert_eq!(
        state.root(ROOT).last_failure_class,
        Some(AnalyticsDeliveryFailureClass::LocalIo)
    );
    assert!(outbox.pending_observation_at(NOW).unwrap().is_none());
}

#[test]
fn count_and_total_byte_bounds_drop_oldest_entries() {
    let (_root, path, outbox) = test_outbox();
    let mut state = OutboxState::empty();
    for index in 0..OUTBOX_MAX_ENTRIES {
        state.entries.push(OutboxEntry {
            schema_version: OUTBOX_SCHEMA_VERSION,
            entry_id: uuid::Uuid::new_v4().to_string(),
            data_root_id: ROOT.to_owned(),
            endpoint_fingerprint: endpoint_fingerprint(ENDPOINT),
            queued_at_epoch_seconds: NOW,
            attempts: 0,
            next_attempt_at_epoch_seconds: 0,
            kind: OutboxEntryKind::Ordinary,
            payload: String::from_utf8(body(&event_id(index))).unwrap(),
        });
    }
    outbox.persist(&state).unwrap();
    let oldest = state.entries[0].entry_id.clone();
    outbox
        .append_at(ENDPOINT, &body(&event_id(999)), NOW)
        .unwrap();
    let bounded = read_current(&path);
    assert_eq!(bounded.entries.len(), OUTBOX_MAX_ENTRIES);
    assert!(!bounded.entries.iter().any(|entry| entry.entry_id == oldest));
    assert_eq!(bounded.root(ROOT).dropped, 1);

    let large = serde_json::to_vec(&serde_json::json!({"padding": "x".repeat(400_000)})).unwrap();
    for _ in 0..6 {
        outbox.append_at(ENDPOINT, &large, NOW).unwrap();
    }
    assert!(fs::metadata(&path).unwrap().len() <= OUTBOX_MAX_BYTES);
    assert!(read_current(&path).entries.len() <= OUTBOX_MAX_ENTRIES);
}

#[test]
fn per_entry_bound_is_enforced_and_counted() {
    let (_root, path, outbox) = test_outbox();
    let oversized = serde_json::to_vec(&serde_json::json!({
        "padding": "x".repeat(OUTBOX_MAX_BODY_BYTES)
    }))
    .unwrap();
    assert!(outbox.append_at(ENDPOINT, &oversized, NOW).is_err());
    let state = read_current(&path);
    assert!(state.entries.is_empty());
    assert_eq!(state.root(ROOT).dropped, 1);
}

#[test]
fn snapshot_is_bounded_to_ten_entries() {
    let (_root, _path, outbox) = test_outbox();
    for index in 0..(OUTBOX_MAX_FLUSH_PER_CALL + 2) {
        outbox
            .append_at(ENDPOINT, &body(&event_id(index)), NOW)
            .unwrap();
    }
    assert_eq!(
        outbox.snapshot_at(ENDPOINT, NOW).unwrap().len(),
        OUTBOX_MAX_FLUSH_PER_CALL
    );
}

#[test]
fn endpoint_fingerprint_prevents_cross_endpoint_replay() {
    let (_root, _path, outbox) = test_outbox();
    outbox
        .append_at(ENDPOINT, &body(&event_id(1)), NOW)
        .unwrap();
    assert!(outbox
        .snapshot_at("https://other.example.test/events", NOW)
        .unwrap()
        .is_empty());
}

#[test]
fn health_is_created_only_after_retry_recovery_and_never_recurses() {
    let (_root, path, outbox) = test_outbox();
    outbox
        .append_at(ENDPOINT, &body(&event_id(1)), NOW)
        .unwrap();
    let first = outbox.snapshot_at(ENDPOINT, NOW).unwrap().remove(0);
    outbox
        .reconcile_at(
            &[(first, retry(AnalyticsDeliveryFailureClass::Server))],
            NOW,
        )
        .unwrap();
    assert!(outbox.pending_observation_at(NOW).unwrap().is_none());

    let retry_at = read_current(&path).entries[0].next_attempt_at_epoch_seconds;
    let recovered = outbox.snapshot_at(ENDPOINT, retry_at).unwrap().remove(0);
    outbox
        .reconcile_at(&[(recovered, DeliveryDisposition::Accepted)], retry_at)
        .unwrap();
    let observation = outbox
        .pending_observation_at(retry_at)
        .unwrap()
        .expect("retry recovery should authorize one health observation");
    assert_eq!(observation.event.retry_attempts, CountBucket::One);

    let health_body = serde_json::to_vec(&serde_json::json!({
        "events": [{"event_name": "analytics_delivery_observation"}]
    }))
    .unwrap();
    outbox
        .queue_observation_at(ENDPOINT, &health_body, &observation, retry_at)
        .unwrap();
    let health = outbox.snapshot_at(ENDPOINT, retry_at).unwrap().remove(0);
    assert_eq!(health.kind, OutboxEntryKind::DeliveryObservation);
    outbox
        .reconcile_at(
            &[(health, retry(AnalyticsDeliveryFailureClass::Transport))],
            retry_at,
        )
        .unwrap();

    let state = read_current(&path);
    assert_eq!(state.root(ROOT).retry_attempts, 0);
    assert_eq!(state.root(ROOT).dropped, 0);
    assert!(!state.root(ROOT).observation_due);
    assert!(outbox.pending_observation_at(retry_at).unwrap().is_none());

    let health_retry_at = state.entries[0].next_attempt_at_epoch_seconds;
    let health = outbox
        .snapshot_at(ENDPOINT, health_retry_at)
        .unwrap()
        .remove(0);
    outbox
        .reconcile_at(
            &[(
                health,
                DeliveryDisposition::Permanent {
                    class: AnalyticsDeliveryFailureClass::ClientRejection,
                },
            )],
            health_retry_at,
        )
        .unwrap();
    let state = read_current(&path);
    assert!(state.entries.is_empty());
    assert_eq!(state.root(ROOT).retry_attempts, 0);
    assert_eq!(state.root(ROOT).dropped, 0);
    assert!(!state.root(ROOT).observation_due);
}

#[test]
fn local_drops_wait_for_a_later_success_before_health_is_due() {
    let (_root, _path, outbox) = test_outbox();
    let oversized = serde_json::to_vec(&serde_json::json!({
        "padding": "x".repeat(OUTBOX_MAX_BODY_BYTES)
    }))
    .unwrap();
    assert!(outbox.append_at(ENDPOINT, &oversized, NOW).is_err());
    assert!(outbox.pending_observation_at(NOW).unwrap().is_none());

    outbox
        .append_at(ENDPOINT, &body(&event_id(1)), NOW)
        .unwrap();
    let delivered = outbox.snapshot_at(ENDPOINT, NOW).unwrap().remove(0);
    outbox
        .reconcile_at(&[(delivered, DeliveryDisposition::Accepted)], NOW)
        .unwrap();

    let observation = outbox
        .pending_observation_at(NOW)
        .unwrap()
        .expect("a successful ordinary delivery should recover a local drop");
    assert_eq!(observation.event.dropped, CountBucket::One);
}

#[test]
fn a_later_failure_defers_coalesced_health_until_another_success() {
    let (_root, path, outbox) = test_outbox();
    outbox
        .append_at(ENDPOINT, &body(&event_id(1)), NOW)
        .unwrap();
    outbox
        .append_at(ENDPOINT, &body(&event_id(2)), NOW)
        .unwrap();
    let snapshot = outbox.snapshot_at(ENDPOINT, NOW).unwrap();

    outbox
        .reconcile_at(
            &[
                (snapshot[0].clone(), DeliveryDisposition::Accepted),
                (
                    snapshot[1].clone(),
                    retry(AnalyticsDeliveryFailureClass::Transport),
                ),
            ],
            NOW,
        )
        .unwrap();
    assert!(outbox.pending_observation_at(NOW).unwrap().is_none());

    let retry_at = read_current(&path).entries[0].next_attempt_at_epoch_seconds;
    let recovered = outbox.snapshot_at(ENDPOINT, retry_at).unwrap().remove(0);
    outbox
        .reconcile_at(&[(recovered, DeliveryDisposition::Accepted)], retry_at)
        .unwrap();
    assert!(outbox.pending_observation_at(retry_at).unwrap().is_some());
}

#[test]
fn purge_removes_payload_and_does_not_create_missing_state() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing").join("outbox.json");
    AnalyticsOutbox::purge(&missing, None).unwrap();
    assert!(!missing.parent().unwrap().exists());

    let path = root.path().join("outbox.json");
    let outbox = AnalyticsOutbox::open_at(path.clone(), ROOT, NOW).unwrap();
    outbox
        .append_at(ENDPOINT, &body(&event_id(1)), NOW)
        .unwrap();
    AnalyticsOutbox::purge(&path, Some(ROOT)).unwrap();
    assert!(!path.exists());
}

#[test]
fn open_and_purge_reclaim_crash_orphaned_temporary_payloads() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox.json");
    let orphan = root.path().join(format!(
        "{OUTBOX_TEMP_PREFIX}{}{OUTBOX_TEMP_SUFFIX}",
        uuid::Uuid::new_v4()
    ));
    fs::write(&orphan, body(&event_id(1))).unwrap();

    let outbox = AnalyticsOutbox::open_at(path.clone(), ROOT, NOW).unwrap();
    assert!(!orphan.exists());
    outbox
        .append_at(ENDPOINT, &body(&event_id(2)), NOW)
        .unwrap();
    let orphan = root.path().join(format!(
        "{OUTBOX_TEMP_PREFIX}{}{OUTBOX_TEMP_SUFFIX}",
        uuid::Uuid::new_v4()
    ));
    fs::write(&orphan, body(&event_id(3))).unwrap();

    AnalyticsOutbox::purge(&path, Some(ROOT)).unwrap();

    assert!(!path.exists());
    assert!(!orphan.exists());
}

#[test]
fn purge_wins_over_in_flight_reconciliation() {
    let (_root, path, outbox) = test_outbox();
    outbox
        .append_at(ENDPOINT, &body(&event_id(1)), NOW)
        .unwrap();
    let snapshot = outbox.snapshot_at(ENDPOINT, NOW).unwrap().remove(0);
    AnalyticsOutbox::purge(&path, Some(ROOT)).unwrap();
    assert!(!outbox.contains_snapshot(&snapshot).unwrap());

    outbox
        .reconcile_at(&[(snapshot, DeliveryDisposition::Accepted)], NOW)
        .unwrap();

    assert!(!path.exists());
}

#[test]
fn unsafe_state_path_still_fails_closed() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox.json");
    fs::create_dir(&path).unwrap();
    assert!(AnalyticsOutbox::open_at(path, ROOT, NOW).is_err());
}

#[cfg(windows)]
#[test]
fn windows_lock_handle_hardens_its_acl() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox.lock");

    let lock = open_lock_file(&path).unwrap();

    verify_private_file(&path).unwrap();
    drop(lock);
}

#[cfg(windows)]
#[test]
fn windows_temporary_handle_hardens_its_acl_before_publish() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox.json");

    write_private_file_durably(&path, b"{}\n").unwrap();

    assert_eq!(fs::read(&path).unwrap(), b"{}\n");
    verify_private_file(&path).unwrap();
}

#[cfg(unix)]
#[test]
fn unsafe_state_permissions_still_fail_closed() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox.json");
    write_private_file_durably(&path, b"not-json").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(AnalyticsOutbox::open_at(path, ROOT, NOW).is_err());
}

#[test]
fn root_scope_owns_snapshots_purge_reconciliation_and_recovery_counters() {
    let (_dir, path, a) = test_outbox();
    let b = AnalyticsOutbox::open_at(path.clone(), OTHER_ROOT, NOW).unwrap();
    a.append_at(ENDPOINT, &body(&event_id(1)), NOW).unwrap();
    b.append_at(ENDPOINT, &body(&event_id(2)), NOW).unwrap();
    let a_snapshot = a.snapshot_at(ENDPOINT, NOW).unwrap().remove(0);
    let b_snapshot = b.snapshot_at(ENDPOINT, NOW).unwrap().remove(0);
    assert_eq!(a_snapshot.payload(), body(&event_id(1)));
    assert_eq!(b_snapshot.payload(), body(&event_id(2)));
    assert!(!b.contains_snapshot(&a_snapshot).unwrap());
    assert!(b
        .reconcile_at(&[(a_snapshot.clone(), DeliveryDisposition::Accepted)], NOW)
        .is_err());

    a.reconcile_at(
        &[(a_snapshot, retry(AnalyticsDeliveryFailureClass::Transport))],
        NOW,
    )
    .unwrap();
    b.reconcile_at(&[(b_snapshot, DeliveryDisposition::Accepted)], NOW)
        .unwrap();
    assert!(
        b.pending_observation_at(NOW).unwrap().is_none(),
        "B must not recover A's retry"
    );
    let retry_at = read_current(&path).entries[0].next_attempt_at_epoch_seconds;
    let a_retry = a.snapshot_at(ENDPOINT, retry_at).unwrap().remove(0);
    a.reconcile_at(&[(a_retry, DeliveryDisposition::Accepted)], retry_at)
        .unwrap();
    let health = a.pending_observation_at(retry_at).unwrap().unwrap();
    assert_eq!(health.event.retry_attempts, CountBucket::One);
    assert!(b
        .queue_observation_at(ENDPOINT, &body(&event_id(3)), &health, retry_at)
        .is_err());
    a.queue_observation_at(ENDPOINT, &body(&event_id(3)), &health, retry_at)
        .unwrap();
    b.append_at(ENDPOINT, &body(&event_id(4)), retry_at)
        .unwrap();
    let before_b = b.snapshot_at(ENDPOINT, retry_at).unwrap().remove(0);
    let before_a = a.snapshot_at(ENDPOINT, retry_at).unwrap().remove(0);

    AnalyticsOutbox::purge(&path, Some(ROOT)).unwrap();
    a.reconcile_at(
        &[(before_a, retry(AnalyticsDeliveryFailureClass::Transport))],
        retry_at,
    )
    .unwrap();
    assert!(a.snapshot_at(ENDPOINT, retry_at).unwrap().is_empty());
    let after_b = b.snapshot_at(ENDPOINT, retry_at).unwrap().remove(0);
    assert_eq!(before_b.entry_id, after_b.entry_id);
    assert_eq!(before_b.payload(), after_b.payload());
    assert!(!read_current(&path).roots.contains_key(ROOT));

    let before = fs::read(&path).unwrap();
    AnalyticsOutbox::purge(&path, None).unwrap();
    assert_eq!(
        before,
        fs::read(&path).unwrap(),
        "an absent identity owns no current entries"
    );
}

#[test]
fn aggregate_bounds_and_counter_cardinality_hold_across_root_churn() {
    let (_dir, path, a) = test_outbox();
    let b = AnalyticsOutbox::open_at(path.clone(), OTHER_ROOT, NOW).unwrap();
    for index in 0..OUTBOX_MAX_ENTRIES {
        a.append_at(ENDPOINT, &body(&event_id(index)), NOW).unwrap();
    }
    b.append_at(ENDPOINT, &body(&event_id(999)), NOW).unwrap();
    let state = read_current(&path);
    assert_eq!(
        state.entries.len(),
        128,
        "entry bound is aggregate, not per root"
    );
    assert_eq!(state.root(ROOT).dropped, 1);
    assert_eq!(state.root(OTHER_ROOT).dropped, 0);
    assert!(!state
        .entries
        .iter()
        .any(|entry| entry.payload == String::from_utf8(body(&event_id(0))).unwrap()));

    let large = serde_json::to_vec(&serde_json::json!({"padding": "x".repeat(400_000)})).unwrap();
    for outbox in [&a, &b, &a, &b, &a, &b] {
        outbox.append_at(ENDPOINT, &large, NOW).unwrap();
        assert!(fs::metadata(&path).unwrap().len() <= 2 * 1024 * 1024);
    }
    assert!(read_current(&path).root(ROOT).dropped > 1);

    // Failed oversized appends create only counters, so no queued-entry bound
    // can incidentally bound this path. Preserve counters for queued roots.
    let oversized =
        serde_json::to_vec(&serde_json::json!({"padding": "x".repeat(OUTBOX_MAX_BODY_BYTES)}))
            .unwrap();
    let a_drops = read_current(&path).root(ROOT).dropped;
    for index in 1000..(1000 + 2 * OUTBOX_MAX_ENTRIES) {
        let id = event_id(index);
        let outbox = AnalyticsOutbox::open_at(path.clone(), &id, NOW).unwrap();
        assert!(outbox.append_at(ENDPOINT, &oversized, NOW).is_err());
        let state = read_current(&path);
        assert!(
            state.roots.len() <= 128,
            "counter-only root records grew without bound"
        );
        assert!(state.entries.len() <= 128);
        assert!(fs::metadata(&path).unwrap().len() <= 2 * 1024 * 1024);
        assert_eq!(state.root(ROOT).dropped, a_drops);
        validate_state(&state).unwrap();
    }
    assert_eq!(read_current(&path).roots.len(), 128);
}

#[test]
fn expiration_and_recovery_observations_remain_owned_by_each_root() {
    let (_dir, path, a) = test_outbox();
    let b = AnalyticsOutbox::open_at(path.clone(), OTHER_ROOT, NOW).unwrap();
    a.append_at(ENDPOINT, &body(&event_id(1)), NOW).unwrap();
    b.append_at(ENDPOINT, &body(&event_id(2)), NOW + 1).unwrap();
    let after_expiry = NOW + OUTBOX_MAX_AGE_SECONDS + 1;
    assert_eq!(b.snapshot_at(ENDPOINT, after_expiry).unwrap().len(), 1);
    assert!(a.snapshot_at(ENDPOINT, after_expiry).unwrap().is_empty());
    assert_eq!(read_current(&path).root(ROOT).dropped, 1);
    assert_eq!(read_current(&path).root(OTHER_ROOT).dropped, 0);

    for outbox in [&a, &b] {
        outbox
            .append_at(ENDPOINT, &body(&event_id(3)), after_expiry)
            .unwrap();
        let snapshot = outbox.snapshot_at(ENDPOINT, after_expiry).unwrap();
        outbox
            .reconcile_at(
                &[(
                    snapshot[0].clone(),
                    DeliveryDisposition::Permanent {
                        class: AnalyticsDeliveryFailureClass::ClientRejection,
                    },
                )],
                after_expiry,
            )
            .unwrap();
        outbox
            .append_at(ENDPOINT, &body(&event_id(4)), after_expiry)
            .unwrap();
        let snapshot = outbox.snapshot_at(ENDPOINT, after_expiry).unwrap();
        outbox
            .reconcile_at(
                &[(snapshot[0].clone(), DeliveryDisposition::Accepted)],
                after_expiry,
            )
            .unwrap();
        let observation = outbox
            .pending_observation_at(after_expiry)
            .unwrap()
            .unwrap();
        outbox
            .queue_observation_at(ENDPOINT, &body(&event_id(5)), &observation, after_expiry)
            .unwrap();
    }
    let state = read_current(&path);
    validate_state(&state).unwrap();
    assert_eq!(
        state
            .entries
            .iter()
            .filter(|entry| entry.kind == OutboxEntryKind::DeliveryObservation)
            .count(),
        2
    );
}

#[test]
fn full_queued_owner_capacity_preserves_surviving_counters_after_sole_entry_eviction() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("outbox.json");
    let owners = (1..=129).map(event_id).collect::<Vec<_>>();
    let check_bounds = || {
        let state = read_current(&path);
        assert!(state.entries.len() <= 128);
        assert!(state.roots.len() <= 128);
        assert!(fs::metadata(&path).unwrap().len() <= 2 * 1024 * 1024);
        validate_state(&state).unwrap();
    };
    for (index, owner) in owners.iter().take(128).enumerate() {
        let outbox = AnalyticsOutbox::open_at(path.clone(), owner, NOW).unwrap();
        outbox
            .append_at(ENDPOINT, &body(&event_id(1000 + index)), NOW)
            .unwrap();
        check_bounds();
        let snapshot = outbox.snapshot_at(ENDPOINT, NOW).unwrap().remove(0);
        outbox
            .reconcile_at(
                &[(snapshot, retry(AnalyticsDeliveryFailureClass::Transport))],
                NOW,
            )
            .unwrap();
        check_bounds();
    }
    let full = read_current(&path);
    assert_eq!(full.entries.len(), 128);
    assert_eq!(full.roots.len(), 128);
    let last = AnalyticsOutbox::open_at(path.clone(), &owners[128], NOW).unwrap();
    last.append_at(ENDPOINT, &body(&event_id(2000)), NOW)
        .unwrap();
    check_bounds();
    let state = read_current(&path);
    assert!(!state
        .entries
        .iter()
        .any(|entry| entry.data_root_id == owners[0]));
    assert_eq!(state.root(&owners[128]).retry_attempts, 0);
    assert_eq!(state.root(&owners[128]).dropped, 0);
    let snapshot = last.snapshot_at(ENDPOINT, NOW).unwrap().remove(0);
    last.reconcile_at(
        &[(snapshot, retry(AnalyticsDeliveryFailureClass::Transport))],
        NOW,
    )
    .unwrap();
    check_bounds();
    for owner in owners.iter().skip(1) {
        let reopened = AnalyticsOutbox::open_at(path.clone(), owner, NOW).unwrap();
        check_bounds();
        let state = read_current(&path);
        let counters = state.root(owner);
        assert_eq!(
            counters.retry_attempts, 1,
            "surviving owner's retry count changed"
        );
        assert_eq!(counters.dropped, 0, "evicted owner's drop was reassigned");
        assert_eq!(counters.failure_sequence, 1);
        assert_eq!(
            counters.last_failure_class,
            Some(AnalyticsDeliveryFailureClass::Transport)
        );
        assert!(!counters.observation_due);
        assert_eq!(
            state
                .entries
                .iter()
                .filter(|entry| &entry.data_root_id == owner)
                .count(),
            1
        );
        drop(reopened);
    }
    let state = read_current(&path);
    assert_eq!(state.entries.len(), 128);
    assert_eq!(state.roots.len(), 128);
    assert!(
        !state.roots.contains_key(&owners[0]),
        "the evicted sole-entry owner's counter-only record can be discarded"
    );
}
