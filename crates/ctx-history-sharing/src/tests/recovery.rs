use sha2::{Digest, Sha256};

use super::*;
use crate::queue::Pending;
use ctx_history_server::{
    CancelPublishOutcome, CancelPublishRequest, CancelPublishResponse, PublicationState,
};

#[derive(Default)]
pub(super) struct RemoteState {
    pub(super) failure: Option<u16>,
    uploads: BTreeMap<String, (UploadSpec, Vec<u8>)>,
    pub(super) receipts: BTreeMap<String, Receipt>,
    pub(super) drop_publish_ack: bool,
    pub(super) expiry: Option<(String, &'static str)>,
    pub(super) cancelled: BTreeMap<String, CancelPublishRequest>,
    pub(super) current: BTreeMap<String, Option<PublicationState>>,
    pub(super) drop_cancel_ack: bool,
}

pub(super) fn accepting_remote() -> (Mock, Arc<Mutex<RemoteState>>) {
    let state = Arc::new(Mutex::new(RemoteState::default()));
    let remote = state.clone();
    let mock = Mock::new(move |request| {
        let publisher = match request.authorization.strip_prefix("Bearer ").unwrap() {
            TOKEN | "synthetic-rotated-token" => "synthetic-publisher",
            "synthetic-other-token" => "other-publisher",
            _ => panic!("unexpected synthetic credential"),
        };
        let mut state = remote.lock().unwrap();
        if let Some(status) = state.failure {
            return Response::Raw(status, "{}".into(), vec![]);
        }
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", path) if path.ends_with("/operations/cancel") => {
                let cancel: CancelPublishRequest = serde_json::from_slice(&request.body).unwrap();
                if cancel.publisher != publisher {
                    return Response::Raw(403, "{}".into(), vec![]);
                }
                let outcome = if let Some(receipt) =
                    state.receipts.get(&cancel.operation.idempotency_key)
                {
                    assert_eq!(receipt.operation, cancel.operation);
                    CancelPublishOutcome::Accepted {
                        receipt: receipt.clone(),
                    }
                } else {
                    if let Some(previous) = state.cancelled.get(&cancel.operation.idempotency_key) {
                        assert_eq!(previous.operation, cancel.operation);
                        assert_eq!(previous.fingerprint, cancel.fingerprint);
                    }
                    state
                        .cancelled
                        .insert(cancel.operation.idempotency_key.clone(), cancel.clone());
                    CancelPublishOutcome::Cancelled {
                        publisher: cancel.publisher.clone(),
                        operation: cancel.operation.clone(),
                        fingerprint: cancel.fingerprint,
                    }
                };
                let response = CancelPublishResponse {
                    outcome,
                    publication: current_publication(&state, &cancel.operation.publication),
                };
                if std::mem::take(&mut state.drop_cancel_ack) {
                    Response::Drop
                } else {
                    Response::json(200, &response)
                }
            }
            ("POST", path) if path.ends_with("/uploads") => {
                let spec: UploadSpec = serde_json::from_slice(&request.body).unwrap();
                let id = format!("upload-{}", state.uploads.len());
                let status = UploadStatus {
                    publisher: publisher.into(),
                    id: id.clone(),
                    received_bytes: 0,
                    expected_bytes: spec.bytes,
                    expires_at: u64::MAX,
                };
                state.uploads.insert(id, (spec, Vec::new()));
                Response::json(200, &status)
            }
            ("PUT", path) => {
                let (path, offset) = path.split_once("?offset=").unwrap();
                let id = path.rsplit('/').next().unwrap();
                if state.expiry.as_ref().is_some_and(|(sha, phase)| {
                    *phase == "chunk" && *sha == state.uploads[id].0.sha256
                }) {
                    return Response::Raw(410, "{}".into(), vec![]);
                }
                let (spec, bytes) = state.uploads.get_mut(id).unwrap();
                assert_eq!(offset.parse::<usize>().unwrap(), bytes.len());
                bytes.extend_from_slice(&request.body);
                Response::json(
                    200,
                    &UploadStatus {
                        publisher: publisher.into(),
                        id: id.into(),
                        received_bytes: bytes.len() as u64,
                        expected_bytes: spec.bytes,
                        expires_at: u64::MAX,
                    },
                )
            }
            ("POST", path) if path.ends_with("/revisions") => {
                let publish: PublishRequest = serde_json::from_slice(&request.body).unwrap();
                if state
                    .cancelled
                    .contains_key(&publish.operation.idempotency_key)
                {
                    return Response::Raw(
                        409,
                        "{\"error\":\"operation_cancelled\"}".into(),
                        vec![],
                    );
                }
                if state.expiry.as_ref().is_some_and(|(sha, phase)| {
                    *phase == "publish" && *sha == publish.member.sha256
                }) {
                    return Response::Raw(410, "{}".into(), vec![]);
                }
                let predecessor = current_publication(&state, &publish.operation.publication)
                    .map(|p| (p.revision, p.sequence));
                assert_eq!(
                    publish.operation.expected_revision,
                    predecessor.as_ref().map(|r| r.0.clone())
                );
                assert_eq!(
                    publish.operation.expected_sequence,
                    predecessor.map(|r| r.1)
                );
                let (spec, bytes) = state.uploads.get(&publish.upload).unwrap();
                assert_eq!(capture::hex(&Sha256::digest(bytes)), spec.sha256);
                assert_eq!(bytes.len() as u64, spec.bytes);
                assert_eq!(publish.member.sha256, spec.sha256);
                let receipt = Receipt {
                    collection: COLLECTION.into(),
                    publisher: publisher.into(),
                    operation: publish.operation,
                    sequence: state
                        .receipts
                        .values()
                        .map(|r| r.sequence)
                        .chain(state.current.values().flatten().map(|p| p.sequence))
                        .max()
                        .unwrap_or(0)
                        + 1,
                    kind: "publish".into(),
                    payload: Some(spec.clone()),
                    accepted_at: 1,
                };
                assert!(state
                    .receipts
                    .insert(receipt.operation.idempotency_key.clone(), receipt.clone())
                    .is_none());
                state.current.insert(
                    receipt.operation.publication.clone(),
                    Some(publication(&receipt)),
                );
                if std::mem::take(&mut state.drop_publish_ack) {
                    Response::Drop
                } else {
                    Response::json(200, &receipt)
                }
            }
            ("GET", path) if path.contains("/receipts/") => {
                let key = path.rsplit('/').next().unwrap();
                match state.receipts.get(key) {
                    Some(receipt) => Response::json(200, receipt),
                    None if state.cancelled.contains_key(key) => {
                        Response::Raw(409, "{\"error\":\"operation_cancelled\"}".into(), vec![])
                    }
                    None => Response::Raw(404, "{}".into(), vec![]),
                }
            }
            ("GET", path) if path.contains("/uploads/") => {
                let id = path.rsplit('/').next().unwrap();
                let (spec, bytes) = &state.uploads[id];
                if state
                    .expiry
                    .as_ref()
                    .is_some_and(|(sha, phase)| *phase == "offset" && *sha == spec.sha256)
                {
                    return Response::Raw(404, "{}".into(), vec![]);
                }
                Response::json(
                    200,
                    &UploadStatus {
                        publisher: publisher.into(),
                        id: id.into(),
                        received_bytes: bytes.len() as u64,
                        expected_bytes: spec.bytes,
                        expires_at: u64::MAX,
                    },
                )
            }
            _ => panic!("unexpected recovery request"),
        }
    });
    (mock, state)
}

pub(super) fn read_pending(path: &Path) -> Pending {
    private_file::read(&path.join("pending.json")).unwrap()
}

pub(super) fn receipt_for(pending: &Pending) -> Receipt {
    Receipt {
        collection: COLLECTION.into(),
        publisher: "synthetic-publisher".into(),
        operation: pending.operation.clone(),
        sequence: 1,
        kind: "publish".into(),
        payload: Some(UploadSpec {
            sha256: pending.member.sha256.clone(),
            bytes: pending.member.bytes,
        }),
        accepted_at: 1,
    }
}

pub(super) fn publication(receipt: &Receipt) -> PublicationState {
    PublicationState {
        publication: receipt.operation.publication.clone(),
        owner: receipt.publisher.clone(),
        revision: receipt.operation.revision.clone(),
        sequence: receipt.sequence,
        writer_epoch: receipt.operation.writer_epoch,
        policy_revision: receipt.operation.policy_revision,
        withdrawn: false,
    }
}

fn current_publication(state: &RemoteState, id: &str) -> Option<PublicationState> {
    state.current.get(id).cloned().unwrap_or_else(|| {
        state
            .receipts
            .values()
            .filter(|r| r.operation.publication == id)
            .max_by_key(|r| r.sequence)
            .map(publication)
    })
}

#[test]
fn backoff_captures_new_sessions_without_replacing_unresolved_revisions() {
    for (failure, error) in [(503, Error::Unavailable), (409, Error::Conflict)] {
        let temp = tempdir().unwrap();
        let data = temp.path().join("data");
        let (server, remote) = accepting_remote();
        remote.lock().unwrap().failure = Some(failure);
        let (store, collector, mut policy) = make_ready(temp.path(), server.endpoint());
        let a_path = store.pending_paths().unwrap().pop().unwrap();
        assert_eq!(collector.tick(), TickOutcome::Failed(error));
        let mut a = read_pending(&a_path);
        a.retry_at = u64::MAX; // Deterministic backoff without sleeping.
        a.save(&a_path).unwrap();
        let a_metadata = fs::read(a_path.join("pending.json")).unwrap();
        let a_bytes = fs::read(a_path.join("payload")).unwrap();

        let original = synthetic_record("session-one", ORIGINAL_TEXT);
        let b_record = synthetic_record("session-two", "committed during the outage");
        commit_records(&data, &[original, b_record.clone()], 2);
        assert_eq!(collector.tick(), TickOutcome::Progress); // Capture B in A's backoff.
        let b_path = store
            .pending_paths()
            .unwrap()
            .into_iter()
            .find(|path| *path != a_path)
            .unwrap();
        let b = read_pending(&b_path);
        let b_bytes = fs::read(b_path.join("payload")).unwrap();
        let stamp: serde_json::Value =
            private_file::read(&store.root().join("capture.json")).unwrap();
        assert_eq!(
            stamp["generation"],
            capture::open_index(&data).unwrap().generation_id()
        );
        assert_eq!(server.requests().len(), 1);

        let mut sleeping_b = read_pending(&b_path);
        sleeping_b.retry_at = u64::MAX;
        sleeping_b.save(&b_path).unwrap();
        assert_eq!(collector.tick(), TickOutcome::Idle); // Unchanged generation checkpoint.
        assert_eq!(server.requests().len(), 1);

        // An even newer revision of A must wait, while B remains independent.
        let corrected = synthetic_record("session-one", "corrected local A");
        commit_records(&data, &[corrected, b_record], 3);
        assert_eq!(collector.tick(), TickOutcome::Idle);
        assert_eq!(store.status().unwrap().pending, 2);
        assert_eq!(fs::read(a_path.join("pending.json")).unwrap(), a_metadata);
        assert_eq!(fs::read(a_path.join("payload")).unwrap(), a_bytes);

        remote.lock().unwrap().failure = None;
        sleeping_b.retry_at = 0;
        sleeping_b.save(&b_path).unwrap();
        store.pause(true).unwrap();
        assert_eq!(collector.tick(), TickOutcome::Paused);
        store.pause(false).unwrap();
        policy.revision += 1;
        policy.sources[0].whole_source = false;
        policy.sources[0].work_roots = vec![temp.path().join("allowed-work")];
        store.set_policy(policy.clone()).unwrap();
        assert_eq!(collector.tick(), TickOutcome::Idle);
        assert_eq!(store.status().unwrap().held, 2);
        assert_eq!(server.requests().len(), 1);

        policy.revision += 1;
        policy.sources[0].whole_source = true;
        policy.sources[0].work_roots.clear();
        store.set_policy(policy).unwrap();
        for _ in 0..3 {
            assert_eq!(collector.tick(), TickOutcome::Progress); // B begin/chunk/accept.
        }
        assert_eq!(store.status().unwrap().stored_sessions, 1);
        assert_eq!(store.status().unwrap().pending, 1);
        assert_eq!(fs::read(a_path.join("pending.json")).unwrap(), a_metadata);
        assert_eq!(fs::read(a_path.join("payload")).unwrap(), a_bytes);
        assert_eq!(server.requests()[2].body, b_bytes);
        let accepted_b = store
            .publication_checkpoint(&b.operation.publication)
            .unwrap()
            .unwrap();
        assert_eq!(accepted_b.revision, b.operation.revision);

        expire_backoff(&store);
        for _ in 0..3 {
            assert_eq!(collector.tick(), TickOutcome::Progress); // A's original operation.
        }
        let accepted_a = store
            .publication_checkpoint(&a.operation.publication)
            .unwrap()
            .unwrap();
        assert_eq!(accepted_a.revision, a.operation.revision);
        assert_eq!(server.requests()[5].body, a_bytes);
        assert_eq!(collector.tick(), TickOutcome::Progress); // Now capture A's successor.
        let successor = read_pending(&a_path);
        assert_eq!(
            successor.operation.expected_revision,
            Some(a.operation.revision)
        );
        assert_eq!(
            successor.operation.expected_sequence,
            Some(accepted_a.sequence)
        );
        assert_ne!(
            successor.operation.idempotency_key,
            a.operation.idempotency_key
        );
        assert_ne!(successor.member.sha256, a.member.sha256);
    }
}

#[test]
fn restart_at_acceptance_boundaries_cleans_only_retired_work_and_advances_neighbors() {
    for boundary in 0..4 {
        let temp = tempdir().unwrap();
        let data = temp.path().join("data");
        let (server, remote) = accepting_remote();
        let (store, collector, _) = make_ready(temp.path(), server.endpoint());
        let a_path = store.pending_paths().unwrap().pop().unwrap();
        let mut a = read_pending(&a_path);
        a.retry_at = u64::MAX;
        a.save(&a_path).unwrap();
        commit_records(
            &data,
            &[
                synthetic_record("session-one", ORIGINAL_TEXT),
                synthetic_record("session-two", "neighbor survives interrupted cleanup"),
            ],
            2,
        );
        assert_eq!(collector.tick(), TickOutcome::Progress);
        a.retry_at = 0;
        a.lookup_receipt = true; // Already persisted before the final POST.
        a.publisher = Some("synthetic-publisher".into());
        a.save(&a_path).unwrap();
        let receipt = receipt_for(&a);
        remote
            .lock()
            .unwrap()
            .receipts
            .insert(a.operation.idempotency_key.clone(), receipt.clone());

        // 0: server accepted, no local receipt; 1: receipt durable, no rename;
        // 2: rename visible, no directory fsync; 3: interrupted recursive delete.
        if boundary >= 1 {
            store.record_receipt(&a, &receipt).unwrap();
        }
        let retired = match boundary {
            2 => {
                let path = store
                    .root()
                    .join("queue")
                    .join(format!(".accepted-{}", uuid::Uuid::new_v4()));
                fs::rename(&a_path, &path).unwrap();
                Some(path)
            }
            3 => {
                let path = store.retire_accepted(&a_path).unwrap();
                fs::remove_file(path.join("pending.json")).unwrap();
                Some(path)
            }
            _ => None,
        };
        assert_eq!(
            store.status().unwrap().pending,
            if boundary < 2 { 2 } else { 1 }
        );
        let restarted = Collector::new(data, store.root().to_owned());
        for _ in 0..5 {
            if store.status().unwrap().pending == 0 {
                break;
            }
            assert_eq!(restarted.tick(), TickOutcome::Progress);
        }
        assert_eq!(store.status().unwrap().pending, 0);
        assert_eq!(store.status().unwrap().stored_sessions, 2);
        assert_eq!(restarted.tick(), TickOutcome::Idle);
        if let Some(path) = retired {
            assert!(!path.exists());
        }
        let stored = store
            .publication_checkpoint(&a.operation.publication)
            .unwrap()
            .unwrap();
        assert_eq!(stored.revision, a.operation.revision);
        let requests = server.requests();
        let published: Vec<PublishRequest> = requests
            .iter()
            .filter(|r| r.method == "POST" && r.path.ends_with("/revisions"))
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect();
        assert_eq!(published.len(), 1); // Only B; A never receives a fresh identity.
        assert_ne!(published[0].operation.publication, a.operation.publication);
        let lookups: Vec<_> = requests.iter().filter(|r| r.method == "GET").collect();
        assert_eq!(lookups.len(), usize::from(boundary < 2));
        if let Some(lookup) = lookups.first() {
            assert!(lookup.path.ends_with(&a.operation.idempotency_key));
        }
    }
}

#[test]
fn unmatched_receipts_and_receipt_write_failures_never_discard_live_bytes() {
    let temp = tempdir().unwrap();
    let server = Mock::new(|_| panic!("cleanup must not use the network"));
    let (store, collector, _) = make_ready(temp.path(), server.endpoint());
    let path = store.pending_paths().unwrap().pop().unwrap();
    let mut pending = read_pending(&path);
    pending.publisher = Some("synthetic-publisher".into());
    pending.save(&path).unwrap();
    let metadata = fs::read(path.join("pending.json")).unwrap();
    let bytes = fs::read(path.join("payload")).unwrap();
    let receipt = receipt_for(&pending);
    let checkpoint_path = store.checkpoint_path(&pending.operation.publication);
    for mismatch in 0..3 {
        let mut invalid = receipt.clone();
        match mismatch {
            0 => invalid.collection = "another-collection".into(),
            1 => invalid.operation.idempotency_key = "another-operation".into(),
            _ => invalid.payload.as_mut().unwrap().sha256 = "0".repeat(64),
        }
        assert_eq!(
            store.accepted(&path, &pending, invalid),
            Err(Error::Protocol)
        );
        assert!(!checkpoint_path.exists());
    }
    // A filesystem failure before the receipt replacement must leave the live
    // entry intact. A directory cannot be atomically replaced by a receipt file.
    fs::create_dir(&checkpoint_path).unwrap();
    assert_eq!(
        store.accepted(&path, &pending, receipt.clone()),
        Err(Error::State)
    );
    fs::remove_dir(&checkpoint_path).unwrap();
    store.cleanup_retired().unwrap();
    assert_eq!(fs::read(path.join("pending.json")).unwrap(), metadata);
    assert_eq!(fs::read(path.join("payload")).unwrap(), bytes);

    // A malformed live entry, even beside an unrelated durable receipt, is
    // never mistaken for cleanup-only work or silently discarded.
    let mut unrelated = receipt.clone();
    unrelated.operation.idempotency_key = "unrelated-acceptance".into();
    private_file::write(&checkpoint_path, &Some(publication(&unrelated))).unwrap();
    fs::remove_file(path.join("pending.json")).unwrap();
    store.cleanup_retired().unwrap();
    assert_eq!(collector.tick(), TickOutcome::Failed(Error::State));
    assert_eq!(fs::read(path.join("payload")).unwrap(), bytes);
    assert!(server.requests().is_empty());

    pending.save(&path).unwrap();
    store.accepted(&path, &pending, receipt).unwrap();
    assert!(!path.exists());
    assert_eq!(store.status().unwrap().pending, 0);
    assert_eq!(store.status().unwrap().stored_sessions, 1);
}

#[test]
fn repeated_staging_expiry_backs_off_and_allows_neighbors_and_new_capture() {
    for phase in ["offset", "chunk", "publish"] {
        let temp = tempdir().unwrap();
        let data = temp.path().join("data");
        let records = [
            synthetic_record("one", "one"),
            synthetic_record("two", "two"),
        ];
        commit_records(&data, &records, 1);
        let (server, remote) = accepting_remote();
        let store = SharingStore::new(data.join("sharing/team"));
        connect(&store, server.endpoint());
        store
            .set_policy(policy(capture::hex(&records[0].source.identity().digest())))
            .unwrap();
        let collector = Collector::new(data.clone(), store.root().to_owned());
        assert_eq!(collector.tick(), TickOutcome::Progress);
        let a_path = store.pending_paths().unwrap().remove(0); // The first sorted entry.
        let a = read_pending(&a_path);
        let bytes = fs::read(a_path.join("payload")).unwrap();
        remote.lock().unwrap().expiry = Some((a.member.sha256.clone(), phase));
        for failure in 1..=2 {
            if failure == 2 {
                expire_backoff(&store); // Only A remains after B completed.
            }
            assert_eq!(collector.tick(), TickOutcome::Progress); // Begin/recreate staging.
            if phase == "offset" {
                let mut pending = read_pending(&a_path);
                pending.reconcile_offset = true;
                pending.save(&a_path).unwrap();
            } else if phase == "publish" {
                assert_eq!(collector.tick(), TickOutcome::Progress);
            }
            assert_eq!(collector.tick(), TickOutcome::Failed(Error::StagingExpired));
            let pending = read_pending(&a_path);
            assert!(pending.upload.is_none());
            assert_eq!(pending.failures, failure);
            assert!(pending.retry_at > 0);
            assert_eq!(pending.operation, a.operation);
            assert_eq!(fs::read(a_path.join("payload")).unwrap(), bytes);
            if failure == 1 {
                for _ in 0..3 {
                    assert_eq!(collector.tick(), TickOutcome::Progress); // B progresses in A's backoff.
                }
                assert_eq!(store.status().unwrap().stored_sessions, 1);
            }
        }
        commit_records(
            &data,
            &[
                records[0].clone(),
                records[1].clone(),
                synthetic_record("three", "three"),
            ],
            2,
        );
        assert_eq!(collector.tick(), TickOutcome::Progress); // C captured in repeated backoff.
        for _ in 0..3 {
            assert_eq!(collector.tick(), TickOutcome::Progress);
        }
        assert_eq!(store.status().unwrap().stored_sessions, 2);
        remote.lock().unwrap().expiry = None;
        expire_backoff(&store);
        for _ in 0..3 {
            assert_eq!(collector.tick(), TickOutcome::Progress);
        }
        let receipt = store
            .publication_checkpoint(&a.operation.publication)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.revision, a.operation.revision);
        assert_eq!(
            remote.lock().unwrap().receipts[&a.operation.idempotency_key].operation,
            a.operation
        );
        assert_eq!(store.status().unwrap().pending, 0);
        assert_eq!(store.status().unwrap().stored_sessions, 3);
    }
}
