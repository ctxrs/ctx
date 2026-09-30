use super::*;
use crate::{auth::authorize_at, types::now};

fn read_publish() -> Grants {
    Grants {
        read: true,
        publish: true,
        manage: false,
    }
}

#[test]
fn default_operator_and_device_credentials_survive_ninety_days_but_revoke_immediately() {
    let root = tempfile::tempdir().unwrap();
    let input = fixture(root.path(), "durable-device", &["ongoing quince"]);
    let (server, owner) = bootstrap(root.path());
    assert_eq!(owner.credential.expires_at, 0);
    let request: InviteRequest = serde_json::from_value(serde_json::json!({
        "name": "ongoing device",
        "grants": {"read": true, "publish": true, "manage": false},
        "enrollment_ttl_seconds": 600
    }))
    .unwrap();
    assert_eq!(request.credential_ttl_seconds, 0);
    let invite = server
        .invite(&owner.credential.secret, &owner.collection, request)
        .unwrap();
    assert!(invite.enrollment.expires_at > now().unwrap());
    let device = server.redeem(&invite.enrollment.secret).unwrap();
    assert_eq!(device.credential.expires_at, 0);
    assert!(matches!(
        server.redeem(&invite.enrollment.secret),
        Err(Error::Unauthorized)
    ));
    // Exercise the same authorizer with time a full year later, independently
    // of the zero-expiry representation asserted above.
    let later = now().unwrap() + 366 * 24 * 3600;
    {
        let connection = server.lock().unwrap();
        assert_eq!(
            authorize_at(
                &connection,
                &owner.credential.secret,
                &owner.collection,
                Access::Manage,
                later
            )
            .unwrap(),
            owner.principal
        );
        assert_eq!(
            authorize_at(
                &connection,
                &device.credential.secret,
                &owner.collection,
                Access::Publish,
                later
            )
            .unwrap(),
            device.principal
        );
        assert!(matches!(
            authorize_at(
                &connection,
                &device.credential.secret,
                &owner.collection,
                Access::Manage,
                later
            ),
            Err(Error::Forbidden)
        ));
    }
    let request = stage(
        &server,
        &device.credential.secret,
        &owner.collection,
        &input,
        "pub",
        None,
        "ongoing",
    );
    server
        .publish(&device.credential.secret, &owner.collection, request)
        .unwrap();
    server.revoke_credential(&device.credential.id).unwrap();
    assert!(matches!(
        server.status(&device.credential.secret, &owner.collection),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        authorize_at(
            &server.lock().unwrap(),
            &device.credential.secret,
            &owner.collection,
            Access::Publish,
            later
        ),
        Err(Error::Forbidden)
    ));
    // The remaining operator credential still administers the collection.
    let replacement = server
        .issue_enrollment(&device.principal, &owner.collection, read_publish(), 600, 0)
        .unwrap();
    let replacement = server.redeem(&replacement.secret).unwrap();
    assert_eq!(replacement.principal, device.principal);
    assert_eq!(replacement.credential.expires_at, 0);
    server
        .status(&replacement.credential.secret, &owner.collection)
        .unwrap();
    server
        .revoke_member(
            &owner.credential.secret,
            &owner.collection,
            &device.principal,
        )
        .unwrap();
    assert!(matches!(
        server.status(&replacement.credential.secret, &owner.collection),
        Err(Error::Forbidden)
    ));
    server.revoke_principal(&owner.principal).unwrap();
    assert!(matches!(
        authorize_at(
            &server.lock().unwrap(),
            &owner.credential.secret,
            &owner.collection,
            Access::Manage,
            later
        ),
        Err(Error::Forbidden)
    ));
}

#[test]
fn explicitly_expiring_credentials_and_short_lived_enrollments_still_expire() {
    let root = tempfile::tempdir().unwrap();
    let (server, owner) = bootstrap(root.path());
    let finite = server
        .issue_credential(&owner.principal, &owner.collection, read_publish(), 3600)
        .unwrap();
    assert!(finite.expires_at > now().unwrap());
    {
        let connection = server.lock().unwrap();
        assert_eq!(
            authorize_at(
                &connection,
                &finite.secret,
                &owner.collection,
                Access::Publish,
                finite.expires_at - 1
            )
            .unwrap(),
            owner.principal
        );
        assert!(matches!(
            authorize_at(
                &connection,
                &finite.secret,
                &owner.collection,
                Access::Publish,
                finite.expires_at
            ),
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            authorize_at(
                &connection,
                &finite.secret,
                &owner.collection,
                Access::Publish,
                finite.expires_at + 1
            ),
            Err(Error::Forbidden)
        ));
    }
    assert!(matches!(
        server.issue_credential(
            &owner.principal,
            &owner.collection,
            read_publish(),
            90 * 24 * 3600 + 1
        ),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        server.issue_enrollment(&owner.principal, &owner.collection, read_publish(), 0, 0),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        server.issue_enrollment(&owner.principal, &owner.collection, read_publish(), 3601, 0),
        Err(Error::Invalid(_))
    ));
    let enrollment = server
        .issue_enrollment(
            &owner.principal,
            &owner.collection,
            read_publish(),
            600,
            3600,
        )
        .unwrap();
    let device = server.redeem(&enrollment.secret).unwrap();
    assert!(device.credential.expires_at > now().unwrap());
    server
        .lock()
        .unwrap()
        .execute(
            "UPDATE credentials SET expires=1 WHERE id=?1",
            [&device.credential.id],
        )
        .unwrap();
    assert!(matches!(
        server.status(&device.credential.secret, &owner.collection),
        Err(Error::Forbidden)
    ));
    let expired = server
        .issue_enrollment(&owner.principal, &owner.collection, read_publish(), 600, 0)
        .unwrap();
    server
        .lock()
        .unwrap()
        .execute(
            "UPDATE enrollments SET expires=1 WHERE id=?1",
            [&expired.id],
        )
        .unwrap();
    assert!(matches!(
        server.redeem(&expired.secret),
        Err(Error::Unauthorized)
    ));
    let invalid = InviteRequest {
        principal: None,
        name: Some("invalid lifetime".into()),
        grants: read_publish(),
        enrollment_ttl_seconds: 0,
        credential_ttl_seconds: 0,
    };
    assert!(matches!(
        server.invite(&owner.credential.secret, &owner.collection, invalid),
        Err(Error::Invalid(_))
    ));
    // Expired finite credentials do not affect the ordinary durable operator.
    server
        .status(&owner.credential.secret, &owner.collection)
        .unwrap();
}
