use super::{
    repair::{Pause, Stage},
    *,
};

fn publisher_grants() -> Grants {
    Grants {
        read: true,
        publish: true,
        manage: false,
    }
}

#[test]
fn distinct_devices_cannot_overwrite_a_reversion_with_a_paused_stale_publish() {
    let root = tempfile::tempdir().unwrap();
    let a = fixture(&root.path().join("a"), "cycle", &["original almond"]);
    let b = fixture(&root.path().join("b"), "cycle", &["changed walnut"]);
    let (server, admin) = bootstrap(root.path());
    let d1 = server
        .issue_credential(
            &admin.principal,
            &admin.collection,
            publisher_grants(),
            3600,
        )
        .unwrap();
    let d2 = server
        .issue_credential(
            &admin.principal,
            &admin.collection,
            publisher_grants(),
            3600,
        )
        .unwrap();
    assert_ne!(d1.id, d2.id);
    let server = Arc::new(server);
    let collection = &admin.collection;
    let first = stage(&server, &d2.secret, collection, &a, "pub", None, "first");
    let first_receipt = server
        .publish(&d2.secret, collection, first.clone())
        .unwrap();
    assert_eq!(first_receipt.sequence, 1);
    server.index_pending(collection, 16).unwrap();
    let old_a = find(&server, &d2.secret, collection, "almond").remove(0);
    let slow = stage(
        &server,
        &d1.secret,
        collection,
        &b,
        "pub",
        Some(a.member.sha256.clone()),
        "slow",
    );
    assert_eq!(slow.operation.expected_sequence, Some(1));
    let pause = Pause::at(&server, Stage::Validation);
    let publishing = {
        let server = server.clone();
        let collection = collection.clone();
        let token = d1.secret.clone();
        let slow = slow.clone();
        std::thread::spawn(move || server.publish(&token, &collection, slow))
    };
    pause.wait();
    let second = stage(
        &server,
        &d2.secret,
        collection,
        &b,
        "pub",
        Some(a.member.sha256.clone()),
        "second",
    );
    let second_receipt = server
        .publish(&d2.secret, collection, second.clone())
        .unwrap();
    assert_eq!(second_receipt.sequence, 2);
    server.index_pending(collection, 16).unwrap();
    let old_b = find(&server, &d2.secret, collection, "walnut").remove(0);
    let third = stage(
        &server,
        &d2.secret,
        collection,
        &a,
        "pub",
        Some(b.member.sha256.clone()),
        "third",
    );
    let third_receipt = server
        .publish(&d2.secret, collection, third.clone())
        .unwrap();
    assert_eq!(third_receipt.sequence, 3);
    drop(pause);
    assert!(matches!(publishing.join().unwrap(), Err(Error::Conflict)));
    // The late check and the initial check must both distinguish A1 from A3.
    assert!(matches!(
        server.publish(&d1.secret, collection, slow),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        server.receipt(&d1.secret, collection, "slow"),
        Err(Error::NotFound)
    ));
    let current = server
        .publication_state(&d1.secret, collection, "pub")
        .unwrap();
    assert_eq!(current.revision, a.member.sha256);
    assert_eq!(current.sequence, 3);
    server.index_pending(collection, 16).unwrap();
    assert_eq!(
        find(&server, &d1.secret, collection, "almond")[0].citation,
        old_a.citation
    );

    let fresh = stage(
        &server,
        &d1.secret,
        collection,
        &b,
        "pub",
        Some(a.member.sha256.clone()),
        "fresh",
    );
    assert_eq!(fresh.operation.expected_sequence, Some(3));
    let mut neighbor = stage(
        &server, &d2.secret, collection, &a, "neighbor", None, "neighbor",
    );
    neighbor.identity.origin = "independent-origin".into();
    assert_eq!(
        server
            .publish(&d2.secret, collection, neighbor)
            .unwrap()
            .sequence,
        4
    );
    let accepted = server
        .publish(&d1.secret, collection, fresh.clone())
        .unwrap();
    assert_eq!(accepted.sequence, 5);
    assert_eq!(
        server
            .publication_state(&d1.secret, collection, "pub")
            .unwrap()
            .sequence,
        5
    );
    assert_eq!(
        server
            .publication_state(&d1.secret, collection, "neighbor")
            .unwrap()
            .sequence,
        4
    );

    for (request, receipt) in [
        (first, first_receipt),
        (second.clone(), second_receipt),
        (third, third_receipt),
    ] {
        assert_eq!(
            serde_json::to_value(server.publish(&d2.secret, collection, request).unwrap()).unwrap(),
            serde_json::to_value(receipt).unwrap(),
        );
    }
    let mut changed_retry = second;
    changed_retry.operation.expected_sequence = Some(3);
    assert!(matches!(
        server.publish(&d2.secret, collection, changed_retry),
        Err(Error::Conflict)
    ));
    assert_eq!(
        server
            .status(&d1.secret, collection)
            .unwrap()
            .stored_sequence,
        5
    );
    server.index_pending(collection, 16).unwrap();
    assert_eq!(
        find(&server, &d1.secret, collection, "walnut")[0].citation,
        old_b.citation
    );
    for (event, body) in [(old_a, "original almond"), (old_b, "changed walnut")] {
        assert_eq!(
            server
                .read_event(&d1.secret, collection, &event.citation)
                .unwrap()
                .record
                .content
                .meaningful_text(),
            body
        );
    }
}

#[test]
fn stale_withdraw_and_manager_remove_fail_but_current_publication_sequences_succeed() {
    let inputs = tempfile::tempdir().unwrap();
    let a = fixture(&inputs.path().join("a"), "removal", &["original pear"]);
    let b = fixture(&inputs.path().join("b"), "removal", &["corrected lime"]);
    for manager in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (server, admin) = bootstrap(root.path());
        let collection = &admin.collection;
        let owner = server.create_principal("publisher").unwrap();
        server
            .set_grants(&owner, collection, publisher_grants())
            .unwrap();
        let writer = server
            .issue_credential(&owner, collection, publisher_grants(), 3600)
            .unwrap();
        let remove = |request| {
            if manager {
                server.remove_publication(&admin.credential.secret, collection, request)
            } else {
                server.withdraw(&writer.secret, collection, request)
            }
        };
        let first = stage(
            &server,
            &writer.secret,
            collection,
            &a,
            "pub",
            None,
            "first",
        );
        server
            .publish(&writer.secret, collection, first.clone())
            .unwrap();
        let mut removal = WithdrawRequest {
            operation: Operation {
                idempotency_key: "remove".into(),
                publication: "pub".into(),
                revision: "withdrawn".into(),
                writer_epoch: 1,
                policy_revision: 1,
                expected_revision: Some(a.member.sha256.clone()),
                expected_sequence: Some(1),
            },
        };
        let second = stage(
            &server,
            &writer.secret,
            collection,
            &b,
            "pub",
            Some(a.member.sha256.clone()),
            "second",
        );
        server.publish(&writer.secret, collection, second).unwrap();
        let third = stage(
            &server,
            &writer.secret,
            collection,
            &a,
            "pub",
            Some(b.member.sha256.clone()),
            "third",
        );
        server.publish(&writer.secret, collection, third).unwrap();
        assert!(matches!(remove(removal.clone()), Err(Error::Conflict)));
        let mut missing = removal.clone();
        missing.operation.expected_sequence = None;
        assert!(matches!(remove(missing), Err(Error::Conflict)));
        removal.operation.expected_sequence = Some(3);
        let mut wrong_hash = removal.clone();
        wrong_hash.operation.expected_revision = Some(b.member.sha256.clone());
        assert!(matches!(remove(wrong_hash), Err(Error::Conflict)));
        let current = server
            .publication_state(&writer.secret, collection, "pub")
            .unwrap();
        assert_eq!(current.sequence, 3);
        assert!(!current.withdrawn);
        let mut neighbor = stage(
            &server,
            &writer.secret,
            collection,
            &a,
            "neighbor",
            None,
            "neighbor",
        );
        neighbor.identity.origin = "other-origin".into();
        server
            .publish(&writer.secret, collection, neighbor)
            .unwrap();
        let receipt = remove(removal.clone()).unwrap();
        assert_eq!(receipt.sequence, 5);
        let state = server
            .publication_state(&writer.secret, collection, "pub")
            .unwrap();
        assert_eq!(state.sequence, 5);
        assert!(state.withdrawn);
        assert_eq!(
            serde_json::to_value(remove(removal).unwrap()).unwrap(),
            serde_json::to_value(receipt).unwrap(),
        );
        assert_eq!(
            server
                .publish(&writer.secret, collection, first)
                .unwrap()
                .sequence,
            1
        );
        assert_eq!(
            server
                .status(&writer.secret, collection)
                .unwrap()
                .stored_sequence,
            5
        );
    }
}

#[test]
fn publish_requires_both_predecessors_absent_or_both_current() {
    let root = tempfile::tempdir().unwrap();
    let a = fixture(&root.path().join("a"), "pair", &["first cherry"]);
    let b = fixture(&root.path().join("b"), "pair", &["second melon"]);
    let (server, token) = bootstrap(root.path());
    let secret = &token.credential.secret;
    let collection = &token.collection;
    let first = stage(&server, secret, collection, &a, "pub", None, "first");
    let mut bad = first.clone();
    bad.operation.expected_sequence = Some(1);
    assert!(matches!(
        server.publish(secret, collection, bad),
        Err(Error::Conflict)
    ));
    let mut bad = first.clone();
    bad.operation.expected_revision = Some(b.member.sha256.clone());
    assert!(matches!(
        server.publish(secret, collection, bad.clone()),
        Err(Error::Conflict)
    ));
    bad.operation.expected_sequence = Some(1);
    assert!(matches!(
        server.publish(secret, collection, bad),
        Err(Error::Conflict)
    ));
    server.publish(secret, collection, first).unwrap();
    let correction = stage(
        &server,
        secret,
        collection,
        &b,
        "pub",
        Some(a.member.sha256),
        "correction",
    );
    let mut bad = correction.clone();
    bad.operation.expected_sequence = None;
    assert!(matches!(
        server.publish(secret, collection, bad),
        Err(Error::Conflict)
    ));
    let mut bad = correction.clone();
    bad.operation.expected_revision = None;
    assert!(matches!(
        server.publish(secret, collection, bad.clone()),
        Err(Error::Conflict)
    ));
    bad.operation.expected_sequence = None;
    assert!(matches!(
        server.publish(secret, collection, bad),
        Err(Error::Conflict)
    ));
    assert_eq!(
        server
            .publish(secret, collection, correction)
            .unwrap()
            .sequence,
        2
    );
}
