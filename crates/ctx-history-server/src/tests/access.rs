use super::*;

fn rights(read: bool, publish: bool, manage: bool) -> Grants {
    Grants {
        read,
        publish,
        manage,
    }
}

fn invite_request(principal: Option<&str>, grants: Grants) -> InviteRequest {
    InviteRequest {
        name: None,
        principal: principal.map(str::to_owned),
        grants,
        enrollment_ttl_seconds: 600,
        credential_ttl_seconds: 0,
    }
}

#[test]
fn one_identity_two_collections_two_devices_and_no_future_grant_expansion() {
    let root = tempfile::tempdir().unwrap();
    let (server, owner) = bootstrap(root.path());
    let a = &owner.collection;
    let b = server.create_collection("second audience").unwrap();
    let invitation = server
        .invite(
            &owner.credential.secret,
            a,
            invite_request(None, rights(true, false, false)),
        )
        .unwrap();
    let first = server.redeem(&invitation.enrollment.secret).unwrap();
    assert_eq!(
        first.enrollment_id.as_deref(),
        Some(invitation.enrollment.id.as_str())
    );
    assert!(matches!(
        server.redeem(&invitation.enrollment.secret),
        Err(Error::Unauthorized)
    ));
    let identity = server.whoami(&first.credential.secret, a).unwrap();
    assert_eq!(identity.principal, invitation.principal);
    assert_eq!(identity.credential_id, first.credential.id);
    assert_eq!(identity.enrollment_id, first.enrollment_id);
    assert!(!identity.server_owner);
    server
        .manage_grants(
            &owner.credential.secret,
            &first.principal,
            a,
            rights(true, true, true),
        )
        .unwrap();
    server
        .manage_grants(
            &owner.credential.secret,
            &first.principal,
            &b,
            rights(true, true, false),
        )
        .unwrap();
    assert_eq!(
        server.whoami(&first.credential.secret, a).unwrap().grants,
        rights(true, false, false)
    );
    assert!(matches!(
        server.status(&first.credential.secret, &b),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        server.invite(
            &first.credential.secret,
            a,
            invite_request(Some(&first.principal), rights(true, true, false))
        ),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        server.invite(
            &first.credential.secret,
            &b,
            invite_request(Some(&first.principal), rights(true, false, false))
        ),
        Err(Error::Forbidden)
    ));
    let second_invitation = server
        .invite(
            &owner.credential.secret,
            &b,
            invite_request(Some(&first.principal), rights(true, true, false)),
        )
        .unwrap();
    let second = server.redeem(&second_invitation.enrollment.secret).unwrap();
    assert_eq!(first.principal, second.principal);
    assert_ne!(first.credential.id, second.credential.id);
    server.status(&second.credential.secret, &b).unwrap();
    assert!(matches!(
        server.status(&second.credential.secret, a),
        Err(Error::Forbidden)
    ));
    server
        .revoke_member(&owner.credential.secret, a, &first.principal)
        .unwrap();
    assert_eq!(
        server.whoami(&first.credential.secret, a).unwrap().grants,
        Grants::default()
    );
    server.status(&second.credential.secret, &b).unwrap();
    server
        .admin_revoke_credential(&owner.credential.secret, &first.credential.id)
        .unwrap();
    assert!(matches!(
        server.whoami(&first.credential.secret, a),
        Err(Error::Forbidden)
    ));
    server.status(&second.credential.secret, &b).unwrap();
    let pending = server
        .invite(
            &second.credential.secret,
            &b,
            invite_request(Some(&second.principal), rights(true, false, false)),
        )
        .unwrap();
    server
        .admin_revoke_principal(&owner.credential.secret, &first.principal)
        .unwrap();
    assert!(matches!(
        server.whoami(&second.credential.secret, &b),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        server.redeem(&pending.enrollment.secret),
        Err(Error::Unauthorized)
    ));
    let credentials = server
        .list_credentials(
            &owner.credential.secret,
            &first.principal,
            AccessListRequest::default(),
        )
        .unwrap();
    assert_eq!(credentials.credentials.len(), 2);
    assert!(credentials.credentials.iter().all(|entry| entry.revoked));
}

#[test]
fn only_self_or_owner_can_enroll_an_existing_principal_without_widening_grants() {
    let root = tempfile::tempdir().unwrap();
    let (server, owner) = bootstrap(root.path());
    let collection = &owner.collection;
    let manager = server
        .invite(
            &owner.credential.secret,
            collection,
            invite_request(None, rights(true, false, true)),
        )
        .unwrap();
    let manager = server.redeem(&manager.enrollment.secret).unwrap();
    let member = server
        .invite(
            &manager.credential.secret,
            collection,
            invite_request(None, rights(true, false, false)),
        )
        .unwrap();
    let member = server.redeem(&member.enrollment.secret).unwrap();
    assert!(matches!(
        server.invite(
            &manager.credential.secret,
            collection,
            invite_request(Some(&member.principal), rights(true, false, false))
        ),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        server.invite(
            &manager.credential.secret,
            collection,
            invite_request(Some(&owner.principal), rights(true, false, false))
        ),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        server.invite(
            &manager.credential.secret,
            collection,
            invite_request(None, rights(true, true, true))
        ),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        server.manage_grants(
            &manager.credential.secret,
            &manager.principal,
            collection,
            rights(true, true, true)
        ),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        server.invite(
            &owner.credential.secret,
            collection,
            invite_request(Some(&member.principal), rights(true, true, false))
        ),
        Err(Error::Forbidden)
    ));
    assert_eq!(
        server
            .whoami(&member.credential.secret, collection)
            .unwrap()
            .grants,
        rights(true, false, false)
    );
    let same = server
        .invite(
            &member.credential.secret,
            collection,
            invite_request(Some(&member.principal), rights(true, false, false)),
        )
        .unwrap();
    let same = server.redeem(&same.enrollment.secret).unwrap();
    assert_eq!(same.principal, member.principal);
    assert!(
        !server
            .whoami(&same.credential.secret, collection)
            .unwrap()
            .server_owner
    );
    // Grant mutation is explicit and immediately reflected, including no access.
    server
        .revoke_member(&manager.credential.secret, collection, &member.principal)
        .unwrap();
    assert_eq!(
        server
            .whoami(&same.credential.secret, collection)
            .unwrap()
            .grants,
        Grants::default()
    );
    assert!(matches!(
        server.status(&same.credential.secret, collection),
        Err(Error::Forbidden)
    ));
    // A collection manager cannot turn even its own device into a root credential.
    assert!(matches!(
        server.issue_owner_credential(&manager.principal, 0),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        server.list_principals(&manager.credential.secret, AccessListRequest::default()),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        server.admin_revoke_principal(&manager.credential.secret, &member.principal),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        server.admin_revoke_credential(&manager.credential.secret, &owner.credential.id),
        Err(Error::Forbidden)
    ));
    let owner_device = server
        .invite(
            &owner.credential.secret,
            collection,
            invite_request(Some(&owner.principal), rights(true, true, true)),
        )
        .unwrap();
    let owner_device = server.redeem(&owner_device.enrollment.secret).unwrap();
    assert!(
        !server
            .whoami(&owner_device.credential.secret, collection)
            .unwrap()
            .server_owner
    );
    assert!(matches!(
        server.list_principals(
            &owner_device.credential.secret,
            AccessListRequest::default()
        ),
        Err(Error::Forbidden)
    ));
    let other_collection = server
        .create_collection("root reaches new collections")
        .unwrap();
    assert!(
        server
            .whoami(&owner.credential.secret, &other_collection)
            .unwrap()
            .server_owner
    );
    assert!(matches!(
        server.whoami(&owner_device.credential.secret, &other_collection),
        Err(Error::Forbidden)
    ));
}

#[test]
fn optional_names_are_display_only_and_enrollment_rechecks_live_grants() {
    let root = tempfile::tempdir().unwrap();
    let (server, owner) = bootstrap(root.path());
    let make = |name| {
        server
            .invite(
                &owner.credential.secret,
                &owner.collection,
                InviteRequest {
                    name,
                    ..invite_request(None, rights(true, true, false))
                },
            )
            .unwrap()
    };
    let unnamed = make(None);
    let a = make(Some("Same Name".into()));
    let b = make(Some("Same Name".into()));
    assert_ne!(a.principal, b.principal);
    assert_ne!(a.principal, unnamed.principal);
    let mut rename = invite_request(Some(&a.principal), rights(true, false, false));
    rename.name = Some("replacement".into());
    assert!(matches!(
        server.invite(&owner.credential.secret, &owner.collection, rename),
        Err(Error::Invalid(_))
    ));
    server
        .set_grants(&a.principal, &owner.collection, rights(true, false, false))
        .unwrap();
    let device = server.redeem(&a.enrollment.secret).unwrap();
    assert_eq!(device.credential.grants, rights(true, false, false));
    server
        .set_grants(&a.principal, &owner.collection, rights(true, true, false))
        .unwrap();
    assert_eq!(
        server
            .whoami(&device.credential.secret, &owner.collection)
            .unwrap()
            .grants,
        rights(true, false, false)
    );
    server
        .set_grants(&b.principal, &owner.collection, Grants::default())
        .unwrap();
    assert!(matches!(
        server.redeem(&b.enrollment.secret),
        Err(Error::Forbidden)
    ));
    let page = server
        .list_principals(&owner.credential.secret, AccessListRequest::default())
        .unwrap();
    assert_eq!(
        page.principals
            .iter()
            .find(|p| p.principal == unnamed.principal)
            .unwrap()
            .name,
        None
    );
    assert_eq!(
        page.principals
            .iter()
            .filter(|p| p.name.as_deref() == Some("Same Name"))
            .count(),
        2
    );
    let mut json = serde_json::to_value(&device).unwrap();
    json.as_object_mut().unwrap().remove("enrollment_id");
    assert!(serde_json::from_value::<TokenFile>(json)
        .unwrap()
        .enrollment_id
        .is_none());
}

async fn request(
    app: &axum::Router,
    method: &str,
    path: &str,
    token: &str,
) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn owner_only_http_inventory_is_paginated_and_never_exposes_secrets() {
    let root = tempfile::tempdir().unwrap();
    let (server, owner) = bootstrap(root.path());
    let invite = server
        .invite(
            &owner.credential.secret,
            &owner.collection,
            invite_request(None, rights(true, true, true)),
        )
        .unwrap();
    let member = server.redeem(&invite.enrollment.secret).unwrap();
    let extra = server
        .issue_credential(
            &member.principal,
            &owner.collection,
            rights(true, false, false),
            0,
        )
        .unwrap();
    let app = router(Arc::new(server));
    let token = &owner.credential.secret;
    let (status, first) = request(&app, "GET", "/v1/principals?limit=1", token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["principals"].as_array().unwrap().len(), 1);
    let next = first["next_cursor"].as_str().unwrap();
    let (status, last) = request(
        &app,
        "GET",
        &format!("/v1/principals?limit=1&after={next}"),
        token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(last["next_cursor"].is_null());
    assert_ne!(
        first["principals"][0]["principal"],
        last["principals"][0]["principal"]
    );
    let path = format!("/v1/principals/{}/credentials?limit=1", member.principal);
    let (status, devices) = request(&app, "GET", &path, token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(devices["credentials"].as_array().unwrap().len(), 1);
    let next = devices["next_cursor"].as_str().unwrap();
    let (_, tail) = request(&app, "GET", &format!("{path}&after={next}"), token).await;
    assert!(tail["next_cursor"].is_null());
    assert_ne!(
        devices["credentials"][0]["credential_id"],
        tail["credentials"][0]["credential_id"]
    );
    for json in [&first, &last, &devices, &tail] {
        let text = json.to_string();
        assert!(!text.contains("digest"));
        assert!(!text.contains("secret"));
        assert!(!text.contains(token));
        assert!(!text.contains(&member.credential.secret));
    }
    for path in ["/v1/principals?limit=0", "/v1/principals?limit=101"] {
        assert_eq!(
            request(&app, "GET", path, token).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    for path in ["/v1/principals".to_owned(), path] {
        assert_eq!(
            request(&app, "GET", &path, &member.credential.secret)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    let who = format!("/v1/collections/{}/whoami", owner.collection);
    let (status, identity) = request(&app, "GET", &who, &member.credential.secret).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(identity["principal"], member.principal);
    assert_eq!(identity["credential_id"], member.credential.id);
    assert_eq!(identity["enrollment_id"], invite.enrollment.id);
    assert_eq!(identity["server_owner"], false);
    let revoke = format!("/v1/credentials/{}/revoke", member.credential.id);
    assert_eq!(
        request(&app, "POST", &revoke, &member.credential.secret)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, "POST", &revoke, token).await.0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", &who, &member.credential.secret)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, "GET", &who, &extra.secret).await.0,
        StatusCode::OK
    );
    let revoke = format!("/v1/principals/{}/revoke", member.principal);
    assert_eq!(
        request(&app, "POST", &revoke, token).await.0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", &who, &extra.secret).await.0,
        StatusCode::FORBIDDEN
    );
}

#[test]
fn lost_exchange_response_is_recovered_by_revoking_only_the_associated_credential() {
    let root = tempfile::tempdir().unwrap();
    let (server, owner) = bootstrap(root.path());
    let first = server
        .invite(
            &owner.credential.secret,
            &owner.collection,
            invite_request(None, rights(true, false, false)),
        )
        .unwrap();
    let first = server.redeem(&first.enrollment.secret).unwrap();
    let lost = server
        .invite(
            &owner.credential.secret,
            &owner.collection,
            invite_request(Some(&first.principal), rights(true, false, false)),
        )
        .unwrap();
    // Committed exchange whose response was not retained by the client.
    drop(server.redeem(&lost.enrollment.secret).unwrap());
    assert!(matches!(
        server.redeem(&lost.enrollment.secret),
        Err(Error::Unauthorized)
    ));
    let page = server
        .list_credentials(
            &owner.credential.secret,
            &first.principal,
            AccessListRequest::default(),
        )
        .unwrap();
    let orphan = page
        .credentials
        .iter()
        .find(|credential| credential.enrollment_id.as_deref() == Some(lost.enrollment.id.as_str()))
        .unwrap();
    assert_ne!(orphan.credential_id, first.credential.id);
    server
        .admin_revoke_credential(&owner.credential.secret, &orphan.credential_id)
        .unwrap();
    server
        .status(&first.credential.secret, &owner.collection)
        .unwrap();
    let replacement = server
        .invite(
            &owner.credential.secret,
            &owner.collection,
            invite_request(Some(&first.principal), rights(true, false, false)),
        )
        .unwrap();
    let replacement = server.redeem(&replacement.enrollment.secret).unwrap();
    assert_eq!(replacement.principal, first.principal);
    server
        .status(&replacement.credential.secret, &owner.collection)
        .unwrap();
    let page = server
        .list_credentials(
            &owner.credential.secret,
            &first.principal,
            AccessListRequest::default(),
        )
        .unwrap();
    assert_eq!(
        page.credentials
            .iter()
            .filter(|credential| credential.revoked)
            .count(),
        1
    );
    assert_eq!(
        page.credentials
            .iter()
            .filter(|credential| !credential.revoked)
            .count(),
        2
    );
}
