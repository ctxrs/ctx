use super::{
    cancellation::{cancel_request, unsubmitted},
    recovery::restore,
    *,
};

#[test]
fn private_restore_rejects_old_settlements_and_rebuilds_only_accepted_sequences() {
    let inputs = tempfile::tempdir().unwrap();
    let input = fixture(inputs.path(), "cancel-recovery", &["recoverable kiwi"]);
    for accepted in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (server, admin) = bootstrap(root.path());
        let secret = &admin.credential.secret;
        let collection = &admin.collection;
        let before = root.path().join("before");
        server.checkpoint(&before).unwrap();
        let request = if accepted {
            let request = stage(&server, secret, collection, &input, "pub", None, "settled");
            server.publish(secret, collection, request.clone()).unwrap();
            request
        } else {
            unsubmitted(&input, "settled")
        };
        let sequence = server.status(secret, collection).unwrap().stored_sequence;
        let pending = server.local_health().unwrap().pending_operations;
        let cancel = cancel_request(&admin.principal, &request);
        let settled = server
            .cancel_publish(secret, collection, cancel.clone())
            .unwrap();
        assert_eq!(
            matches!(settled.outcome, CancelPublishOutcome::Accepted { .. }),
            accepted
        );
        assert_eq!(
            server.status(secret, collection).unwrap().stored_sequence,
            sequence
        );
        assert_eq!(server.local_health().unwrap().pending_operations, pending);
        assert_eq!(
            serde_json::to_value(
                server
                    .cancel_publish(secret, collection, cancel.clone())
                    .unwrap()
            )
            .unwrap(),
            serde_json::to_value(&settled).unwrap()
        );
        let current = root.path().join("current");
        server.checkpoint(&current).unwrap();
        drop(server);
        // Even a checkpoint preceding the cancellation is safe because the old
        // publisher can neither resubmit its upload nor settle its operation.
        for checkpoint in [&before, &current] {
            let recovery_root = tempfile::tempdir_in(root.path()).unwrap();
            let (server, info, owner) = restore(checkpoint, recovery_root.path());
            assert!(matches!(
                server.cancel_publish(secret, collection, cancel.clone()),
                Err(Error::Forbidden)
            ));
            assert!(matches!(
                server.publish(secret, collection, request.clone()),
                Err(Error::Forbidden)
            ));
            assert!(matches!(
                server.cancel_publish(&owner.secret, collection, cancel.clone()),
                Err(Error::Forbidden)
            ));
            let saved_sequence = if checkpoint == &current { sequence } else { 0 };
            assert_eq!(
                server.local_health().unwrap().pending_operations,
                saved_sequence
            );
            let mut never = unsubmitted(&input, "cancelled-neighbor");
            never.operation.publication = "never".into();
            server
                .cancel_publish(
                    &owner.secret,
                    collection,
                    cancel_request(&info.principal, &never),
                )
                .unwrap();
            let mut fresh = stage(
                &server,
                &owner.secret,
                collection,
                &input,
                "fresh",
                None,
                "fresh",
            );
            fresh.identity.origin = "fresh-origin".into();
            assert_eq!(
                server
                    .publish(&owner.secret, collection, fresh)
                    .unwrap()
                    .sequence,
                saved_sequence + 1
            );
            server.rebuild_collection(collection).unwrap();
            let status = server.index_pending(collection, 16).unwrap();
            assert_eq!(status.searchable_sequence, saved_sequence + 1);
            assert_eq!(server.local_health().unwrap().pending_operations, 0);
            assert_eq!(
                find(&server, &owner.secret, collection, "kiwi").len() as u64,
                saved_sequence + 1
            );
            let counts: (u64, u64, u64) = server
                .lock()
                .unwrap()
                .query_row(
                    "SELECT count(*),count(sequence),count(receipt) FROM operations",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(
                counts,
                (
                    if checkpoint == &current { 3 } else { 2 },
                    saved_sequence + 1,
                    saved_sequence + 1
                )
            );
        }
    }
}
