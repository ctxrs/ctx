use super::{
    repair::{Pause, Stage},
    *,
};
use rusqlite::params;

pub(super) fn unsubmitted(input: &Fixture, key: &str) -> PublishRequest {
    PublishRequest {
        operation: Operation {
            idempotency_key: key.into(),
            publication: "pub".into(),
            revision: input.member.sha256.clone(),
            writer_epoch: 1,
            policy_revision: 1,
            expected_revision: None,
            expected_sequence: None,
        },
        identity: input.identity.clone(),
        member: input.member.clone(),
        upload: "never-staged".into(),
    }
}

pub(super) fn cancel_request(publisher: &str, request: &PublishRequest) -> CancelPublishRequest {
    CancelPublishRequest {
        publisher: publisher.into(),
        operation: request.operation.clone(),
        fingerprint: publish_fingerprint(
            publisher,
            &request.operation,
            &request.identity,
            &request.member,
        )
        .unwrap(),
    }
}

fn publish_only() -> Grants {
    Grants {
        read: false,
        publish: true,
        manage: false,
    }
}

#[tokio::test]
async fn cancellation_without_dispatch_is_terminal_on_http_and_fresh_key_can_publish() {
    let root = tempfile::tempdir().unwrap();
    let input = fixture(root.path(), "cancel-http", &["permitted pear"]);
    let (server, admin) = bootstrap(root.path());
    let device = server
        .issue_credential(&admin.principal, publish_only(), 3600)
        .unwrap();
    let server = Arc::new(server);
    let app = router(server.clone());
    let request = unsubmitted(&input, "never-sent");
    let cancel = cancel_request(&admin.principal, &request);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/v1/collections/{}/operations/cancel",
                    admin.collection
                ))
                .header("authorization", format!("Bearer {}", device.secret))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&cancel).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let wire: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(wire["outcome"]["status"], "cancelled");
    assert!(wire["publication"].is_null());
    let outcome: CancelPublishResponse = serde_json::from_slice(&body).unwrap();
    assert!(matches!(
        outcome.outcome,
        CancelPublishOutcome::Cancelled { .. }
    ));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/collections/{}/revisions", admin.collection))
                .header("authorization", format!("Bearer {}", device.secret))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let wire: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024).await.unwrap()).unwrap();
    assert_eq!(wire["error"], "operation_cancelled");
    assert!(matches!(
        server.receipt(&device.secret, &admin.collection, "never-sent"),
        Err(Error::OperationCancelled)
    ));
    assert_eq!(server.local_health().unwrap().pending_operations, 0);
    assert_eq!(
        server
            .status(&admin.credential.secret, &admin.collection)
            .unwrap()
            .stored_sequence,
        0
    );
    let fresh = stage(
        &server,
        &device.secret,
        &admin.collection,
        &input,
        "pub",
        None,
        "approved",
    );
    assert_eq!(
        server
            .publish(&device.secret, &admin.collection, fresh)
            .unwrap()
            .sequence,
        1
    );
    let repeated = server
        .cancel_publish(&device.secret, &admin.collection, cancel)
        .unwrap();
    assert!(matches!(
        repeated.outcome,
        CancelPublishOutcome::Cancelled { .. }
    ));
    assert_eq!(repeated.publication.unwrap().sequence, 1);
}

#[test]
fn cancel_wins_validation_and_publish_wins_returns_original_receipt_with_current_state() {
    let root = tempfile::tempdir().unwrap();
    let a = fixture(&root.path().join("a"), "race-cancel", &["original mango"]);
    let b = fixture(&root.path().join("b"), "race-cancel", &["corrected lime"]);
    let (server, admin) = bootstrap(root.path());
    let other_device = server
        .issue_credential(&admin.principal, publish_only(), 3600)
        .unwrap();
    let server = Arc::new(server);
    let collection = &admin.collection;
    let slow = stage(
        &server,
        &admin.credential.secret,
        collection,
        &a,
        "pub",
        None,
        "slow",
    );
    let cancel = cancel_request(&admin.principal, &slow);
    let pause = Pause::at(&server, Stage::Validation);
    let publishing = {
        let server = server.clone();
        let secret = admin.credential.secret.clone();
        let collection = collection.clone();
        std::thread::spawn(move || server.publish(&secret, &collection, slow))
    };
    pause.wait();
    let settled = server
        .cancel_publish(&other_device.secret, collection, cancel)
        .unwrap();
    assert!(matches!(
        settled.outcome,
        CancelPublishOutcome::Cancelled { .. }
    ));
    assert!(settled.publication.is_none());
    drop(pause);
    assert!(matches!(
        publishing.join().unwrap(),
        Err(Error::OperationCancelled)
    ));
    assert!(!server
        .collection_root(collection)
        .join("payloads")
        .join(&a.member.sha256)
        .exists());
    assert_eq!(server.local_health().unwrap().pending_operations, 0);

    let first = stage(
        &server,
        &other_device.secret,
        collection,
        &a,
        "pub",
        None,
        "accepted",
    );
    let original = server
        .publish(&other_device.secret, collection, first.clone())
        .unwrap();
    let second = stage(
        &server,
        &other_device.secret,
        collection,
        &b,
        "pub",
        Some(a.member.sha256.clone()),
        "second",
    );
    server
        .publish(&other_device.secret, collection, second)
        .unwrap();
    let cancel = cancel_request(&admin.principal, &first);
    let settled = server
        .cancel_publish(&other_device.secret, collection, cancel.clone())
        .unwrap();
    let CancelPublishOutcome::Accepted { receipt } = settled.outcome else {
        panic!("publish won")
    };
    assert_eq!(
        serde_json::to_value(&receipt).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
    let observed = settled.publication.unwrap();
    assert_eq!(observed.revision, b.member.sha256);
    assert_eq!(observed.sequence, 2);
    assert_eq!(
        server
            .publish(&other_device.secret, collection, first)
            .unwrap()
            .sequence,
        1
    );
    let mut fresh = stage(
        &server,
        &other_device.secret,
        collection,
        &a,
        "pub",
        Some(observed.revision),
        "approved-next",
    );
    fresh.operation.expected_sequence = Some(observed.sequence);
    assert_eq!(
        server
            .publish(&other_device.secret, collection, fresh)
            .unwrap()
            .sequence,
        3
    );
    let repeated = server
        .cancel_publish(&other_device.secret, collection, cancel)
        .unwrap();
    let CancelPublishOutcome::Accepted { receipt } = repeated.outcome else {
        panic!("stable accepted outcome")
    };
    assert_eq!(
        serde_json::to_value(receipt).unwrap(),
        serde_json::to_value(original).unwrap()
    );
    assert_eq!(repeated.publication.unwrap().sequence, 3);
}

#[test]
fn cancellation_binds_principal_operation_fingerprint_and_current_publish_authority() {
    let root = tempfile::tempdir().unwrap();
    let input = fixture(root.path(), "binding", &["private apple"]);
    let (server, admin) = bootstrap(root.path());
    let publisher = server
        .issue_credential(&admin.principal, publish_only(), 3600)
        .unwrap();
    let q = server.create_principal("other publisher").unwrap();
    server
        .set_grants(&q, &admin.collection, publish_only())
        .unwrap();
    let other = server.issue_credential(&q, publish_only(), 3600).unwrap();
    let request = unsubmitted(&input, "shared-key");
    let p_cancel = cancel_request(&admin.principal, &request);
    assert!(matches!(
        server.cancel_publish(&other.secret, &admin.collection, p_cancel.clone()),
        Err(Error::Forbidden)
    ));
    let q_cancel = cancel_request(&q, &request);
    server
        .cancel_publish(&other.secret, &admin.collection, q_cancel.clone())
        .unwrap();
    let upload = server
        .begin_upload(
            &publisher.secret,
            &admin.collection,
            UploadSpec {
                sha256: input.member.sha256.clone(),
                bytes: input.member.bytes,
            },
        )
        .unwrap();
    assert_eq!(upload.publisher, admin.principal);
    // Q's denial never reserves this publication or P's identical key.
    let staged = stage(
        &server,
        &publisher.secret,
        &admin.collection,
        &input,
        "pub",
        None,
        "shared-key",
    );
    let status = server
        .upload_status(&admin.credential.secret, &admin.collection, &staged.upload)
        .unwrap();
    assert_eq!(status.publisher, admin.principal);
    assert_eq!(
        server
            .upload_chunk(
                &publisher.secret,
                &admin.collection,
                &staged.upload,
                0,
                &input.bytes[..input.bytes.len().min(1024)]
            )
            .unwrap()
            .publisher,
        admin.principal
    );
    server
        .publish(&publisher.secret, &admin.collection, staged)
        .unwrap();
    let settled = server
        .cancel_publish(&publisher.secret, &admin.collection, p_cancel.clone())
        .unwrap();
    assert!(matches!(
        settled.outcome,
        CancelPublishOutcome::Accepted { .. }
    ));
    // Publishers can inspect upload/index coverage, but cannot read content.
    assert_eq!(
        server
            .status(&publisher.secret, &admin.collection)
            .unwrap()
            .stored_sequence,
        1
    );
    assert!(matches!(
        server.search(
            &publisher.secret,
            &admin.collection,
            SearchRequest {
                q: "apple".into(),
                limit: 1
            }
        ),
        Err(Error::Forbidden)
    ));
    let mut wrong = p_cancel.clone();
    wrong.fingerprint = "0".repeat(64);
    assert!(matches!(
        server.cancel_publish(&publisher.secret, &admin.collection, wrong),
        Err(Error::Conflict)
    ));
    let mut wrong = p_cancel.clone();
    wrong.operation.policy_revision += 1;
    assert!(matches!(
        server.cancel_publish(&publisher.secret, &admin.collection, wrong),
        Err(Error::Conflict)
    ));
    let mut wrong = q_cancel.clone();
    wrong.operation.policy_revision += 1;
    assert!(matches!(
        server.cancel_publish(&other.secret, &admin.collection, wrong),
        Err(Error::Conflict)
    ));
    // A repeated Q fence is stable after P claims the publication. A new Q
    // fence on P's publication is forbidden rather than affecting its owner.
    server
        .cancel_publish(&other.secret, &admin.collection, q_cancel.clone())
        .unwrap();
    let mut foreign = q_cancel;
    foreign.operation.idempotency_key = "foreign".into();
    assert!(matches!(
        server.cancel_publish(&other.secret, &admin.collection, foreign),
        Err(Error::Forbidden)
    ));

    let removal = WithdrawRequest {
        operation: Operation {
            idempotency_key: "withdraw".into(),
            publication: "pub".into(),
            revision: "gone".into(),
            expected_revision: Some(input.member.sha256.clone()),
            expected_sequence: Some(1),
            writer_epoch: 1,
            policy_revision: 1,
        },
    };
    server
        .withdraw(&publisher.secret, &admin.collection, removal.clone())
        .unwrap();
    let wrong_kind = CancelPublishRequest {
        publisher: admin.principal.clone(),
        fingerprint: crate::operations::fingerprint(&(
            "withdraw",
            &admin.principal,
            &removal.operation,
        ))
        .unwrap(),
        operation: removal.operation,
    };
    assert!(matches!(
        server.cancel_publish(&publisher.secret, &admin.collection, wrong_kind),
        Err(Error::Conflict)
    ));
    let expired = server
        .issue_credential(&admin.principal, publish_only(), 3600)
        .unwrap();
    server
        .lock()
        .unwrap()
        .execute(
            "UPDATE credentials SET expires=0 WHERE id=?1",
            [&expired.id],
        )
        .unwrap();
    assert!(matches!(
        server.cancel_publish(&expired.secret, &admin.collection, p_cancel.clone()),
        Err(Error::Forbidden)
    ));
    server.revoke_credential(&publisher.id).unwrap();
    assert!(matches!(
        server.cancel_publish(&publisher.secret, &admin.collection, p_cancel.clone()),
        Err(Error::Forbidden)
    ));
    server
        .set_grants(
            &admin.principal,
            &admin.collection,
            Grants {
                read: true,
                publish: false,
                manage: true,
            },
        )
        .unwrap();
    assert!(matches!(
        server.cancel_publish(&admin.credential.secret, &admin.collection, p_cancel),
        Err(Error::Forbidden)
    ));
}

#[test]
fn terminal_settlement_rechecks_expiry_and_rolls_back_both_outcome_fences() {
    let inputs = tempfile::tempdir().unwrap();
    let input = fixture(inputs.path(), "late-cancel", &["safe berry"]);
    for accepted in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let (server, admin) = bootstrap(root.path());
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
        let event = if accepted { "UPDATE" } else { "INSERT" };
        server.lock().unwrap().execute_batch(&format!(
            "CREATE TEMP TRIGGER expire_settlement AFTER {event} ON main.operations BEGIN UPDATE credentials SET expires=0; END;"
        )).unwrap();
        let cancel = cancel_request(&admin.principal, &request);
        assert!(matches!(
            server.cancel_publish(&admin.credential.secret, &admin.collection, cancel.clone()),
            Err(Error::Forbidden)
        ));
        let fenced: u64 = server
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM operations WHERE cancel_fenced=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fenced, 0);
        server
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER expire_settlement")
            .unwrap();
        let response = server
            .cancel_publish(&admin.credential.secret, &admin.collection, cancel)
            .unwrap();
        assert_eq!(
            matches!(response.outcome, CancelPublishOutcome::Accepted { .. }),
            accepted
        );
        assert_eq!(
            server
                .lock()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM operations WHERE cancel_fenced=1 AND collection=?1",
                    params![admin.collection],
                    |r| r.get::<_, u64>(0)
                )
                .unwrap(),
            1
        );
    }
}
