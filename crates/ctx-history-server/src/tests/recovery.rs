use super::*;

pub(super) fn restore(
    checkpoint: &Path,
    root: &Path,
) -> (HistoryServer, RestoreInfo, IssuedSecret) {
    let destination = root.join("restored");
    let token_file = root.join("recovery-owner.json");
    let info = HistoryServer::restore_checkpoint(checkpoint, &destination, &token_file).unwrap();
    let file =
        ctx_history_platform::platform_security::open_verified_private_file(&token_file).unwrap();
    let token: TokenFile = serde_json::from_reader(file).unwrap();
    assert_eq!(token.credential.expires_at, 0);
    assert_eq!(token.principal, info.principal);
    assert_eq!(
        token.collection,
        info.collections.first().cloned().unwrap_or_default()
    );
    (
        HistoryServer::open(ServerConfig::new(destination)).unwrap(),
        info,
        token.credential,
    )
}

fn all_grants() -> Grants {
    Grants {
        read: true,
        publish: true,
        manage: true,
    }
}

fn invitation(server: &HistoryServer, secret: &str, collection: &str) -> EnrollmentFile {
    server
        .invite(
            secret,
            collection,
            InviteRequest {
                principal: None,
                name: Some("new member".into()),
                grants: all_grants(),
                enrollment_ttl_seconds: 600,
                credential_ttl_seconds: 3600,
            },
        )
        .unwrap()
}

#[test]
fn older_checkpoint_restores_privately_and_never_reactivates_old_members() {
    let root = tempfile::tempdir().unwrap();
    let input = fixture(
        &root.path().join("source"),
        "recovery",
        &["private apricot", "exact pear"],
    );
    let (server, admin) = bootstrap(root.path());
    let collection = &admin.collection;
    let old_invite = invitation(&server, &admin.credential.secret, collection);
    let member = server.redeem(&old_invite.enrollment.secret).unwrap();
    let pending_invite = invitation(&server, &admin.credential.secret, collection);
    let request = stage(
        &server,
        &member.credential.secret,
        collection,
        &input,
        "pub",
        None,
        "saved",
    );
    server
        .publish(&member.credential.secret, collection, request.clone())
        .unwrap();
    server.index_pending(collection, 16).unwrap();
    let hit = find(&server, &member.credential.secret, collection, "apricot").remove(0);
    let before = server
        .read_event(&member.credential.secret, collection, &hit.citation)
        .unwrap();
    let session_before = server
        .read_session(
            &member.credential.secret,
            collection,
            &hit.session_citation,
            SessionRequest::default(),
        )
        .unwrap();
    let stale = stage(
        &server,
        &member.credential.secret,
        collection,
        &input,
        "pending",
        None,
        "pending",
    );
    let second = server.create_collection("second").unwrap();
    server
        .set_grants(&admin.principal, &second, all_grants())
        .unwrap();
    let checkpoint = root.path().join("checkpoint");
    server.checkpoint(&checkpoint).unwrap();
    // This user was active in the checkpoint and revoked only afterward.
    server.revoke_principal(&member.principal).unwrap();
    server
        .revoke_member(
            &admin.credential.secret,
            collection,
            &pending_invite.principal,
        )
        .unwrap();
    drop(server);

    let (server, info, owner) = restore(&checkpoint, root.path());
    let mut expected = vec![collection.clone(), second.clone()];
    expected.sort();
    assert_eq!(info.collections, expected);
    assert_ne!(info.principal, admin.principal);
    assert_ne!(info.principal, member.principal);
    assert_eq!(server.local_health().unwrap().staged_uploads, 0);
    assert_eq!(server.local_health().unwrap().pending_operations, 1);
    for old in [&admin.credential.secret, &member.credential.secret] {
        assert!(matches!(
            server.status(old, collection),
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            server.read_event(old, collection, &hit.citation),
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            server.begin_upload(
                old,
                collection,
                UploadSpec {
                    sha256: input.member.sha256.clone(),
                    bytes: input.member.bytes,
                }
            ),
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            server.upload_status(old, collection, &stale.upload),
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            server.upload_chunk(old, collection, &stale.upload, 0, b"x"),
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            server.publish(old, collection, request.clone()),
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            server.cancel_publish(
                old,
                collection,
                super::cancellation::cancel_request(&member.principal, &stale)
            ),
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            server.withdraw(old, collection, removal(&input, "pub", 1)),
            Err(Error::Forbidden)
        ));
    }
    assert!(matches!(
        server.redeem(&pending_invite.enrollment.secret),
        Err(Error::Unauthorized)
    ));
    assert!(matches!(
        server.redeem(&old_invite.enrollment.secret),
        Err(Error::Unauthorized)
    ));
    for old_principal in [
        &admin.principal,
        &member.principal,
        &pending_invite.principal,
    ] {
        assert!(matches!(
            server.issue_credential(old_principal, collection, all_grants(), 3600),
            Err(Error::Unauthorized)
        ));
        assert!(matches!(
            server.issue_enrollment(old_principal, collection, all_grants(), 600, 3600),
            Err(Error::Unauthorized)
        ));
        assert!(matches!(
            server.manage_grants(&owner.secret, old_principal, collection, all_grants()),
            Err(Error::Unauthorized)
        ));
    }
    // All original provenance/locators remain, and no old grant survives.
    let connection = server.lock().unwrap();
    let grants: Vec<String> = connection
        .prepare("SELECT DISTINCT principal FROM grants")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(grants, vec![info.principal.clone()]);
    let active: u64 = connection
        .query_row("SELECT count(*) FROM principals WHERE revoked=0", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(active, 1);
    drop(connection);
    assert_eq!(
        server
            .publication_state(&owner.secret, collection, "pub")
            .unwrap()
            .owner,
        member.principal
    );
    for restored_collection in &info.collections {
        server.status(&owner.secret, restored_collection).unwrap();
        server.index_pending(restored_collection, 16).unwrap();
    }
    let after = server
        .read_event(&owner.secret, collection, &hit.citation)
        .unwrap();
    assert_eq!(
        serde_json::to_value(after).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    let session_after = server
        .read_session(
            &owner.secret,
            collection,
            &hit.session_citation,
            SessionRequest::default(),
        )
        .unwrap();
    assert_eq!(
        serde_json::to_value(session_after).unwrap(),
        serde_json::to_value(session_before).unwrap()
    );
    assert_eq!(
        find(&server, &owner.secret, collection, "apricot")[0].citation,
        hit.citation
    );
    assert_eq!(
        fs::read(
            server
                .collection_root(collection)
                .join("payloads")
                .join(&input.member.sha256)
        )
        .unwrap(),
        input.bytes
    );
    assert_eq!(server.local_health().unwrap().pending_operations, 0);

    // Explicit fresh enrollment enables ordinary access and publishing again.
    let invite = invitation(&server, &owner.secret, collection);
    assert_ne!(invite.principal, member.principal);
    let joined = server.redeem(&invite.enrollment.secret).unwrap();
    assert_eq!(joined.collection, *collection);
    server
        .read_event(&joined.credential.secret, collection, &hit.citation)
        .unwrap();
    assert!(matches!(
        server.status(&joined.credential.secret, &second),
        Err(Error::Forbidden)
    ));
    server
        .manage_grants(&owner.secret, &joined.principal, &second, all_grants())
        .unwrap();
    assert!(matches!(
        server.status(&joined.credential.secret, &second),
        Err(Error::Forbidden)
    ));
    let device = server
        .invite(
            &owner.secret,
            &second,
            InviteRequest {
                principal: Some(joined.principal.clone()),
                name: None,
                grants: all_grants(),
                enrollment_ttl_seconds: 600,
                credential_ttl_seconds: 0,
            },
        )
        .unwrap();
    let second_device = server.redeem(&device.enrollment.secret).unwrap();
    assert_eq!(second_device.principal, joined.principal);
    server
        .status(&second_device.credential.secret, &second)
        .unwrap();
    let fresh = stage(
        &server,
        &second_device.credential.secret,
        &second,
        &input,
        "new",
        None,
        "new",
    );
    server
        .publish(&second_device.credential.secret, &second, fresh)
        .unwrap();
    server.index_pending(&second, 16).unwrap();
    assert_eq!(
        find(&server, &second_device.credential.secret, &second, "pear").len(),
        1
    );
    drop(server);
    let reopened = HistoryServer::open(ServerConfig::new(root.path().join("restored"))).unwrap();
    assert!(matches!(
        reopened.status(&member.credential.secret, collection),
        Err(Error::Forbidden)
    ));
    reopened.status(&owner.secret, &second).unwrap();
}

fn removal(input: &Fixture, publication: &str, sequence: u64) -> WithdrawRequest {
    WithdrawRequest {
        operation: Operation {
            idempotency_key: format!("remove-{publication}"),
            publication: publication.into(),
            writer_epoch: 1,
            policy_revision: 1,
            expected_revision: Some(input.member.sha256.clone()),
            expected_sequence: Some(sequence),
            revision: "withdrawn".into(),
        },
    }
}

#[test]
fn owner_reviews_later_withdrawals_before_explicitly_resharing() {
    let root = tempfile::tempdir().unwrap();
    let input = fixture(&root.path().join("input"), "withdrawn", &["review plum"]);
    let (server, admin) = bootstrap(root.path());
    let collection = &admin.collection;
    let secret = &admin.credential.secret;
    let request = stage(&server, secret, collection, &input, "pub", None, "saved");
    server.publish(secret, collection, request).unwrap();
    server.index_pending(collection, 16).unwrap();
    let hit = find(&server, secret, collection, "plum").remove(0);
    let checkpoint = root.path().join("checkpoint");
    server.checkpoint(&checkpoint).unwrap();
    server
        .withdraw(secret, collection, removal(&input, "pub", 1))
        .unwrap();
    server.index_pending(collection, 16).unwrap();
    assert!(matches!(
        server.read_event(secret, collection, &hit.citation),
        Err(Error::NotFound)
    ));
    let withdrawn_checkpoint = root.path().join("withdrawn-checkpoint");
    server.checkpoint(&withdrawn_checkpoint).unwrap();
    drop(server);

    let (server, _, owner) = restore(&checkpoint, root.path());
    server.index_pending(collection, 16).unwrap();
    // The post-checkpoint withdrawal is lost. Only the new owner can review it.
    assert!(matches!(
        server.read_event(secret, collection, &hit.citation),
        Err(Error::Forbidden)
    ));
    assert_eq!(
        server
            .read_event(&owner.secret, collection, &hit.citation)
            .unwrap()
            .record
            .content
            .meaningful_text(),
        "review plum"
    );
    assert!(
        !server
            .publication_state(&owner.secret, collection, "pub")
            .unwrap()
            .withdrawn
    );
    // Management removal already handles history owned by a departed principal.
    server
        .remove_publication(&owner.secret, collection, removal(&input, "pub", 1))
        .unwrap();
    server.index_pending(collection, 16).unwrap();
    let invited = invitation(&server, &owner.secret, collection);
    let joined = server.redeem(&invited.enrollment.secret).unwrap();
    assert!(find(&server, &joined.credential.secret, collection, "plum").is_empty());
    assert!(matches!(
        server.read_event(&joined.credential.secret, collection, &hit.citation),
        Err(Error::NotFound)
    ));

    // Withdrawals present in the checkpoint remain withdrawn after projection rebuild.
    let current_root = root.path().join("current");
    fs::create_dir(&current_root).unwrap();
    let (current, _, current_owner) = restore(&withdrawn_checkpoint, &current_root);
    current.index_pending(collection, 16).unwrap();
    assert!(
        current
            .publication_state(&current_owner.secret, collection, "pub")
            .unwrap()
            .withdrawn
    );
    assert!(matches!(
        current.read_event(&current_owner.secret, collection, &hit.citation),
        Err(Error::NotFound)
    ));
    assert!(find(&current, &current_owner.secret, collection, "plum").is_empty());
}

#[test]
fn checkpoint_validation_preserves_existing_roots_and_credentials() {
    let root = tempfile::tempdir().unwrap();
    let input = fixture(&root.path().join("input"), "integrity", &["saved peach"]);
    let (server, admin) = bootstrap(root.path());
    let request = stage(
        &server,
        &admin.credential.secret,
        &admin.collection,
        &input,
        "pub",
        None,
        "saved",
    );
    server
        .publish(&admin.credential.secret, &admin.collection, request)
        .unwrap();
    let checkpoint = root.path().join("checkpoint");
    server.checkpoint(&checkpoint).unwrap();
    let original = fs::read(checkpoint.join("authority.sqlite")).unwrap();
    let original_manifest = fs::read(checkpoint.join("checkpoint.json")).unwrap();
    let destination = root.path().join("restored");
    let token = root.path().join("owner.json");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("keep"), b"existing data").unwrap();
    assert!(matches!(
        HistoryServer::restore_checkpoint(&checkpoint, &destination, &token),
        Err(Error::Conflict)
    ));
    assert_eq!(
        fs::read(destination.join("keep")).unwrap(),
        b"existing data"
    );
    assert!(!token.exists());
    fs::remove_dir_all(&destination).unwrap();
    fs::write(&token, b"existing credential").unwrap();
    assert!(matches!(
        HistoryServer::restore_checkpoint(&checkpoint, &destination, &token),
        Err(Error::Conflict)
    ));
    assert_eq!(fs::read(&token).unwrap(), b"existing credential");
    assert!(!destination.exists());
    fs::remove_file(&token).unwrap();

    for corruption in [
        "version",
        "catalog",
        "payload",
        "inventory",
        "missing",
        "live-root",
    ] {
        let payload = checkpoint
            .join("collections")
            .join(&admin.collection)
            .join("payloads")
            .join(&input.member.sha256);
        let mut manifest: CheckpointInfo = serde_json::from_slice(&original_manifest).unwrap();
        match corruption {
            "version" => manifest.version += 1,
            "catalog" => {
                fs::write(checkpoint.join("authority.sqlite"), b"broken database").unwrap()
            }
            "payload" => {
                let mut bytes = input.bytes.clone();
                bytes[0] ^= 1;
                fs::write(&payload, bytes).unwrap();
            }
            "inventory" => manifest.payloads += 1,
            "missing" => fs::remove_file(&payload).unwrap(),
            "live-root" => (),
            _ => unreachable!(),
        }
        fs::write(
            checkpoint.join("checkpoint.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let source = if corruption == "live-root" {
            root.path().join("server")
        } else {
            checkpoint.clone()
        };
        assert!(
            HistoryServer::restore_checkpoint(&source, &destination, &token).is_err(),
            "{corruption}"
        );
        assert!(!destination.exists(), "{corruption}");
        assert!(!token.exists(), "{corruption}");
        fs::write(checkpoint.join("authority.sqlite"), &original).unwrap();
        fs::write(checkpoint.join("checkpoint.json"), &original_manifest).unwrap();
        fs::write(&payload, &input.bytes).unwrap();
    }
    assert!(matches!(
        HistoryServer::open(ServerConfig::new(&checkpoint)),
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        fs::read(checkpoint.join("authority.sqlite")).unwrap(),
        original
    );
    assert_eq!(
        fs::read(checkpoint.join("checkpoint.json")).unwrap(),
        original_manifest
    );
    assert!(!checkpoint.join("server.lock").exists());
    // The nearest ordinary case still succeeds after all failed attempts.
    let (_, info, _) = restore(&checkpoint, root.path());
    assert_eq!(info.collections, vec![admin.collection]);
    assert_eq!(
        fs::read(checkpoint.join("authority.sqlite")).unwrap(),
        original
    );
}

#[test]
fn empty_checkpoint_refuses_without_creating_a_credential_or_root() {
    let root = tempfile::tempdir().unwrap();
    let server = HistoryServer::open(ServerConfig::new(root.path().join("empty"))).unwrap();
    let checkpoint = root.path().join("checkpoint");
    server.checkpoint(&checkpoint).unwrap();
    let destination = root.path().join("restored");
    let token = destination.join("operator.json");
    assert!(matches!(
        HistoryServer::restore_checkpoint(&checkpoint, &destination, &token),
        Err(Error::Invalid(
            "checkpoint has no collections; initialize a new server instead"
        ))
    ));
    assert!(!destination.exists());
}

#[test]
fn default_operator_credential_is_published_inside_the_fresh_root() {
    let root = tempfile::tempdir().unwrap();
    let (server, admin) = bootstrap(root.path());
    let checkpoint = root.path().join("checkpoint");
    server.checkpoint(&checkpoint).unwrap();
    let destination = root.path().join("restored");
    let token_path = destination.join("operator.json");
    let info = HistoryServer::restore_checkpoint(&checkpoint, &destination, &token_path).unwrap();
    assert_eq!(info.collections, vec![admin.collection.clone()]);
    let file =
        ctx_history_platform::platform_security::open_verified_private_file(&token_path).unwrap();
    let token: TokenFile = serde_json::from_reader(file).unwrap();
    assert_eq!(token.principal, info.principal);
    assert_eq!(token.collection, admin.collection);
    let restored = HistoryServer::open(ServerConfig::new(&destination)).unwrap();
    restored
        .status(&token.credential.secret, &token.collection)
        .unwrap();
    assert!(matches!(
        restored.status(&admin.credential.secret, &admin.collection),
        Err(Error::Forbidden)
    ));
    let bytes = fs::read(&token_path).unwrap();
    assert!(matches!(
        HistoryServer::restore_checkpoint(&checkpoint, &destination, &token_path),
        Err(Error::Conflict)
    ));
    assert_eq!(fs::read(&token_path).unwrap(), bytes);
    let credentials: u64 = restored
        .lock()
        .unwrap()
        .query_row("SELECT count(*) FROM credentials", [], |r| r.get(0))
        .unwrap();
    assert_eq!(credentials, 1);
}
