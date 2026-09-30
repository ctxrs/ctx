use std::collections::BTreeSet;

use super::*;

fn enrollment_server() -> (Mock, Arc<AtomicBool>) {
    let revoked = Arc::new(AtomicBool::new(false));
    let old_revoked = revoked.clone();
    let mut consumed = BTreeSet::new();
    let server = Mock::new(move |request| {
        if request.path == "/v1/enroll" {
            assert_eq!(request.method, "POST");
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            let secret = body["enrollment"].as_str().unwrap();
            if !consumed.insert(secret.to_owned()) {
                return Response::json(401, &json!({}));
            }
            if secret == "lost-response" {
                return Response::Drop;
            }
            if secret == "expired" {
                return Response::json(401, &json!({}));
            }
            let (principal, token, publish) = match secret {
                "first" => ("publisher-one", "original", true),
                "rotated" => ("publisher-one", "rotated", true),
                "reader" => ("publisher-one", "same-reader", false),
                "other" => ("publisher-two", "other", true),
                "missing-principal" => ("", "bad-result", true),
                _ => ("publisher-one", "bad-result", true),
            };
            return Response::json(
                200,
                &json!({
                    "collection": if secret == "wrong-collection" { "other-collection" } else { COLLECTION },
                    "principal": principal,
                    "enrollment_id": format!("enrollment-{secret}"),
                    "credential": {
                        "id": if secret == "missing-id" { String::new() } else { format!("credential-{secret}") },
                        "secret": token, "expires_at": 0,
                        "grants": { "read": true, "publish": publish, "manage": false }
                    }
                }),
            );
        }
        assert_eq!(request.method, "GET");
        let principal = match request.authorization.as_str() {
            "Bearer original" if !old_revoked.load(Ordering::Acquire) => "publisher-one",
            "Bearer rotated" | "Bearer same-reader" => "publisher-one",
            "Bearer other" => "publisher-two",
            _ => return Response::json(403, &json!({})),
        };
        if request.path == format!("/v1/collections/{COLLECTION}/whoami") {
            Response::json(
                200,
                &json!({
                    "collection": COLLECTION, "principal": principal,
                    "credential_id": "credential-from-whoami", "enrollment_id": null,
                    "server_owner": false,
                    "grants": { "read": true, "publish": request.authorization != "Bearer same-reader", "manage": false }
                }),
            )
        } else {
            assert_eq!(request.path, format!("/v1/collections/{COLLECTION}/status"));
            Response::json(
                200,
                &json!({
                    "collection": COLLECTION, "principal": principal,
                    "stored_sequence": 0, "searchable_sequence": 0, "generation": null,
                    "reads_available": true, "off_host_checkpoint": null
                }),
            )
        }
    });
    (server, revoked)
}

fn settings_bytes(store: &SharingStore) -> Vec<u8> {
    fs::read(store.root().join("settings.json")).unwrap()
}

#[test]
fn exact_saved_enrollment_repeats_offline_without_changing_credentials_consent_or_work() {
    let temp = tempdir().unwrap();
    let (server, _) = enrollment_server();
    let store = SharingStore::new(temp.path().join("sharing"));
    let destination = connection(&server);
    assert!(!store
        .enroll(destination.clone(), "first", Some("publisher-one"), false)
        .unwrap());
    let saved = store.settings().unwrap().unwrap();
    assert!(saved.policy.is_none());
    assert!(!saved.paused);
    let receipt = saved.enrollment.unwrap();
    assert_eq!(receipt.principal, "publisher-one");
    assert_eq!(receipt.credential_id, "credential-first");
    assert_eq!(receipt.enrollment_id.as_deref(), Some("enrollment-first"));
    assert_eq!(receipt.fingerprint.len(), 64);
    assert!(!String::from_utf8(settings_bytes(&store))
        .unwrap()
        .contains("\"first\""));
    store.set_policy(policy()).unwrap();
    store.pause(true).unwrap();
    let retained = retain_work(&store);
    let before = settings_bytes(&store);
    assert_eq!(server.requests().len(), 2);
    drop(server);
    for claim in [None, Some("publisher-one")] {
        assert!(store
            .enroll(destination.clone(), "first", claim, false)
            .unwrap());
        assert_eq!(settings_bytes(&store), before);
        assert_retained(&retained);
    }
    assert_eq!(
        store.enroll(destination.clone(), "first", Some("publisher-two"), false),
        Err(Error::Credentials)
    );
    for changed in [
        Connection {
            endpoint: Endpoint::parse("https://different.example").unwrap(),
            ..destination.clone()
        },
        Connection {
            collection: "other-collection".into(),
            ..destination
        },
    ] {
        assert_eq!(
            store.enroll(changed, "first", None, false),
            Err(Error::DestinationChanged)
        );
    }
    assert_eq!(settings_bytes(&store), before);
    assert_retained(&retained);
}

#[test]
fn same_user_rotations_preserve_work_after_revocation_and_read_only_never_resumes() {
    let temp = tempdir().unwrap();
    let (server, revoked) = enrollment_server();
    let store = SharingStore::new(temp.path().join("sharing"));
    let destination = connection(&server);
    store
        .enroll(destination.clone(), "first", None, false)
        .unwrap();
    store.set_policy(policy()).unwrap();
    let retained = retain_work(&store);
    revoked.store(true, Ordering::Release);
    assert!(!store
        .enroll(destination.clone(), "reader", Some("publisher-one"), false)
        .unwrap());
    let reader = store.settings().unwrap().unwrap();
    assert_eq!(reader.credentials.publish(), Err(Error::MissingCredential));
    assert!(reader.paused);
    assert_eq!(reader.policy, Some(policy()));
    let before = settings_bytes(&store);
    // Omitting --read-only on a rerun cannot broaden the saved credentials.
    assert!(store
        .enroll(destination.clone(), "reader", None, false)
        .unwrap());
    assert_eq!(settings_bytes(&store), before);
    assert!(!store.enroll(destination, "rotated", None, false).unwrap());
    let rotated = store.settings().unwrap().unwrap();
    assert_eq!(rotated.credentials.publish().unwrap(), "rotated");
    assert!(rotated.paused);
    assert_eq!(rotated.publisher.as_deref(), Some("publisher-one"));
    assert_eq!(rotated.policy, Some(policy()));
    assert_eq!(server.requests().len(), 4);
    assert_retained(&retained);
}

#[test]
fn other_user_is_rejected_before_exchange_and_forged_claim_cannot_adopt_consent() {
    let temp = tempdir().unwrap();
    let (server, _) = enrollment_server();
    let store = SharingStore::new(temp.path().join("sharing"));
    let destination = connection(&server);
    store
        .enroll(destination.clone(), "first", Some("publisher-one"), false)
        .unwrap();
    store.set_policy(policy()).unwrap();
    let retained = retain_work(&store);
    let before = settings_bytes(&store);
    assert_eq!(
        store.enroll(destination.clone(), "other", Some("publisher-two"), true),
        Err(Error::Credentials)
    );
    assert_eq!(server.requests().len(), 2);
    // A dishonest file label cannot turn the actual other user into this user.
    assert_eq!(
        store.enroll(destination.clone(), "other", Some("publisher-one"), false),
        Err(Error::Credentials)
    );
    assert_eq!(settings_bytes(&store), before);
    assert_retained(&retained);
    // New local state is not evidence that a single-use invitation is reusable.
    let fresh = SharingStore::new(temp.path().join("fresh"));
    assert_eq!(
        fresh.enroll(destination, "first", None, false),
        Err(Error::Unauthorized)
    );
    assert!(fresh.connection().unwrap().is_none());
}

#[test]
fn invalid_or_lost_enrollment_response_keeps_settings_and_has_no_recovery_journal() {
    let temp = tempdir().unwrap();
    let (server, _) = enrollment_server();
    let store = SharingStore::new(temp.path().join("sharing"));
    let destination = connection(&server);
    store
        .enroll(destination.clone(), "first", None, false)
        .unwrap();
    let retained = retain_work(&store);
    let before = settings_bytes(&store);
    for (secret, expected) in [
        ("missing-principal", Error::Credentials),
        ("missing-id", Error::Credentials),
        ("wrong-collection", Error::Protocol),
        ("expired", Error::Unauthorized),
        ("lost-response", Error::Unavailable),
    ] {
        assert_eq!(
            store.enroll(destination.clone(), secret, None, false),
            Err(expected)
        );
        assert_eq!(settings_bytes(&store), before);
        assert_retained(&retained);
    }
    assert_eq!(
        store.enroll(destination.clone(), "lost-response", None, false),
        Err(Error::Unauthorized)
    );
    // Reissuing for the same user is the supported repair after response loss.
    assert!(!store.enroll(destination, "rotated", None, false).unwrap());
    assert!(store.policy().unwrap().is_none());
    assert_retained(&retained);
}

#[test]
fn legacy_reader_authenticates_before_enrollment_but_bound_publisher_needs_no_old_token() {
    let temp = tempdir().unwrap();
    let (server, _) = enrollment_server();
    let store = SharingStore::new(temp.path().join("reader"));
    let destination = connection(&server);
    store
        .connect(
            destination.clone(),
            Credentials::read_only("same-reader".into()).unwrap(),
        )
        .unwrap();
    let before = settings_bytes(&store);
    assert_eq!(
        store.enroll(destination.clone(), "other", Some("publisher-two"), true),
        Err(Error::Credentials)
    );
    assert_eq!(settings_bytes(&store), before);
    assert!(server
        .requests()
        .iter()
        .all(|request| request.method == "GET"));
    assert!(!store
        .enroll(destination, "reader", Some("publisher-one"), false)
        .unwrap());
    assert!(store.policy().unwrap().is_none());

    let (server, revoked) = enrollment_server();
    let store = SharingStore::new(temp.path().join("publisher"));
    let destination = connection(&server);
    store
        .connect(
            destination.clone(),
            Credentials::device("original".into()).unwrap(),
        )
        .unwrap();
    store.set_policy(policy()).unwrap();
    revoked.store(true, Ordering::Release);
    assert!(!store
        .enroll(destination, "rotated", Some("publisher-one"), false)
        .unwrap());
    assert_eq!(server.requests().len(), 2);
    assert_eq!(store.policy().unwrap(), Some(policy()));
}

#[test]
fn revoked_unbound_legacy_reader_preserves_state_and_invitation_for_a_new_store() {
    let temp = tempdir().unwrap();
    let (server, revoked) = enrollment_server();
    let store = SharingStore::new(temp.path().join("legacy-reader"));
    let destination = connection(&server);
    store
        .connect(
            destination.clone(),
            Credentials::read_only("original".into()).unwrap(),
        )
        .unwrap();
    // The baseline client saved neither enrollment nor publisher identity.
    let mut legacy = serde_json::to_value(store.settings().unwrap().unwrap()).unwrap();
    legacy.as_object_mut().unwrap().remove("enrollment");
    legacy.as_object_mut().unwrap().remove("publisher");
    private_file::write(&store.root().join("settings.json"), &legacy).unwrap();
    let before = settings_bytes(&store);
    let retained = retain_work(&store);
    revoked.store(true, Ordering::Release);

    assert_eq!(
        store.enroll(destination.clone(), "reader", Some("publisher-one"), true),
        Err(Error::Forbidden)
    );
    assert_eq!(settings_bytes(&store), before);
    assert_retained(&retained);
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(
        requests[0].path,
        format!("/v1/collections/{COLLECTION}/whoami")
    );

    // The same invitation still works under a new connection name/store.
    let recovered = SharingStore::new(temp.path().join("recovered-reader"));
    assert!(!recovered
        .enroll(destination, "reader", Some("publisher-one"), true)
        .unwrap());
    assert_eq!(
        recovered.saved_principal().unwrap().as_deref(),
        Some("publisher-one")
    );
    assert!(!recovered.has_publish_credential().unwrap());
    assert!(recovered.policy().unwrap().is_none());
    assert_eq!(server.requests().len(), 2);
    assert_eq!(server.requests()[1].path, "/v1/enroll");
    assert_eq!(settings_bytes(&store), before);
    assert_retained(&retained);
}

#[test]
fn enrolled_identity_survives_token_rotation_and_rejects_another_reader() {
    let temp = tempdir().unwrap();
    let (server, _) = enrollment_server();
    let store = SharingStore::new(temp.path().join("sharing"));
    let destination = connection(&server);
    store
        .enroll(destination.clone(), "first", None, true)
        .unwrap();
    let before = settings_bytes(&store);
    assert_eq!(
        store.connect(
            destination.clone(),
            Credentials::read_only("other".into()).unwrap()
        ),
        Err(Error::Credentials)
    );
    assert_eq!(settings_bytes(&store), before);
    store
        .connect(
            destination.clone(),
            Credentials::read_only("same-reader".into()).unwrap(),
        )
        .unwrap();
    let rotated = settings_bytes(&store);
    assert!(store.enroll(destination, "first", None, false).unwrap());
    assert_eq!(settings_bytes(&store), rotated);
    assert_eq!(
        store.settings().unwrap().unwrap().credentials.publish(),
        Err(Error::MissingCredential)
    );
}

#[test]
fn concurrent_reruns_redeem_only_once_and_save_one_credential() {
    let temp = tempdir().unwrap();
    let (server, _) = enrollment_server();
    let store = SharingStore::new(temp.path().join("sharing"));
    let destination = connection(&server);
    let mut results = std::thread::scope(|scope| {
        let first = scope.spawn(|| store.enroll(destination.clone(), "first", None, false));
        let second = scope.spawn(|| store.enroll(destination.clone(), "first", None, false));
        vec![
            first.join().unwrap().unwrap(),
            second.join().unwrap().unwrap(),
        ]
    });
    results.sort();
    assert_eq!(results, vec![false, true]);
    assert_eq!(server.requests().len(), 1);
    assert_eq!(
        store
            .settings()
            .unwrap()
            .unwrap()
            .credentials
            .publish()
            .unwrap(),
        "original"
    );
}

#[test]
fn enrolled_client_can_disable_publishing_with_its_unchanged_token_offline() {
    for reader in ["original", "same-reader"] {
        let temp = tempdir().unwrap();
        let (server, _) = enrollment_server();
        let store = SharingStore::new(temp.path().join("sharing"));
        let destination = connection(&server);
        store
            .enroll(destination.clone(), "first", None, false)
            .unwrap();
        store
            .connect(
                destination.clone(),
                Credentials::new(Some(reader.into()), Some("original".into())).unwrap(),
            )
            .unwrap();
        store.set_policy(policy()).unwrap();
        let retained = retain_work(&store);
        drop(server);
        store
            .connect(
                destination.clone(),
                Credentials::read_only(reader.into()).unwrap(),
            )
            .unwrap();
        let saved = store.settings().unwrap().unwrap();
        assert!(saved.paused);
        assert_eq!(saved.credentials.publish(), Err(Error::MissingCredential));
        assert_eq!(saved.policy, Some(policy()));
        assert_eq!(saved.credentials.read().unwrap(), reader);
        let before = settings_bytes(&store);
        assert_eq!(
            store.connect(
                destination,
                Credentials::read_only("rotated".into()).unwrap()
            ),
            Err(Error::Unavailable)
        );
        assert_eq!(settings_bytes(&store), before);
        assert_retained(&retained);
    }
}

#[test]
fn saved_enrollment_honors_read_only_offline_and_never_restores_publishing_on_repeat() {
    let temp = tempdir().unwrap();
    let (server, _) = enrollment_server();
    let store = SharingStore::new(temp.path().join("sharing"));
    let destination = connection(&server);
    store
        .enroll(destination.clone(), "first", None, false)
        .unwrap();
    store.set_policy(policy()).unwrap();
    let retained = retain_work(&store);
    drop(server);
    assert!(store
        .enroll(destination.clone(), "first", None, true)
        .unwrap());
    let saved = store.settings().unwrap().unwrap();
    assert_eq!(saved.credentials.publish(), Err(Error::MissingCredential));
    assert_eq!(saved.credentials.read().unwrap(), "original");
    assert!(saved.paused);
    assert_eq!(saved.policy, Some(policy()));
    assert_eq!(saved.enrollment.unwrap().credential_id, "credential-first");
    let narrowed = settings_bytes(&store);
    for read_only in [true, false, true] {
        assert!(store
            .enroll(destination.clone(), "first", None, read_only)
            .unwrap());
        assert_eq!(settings_bytes(&store), narrowed);
        assert_retained(&retained);
    }
}
