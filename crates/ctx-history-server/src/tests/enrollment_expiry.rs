use super::*;
use crate::{auth::authorize_at, types::now};

fn read() -> Grants {
    Grants {
        read: true,
        publish: false,
        manage: false,
    }
}
fn invitation(principal: &str, credential_ttl_seconds: u64) -> InviteRequest {
    InviteRequest {
        name: None,
        principal: Some(principal.into()),
        grants: read(),
        enrollment_ttl_seconds: 3600,
        credential_ttl_seconds,
    }
}

#[test]
fn finite_self_enrollment_cannot_create_durable_access_or_extend_after_delay() {
    let root = tempfile::tempdir().unwrap();
    let (server, owner) = bootstrap(root.path());
    let principal = server.create_principal("finite member").unwrap();
    server
        .set_grants(&principal, &owner.collection, read())
        .unwrap();
    let issuer = server
        .issue_credential(&principal, &owner.collection, read(), 3600)
        .unwrap();
    let invite = server
        .invite(&issuer.secret, &owner.collection, invitation(&principal, 0))
        .unwrap();
    assert_eq!(invite.enrollment.expires_at, issuer.expires_at);
    let child = server.redeem(&invite.enrollment.secret).unwrap();
    assert_eq!(child.credential.expires_at, issuer.expires_at);
    assert_ne!(child.credential.expires_at, 0);
    let delayed = server
        .invite(
            &issuer.secret,
            &owner.collection,
            invitation(&principal, 3600),
        )
        .unwrap();
    assert_eq!(delayed.enrollment.expires_at, issuer.expires_at);
    // Advance the elapsed portion of these persisted absolute lifetimes without
    // sleeping. The requested TTL stays 3600: exchange must not restart it past
    // the issuer deadline, even though 1800 seconds of that window remain.
    let remaining_deadline = now().unwrap() + 1800;
    server
        .lock()
        .unwrap()
        .execute(
            "UPDATE enrollments SET expires=?1,credential_expires_ceiling=?1 WHERE id=?2",
            rusqlite::params![remaining_deadline, delayed.enrollment.id],
        )
        .unwrap();
    server
        .lock()
        .unwrap()
        .execute(
            "UPDATE credentials SET expires=?1 WHERE id=?2",
            rusqlite::params![remaining_deadline, issuer.id],
        )
        .unwrap();
    let delayed = server.redeem(&delayed.enrollment.secret).unwrap();
    assert_eq!(delayed.credential.expires_at, remaining_deadline);
    assert!(authorize_at(
        &server.lock().unwrap(),
        &delayed.credential.secret,
        &owner.collection,
        Access::Read,
        remaining_deadline - 1
    )
    .is_ok());
    assert!(matches!(
        authorize_at(
            &server.lock().unwrap(),
            &delayed.credential.secret,
            &owner.collection,
            Access::Read,
            remaining_deadline
        ),
        Err(Error::Forbidden)
    ));
    let expired = server
        .invite(&issuer.secret, &owner.collection, invitation(&principal, 0))
        .unwrap();
    server
        .lock()
        .unwrap()
        .execute(
            "UPDATE enrollments SET expires=1 WHERE id=?1",
            [&expired.enrollment.id],
        )
        .unwrap();
    assert!(matches!(
        server.redeem(&expired.enrollment.secret),
        Err(Error::Unauthorized)
    ));
    // Per-device revocation remains independent; no credential ancestry is kept.
    server.revoke_credential(&issuer.id).unwrap();
    server
        .status(&child.credential.secret, &owner.collection)
        .unwrap();
    server
        .status(&delayed.credential.secret, &owner.collection)
        .unwrap();
}

#[test]
fn durable_self_enrollment_and_explicit_owner_renewal_keep_requested_lifetimes() {
    let root = tempfile::tempdir().unwrap();
    let (server, owner) = bootstrap(root.path());
    let principal = server.create_principal("renewed member").unwrap();
    server
        .set_grants(&principal, &owner.collection, read())
        .unwrap();
    let durable = server
        .issue_credential(&principal, &owner.collection, read(), 0)
        .unwrap();
    let invite = server
        .invite(
            &durable.secret,
            &owner.collection,
            invitation(&principal, 0),
        )
        .unwrap();
    assert_ne!(invite.enrollment.expires_at, 0);
    let child = server.redeem(&invite.enrollment.secret).unwrap();
    assert_eq!(child.credential.expires_at, 0);
    let finite = server
        .invite(
            &durable.secret,
            &owner.collection,
            invitation(&principal, 600),
        )
        .unwrap();
    let before = now().unwrap();
    let finite = server.redeem(&finite.enrollment.secret).unwrap();
    assert!(finite.credential.expires_at >= before + 600);
    assert!(finite.credential.expires_at <= now().unwrap() + 600);
    let owner_device = server
        .issue_owner_credential(&owner.principal, 600)
        .unwrap();
    let renewal = server
        .invite(
            &owner_device.secret,
            &owner.collection,
            invitation(&principal, 0),
        )
        .unwrap();
    assert!(renewal.enrollment.expires_at > owner_device.expires_at);
    let renewal = server.redeem(&renewal.enrollment.secret).unwrap();
    assert_eq!(renewal.credential.expires_at, 0);
    assert!(
        !server
            .whoami(&renewal.credential.secret, &owner.collection)
            .unwrap()
            .server_owner
    );
}
