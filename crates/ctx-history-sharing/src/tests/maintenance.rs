use super::*;
use crate::queue::Pending;

fn abandoned_scratch(store: &SharingStore) -> Vec<std::path::PathBuf> {
    let paths = vec![
        store.root().join(".capture-crashed"),
        store.root().join("queue/.pending-crashed"),
    ];
    for path in &paths {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("scratch"), b"uncommitted synthetic bytes").unwrap();
    }
    paths
}

#[test]
fn uploader_reclaims_only_abandoned_scratch_and_removal_preserves_explicit_previews() {
    let temp = tempdir().unwrap();
    let (server, _) = recovery::accepting_remote();
    let (store, collector, _) = make_ready(temp.path(), server.endpoint());
    let live = store.pending_paths().unwrap().pop().unwrap();
    let a: Pending = private_file::read(&live.join("pending.json")).unwrap();
    let payload = fs::read(live.join("payload")).unwrap();
    let preview = store.root().join("preview-retained");
    fs::create_dir(&preview).unwrap();
    fs::write(preview.join("member"), b"explicitly saved preview").unwrap();
    let receipt = Receipt {
        collection: COLLECTION.into(),
        publisher: "synthetic-publisher".into(),
        operation: ctx_history_server::Operation {
            publication: "0".repeat(64),
            idempotency_key: "another-accepted-operation".into(),
            ..a.operation.clone()
        },
        sequence: 9,
        kind: "publish".into(),
        accepted_at: 1,
        payload: Some(UploadSpec {
            sha256: a.member.sha256.clone(),
            bytes: a.member.bytes,
        }),
    };
    let checkpoint_path = store.checkpoint_path(&receipt.operation.publication);
    private_file::write(&checkpoint_path, &Some(recovery::publication(&receipt))).unwrap();
    let receipt_bytes = fs::read(&checkpoint_path).unwrap();
    let scratch = abandoned_scratch(&store);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&preview, store.root().join(".capture-symlink")).unwrap();

    let owner = store.lock("uploader.lock", false).unwrap();
    assert_eq!(collector.tick(), TickOutcome::Failed(Error::Busy));
    assert!(scratch.iter().all(|p| p.exists()));
    assert_eq!(server.requests().len(), 1); // Initial policy authentication only.
    drop(owner);
    assert_eq!(collector.tick(), TickOutcome::Progress);
    assert!(scratch.iter().all(|p| !p.exists()));
    let current: Pending = private_file::read(&live.join("pending.json")).unwrap();
    assert_eq!(current.operation, a.operation);
    assert_eq!(fs::read(live.join("payload")).unwrap(), payload);
    assert_eq!(fs::read(&checkpoint_path).unwrap(), receipt_bytes);
    assert_eq!(
        fs::read(preview.join("member")).unwrap(),
        b"explicitly saved preview"
    );
    for _ in 0..2 {
        assert_eq!(collector.tick(), TickOutcome::Progress);
    }
    assert_eq!(store.status().unwrap().pending, 0);
    assert_eq!(fs::read(&checkpoint_path).unwrap(), receipt_bytes);

    let scratch = abandoned_scratch(&store);
    store.remove().unwrap();
    assert!(scratch.iter().all(|p| !p.exists()));
    assert_eq!(
        fs::read(preview.join("member")).unwrap(),
        b"explicitly saved preview"
    );
    assert!(!store.status().unwrap().connected);
}

#[test]
fn erasure_discards_queued_bytes_and_blocks_recapture_without_touching_peers() {
    for (uploaded, submitted) in [(false, false), (true, false), (false, true)] {
        let temp = tempdir().unwrap();
        let server = publishing_mock(|_| panic!("erasure must not contact the server"));
        let (store, collector, _) = make_ready(temp.path(), server.endpoint());
        let path = store.pending_paths().unwrap().pop().unwrap();
        let mut pending: Pending = private_file::read(&path.join("pending.json")).unwrap();
        pending.upload = uploaded.then(|| UploadStatus {
            publisher: "synthetic-publisher".into(),
            id: "synthetic-upload".into(),
            received_bytes: pending.member.bytes,
            expected_bytes: pending.member.bytes,
            expires_at: u64::MAX,
        });
        pending.publish_attempted = submitted;
        pending.retry_at = u64::MAX;
        pending.save(&path).unwrap();
        let session = pending.member.session_id.as_uuid();
        let original = synthetic_record("session-one", ORIGINAL_TEXT);
        let peer = synthetic_record("session-two", "keep this peer");
        commit_records(&temp.path().join("data"), &[original, peer.clone()], 2);
        assert_eq!(collector.tick(), TickOutcome::Progress);
        let peer_path = store
            .pending_paths()
            .unwrap()
            .into_iter()
            .find(|p| *p != path)
            .unwrap();
        let peer_payload = fs::read(peer_path.join("payload")).unwrap();
        let mut queued_peer: Pending = private_file::read(&peer_path.join("pending.json")).unwrap();
        assert_eq!(queued_peer.member.session_id, peer.session_id);
        queued_peer.retry_at = u64::MAX;
        queued_peer.save(&peer_path).unwrap();
        let scratch = abandoned_scratch(&store);
        let receipt = store.erase_session(session).unwrap();
        assert_eq!(receipt.discarded_uploads, 1);
        assert_eq!(
            receipt.discarded_uploads_may_have_reached_server,
            uploaded || submitted
        );
        assert!(!path.exists());
        assert!(scratch.iter().all(|p| !p.exists()));
        assert_eq!(store.pending_paths().unwrap(), vec![peer_path.clone()]);
        assert_eq!(fs::read(peer_path.join("payload")).unwrap(), peer_payload);
        // A crash before Core erasure leaves the original record available to
        // capture. The durable sharing exclusion must still prevent upload.
        assert_eq!(collector.tick(), TickOutcome::Idle);
        assert_eq!(store.pending_paths().unwrap(), vec![peer_path.clone()]);
        assert_eq!(fs::read(peer_path.join("payload")).unwrap(), peer_payload);
        let retry = store.erase_session(session).unwrap();
        assert_eq!(retry.discarded_uploads, 0);
        assert_eq!(
            retry.discarded_uploads_may_have_reached_server,
            uploaded || submitted
        );
        assert_eq!(server.requests().len(), 1); // Policy authentication only.
    }
}
