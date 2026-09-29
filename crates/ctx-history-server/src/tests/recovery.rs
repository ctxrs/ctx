use super::*;

pub(super) fn with_floor(root: &Path) -> (HistoryServer, TokenFile, std::path::PathBuf) {
    let authority = root.join("security-floor.json");
    let mut config = ServerConfig::new(root.join("server"));
    config.authority_file = Some(authority.clone());
    let server = HistoryServer::open(config).unwrap();
    let device = root.join("device.json");
    server.bootstrap("team", &device).unwrap();
    let token = serde_json::from_slice(&fs::read(device).unwrap()).unwrap();
    (server, token, authority)
}

#[test]
fn checkpoint_survives_additive_acceptance_and_lost_receipts_can_be_retried() {
    let root = tempfile::tempdir().unwrap();
    let a = fixture(&root.path().join("a"), "a", &["saved apricot"]);
    let b = fixture(&root.path().join("b"), "b", &["later banana"]);
    let (server, token, authority) = with_floor(root.path());
    let secret = &token.credential.secret;
    let collection = &token.collection;
    let a_request = stage(&server, secret, collection, &a, "a", None, "saved-a");
    let a_receipt = server
        .publish(secret, collection, a_request.clone())
        .unwrap();
    let checkpoint = root.path().join("checkpoint");
    server.checkpoint(&checkpoint).unwrap();
    let floor = fs::read(&authority).unwrap();
    let b_request = stage(&server, secret, collection, &b, "b", None, "lost-b");
    let lost = server
        .publish(secret, collection, b_request.clone())
        .unwrap();
    assert_eq!(lost.sequence, 2);
    assert_eq!(fs::read(&authority).unwrap(), floor);
    server.index_pending(collection, 16).unwrap();
    let b_citation = find(&server, secret, collection, "banana")
        .remove(0)
        .citation;
    drop(server);

    let restored_root = root.path().join("restored");
    HistoryServer::restore_checkpoint_with_authority(&checkpoint, &restored_root, &authority)
        .unwrap();
    let server = HistoryServer::open(ServerConfig::new(restored_root)).unwrap();
    assert_eq!(fs::read(&authority).unwrap(), floor);
    assert_eq!(
        server.status(secret, collection).unwrap().stored_sequence,
        1
    );
    assert_eq!(
        server
            .publication_state(secret, collection, "a")
            .unwrap()
            .sequence,
        1
    );
    assert!(matches!(
        server.receipt(secret, collection, "lost-b"),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        server.publication_state(secret, collection, "b"),
        Err(Error::NotFound)
    ));
    assert_eq!(
        serde_json::to_value(server.publish(secret, collection, a_request).unwrap()).unwrap(),
        serde_json::to_value(a_receipt).unwrap(),
    );
    server.index_pending(collection, 16).unwrap();
    assert!(matches!(
        server.read_event(secret, collection, &b_citation),
        Err(Error::NotFound)
    ));
    assert_eq!(find(&server, secret, collection, "apricot").len(), 1);
    assert!(find(&server, secret, collection, "banana").is_empty());

    // Upload IDs are staging handles, not durable operation identity. The
    // checkpoint lost B's bytes and receipt, so the client restages the same op.
    let retry = stage(&server, secret, collection, &b, "b", None, "lost-b");
    assert_eq!(retry.operation, lost.operation);
    let accepted = server.publish(secret, collection, retry.clone()).unwrap();
    assert_eq!(accepted.operation, lost.operation);
    assert_eq!(accepted.sequence, 2);
    assert_eq!(
        serde_json::to_value(server.publish(secret, collection, retry).unwrap()).unwrap(),
        serde_json::to_value(accepted).unwrap(),
    );
    server.index_pending(collection, 16).unwrap();
    assert_eq!(find(&server, secret, collection, "banana").len(), 1);
    assert_eq!(
        server
            .read_event(secret, collection, &b_citation)
            .unwrap()
            .record
            .content
            .meaningful_text(),
        "later banana",
    );
    assert_eq!(fs::read(&authority).unwrap(), floor);
}

#[test]
fn content_correction_is_backup_data_but_policy_advance_is_security_authority() {
    let root = tempfile::tempdir().unwrap();
    let a = fixture(&root.path().join("a"), "same", &["original plum"]);
    let b = fixture(&root.path().join("b"), "same", &["corrected peach"]);
    let (server, token, authority) = with_floor(root.path());
    let secret = &token.credential.secret;
    let collection = &token.collection;
    let initial = stage(&server, secret, collection, &a, "pub", None, "a");
    server.publish(secret, collection, initial).unwrap();
    server.index_pending(collection, 16).unwrap();
    let old = find(&server, secret, collection, "plum").remove(0);
    let checkpoint = root.path().join("checkpoint");
    server.checkpoint(&checkpoint).unwrap();
    let floor = fs::read(&authority).unwrap();
    let correction = stage(
        &server,
        secret,
        collection,
        &b,
        "pub",
        Some(a.member.sha256.clone()),
        "correction",
    );
    server
        .publish(secret, collection, correction.clone())
        .unwrap();
    assert_eq!(fs::read(&authority).unwrap(), floor);
    assert!(!server.status(secret, collection).unwrap().reads_available);
    server.index_pending(collection, 16).unwrap();
    assert!(find(&server, secret, collection, "plum").is_empty());
    assert_eq!(find(&server, secret, collection, "peach").len(), 1);
    // Corrections are not deletion: old exact citations stay authorized even
    // in the live server. Withdrawal is the boundary that hides these bytes.
    assert_eq!(
        server
            .read_event(secret, collection, &old.citation)
            .unwrap()
            .record
            .content
            .meaningful_text(),
        "original plum"
    );
    drop(server);

    let restored_root = root.path().join("restored");
    HistoryServer::restore_checkpoint_with_authority(&checkpoint, &restored_root, &authority)
        .unwrap();
    let server = HistoryServer::open(ServerConfig::new(restored_root)).unwrap();
    assert!(matches!(
        server.receipt(secret, collection, "correction"),
        Err(Error::NotFound)
    ));
    assert_eq!(
        server
            .publication_state(secret, collection, "pub")
            .unwrap()
            .revision,
        a.member.sha256
    );
    server.index_pending(collection, 16).unwrap();
    assert_eq!(find(&server, secret, collection, "plum").len(), 1);
    let retry = stage(
        &server,
        secret,
        collection,
        &b,
        "pub",
        Some(a.member.sha256.clone()),
        "correction",
    );
    assert_eq!(retry.operation, correction.operation);
    server.publish(secret, collection, retry).unwrap();
    assert_eq!(fs::read(&authority).unwrap(), floor);

    // Reuse immutable A at policy 2. The bytes already exist, but accepting
    // older policy-1 operations again after restore would revive retired consent.
    let mut policy = stage(
        &server,
        secret,
        collection,
        &a,
        "pub",
        Some(b.member.sha256.clone()),
        "policy",
    );
    policy.operation.policy_revision = 2;
    server.publish(secret, collection, policy).unwrap();
    assert_ne!(fs::read(&authority).unwrap(), floor);
    assert!(matches!(
        HistoryServer::restore_checkpoint_with_authority(
            &checkpoint,
            &root.path().join("stale"),
            &authority,
        ),
        Err(Error::RecoveryClosed)
    ));
    let current = root.path().join("current");
    server.checkpoint(&current).unwrap();
    drop(server);
    let current_root = root.path().join("current-restored");
    HistoryServer::restore_checkpoint_with_authority(&current, &current_root, &authority).unwrap();
    let server = HistoryServer::open(ServerConfig::new(current_root)).unwrap();
    let mut stale = stage(
        &server,
        secret,
        collection,
        &b,
        "pub",
        Some(a.member.sha256),
        "after-policy",
    );
    assert!(matches!(
        server.publish(secret, collection, stale.clone()),
        Err(Error::Conflict)
    ));
    stale.operation.policy_revision = 2;
    server.publish(secret, collection, stale).unwrap();
}

#[test]
fn security_mutations_reject_older_checkpoints_and_current_checkpoints_recover() {
    let inputs = tempfile::tempdir().unwrap();
    let input = fixture(inputs.path(), "security", &["protected quince"]);
    for action in ["credential", "principal", "grants", "withdraw", "retire"] {
        let root = tempfile::tempdir().unwrap();
        let (server, token, authority) = with_floor(root.path());
        let collection = &token.collection;
        let writer = server
            .issue_credential(
                &token.principal,
                Grants {
                    read: true,
                    publish: true,
                    manage: false,
                },
                3600,
            )
            .unwrap();
        let request = stage(
            &server,
            &writer.secret,
            collection,
            &input,
            "pub",
            None,
            "accepted",
        );
        server.publish(&writer.secret, collection, request).unwrap();
        let old = root.path().join("old");
        server.checkpoint(&old).unwrap();
        let floor = fs::read(&authority).unwrap();
        let removal = WithdrawRequest {
            operation: Operation {
                idempotency_key: "remove".into(),
                publication: "pub".into(),
                writer_epoch: 1,
                policy_revision: 1,
                expected_revision: Some(input.member.sha256.clone()),
                expected_sequence: Some(1),
                revision: "withdrawn".into(),
            },
        };
        match action {
            "credential" => server.revoke_credential(&writer.id).unwrap(),
            "principal" => server.revoke_principal(&token.principal).unwrap(),
            "grants" => server
                .manage_grants(
                    &token.credential.secret,
                    &token.principal,
                    collection,
                    Grants {
                        read: true,
                        publish: false,
                        manage: true,
                    },
                )
                .unwrap(),
            "withdraw" => {
                server
                    .withdraw(&writer.secret, collection, removal)
                    .unwrap();
            }
            "retire" => {
                server
                    .remove_publication(&token.credential.secret, collection, removal)
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert_ne!(fs::read(&authority).unwrap(), floor, "{action}");
        assert!(
            matches!(
                HistoryServer::restore_checkpoint_with_authority(
                    &old,
                    &root.path().join("stale"),
                    &authority,
                ),
                Err(Error::RecoveryClosed)
            ),
            "{action}"
        );
        let current = root.path().join("current");
        server.checkpoint(&current).unwrap();
        let latest = fs::read(&authority).unwrap();
        drop(server);
        let restored_root = root.path().join("restored");
        HistoryServer::restore_checkpoint_with_authority(&current, &restored_root, &authority)
            .unwrap();
        let server = HistoryServer::open(ServerConfig::new(restored_root)).unwrap();
        assert!(!server.local_health().unwrap().recovery_closed, "{action}");
        assert_eq!(fs::read(&authority).unwrap(), latest);
        if matches!(action, "credential" | "principal" | "grants") {
            assert!(
                matches!(
                    server.begin_upload(
                        &writer.secret,
                        collection,
                        UploadSpec {
                            sha256: input.member.sha256.clone(),
                            bytes: input.member.bytes,
                        }
                    ),
                    Err(Error::Forbidden)
                ),
                "{action}"
            );
        } else {
            assert!(
                server
                    .publication_state(&writer.secret, collection, "pub")
                    .unwrap()
                    .withdrawn
            );
            let mut revive = stage(
                &server,
                &writer.secret,
                collection,
                &input,
                "pub",
                Some("withdrawn".into()),
                "revive",
            );
            assert!(matches!(
                server.publish(&writer.secret, collection, revive.clone()),
                Err(Error::Conflict)
            ));
            revive.operation.writer_epoch = 2;
            assert!(matches!(
                server.publish(&writer.secret, collection, revive),
                Err(Error::Conflict)
            ));
        }
    }
}
