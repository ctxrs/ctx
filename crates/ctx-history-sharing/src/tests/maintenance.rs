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
