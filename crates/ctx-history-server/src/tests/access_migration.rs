use super::*;
use rusqlite::{params, types::Value, Connection};

fn all() -> Grants {
    Grants {
        read: true,
        publish: true,
        manage: true,
    }
}
fn read_only() -> Grants {
    Grants {
        read: true,
        publish: false,
        manage: false,
    }
}

// Reproduce the actual v1 access tables, keeping independently authored history
// and every original credential digest. No installed/user catalog is touched.
fn make_v1(path: &Path) {
    let connection = Connection::open(path).unwrap();
    connection.execute_batch(
        "BEGIN;
         CREATE TABLE credentials_v1 (id TEXT PRIMARY KEY, digest BLOB NOT NULL UNIQUE,
             principal TEXT NOT NULL REFERENCES principals(id), scope INTEGER NOT NULL,
             expires INTEGER NOT NULL, revoked INTEGER NOT NULL DEFAULT 0);
         INSERT INTO credentials_v1 SELECT id,digest,principal,scope,expires,revoked FROM credentials;
         DROP TABLE credentials;
         ALTER TABLE credentials_v1 RENAME TO credentials;
         ALTER TABLE principals DROP COLUMN server_owner;
         ALTER TABLE enrollments DROP COLUMN credential_expires_ceiling;
         PRAGMA user_version=1;
         COMMIT;"
    ).unwrap();
}

fn evidence(connection: &Connection) -> Vec<Vec<Vec<Value>>> {
    [
        "publications",
        "revisions",
        "event_refs",
        "operations",
        "grants",
    ]
    .iter()
    .map(|table| {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let width = statement.column_count();
        let rows = statement
            .query_map([], |row| (0..width).map(|i| row.get(i)).collect())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        rows
    })
    .collect()
}

#[test]
fn v1_migration_preserves_history_and_reenrolls_all_unscoped_nonoperator_devices() {
    let root = tempfile::tempdir().unwrap();
    let input = fixture(
        &root.path().join("input"),
        "catalog-migration",
        &["retained kumquat"],
    );
    let (server, owner) = bootstrap(root.path());
    let a = &owner.collection;
    let b = server.create_collection("other").unwrap();
    server.set_grants(&owner.principal, &b, all()).unwrap();
    let single = server.create_principal("single collection").unwrap();
    server.set_grants(&single, a, all()).unwrap();
    let single_device = server.issue_credential(&single, a, all(), 0).unwrap();
    server.set_grants(&single, a, read_only()).unwrap();
    let multi = server.create_principal("two collections").unwrap();
    server.set_grants(&multi, a, all()).unwrap();
    server.set_grants(&multi, &b, all()).unwrap();
    let ambiguous = server.issue_credential(&multi, a, all(), 0).unwrap();
    let owner_device = server
        .issue_credential(&owner.principal, a, all(), 0)
        .unwrap();
    let enrollment = server
        .issue_enrollment(&single, a, read_only(), 600, 0)
        .unwrap();
    let publication = stage(
        &server,
        &ambiguous.secret,
        a,
        &input,
        "retained",
        None,
        "one",
    );
    server.publish(&ambiguous.secret, a, publication).unwrap();
    server.index_pending(a, 16).unwrap();
    let hit = find(&server, &owner.credential.secret, a, "kumquat").remove(0);
    let before = serde_json::to_value(
        server
            .read_event(&owner.credential.secret, a, &hit.citation)
            .unwrap(),
    )
    .unwrap();
    let before_session = serde_json::to_value(
        server
            .read_session(
                &owner.credential.secret,
                a,
                &hit.session_citation,
                SessionRequest::default(),
            )
            .unwrap(),
    )
    .unwrap();
    let retained = evidence(&server.lock().unwrap());
    let server_root = root.path().join("server");
    let payload = server_root
        .join("collections")
        .join(a)
        .join("payloads")
        .join(&input.member.sha256);
    let original_bytes = fs::read(&payload).unwrap();
    // The library bootstrap path is intentionally outside the root. Reproduce
    // the CLI's real protected pointer to verify custom operator paths too.
    write_token_file(
        &server_root.join("operator-file.json"),
        &root.path().join("device.json"),
    )
    .unwrap();
    drop(server);
    make_v1(&server_root.join("authority.sqlite"));
    let server = HistoryServer::open(ServerConfig::new(&server_root)).unwrap();
    assert_eq!(
        server
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        2
    );
    assert_eq!(retained, evidence(&server.lock().unwrap()));
    assert_eq!(fs::read(&payload).unwrap(), original_bytes);
    let after = serde_json::to_value(
        server
            .read_event(&owner.credential.secret, a, &hit.citation)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        before_session,
        serde_json::to_value(
            server
                .read_session(
                    &owner.credential.secret,
                    a,
                    &hit.session_citation,
                    SessionRequest::default()
                )
                .unwrap()
        )
        .unwrap()
    );
    assert!(
        server
            .whoami(&owner.credential.secret, &b)
            .unwrap()
            .server_owner
    );
    assert!(matches!(
        server.whoami(&ambiguous.secret, a),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        server.whoami(&owner_device.secret, a),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        server.redeem(&enrollment.secret),
        Err(Error::Unauthorized)
    ));
    // Even one current membership cannot prove a historical token audience.
    assert!(matches!(
        server.whoami(&single_device.secret, a),
        Err(Error::Forbidden)
    ));
    server.set_grants(&single, a, all()).unwrap();
    server.set_grants(&single, &b, all()).unwrap();
    assert!(matches!(
        server.status(&single_device.secret, a),
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        server.status(&single_device.secret, &b),
        Err(Error::Forbidden)
    ));
    // The same principal retains publication identity and obtains a new scoped
    // device through the ordinary authenticated owner enrollment path.
    let invite = server
        .invite(
            &owner.credential.secret,
            a,
            InviteRequest {
                principal: Some(multi.clone()),
                name: None,
                grants: all(),
                enrollment_ttl_seconds: 600,
                credential_ttl_seconds: 0,
            },
        )
        .unwrap();
    let device = server.redeem(&invite.enrollment.secret).unwrap();
    assert_eq!(device.principal, multi);
    assert_eq!(
        before,
        serde_json::to_value(
            server
                .read_event(&device.credential.secret, a, &hit.citation)
                .unwrap()
        )
        .unwrap()
    );
    assert!(matches!(
        server.status(&device.credential.secret, &b),
        Err(Error::Forbidden)
    ));
    drop(server);
    let reopened = HistoryServer::open(ServerConfig::new(server_root)).unwrap();
    assert!(
        reopened
            .whoami(&owner.credential.secret, &b)
            .unwrap()
            .server_owner
    );
    reopened.status(&device.credential.secret, a).unwrap();
}

#[test]
fn migration_never_promotes_manage_grants_or_unproven_operator_descriptors() {
    for mode in [
        "missing",
        "wrong-principal",
        "wrong-secret",
        "no-audit",
        "expired",
        "revoked",
    ] {
        let root = tempfile::tempdir().unwrap();
        let (server, owner) = bootstrap(root.path());
        let principal = server.create_principal("manager").unwrap();
        server
            .set_grants(&principal, &owner.collection, all())
            .unwrap();
        let mut ordinary = server
            .issue_credential(&principal, &owner.collection, all(), 0)
            .unwrap();
        let ordinary_secret = ordinary.secret.clone();
        let server_root = root.path().join("server");
        if mode == "wrong-principal" {
            write_token_file(
                &server_root.join("operator.json"),
                &TokenFile {
                    principal: principal.clone(),
                    collection: owner.collection.clone(),
                    credential: ordinary,
                    enrollment_id: None,
                },
            )
            .unwrap();
        } else if mode == "wrong-secret" {
            ordinary.id = owner.credential.id.clone();
            write_token_file(
                &server_root.join("operator.json"),
                &TokenFile {
                    principal: owner.principal.clone(),
                    collection: owner.collection.clone(),
                    credential: ordinary,
                    enrollment_id: None,
                },
            )
            .unwrap();
        } else if mode == "no-audit" {
            server
                .lock()
                .unwrap()
                .execute("DELETE FROM audit WHERE action='bootstrap'", [])
                .unwrap();
            write_token_file(&server_root.join("operator.json"), &owner).unwrap();
        }
        if mode == "expired" || mode == "revoked" {
            write_token_file(&server_root.join("operator.json"), &owner).unwrap();
            let update = if mode == "expired" {
                "UPDATE credentials SET expires=1 WHERE id=?1"
            } else {
                "UPDATE credentials SET revoked=1 WHERE id=?1"
            };
            server
                .lock()
                .unwrap()
                .execute(update, [&owner.credential.id])
                .unwrap();
        }
        drop(server);
        make_v1(&server_root.join("authority.sqlite"));
        let server = HistoryServer::open(ServerConfig::new(server_root)).unwrap();
        assert!(matches!(
            server.whoami(&ordinary_secret, &owner.collection),
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            server.whoami(&owner.credential.secret, &owner.collection),
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            server.list_principals(&ordinary_secret, AccessListRequest::default()),
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            server.issue_owner_credential(&principal, 0),
            Err(Error::Forbidden)
        ));
        if mode == "no-audit" {
            assert!(matches!(
                server.issue_owner_credential(&owner.principal, 0),
                Err(Error::Forbidden)
            ));
        } else {
            // Exclusive local operator can replace lost owner credentials only
            // for the principal established by the original bootstrap audit.
            let recovered = server.issue_owner_credential(&owner.principal, 0).unwrap();
            assert!(
                server
                    .whoami(&recovered.secret, &owner.collection)
                    .unwrap()
                    .server_owner
            );
        }
    }
}

#[test]
fn checkpoints_with_v1_and_v2_catalogs_restore_exact_citations_with_a_fresh_owner() {
    let root = tempfile::tempdir().unwrap();
    let input = fixture(
        &root.path().join("input"),
        "checkpoint-migration",
        &["saved quince"],
    );
    let (server, owner) = bootstrap(root.path());
    let request = stage(
        &server,
        &owner.credential.secret,
        &owner.collection,
        &input,
        "pub",
        None,
        "saved",
    );
    server
        .publish(&owner.credential.secret, &owner.collection, request)
        .unwrap();
    server.index_pending(&owner.collection, 16).unwrap();
    let hit = find(
        &server,
        &owner.credential.secret,
        &owner.collection,
        "quince",
    )
    .remove(0);
    let before = serde_json::to_value(
        server
            .read_event(&owner.credential.secret, &owner.collection, &hit.citation)
            .unwrap(),
    )
    .unwrap();
    let second = server.create_collection("second").unwrap();
    for version in [1, 2] {
        let checkpoint = root.path().join(format!("checkpoint-{version}"));
        server.checkpoint(&checkpoint).unwrap();
        if version == 1 {
            make_v1(&checkpoint.join("authority.sqlite"));
            let path = checkpoint.join("checkpoint.json");
            let mut info: CheckpointInfo =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            info.catalog_sha256 = hex::encode(Sha256::digest(
                fs::read(checkpoint.join("authority.sqlite")).unwrap(),
            ));
            fs::write(path, serde_json::to_vec(&info).unwrap()).unwrap();
        }
        let destination = root.path().join(format!("restored-{version}"));
        let tokens = destination.join("operator.json");
        let restored_info =
            HistoryServer::restore_checkpoint(&checkpoint, &destination, &tokens).unwrap();
        assert_ne!(restored_info.principal, owner.principal);
        let fresh: TokenFile = serde_json::from_slice(&fs::read(tokens).unwrap()).unwrap();
        let restored = HistoryServer::open(ServerConfig::new(&destination)).unwrap();
        assert_eq!(
            restored
                .lock()
                .unwrap()
                .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
                .unwrap(),
            2
        );
        assert!(
            restored
                .whoami(&fresh.credential.secret, &second)
                .unwrap()
                .server_owner
        );
        assert!(matches!(
            restored.whoami(&owner.credential.secret, &owner.collection),
            Err(Error::Forbidden)
        ));
        assert!(matches!(
            restored.issue_owner_credential(&owner.principal, 0),
            Err(Error::Unauthorized)
        ));
        restored.index_pending(&owner.collection, 16).unwrap();
        assert_eq!(
            before,
            serde_json::to_value(
                restored
                    .read_event(&fresh.credential.secret, &owner.collection, &hit.citation)
                    .unwrap()
            )
            .unwrap()
        );
        let owner_flags: u64 = restored
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM principals WHERE server_owner=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(owner_flags, 1);
        let credential: (bool, Option<String>) = restored
            .lock()
            .unwrap()
            .query_row(
                "SELECT server_owner,collection FROM credentials WHERE id=?1",
                params![fresh.credential.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(credential, (true, None));
        // A root restored by v1 proves ownership through its latest restore
        // audit and protected default operator file, not the old bootstrap user.
        drop(restored);
        make_v1(&destination.join("authority.sqlite"));
        let reopened = HistoryServer::open(ServerConfig::new(&destination)).unwrap();
        assert!(
            reopened
                .whoami(&fresh.credential.secret, &second)
                .unwrap()
                .server_owner
        );
        assert!(matches!(
            reopened.whoami(&owner.credential.secret, &owner.collection),
            Err(Error::Forbidden)
        ));
    }
}

#[test]
fn migration_rolls_back_on_failure_and_rejects_future_versions() {
    let root = tempfile::tempdir().unwrap();
    let (server, _) = bootstrap(root.path());
    drop(server);
    let config = ServerConfig::new(root.path().join("server"));
    let database = config.root.join("authority.sqlite");
    make_v1(&database);
    let connection = Connection::open(&database).unwrap();
    connection.execute_batch("CREATE TRIGGER deny_migration BEFORE INSERT ON audit BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
    assert!(matches!(
        HistoryServer::open(config.clone()),
        Err(Error::Sql(_))
    ));
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        1
    );
    assert!(connection
        .prepare("SELECT server_owner FROM principals")
        .is_err());
    connection
        .execute_batch("DROP TRIGGER deny_migration; PRAGMA user_version=3;")
        .unwrap();
    assert!(matches!(
        HistoryServer::open(config),
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        3
    );
}
