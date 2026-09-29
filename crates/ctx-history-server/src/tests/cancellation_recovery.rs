use super::{
    cancellation::{cancel_request, unsubmitted},
    recovery::with_floor,
    *,
};

#[test]
fn both_terminal_settlements_fence_recovery_and_rebuild_only_accepted_sequences() {
    let inputs = tempfile::tempdir().unwrap();
    let input = fixture(inputs.path(), "cancel-recovery", &["recoverable kiwi"]);
    for accepted in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (server, admin, authority) = with_floor(root.path());
        let secret = &admin.credential.secret;
        let collection = &admin.collection;
        let before = root.path().join("before");
        server.checkpoint(&before).unwrap();
        let old_floor = fs::read(&authority).unwrap();
        let request = if accepted {
            let request = stage(&server, secret, collection, &input, "pub", None, "settled");
            server.publish(secret, collection, request.clone()).unwrap();
            request
        } else {
            unsubmitted(&input, "settled")
        };
        assert_eq!(
            fs::read(&authority).unwrap(),
            old_floor,
            "ordinary acceptance is data only"
        );
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
        let fenced_floor = fs::read(&authority).unwrap();
        assert_ne!(
            fenced_floor, old_floor,
            "both first settlements fence restore"
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
        assert_eq!(
            fs::read(&authority).unwrap(),
            fenced_floor,
            "lost-response retry does not advance floor"
        );
        assert!(matches!(
            HistoryServer::restore_checkpoint_with_authority(
                &before,
                &root.path().join("stale"),
                &authority
            ),
            Err(Error::RecoveryClosed)
        ));
        let current = root.path().join("current");
        server.checkpoint(&current).unwrap();
        drop(server);
        let restored = root.path().join("restored");
        HistoryServer::restore_checkpoint_with_authority(&current, &restored, &authority).unwrap();
        let server = HistoryServer::open(ServerConfig::new(&restored)).unwrap();
        assert_eq!(
            serde_json::to_value(server.cancel_publish(secret, collection, cancel).unwrap())
                .unwrap(),
            serde_json::to_value(settled).unwrap()
        );
        assert_eq!(fs::read(&authority).unwrap(), fenced_floor);
        if accepted {
            assert_eq!(
                server
                    .publish(secret, collection, request)
                    .unwrap()
                    .sequence,
                1
            );
        } else {
            assert!(matches!(
                server.publish(secret, collection, request),
                Err(Error::OperationCancelled)
            ));
        }
        // Cancelled outcomes surrounding real receipts neither allocate an
        // index sequence nor manufacture holes in receipt-derived rebuilds.
        let mut never = unsubmitted(&input, "cancelled-neighbor");
        never.operation.publication = "never".into();
        server
            .cancel_publish(secret, collection, cancel_request(&admin.principal, &never))
            .unwrap();
        let mut fresh = stage(&server, secret, collection, &input, "fresh", None, "fresh");
        fresh.identity.origin = "fresh-origin".into();
        let receipt = server.publish(secret, collection, fresh).unwrap();
        assert_eq!(receipt.sequence, sequence + 1);
        server.rebuild_collection(collection).unwrap();
        let status = server.index_pending(collection, 16).unwrap();
        assert_eq!(status.searchable_sequence, sequence + 1);
        assert_eq!(server.local_health().unwrap().pending_operations, 0);
        assert_eq!(
            find(&server, secret, collection, "kiwi").len() as u64,
            sequence + 1
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
        assert_eq!(counts, (3, sequence + 1, sequence + 1));
    }
}

#[test]
fn settlement_floor_failure_returns_no_success_and_restart_finishes_both_fences() {
    let inputs = tempfile::tempdir().unwrap();
    let input = fixture(inputs.path(), "cancel-floor", &["retained plum"]);
    for accepted in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (server, admin, authority) = with_floor(root.path());
        let request = if accepted {
            let request = stage(
                &server,
                &admin.credential.secret,
                &admin.collection,
                &input,
                "pub",
                None,
                "settle",
            );
            server
                .publish(&admin.credential.secret, &admin.collection, request.clone())
                .unwrap();
            request
        } else {
            unsubmitted(&input, "settle")
        };
        let cancel = cancel_request(&admin.principal, &request);
        let old_floor = fs::read(&authority).unwrap();
        fs::remove_file(&authority).unwrap();
        fs::create_dir(&authority).unwrap();
        assert!(matches!(
            server.cancel_publish(&admin.credential.secret, &admin.collection, cancel.clone()),
            Err(Error::Io(_))
        ));
        assert!(matches!(server.local_health(), Err(Error::Unavailable)));
        assert!(matches!(
            server.cancel_publish(&admin.credential.secret, &admin.collection, cancel.clone()),
            Err(Error::Unavailable)
        ));
        // The surviving live catalog has committed the fence. Startup may
        // finish its interrupted floor write, never lower a newer floor.
        fs::remove_dir(&authority).unwrap();
        let mut file =
            ctx_history_platform::platform_security::create_private_file_new(&authority).unwrap();
        std::io::Write::write_all(&mut file, &old_floor).unwrap();
        file.sync_all().unwrap();
        drop(file);
        drop(server);
        let server = HistoryServer::open(ServerConfig::new(root.path().join("server"))).unwrap();
        let current_floor = fs::read(&authority).unwrap();
        assert_ne!(current_floor, old_floor);
        let result = server
            .cancel_publish(&admin.credential.secret, &admin.collection, cancel)
            .unwrap();
        assert_eq!(
            matches!(result.outcome, CancelPublishOutcome::Accepted { .. }),
            accepted
        );
        assert_eq!(fs::read(&authority).unwrap(), current_floor);
        if accepted {
            assert_eq!(
                server
                    .publish(&admin.credential.secret, &admin.collection, request)
                    .unwrap()
                    .sequence,
                1
            );
            assert!(server
                .collection_root(&admin.collection)
                .join("payloads")
                .join(&input.member.sha256)
                .is_file());
        } else {
            assert!(matches!(
                server.publish(&admin.credential.secret, &admin.collection, request),
                Err(Error::OperationCancelled)
            ));
        }
    }
}
