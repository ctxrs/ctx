use super::*;

#[tokio::test]
async fn recovered_manager_can_review_more_than_a_hundred_publications_across_collections() {
    let root = tempfile::tempdir().unwrap();
    let input = fixture(root.path(), "inventory", &["exact retained kiwi"]);
    let (server, admin) = bootstrap(root.path());
    let second = server.create_collection("second").unwrap();
    server
        .set_grants(
            &admin.principal,
            &second,
            Grants {
                read: true,
                publish: true,
                manage: true,
            },
        )
        .unwrap();
    for (collection, count) in [(&admin.collection, 103), (&second, 2)] {
        for number in 0..count {
            let id = format!("publication-{number:03}");
            let mut request = stage(
                &server,
                &admin.credential.secret,
                collection,
                &input,
                &id,
                None,
                &id,
            );
            request.identity.origin = id;
            server
                .publish(&admin.credential.secret, collection, request)
                .unwrap();
        }
    }
    let checkpoint = root.path().join("checkpoint");
    server.checkpoint(&checkpoint).unwrap();
    drop(server);
    let (server, restored, owner) = super::recovery::restore(&checkpoint, root.path());
    assert_eq!(restored.collections.len(), 2);
    // Inventory works before the derived search generation exists. Exact reads
    // retain the ordinary gate until the indexer publishes a safe generation.
    let mut after = None;
    let mut entries = Vec::new();
    loop {
        let page = server
            .list_publications(
                &owner.secret,
                &admin.collection,
                PublicationListRequest { after, limit: 37 },
            )
            .unwrap();
        assert!(!page.publications.is_empty());
        assert!(page.publications.len() <= 37);
        after = page.next_cursor;
        entries.extend(page.publications);
        if after.is_none() {
            break;
        }
    }
    assert_eq!(entries.len(), 103);
    for (number, entry) in entries.iter().enumerate() {
        assert_eq!(entry.state.publication, format!("publication-{number:03}"));
        assert_eq!(entry.state.owner, admin.principal);
        assert_eq!(entry.state.revision, input.member.sha256);
        assert_eq!(entry.retained_revision, input.member.sha256);
        assert_eq!(entry.state.sequence, number as u64 + 1);
        assert!(!entry.state.withdrawn);
        assert_eq!(entry.session_citations.len(), 1);
    }
    let last = entries.last().unwrap();
    assert!(matches!(
        server.read_session(
            &owner.secret,
            &admin.collection,
            &last.session_citations[0],
            SessionRequest::default(),
        ),
        Err(Error::Unavailable)
    ));
    server.index_pending(&admin.collection, 256).unwrap();
    let page = server
        .read_session(
            &owner.secret,
            &admin.collection,
            &last.session_citations[0],
            SessionRequest::default(),
        )
        .unwrap();
    assert_eq!(page.events.len(), 1);
    assert_eq!(
        page.events[0].record.content.meaningful_text(),
        "exact retained kiwi"
    );
    assert_eq!(page.events[0].provenance.publisher, admin.principal);
    let other = server
        .list_publications(&owner.secret, &second, PublicationListRequest::default())
        .unwrap();
    assert_eq!(other.publications.len(), 2);
    assert!(other.next_cursor.is_none());
    assert_ne!(
        other.publications[0].session_citations,
        entries[0].session_citations
    );

    // The manager removes another publisher's entry before explicitly resharing.
    server
        .remove_publication(
            &owner.secret,
            &admin.collection,
            WithdrawRequest {
                operation: Operation {
                    idempotency_key: "review-removal".into(),
                    publication: last.state.publication.clone(),
                    writer_epoch: last.state.writer_epoch,
                    policy_revision: last.state.policy_revision,
                    expected_revision: Some(last.state.revision.clone()),
                    expected_sequence: Some(last.state.sequence),
                    revision: "withdrawn".into(),
                },
            },
        )
        .unwrap();
    let after = server
        .list_publications(
            &owner.secret,
            &admin.collection,
            PublicationListRequest {
                after: None,
                limit: 100,
            },
        )
        .unwrap()
        .next_cursor
        .unwrap();
    let tail = server
        .list_publications(
            &owner.secret,
            &admin.collection,
            PublicationListRequest {
                after: Some(after.clone()),
                limit: 100,
            },
        )
        .unwrap();
    assert_eq!(tail.publications.len(), 3);
    assert!(tail.publications[2].state.withdrawn);
    assert!(tail.publications[2].session_citations.is_empty());
    assert!(tail.next_cursor.is_none());
    assert!(matches!(
        server.read_session(
            &owner.secret,
            &admin.collection,
            &entries[0].session_citations[0],
            SessionRequest::default(),
        ),
        Err(Error::Unavailable)
    ));
    server.index_pending(&admin.collection, 256).unwrap();
    let invite = server
        .invite(
            &owner.secret,
            &admin.collection,
            InviteRequest {
                name: "reader".into(),
                grants: Grants {
                    read: true,
                    publish: false,
                    manage: false,
                },
                enrollment_ttl_seconds: 600,
                credential_ttl_seconds: 3600,
            },
        )
        .unwrap();
    let reader = server.redeem(&invite.enrollment.secret).unwrap();
    server
        .read_session(
            &reader.credential.secret,
            &admin.collection,
            &entries[0].session_citations[0],
            SessionRequest::default(),
        )
        .unwrap();
    assert!(matches!(
        server.read_session(
            &reader.credential.secret,
            &admin.collection,
            &last.session_citations[0],
            SessionRequest::default()
        ),
        Err(Error::NotFound)
    ));
    for token in [&reader.credential.secret, &admin.credential.secret] {
        assert!(matches!(
            server.list_publications(token, &admin.collection, PublicationListRequest::default()),
            Err(Error::Forbidden)
        ));
    }
    assert!(matches!(
        server.list_publications(
            &reader.credential.secret,
            &second,
            PublicationListRequest::default()
        ),
        Err(Error::Forbidden)
    ));
    for limit in [0, 101] {
        assert!(matches!(
            server.list_publications(
                &owner.secret,
                &admin.collection,
                PublicationListRequest { after: None, limit }
            ),
            Err(Error::Invalid(_))
        ));
    }
    let app = router(Arc::new(server));
    let url = format!(
        "/v1/collections/{}/publications?after={after}&limit=3",
        admin.collection
    );
    for (secret, expected) in [
        (&owner.secret, StatusCode::OK),
        (&reader.credential.secret, StatusCode::FORBIDDEN),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&url)
                    .header("authorization", format!("Bearer {secret}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        if expected == StatusCode::OK {
            let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
            let page: PublicationPage = serde_json::from_slice(&body).unwrap();
            assert_eq!(page.publications.len(), 3);
            assert_eq!(page.publications[0].state.publication, "publication-100");
            assert!(page.next_cursor.is_none());
        }
    }
}

#[tokio::test]
async fn status_identifies_the_presented_credential_including_publish_only_and_rotation() {
    let root = tempfile::tempdir().unwrap();
    let (server, admin) = bootstrap(root.path());
    let publish = Grants {
        read: false,
        publish: true,
        manage: false,
    };
    let invitation = server
        .invite(
            &admin.credential.secret,
            &admin.collection,
            InviteRequest {
                name: "publisher".into(),
                grants: publish,
                enrollment_ttl_seconds: 600,
                credential_ttl_seconds: 3600,
            },
        )
        .unwrap();
    let publisher = server.redeem(&invitation.enrollment.secret).unwrap();
    let replacement = server
        .issue_credential(&publisher.principal, publish, 3600)
        .unwrap();
    for token in [&publisher.credential.secret, &replacement.secret] {
        let status = server.status(token, &admin.collection).unwrap();
        assert_eq!(status.principal, publisher.principal);
    }
    assert_eq!(
        server
            .status(&admin.credential.secret, &admin.collection)
            .unwrap()
            .principal,
        admin.principal
    );
    let other = server
        .invite(
            &admin.credential.secret,
            &admin.collection,
            InviteRequest {
                name: "other publisher".into(),
                grants: publish,
                enrollment_ttl_seconds: 600,
                credential_ttl_seconds: 3600,
            },
        )
        .unwrap();
    let other = server.redeem(&other.enrollment.secret).unwrap();
    assert_ne!(
        server
            .status(&other.credential.secret, &admin.collection)
            .unwrap()
            .principal,
        publisher.principal
    );
    server.revoke_credential(&publisher.credential.id).unwrap();
    assert!(matches!(
        server.status(&publisher.credential.secret, &admin.collection),
        Err(Error::Forbidden)
    ));
    assert_eq!(
        server
            .status(&replacement.secret, &admin.collection)
            .unwrap()
            .principal,
        publisher.principal
    );
    let app = router(Arc::new(server));
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/collections/{}/status", admin.collection))
                .header("authorization", format!("Bearer {}", replacement.secret))
                .header("x-principal", &admin.principal)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let status: CollectionStatus = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status.principal, publisher.principal);
}

#[test]
fn recovery_inventory_exposes_all_retained_revisions_and_removal_hides_them_together() {
    let root = tempfile::tempdir().unwrap();
    let a = fixture(
        &root.path().join("a"),
        "corrected",
        &["synthetic private almond"],
    );
    let b = fixture(
        &root.path().join("b"),
        "corrected",
        &["ordinary corrected pear"],
    );
    let sibling = fixture(
        &root.path().join("sibling"),
        "sibling",
        &["ordinary sibling kiwi"],
    );
    let (server, admin) = bootstrap(root.path());
    let collection = &admin.collection;
    let secret = &admin.credential.secret;
    let request = stage(&server, secret, collection, &a, "corrected", None, "a");
    server.publish(secret, collection, request).unwrap();
    let request = stage(
        &server,
        secret,
        collection,
        &b,
        "corrected",
        Some(a.member.sha256.clone()),
        "b",
    );
    server.publish(secret, collection, request).unwrap();
    let request = stage(
        &server, secret, collection, &sibling, "sibling", None, "sibling",
    );
    server.publish(secret, collection, request).unwrap();
    let checkpoint = root.path().join("checkpoint");
    server.checkpoint(&checkpoint).unwrap();
    drop(server);
    let (server, _, owner) = super::recovery::restore(&checkpoint, root.path());
    server.index_pending(collection, 16).unwrap();
    assert!(find(&server, &owner.secret, collection, "almond").is_empty());

    // Start with no old citation or search term. A one-row page forces the
    // opaque cursor to advance between two revisions of the same publication.
    let mut after = None;
    let mut entries = Vec::new();
    loop {
        let page = server
            .list_publications(
                &owner.secret,
                collection,
                PublicationListRequest { after, limit: 1 },
            )
            .unwrap();
        assert_eq!(page.publications.len(), 1);
        after = page.next_cursor;
        entries.extend(page.publications);
        if after.is_none() {
            break;
        }
    }
    assert_eq!(entries.len(), 3);
    let corrected: Vec<_> = entries
        .iter()
        .filter(|entry| entry.state.publication == "corrected")
        .collect();
    assert_eq!(corrected.len(), 2);
    let mut bodies = Vec::new();
    for entry in &corrected {
        assert_eq!(entry.state.owner, admin.principal);
        assert_eq!(entry.state.revision, b.member.sha256);
        assert_eq!(entry.state.sequence, 2);
        let citation = &entry.session_citations[0];
        assert_eq!(
            Citation::parse(citation).unwrap().revision,
            entry.retained_revision
        );
        let page = server
            .read_session(
                &owner.secret,
                collection,
                citation,
                SessionRequest::default(),
            )
            .unwrap();
        assert_eq!(page.events[0].provenance.publisher, admin.principal);
        bodies.push(page.events[0].record.content.meaningful_text().to_owned());
    }
    bodies.sort();
    assert_eq!(
        bodies,
        vec!["ordinary corrected pear", "synthetic private almond"]
    );
    let ordinary = entries
        .iter()
        .find(|entry| entry.state.publication == "sibling")
        .unwrap();
    let ordinary_before = server
        .read_session(
            &owner.secret,
            collection,
            &ordinary.session_citations[0],
            SessionRequest::default(),
        )
        .unwrap();
    let current = &corrected[0].state;
    server
        .remove_publication(
            &owner.secret,
            collection,
            WithdrawRequest {
                operation: Operation {
                    idempotency_key: "review-removal".into(),
                    publication: current.publication.clone(),
                    writer_epoch: current.writer_epoch,
                    policy_revision: current.policy_revision,
                    expected_revision: Some(current.revision.clone()),
                    expected_sequence: Some(current.sequence),
                    revision: "withdrawn".into(),
                },
            },
        )
        .unwrap();
    server.index_pending(collection, 16).unwrap();
    let invitation = server
        .invite(
            &owner.secret,
            collection,
            InviteRequest {
                name: "reader after review".into(),
                grants: Grants {
                    read: true,
                    publish: false,
                    manage: false,
                },
                enrollment_ttl_seconds: 600,
                credential_ttl_seconds: 0,
            },
        )
        .unwrap();
    let reader = server.redeem(&invitation.enrollment.secret).unwrap();
    for entry in corrected {
        assert!(matches!(
            server.read_session(
                &reader.credential.secret,
                collection,
                &entry.session_citations[0],
                SessionRequest::default()
            ),
            Err(Error::NotFound)
        ));
    }
    let ordinary_after = server
        .read_session(
            &reader.credential.secret,
            collection,
            &ordinary.session_citations[0],
            SessionRequest::default(),
        )
        .unwrap();
    assert_eq!(
        serde_json::to_value(ordinary_before).unwrap(),
        serde_json::to_value(ordinary_after).unwrap()
    );
    let page = server
        .list_publications(&owner.secret, collection, PublicationListRequest::default())
        .unwrap();
    assert_eq!(page.publications.len(), 3);
    for entry in page
        .publications
        .iter()
        .filter(|entry| entry.state.publication == "corrected")
    {
        assert!(entry.state.withdrawn);
        assert!(entry.session_citations.is_empty());
    }
    assert!(matches!(
        server.list_publications(
            &reader.credential.secret,
            collection,
            PublicationListRequest::default()
        ),
        Err(Error::Forbidden)
    ));
    assert!(server
        .list_publications(
            &owner.secret,
            collection,
            PublicationListRequest {
                after: Some("not-a-cursor".into()),
                limit: 1
            }
        )
        .is_err());
}
