use super::recovery::{accepting_remote, publication, read_pending, receipt_for};
use super::*;
use crate::queue::Pending;
use ctx_history_server::{
    publish_fingerprint, CancelPublishOutcome, CancelPublishRequest, CancelPublishResponse,
};

fn reviewed_ready(root: &Path, endpoint: Endpoint) -> (SharingStore, Collector) {
    let data = root.join("data");
    let record = seed(&data);
    let store = SharingStore::new(data.join("sharing/team"));
    connect(&store, endpoint);
    let policy = store
        .prepare_policy(
            &data,
            &root.join("review-a"),
            PublicationMode::Reviewed {
                revisions: BTreeSet::new(),
            },
            policy(capture::hex(&record.source.identity().digest())).sources,
        )
        .unwrap();
    store.set_policy(policy).unwrap();
    let collector = Collector::new(data, store.root().to_owned());
    assert_eq!(collector.tick(), TickOutcome::Progress);
    (store, collector)
}

fn approve_replacement(root: &Path, store: &SharingStore) {
    let data = root.join("data");
    commit_records(
        &data,
        &[synthetic_record("session-one", "approved replacement B")],
        2,
    );
    let prepared = store
        .prepare_policy(
            &data,
            &root.join("review-b"),
            PublicationMode::Reviewed {
                revisions: BTreeSet::new(),
            },
            store.policy().unwrap().unwrap().sources,
        )
        .unwrap();
    store.set_policy(prepared).unwrap();
}

#[test]
fn reviewed_replacement_retires_denied_unsubmitted_work_without_sending_old_bytes() {
    // Never contacted, staging begun, and bytes staged: none submitted a final
    // publication, so all can yield to an approved replacement without a GET.
    for staged_steps in 0..=2 {
        let temp = tempdir().unwrap();
        let (server, remote) = accepting_remote();
        let (store, collector) = reviewed_ready(temp.path(), server.endpoint());
        for _ in 0..staged_steps {
            assert_eq!(collector.tick(), TickOutcome::Progress);
        }
        let path = store.pending_paths().unwrap().pop().unwrap();
        let mut a = read_pending(&path);
        assert!(!a.may_have_submitted());
        a.retry_at = u64::MAX;
        a.save(&path).unwrap();
        let connection = store.connection().unwrap();
        let cutoff = server.requests().len();
        approve_replacement(temp.path(), &store);
        assert!(!a.allowed(&store.policy().unwrap().unwrap()));

        let restarted = Collector::new(temp.path().join("data"), store.root().to_owned());
        assert_eq!(restarted.tick(), TickOutcome::Progress); // Discard A and capture B.
        assert_eq!(server.requests().len(), cutoff);
        let b = read_pending(&path);
        assert_ne!(b.operation.idempotency_key, a.operation.idempotency_key);
        assert_ne!(b.member.sha256, a.member.sha256);
        assert_eq!(b.operation.expected_revision, None);
        assert_eq!(b.operation.expected_sequence, None);
        let b_bytes = fs::read(path.join("payload")).unwrap();
        for _ in 0..3 {
            assert_eq!(restarted.tick(), TickOutcome::Progress);
        }
        assert_eq!(store.connection().unwrap(), connection);
        assert_eq!(store.status().unwrap().stored_sessions, 1);
        assert_eq!(remote.lock().unwrap().receipts.len(), 1);
        let requests = server.requests();
        assert_eq!(requests.len() - cutoff, 3);
        assert_eq!(requests[cutoff + 1].body, b_bytes);
        let submitted: PublishRequest = serde_json::from_slice(&requests[cutoff + 2].body).unwrap();
        assert_eq!(submitted.operation, b.operation);
    }
}

#[test]
fn denied_accepted_revision_settles_before_approved_replacement() {
    let temp = tempdir().unwrap();
    let (server, remote) = accepting_remote();
    let (store, collector) = reviewed_ready(temp.path(), server.endpoint());
    remote.lock().unwrap().drop_publish_ack = true;
    assert_eq!(collector.tick(), TickOutcome::Progress);
    assert_eq!(collector.tick(), TickOutcome::Progress);
    assert_eq!(collector.tick(), TickOutcome::Failed(Error::Unavailable));
    let path = store.pending_paths().unwrap().pop().unwrap();
    let a = read_pending(&path);
    let a_bytes = fs::read(path.join("payload")).unwrap();
    assert!(a.publish_attempted && a.lookup_receipt);
    let cutoff = server.requests().len();
    approve_replacement(temp.path(), &store);
    assert!(!a.allowed(&store.policy().unwrap().unwrap()));
    expire_backoff(&store);
    let restarted = Collector::new(temp.path().join("data"), store.root().to_owned());
    store.pause(true).unwrap();
    assert_eq!(restarted.tick(), TickOutcome::Paused);
    assert_eq!(server.requests().len(), cutoff);
    store.pause(false).unwrap();

    remote.lock().unwrap().failure = Some(403);
    assert_eq!(restarted.tick(), TickOutcome::Failed(Error::Forbidden));
    assert_eq!(read_pending(&path).operation, a.operation);
    assert_eq!(fs::read(path.join("payload")).unwrap(), a_bytes);
    remote.lock().unwrap().failure = None;
    expire_backoff(&store);
    assert_eq!(restarted.tick(), TickOutcome::Progress); // Fence A's original accepted outcome.
    assert_eq!(store.status().unwrap().pending, 0);
    assert_eq!(store.status().unwrap().stored_sessions, 1);
    assert!(!a.allowed(&store.policy().unwrap().unwrap()));
    let requests = server.requests();
    for request in &requests[cutoff..] {
        assert_eq!(request.method, "POST");
        assert!(request.path.ends_with("/operations/cancel"));
        let cancel: CancelPublishRequest = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(cancel.operation, a.operation);
        assert_eq!(cancel.fingerprint, cancellation(&a).fingerprint);
    }
    assert_eq!(restarted.tick(), TickOutcome::Progress); // Capture B against accepted A.
    let b = read_pending(&path);
    assert_eq!(
        b.operation.expected_revision,
        Some(a.operation.revision.clone())
    );
    assert_eq!(b.operation.expected_sequence, Some(1));
    remote.lock().unwrap().expiry = Some((b.member.sha256.clone(), "chunk"));
    assert_eq!(restarted.tick(), TickOutcome::Progress);
    assert_eq!(restarted.tick(), TickOutcome::Failed(Error::StagingExpired));
    assert_eq!(read_pending(&path).operation, b.operation); // Keep the exact accepted sequence on retry.
    remote.lock().unwrap().expiry = None;
    expire_backoff(&store);
    for _ in 0..3 {
        assert_eq!(restarted.tick(), TickOutcome::Progress);
    }
    assert_eq!(remote.lock().unwrap().receipts.len(), 2);
    let publications: Vec<PublishRequest> = server
        .requests()
        .iter()
        .filter(|r| r.path.ends_with("/revisions"))
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(publications.len(), 2);
    assert_eq!(publications[0].operation, a.operation);
    assert_eq!(publications[1].operation, b.operation);
}

#[test]
fn denied_uncertain_submission_settles_after_missing_receipts_and_retries_lost_cancellation_ack() {
    let temp = tempdir().unwrap();
    let (server, remote) = accepting_remote();
    let (store, collector) = reviewed_ready(temp.path(), server.endpoint());
    assert_eq!(collector.tick(), TickOutcome::Progress);
    assert_eq!(collector.tick(), TickOutcome::Progress);
    remote.lock().unwrap().failure = Some(503);
    assert_eq!(collector.tick(), TickOutcome::Failed(Error::Unavailable));
    remote.lock().unwrap().failure = None;
    expire_backoff(&store);
    assert_eq!(collector.tick(), TickOutcome::Progress); // Authorized receipt404.
    let path = store.pending_paths().unwrap().pop().unwrap();
    let a = read_pending(&path);
    assert!(!a.lookup_receipt && a.publish_attempted);
    let bytes = fs::read(path.join("payload")).unwrap();
    let cutoff = server.requests().len();
    approve_replacement(temp.path(), &store);
    let restarted = Collector::new(temp.path().join("data"), store.root().to_owned());
    remote.lock().unwrap().failure = Some(503);
    for _ in 0..2 {
        expire_backoff(&store);
        assert_eq!(restarted.tick(), TickOutcome::Failed(Error::Unavailable));
        assert_eq!(read_pending(&path).operation, a.operation);
        assert_eq!(fs::read(path.join("payload")).unwrap(), bytes);
        assert_eq!(restarted.tick(), TickOutcome::Idle); // Observe B while receipt is in backoff.
        let selection = store.status().unwrap().selection.unwrap();
        assert_eq!(selection.selected(), 1);
    }
    for request in &server.requests()[cutoff..] {
        assert_eq!(request.method, "POST");
        assert!(request.path.ends_with("/operations/cancel"));
    }
    remote.lock().unwrap().failure = None;
    remote.lock().unwrap().drop_cancel_ack = true;
    expire_backoff(&store);
    assert_eq!(restarted.tick(), TickOutcome::Failed(Error::Unavailable));
    assert_eq!(remote.lock().unwrap().cancelled.len(), 1);
    assert_eq!(fs::read(path.join("payload")).unwrap(), bytes);
    expire_backoff(&store);
    assert_eq!(restarted.tick(), TickOutcome::Progress);
    assert!(store
        .publication_checkpoint(&a.operation.publication)
        .unwrap()
        .is_none());
    assert!(store.checkpoint_path(&a.operation.publication).exists());
    assert_eq!(store.status().unwrap().stored_sessions, 0);
    assert_eq!(restarted.tick(), TickOutcome::Progress);
    let b = read_pending(&path);
    assert_eq!(b.operation.expected_revision, None);
    assert_eq!(b.operation.expected_sequence, None);
    for _ in 0..3 {
        assert_eq!(restarted.tick(), TickOutcome::Progress);
    }
    assert_eq!(remote.lock().unwrap().receipts.len(), 1);
    assert_eq!(remote.lock().unwrap().cancelled.len(), 1);
}

fn cancellation(pending: &Pending) -> CancelPublishRequest {
    let publisher = pending.publisher.clone().unwrap();
    CancelPublishRequest {
        fingerprint: publish_fingerprint(
            &publisher,
            &pending.operation,
            &pending.identity,
            &pending.member,
        )
        .unwrap(),
        publisher,
        operation: pending.operation.clone(),
    }
}

fn before_dispatch(store: &SharingStore, collector: &Collector) -> (std::path::PathBuf, Pending) {
    assert_eq!(collector.tick(), TickOutcome::Progress);
    assert_eq!(collector.tick(), TickOutcome::Progress);
    let path = store.pending_paths().unwrap().pop().unwrap();
    let mut pending = read_pending(&path);
    pending.publish_attempted = true;
    pending.lookup_receipt = true;
    pending.save(&path).unwrap(); // Crash immediately before first HTTP publication.
    (path, pending)
}

#[test]
fn lost_cancellation_ack_then_reviewed_reapproval_finishes_settlement_before_receipt_lookup() {
    reapprove_after_lost_cancellation(true);
}

#[test]
fn lost_cancellation_ack_then_automatic_reapproval_finishes_settlement_before_publish() {
    reapprove_after_lost_cancellation(false);
}

fn reapprove_after_lost_cancellation(lookup_receipt: bool) {
    let temp = tempdir().unwrap();
    let (server, remote) = accepting_remote();
    let (store, collector) = reviewed_ready(temp.path(), server.endpoint());
    let (path, mut a) = before_dispatch(&store, &collector);
    a.lookup_receipt = lookup_receipt;
    a.save(&path).unwrap();
    let bytes = fs::read(path.join("payload")).unwrap();
    let mut policy = store.policy().unwrap().unwrap();
    policy.revision += 1;
    policy.mode = PublicationMode::Reviewed {
        revisions: BTreeSet::new(),
    };
    store.set_policy(policy.clone()).unwrap();
    remote.lock().unwrap().drop_cancel_ack = true;
    assert_eq!(collector.tick(), TickOutcome::Failed(Error::Unavailable));
    assert_eq!(remote.lock().unwrap().cancelled.len(), 1);
    let mut saved = read_pending(&path);
    assert!(saved.settlement_started);
    assert_eq!(saved.lookup_receipt, lookup_receipt);
    assert_eq!(saved.operation, a.operation);
    assert_eq!(saved.publisher, a.publisher);
    assert_eq!(fs::read(path.join("payload")).unwrap(), bytes);

    // The real server returns terminal409, not404, for this key. Neither
    // response permits the collector to abandon settlement or reuse the key.
    assert!(matches!(
        store
            .remote_client()
            .unwrap()
            .receipt(&a.operation.idempotency_key),
        Err(Error::Conflict)
    ));
    saved.retry_at = u64::MAX;
    saved.save(&path).unwrap();
    policy.revision += 1;
    policy.mode = if lookup_receipt {
        PublicationMode::Reviewed {
            revisions: BTreeSet::from([a.member.sha256.clone()]),
        }
    } else {
        PublicationMode::Automatic
    };
    store.set_policy(policy.clone()).unwrap();
    assert!(saved.allowed(&policy));
    let cutoff = server.requests().len();
    assert_eq!(collector.tick(), TickOutcome::Idle); // Reapproved A remains in backoff.
    assert_eq!(server.requests().len(), cutoff);
    let stamp_path = store.root().join("capture.json");
    let before: serde_json::Value = private_file::read(&stamp_path).unwrap();
    assert_eq!(before["complete"], true); // Identical pending bytes completed capture.
    assert_eq!(before["policy"], policy.revision);
    assert_eq!(store.status().unwrap().selection.unwrap().selected(), 1);

    expire_backoff(&store);
    let restarted = Collector::new(temp.path().join("data"), store.root().to_owned());
    assert_eq!(restarted.tick(), TickOutcome::Progress); // Must finish cancellation.
    let requests = server.requests();
    assert_eq!(requests.len(), cutoff + 1);
    assert!(requests[cutoff].path.ends_with("/operations/cancel"));
    let cancel: CancelPublishRequest = serde_json::from_slice(&requests[cutoff].body).unwrap();
    assert_eq!(cancel.operation, a.operation);
    assert_eq!(cancel.fingerprint, cancellation(&a).fingerprint);
    assert!(!path.exists());
    assert_eq!(store.status().unwrap().stored_sessions, 0);
    let after: serde_json::Value = private_file::read(&stamp_path).unwrap();
    assert_eq!(after["complete"], false);
    for field in ["generation", "policy", "counts"] {
        assert_eq!(after[field], before[field]);
    }

    let restarted = Collector::new(temp.path().join("data"), store.root().to_owned());
    assert_eq!(restarted.tick(), TickOutcome::Progress); // No new generation/policy.
    let fresh = read_pending(&path);
    assert_ne!(fresh.operation.idempotency_key, a.operation.idempotency_key);
    assert_eq!(fresh.member.sha256, a.member.sha256);
    assert_eq!(fresh.operation.expected_revision, None);
    assert_eq!(fresh.operation.expected_sequence, None);
    assert!(!fresh.settlement_started);
    assert_eq!(fs::read(path.join("payload")).unwrap(), bytes);
    for _ in 0..3 {
        assert_eq!(restarted.tick(), TickOutcome::Progress);
    }
    assert_eq!(store.status().unwrap().pending, 0);
    assert_eq!(remote.lock().unwrap().receipts.len(), 1);
    assert_eq!(remote.lock().unwrap().cancelled.len(), 1);
    let publications: Vec<PublishRequest> = server
        .requests()
        .iter()
        .filter(|r| r.path.ends_with("/revisions"))
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert_eq!(publications.len(), 1);
    assert_eq!(publications[0].operation, fresh.operation);
}

#[test]
fn capture_invalidation_failure_preserves_terminal_work_until_it_can_retire() {
    let temp = tempdir().unwrap();
    let (server, remote) = accepting_remote();
    let (store, collector) = reviewed_ready(temp.path(), server.endpoint());
    let (path, a) = before_dispatch(&store, &collector);
    let bytes = fs::read(path.join("payload")).unwrap();
    let mut policy = store.policy().unwrap().unwrap();
    policy.revision += 1;
    policy.mode = PublicationMode::Reviewed {
        revisions: BTreeSet::new(),
    };
    store.set_policy(policy.clone()).unwrap();
    let stamp_path = store.root().join("capture.json");
    let stamp: serde_json::Value = private_file::read(&stamp_path).unwrap();
    fs::remove_file(&stamp_path).unwrap();
    fs::create_dir(&stamp_path).unwrap();
    assert_eq!(collector.tick(), TickOutcome::Failed(Error::State));
    assert!(store.checkpoint_path(&a.operation.publication).exists());
    assert_eq!(remote.lock().unwrap().cancelled.len(), 1);
    let saved = read_pending(&path);
    assert!(saved.settlement_started);
    assert_eq!(saved.operation, a.operation);
    assert_eq!(fs::read(path.join("payload")).unwrap(), bytes);

    fs::remove_dir(&stamp_path).unwrap();
    private_file::write(&stamp_path, &stamp).unwrap();
    policy.revision += 1;
    policy.mode = PublicationMode::Automatic;
    store.set_policy(policy).unwrap();
    expire_backoff(&store);
    let restarted = Collector::new(temp.path().join("data"), store.root().to_owned());
    assert_eq!(restarted.tick(), TickOutcome::Progress);
    assert!(!path.exists());
    let stamp: serde_json::Value = private_file::read(&stamp_path).unwrap();
    assert_eq!(stamp["complete"], false);
    assert_eq!(restarted.tick(), TickOutcome::Progress);
    let fresh = read_pending(&path);
    assert_ne!(fresh.operation.idempotency_key, a.operation.idempotency_key);
    assert_eq!(fresh.member.sha256, a.member.sha256);
}

#[test]
fn generic_publish_and_receipt_conflicts_retain_the_operation_without_starting_settlement() {
    let temp = tempdir().unwrap();
    let (server, remote) = accepting_remote();
    let (store, collector) = reviewed_ready(temp.path(), server.endpoint());
    assert_eq!(collector.tick(), TickOutcome::Progress);
    assert_eq!(collector.tick(), TickOutcome::Progress);
    let path = store.pending_paths().unwrap().pop().unwrap();
    let a = read_pending(&path);
    let bytes = fs::read(path.join("payload")).unwrap();
    remote.lock().unwrap().failure = Some(409);
    assert_eq!(collector.tick(), TickOutcome::Failed(Error::Conflict));
    expire_backoff(&store);
    let restarted = Collector::new(temp.path().join("data"), store.root().to_owned());
    assert_eq!(restarted.tick(), TickOutcome::Failed(Error::Conflict));
    let saved = read_pending(&path);
    assert!(!saved.settlement_started);
    assert!(saved.publish_attempted && saved.lookup_receipt);
    assert_eq!(saved.operation, a.operation);
    assert_eq!(fs::read(path.join("payload")).unwrap(), bytes);
    assert!(remote.lock().unwrap().cancelled.is_empty());
    assert!(!store.checkpoint_path(&a.operation.publication).exists());

    remote.lock().unwrap().failure = None;
    expire_backoff(&store);
    assert_eq!(restarted.tick(), TickOutcome::Progress); // Missing, not cancelled, receipt.
    assert_eq!(restarted.tick(), TickOutcome::Progress); // Original key still succeeds.
    assert_eq!(remote.lock().unwrap().receipts.len(), 1);
    assert!(remote
        .lock()
        .unwrap()
        .receipts
        .contains_key(&a.operation.idempotency_key));
    assert!(server
        .requests()
        .iter()
        .all(|r| !r.path.ends_with("/operations/cancel")));
}

#[test]
fn crash_before_dispatch_is_cancelled_and_credentials_remain_bound_to_original_publisher() {
    let temp = tempdir().unwrap();
    let (server, remote) = accepting_remote();
    let (store, collector) = reviewed_ready(temp.path(), server.endpoint());
    let (path, a) = before_dispatch(&store, &collector);
    let bytes = fs::read(path.join("payload")).unwrap();
    approve_replacement(temp.path(), &store);
    let connection = store.connection().unwrap().unwrap();
    assert_eq!(
        store.connect(
            connection.clone(),
            Credentials::device("synthetic-other-token".into()).unwrap(),
        ),
        Err(Error::Credentials)
    );
    assert_eq!(fs::read(path.join("payload")).unwrap(), bytes);
    assert_eq!(read_pending(&path).publisher, a.publisher);
    assert!(remote.lock().unwrap().cancelled.is_empty());
    store
        .connect(
            connection,
            Credentials::device("synthetic-rotated-token".into()).unwrap(),
        )
        .unwrap();
    let cutoff = server.requests().len();
    expire_backoff(&store);
    assert_eq!(collector.tick(), TickOutcome::Progress);
    assert_eq!(remote.lock().unwrap().cancelled.len(), 1);
    assert_eq!(store.status().unwrap().stored_sessions, 0);
    for request in &server.requests()[cutoff..] {
        assert!(request.path.ends_with("/operations/cancel"));
        let request: CancelPublishRequest = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(request.publisher, a.publisher.clone().unwrap());
        assert_eq!(request.fingerprint, cancellation(&a).fingerprint);
    }
    assert_eq!(collector.tick(), TickOutcome::Progress);
    for _ in 0..3 {
        assert_eq!(collector.tick(), TickOutcome::Progress);
    }
    assert_eq!(remote.lock().unwrap().receipts.len(), 1);
    assert!(server
        .requests()
        .iter()
        .filter(|r| r.path.ends_with("/revisions"))
        .all(|r| serde_json::from_slice::<PublishRequest>(&r.body)
            .unwrap()
            .operation
            .idempotency_key
            != a.operation.idempotency_key));
}

#[test]
fn accepted_settlement_uses_current_observation_not_historical_receipt_and_skips_current_bytes() {
    for already_current in [false, true] {
        let temp = tempdir().unwrap();
        let (server, remote) = accepting_remote();
        let (store, collector) = reviewed_ready(temp.path(), server.endpoint());
        remote.lock().unwrap().drop_publish_ack = true;
        assert_eq!(collector.tick(), TickOutcome::Progress);
        assert_eq!(collector.tick(), TickOutcome::Progress);
        assert_eq!(collector.tick(), TickOutcome::Failed(Error::Unavailable));
        let path = store.pending_paths().unwrap().pop().unwrap();
        let a = read_pending(&path);
        approve_replacement(temp.path(), &store);
        let policy = store.policy().unwrap().unwrap();
        let PublicationMode::Reviewed { revisions } = policy.mode else {
            panic!("reviewed fixture")
        };
        let mut current = publication(&receipt_for(&a));
        current.sequence = 7;
        current.revision = if already_current {
            revisions.into_iter().next().unwrap()
        } else {
            "e".repeat(64)
        };
        remote
            .lock()
            .unwrap()
            .current
            .insert(a.operation.publication.clone(), Some(current.clone()));
        expire_backoff(&store);
        assert_eq!(collector.tick(), TickOutcome::Progress);
        let saved = store
            .publication_checkpoint(&a.operation.publication)
            .unwrap()
            .unwrap();
        assert_eq!(
            (saved.revision, saved.sequence),
            (current.revision.clone(), 7)
        );
        let requests = server.requests().len();
        if already_current {
            assert_eq!(collector.tick(), TickOutcome::Idle);
            assert_eq!(store.status().unwrap().pending, 0);
            assert_eq!(server.requests().len(), requests);
        } else {
            assert_eq!(collector.tick(), TickOutcome::Progress);
            let b = read_pending(&path);
            assert_eq!(b.operation.expected_revision, Some(current.revision));
            assert_eq!(b.operation.expected_sequence, Some(7));
            for _ in 0..3 {
                assert_eq!(collector.tick(), TickOutcome::Progress);
            }
            assert_eq!(store.status().unwrap().last_accepted_sequence, Some(8));
        }
    }
}

#[test]
fn settlement_checkpoint_and_retirement_crashes_recover_with_exact_current_predecessor() {
    for boundary in 0..3 {
        let temp = tempdir().unwrap();
        let (server, remote) = accepting_remote();
        let (store, collector) = reviewed_ready(temp.path(), server.endpoint());
        let (path, a) = before_dispatch(&store, &collector);
        approve_replacement(temp.path(), &store);
        let request = cancellation(&a);
        let mut current = publication(&receipt_for(&a));
        current.revision = "c".repeat(64);
        current.sequence = 11;
        remote
            .lock()
            .unwrap()
            .current
            .insert(a.operation.publication.clone(), Some(current.clone()));
        let response = store
            .remote_client()
            .unwrap()
            .cancel_publish(&request)
            .unwrap();
        let checkpoint = store.checkpoint_path(&a.operation.publication);
        if boundary == 0 {
            fs::create_dir(&checkpoint).unwrap();
            assert_eq!(
                store.settled(&path, &a, &request, &response),
                Err(Error::State)
            );
            assert!(path.join("payload").exists());
            fs::remove_dir(&checkpoint).unwrap();
        } else {
            store.record_settlement(&a, &request, &response).unwrap();
        }
        let retired = if boundary == 2 {
            let retired = store
                .root()
                .join("queue")
                .join(format!(".settled-{}", uuid::Uuid::new_v4()));
            fs::rename(&path, &retired).unwrap();
            Some(retired)
        } else {
            None
        };
        let restarted = Collector::new(temp.path().join("data"), store.root().to_owned());
        if boundary < 2 {
            assert_eq!(restarted.tick(), TickOutcome::Progress);
        }
        assert_eq!(restarted.tick(), TickOutcome::Progress);
        let b = read_pending(&path);
        assert_eq!(b.operation.expected_revision, Some(current.revision));
        assert_eq!(b.operation.expected_sequence, Some(11));
        if let Some(retired) = retired {
            assert!(!retired.exists());
        }
        for _ in 0..3 {
            assert_eq!(restarted.tick(), TickOutcome::Progress);
        }
        assert_eq!(store.status().unwrap().last_accepted_sequence, Some(12));
        assert_eq!(remote.lock().unwrap().cancelled.len(), 1);
        assert_eq!(remote.lock().unwrap().receipts.len(), 1);
    }
}

#[test]
fn invalid_settlement_and_withdrawn_or_other_owner_state_preserve_pending_work() {
    let temp = tempdir().unwrap();
    let (server, _) = accepting_remote();
    let (store, collector) = reviewed_ready(temp.path(), server.endpoint());
    let (path, a) = before_dispatch(&store, &collector);
    approve_replacement(temp.path(), &store);
    let request = cancellation(&a);
    let bytes = fs::read(path.join("payload")).unwrap();
    let checkpoint = store.checkpoint_path(&a.operation.publication);
    for mismatch in 0..4 {
        let mut invalid = CancelPublishResponse {
            outcome: CancelPublishOutcome::Cancelled {
                publisher: request.publisher.clone(),
                operation: request.operation.clone(),
                fingerprint: request.fingerprint.clone(),
            },
            publication: None,
        };
        if let CancelPublishOutcome::Cancelled {
            publisher,
            operation,
            fingerprint,
        } = &mut invalid.outcome
        {
            match mismatch {
                0 => *publisher = "unrelated-owner".into(),
                1 => operation.idempotency_key = "unrelated-operation".into(),
                2 => *fingerprint = "f".repeat(64),
                _ => {
                    let mut p = publication(&receipt_for(&a));
                    p.publication = "other-publication".into();
                    invalid.publication = Some(p);
                }
            }
        }
        assert_eq!(
            store.settled(&path, &a, &request, &invalid),
            Err(Error::Protocol)
        );
        assert!(!checkpoint.exists());
        assert_eq!(fs::read(path.join("payload")).unwrap(), bytes);
    }
    for reason in 0..3 {
        let mut current = publication(&receipt_for(&a));
        match reason {
            0 => current.withdrawn = true,
            1 => current.owner = "other-owner".into(),
            _ => current.writer_epoch += 1,
        }
        let response = CancelPublishResponse {
            outcome: CancelPublishOutcome::Cancelled {
                publisher: request.publisher.clone(),
                operation: request.operation.clone(),
                fingerprint: request.fingerprint.clone(),
            },
            publication: Some(current.clone()),
        };
        assert_eq!(
            store.settled(&path, &a, &request, &response),
            Err(Error::Conflict)
        );
        let saved = store
            .publication_checkpoint(&a.operation.publication)
            .unwrap()
            .unwrap();
        assert_eq!(saved.owner, current.owner);
        assert_eq!(saved.withdrawn, current.withdrawn);
        assert_eq!(saved.writer_epoch, current.writer_epoch);
        assert_eq!(fs::read(path.join("payload")).unwrap(), bytes);
        assert_eq!(read_pending(&path).operation, a.operation);
    }
}

#[test]
fn staging_principal_changes_never_rebind_a_saved_operation() {
    for reset_staging in [false, true] {
        let temp = tempdir().unwrap();
        let (server, remote) = accepting_remote();
        let (store, collector) = reviewed_ready(temp.path(), server.endpoint());
        assert_eq!(collector.tick(), TickOutcome::Progress);
        let path = store.pending_paths().unwrap().pop().unwrap();
        let a = read_pending(&path);
        if reset_staging {
            remote.lock().unwrap().expiry = Some((a.member.sha256.clone(), "chunk"));
            assert_eq!(collector.tick(), TickOutcome::Failed(Error::StagingExpired));
            remote.lock().unwrap().expiry = None;
            expire_backoff(&store);
        }
        let connection = store.connection().unwrap().unwrap();
        assert_eq!(
            store.connect(
                connection.clone(),
                Credentials::device("synthetic-other-token".into()).unwrap(),
            ),
            Err(Error::Credentials)
        );
        let saved = read_pending(&path);
        assert_eq!(saved.publisher, a.publisher);
        assert_eq!(saved.operation, a.operation);
        assert!(!saved.publish_attempted);
        assert!(remote.lock().unwrap().receipts.is_empty());
        store
            .connect(
                connection,
                Credentials::device("synthetic-rotated-token".into()).unwrap(),
            )
            .unwrap();
        expire_backoff(&store);
        for _ in 0..3 {
            if store.status().unwrap().pending == 0 {
                break;
            }
            assert_eq!(collector.tick(), TickOutcome::Progress);
        }
        assert_eq!(store.status().unwrap().pending, 0);
        assert_eq!(remote.lock().unwrap().receipts.len(), 1);
    }
}
