use std::{
    collections::BTreeMap,
    fs,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use ctx_history_archive::ArchiveIdentity;
use serde_json::json;
use tempfile::tempdir;

use super::*;
use crate::tests::mock::{Mock, Response};
use crate::{Backfill, PublicationMode, SourceSelection};

const COLLECTION: &str = "00000000-0000-4000-8000-000000000001";

fn status_server() -> (Mock, Arc<AtomicBool>) {
    let revoked = Arc::new(AtomicBool::new(false));
    let old_revoked = revoked.clone();
    let server = Mock::new(move |request| {
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, format!("/v1/collections/{COLLECTION}/status"));
        assert!(request.body.is_empty());
        let principal = match request.authorization.as_str() {
            "Bearer original" if !old_revoked.load(Ordering::Acquire) => "publisher-one",
            "Bearer rotated" => "publisher-one",
            "Bearer other" => "publisher-two",
            "Bearer reader" => "reader-only",
            "Bearer missing-principal" => "",
            "Bearer wrong-collection" => "publisher-one",
            "Bearer unavailable" => return Response::Drop,
            _ => return Response::json(403, &json!({})),
        };
        Response::json(
            200,
            &json!({
                "collection": if request.authorization == "Bearer wrong-collection" {
                    "00000000-0000-4000-8000-000000000002"
                } else { COLLECTION },
                "principal": principal,
                "stored_sequence": 0,
                "searchable_sequence": 0,
                "generation": null,
                "reads_available": true,
                "off_host_checkpoint": null
            }),
        )
    });
    (server, revoked)
}

fn connection(server: &Mock) -> Connection {
    Connection {
        endpoint: server.endpoint(),
        collection: COLLECTION.into(),
    }
}

fn policy() -> SharingPolicy {
    SharingPolicy {
        revision: 1,
        archive_identity: ArchiveIdentity {
            origin: "synthetic-origin".into(),
            view: "synthetic-view".into(),
        },
        writer_epoch: 1,
        mode: PublicationMode::Automatic,
        sources: vec![SourceSelection {
            source_id: Some("synthetic-source".into()),
            profile_root: None,
            baseline_revisions: BTreeMap::new(),
            backfill: Backfill::All,
            include_future: true,
            whole_source: true,
            work_roots: vec![],
        }],
    }
}

// Config must leave retained work byte-identical without interpreting it.
fn retain_work(store: &SharingStore) -> Vec<(PathBuf, Vec<u8>)> {
    [
        (
            "queue/synthetic/payload",
            b"queued original bytes".as_slice(),
        ),
        (
            "receipts/synthetic.json",
            b"retained receipt bytes".as_slice(),
        ),
    ]
    .into_iter()
    .map(|(name, bytes)| {
        let path = store.root().join(name);
        create_private_directory_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        (path, bytes.to_vec())
    })
    .collect()
}

fn assert_retained(work: &[(PathBuf, Vec<u8>)]) {
    for (path, bytes) in work {
        assert_eq!(&fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn authenticated_publish_token_binds_consent_and_rotates_after_old_token_revocation() {
    let temp = tempdir().unwrap();
    let (server, revoked) = status_server();
    let store = SharingStore::new(temp.path().join("sharing"));
    let connection = connection(&server);
    store
        .connect(
            connection.clone(),
            Credentials::new(Some("reader".into()), Some("original".into())).unwrap(),
        )
        .unwrap();
    assert!(server.requests().is_empty());
    store.set_policy(policy()).unwrap();
    assert_eq!(server.requests()[0].authorization, "Bearer original");
    assert_eq!(
        store.settings().unwrap().unwrap().publisher.as_deref(),
        Some("publisher-one")
    );
    let work = retain_work(&store);
    store.pause(true).unwrap();
    revoked.store(true, Ordering::Release);

    store
        .connect(
            connection,
            Credentials::new(Some("reader".into()), Some("rotated".into())).unwrap(),
        )
        .unwrap();
    let saved = store.settings().unwrap().unwrap();
    assert_eq!(saved.credentials.publish().unwrap(), "rotated");
    assert_eq!(saved.policy, Some(policy()));
    assert!(saved.paused);
    assert_eq!(saved.publisher.as_deref(), Some("publisher-one"));
    assert_eq!(server.requests().len(), 2);
    assert_eq!(server.requests()[1].authorization, "Bearer rotated");
    assert_retained(&work);
}

#[test]
fn replacement_without_matching_authority_preserves_settings_and_retained_work() {
    let temp = tempdir().unwrap();
    let (server, _) = status_server();
    let store = SharingStore::new(temp.path().join("sharing"));
    let connection = connection(&server);
    store
        .connect(
            connection.clone(),
            Credentials::device("original".into()).unwrap(),
        )
        .unwrap();
    store.set_policy(policy()).unwrap();
    let work = retain_work(&store);
    let before = fs::read(store.root().join("settings.json")).unwrap();
    for (token, error) in [
        ("other", Error::Credentials),
        ("denied", Error::Forbidden),
        ("unavailable", Error::Unavailable),
        ("missing-principal", Error::Credentials),
        ("wrong-collection", Error::Protocol),
    ] {
        assert_eq!(
            store.connect(
                connection.clone(),
                Credentials::device(token.into()).unwrap()
            ),
            Err(error)
        );
        assert_eq!(
            fs::read(store.root().join("settings.json")).unwrap(),
            before
        );
        assert_retained(&work);
    }
}

#[test]
fn offline_narrowing_and_reader_downgrade_preserve_bound_work_without_resuming() {
    let temp = tempdir().unwrap();
    let (server, _) = status_server();
    let store = SharingStore::new(temp.path().join("sharing"));
    let connection = connection(&server);
    store
        .connect(
            connection.clone(),
            Credentials::device("original".into()).unwrap(),
        )
        .unwrap();
    store.set_policy(policy()).unwrap();
    let work = retain_work(&store);
    // The endpoint is now unavailable; local operations must still succeed.
    drop(server);
    store
        .connect(
            connection.clone(),
            Credentials::device("original".into()).unwrap(),
        )
        .unwrap();
    let mut narrowed = policy();
    narrowed.revision = 2;
    narrowed.sources[0].include_future = false;
    store.set_policy(narrowed.clone()).unwrap();
    store
        .connect(connection, Credentials::read_only("reader".into()).unwrap())
        .unwrap();
    let saved = store.settings().unwrap().unwrap();
    assert!(saved.paused);
    assert_eq!(saved.credentials.publish(), Err(Error::MissingCredential));
    assert_eq!(saved.policy, Some(narrowed));
    assert_eq!(saved.publisher.as_deref(), Some("publisher-one"));
    assert_retained(&work);
}

#[test]
fn reconnecting_a_publisher_after_reader_downgrade_keeps_explicit_pause() {
    let temp = tempdir().unwrap();
    let (server, _) = status_server();
    let store = SharingStore::new(temp.path().join("sharing"));
    let connection = connection(&server);
    store
        .connect(
            connection.clone(),
            Credentials::device("original".into()).unwrap(),
        )
        .unwrap();
    store.set_policy(policy()).unwrap();
    store
        .connect(
            connection.clone(),
            Credentials::read_only("reader".into()).unwrap(),
        )
        .unwrap();
    assert_eq!(server.requests().len(), 1);
    assert_eq!(
        store.connect(
            connection.clone(),
            Credentials::device("other".into()).unwrap()
        ),
        Err(Error::Credentials)
    );
    store
        .connect(connection, Credentials::device("rotated".into()).unwrap())
        .unwrap();
    assert!(store.settings().unwrap().unwrap().paused);
    assert_eq!(store.policy().unwrap(), Some(policy()));
}

#[test]
fn first_authorization_requires_online_identity_but_reader_connections_do_not() {
    let temp = tempdir().unwrap();
    let (server, _) = status_server();
    let store = SharingStore::new(temp.path().join("sharing"));
    let connection = connection(&server);
    store
        .connect(
            connection.clone(),
            Credentials::read_only("reader".into()).unwrap(),
        )
        .unwrap();
    assert!(!store.settings().unwrap().unwrap().paused);
    assert_eq!(store.set_policy(policy()), Err(Error::MissingCredential));
    assert!(server.requests().is_empty());
    store
        .connect(
            connection,
            Credentials::device("unavailable".into()).unwrap(),
        )
        .unwrap();
    let before = fs::read(store.root().join("settings.json")).unwrap();
    assert_eq!(store.set_policy(policy()), Err(Error::Unavailable));
    assert_eq!(
        fs::read(store.root().join("settings.json")).unwrap(),
        before
    );
    assert!(store.policy().unwrap().is_none());
}

#[test]
fn unbound_policy_cannot_be_adopted_by_replacement_or_reselection() {
    let temp = tempdir().unwrap();
    let (server, _) = status_server();
    let store = SharingStore::new(temp.path().join("sharing"));
    let connection = connection(&server);
    store
        .connect(
            connection.clone(),
            Credentials::device("original".into()).unwrap(),
        )
        .unwrap();
    let mut old = serde_json::to_value(store.settings().unwrap().unwrap()).unwrap();
    old["policy"] = serde_json::to_value(policy()).unwrap();
    old.as_object_mut().unwrap().remove("publisher");
    private_file::write(&store.root().join("settings.json"), &old).unwrap();
    let before = fs::read(store.root().join("settings.json")).unwrap();
    assert_eq!(
        store.connect(
            connection.clone(),
            Credentials::device("rotated".into()).unwrap()
        ),
        Err(Error::Credentials)
    );
    let mut next = policy();
    next.revision = 2;
    assert_eq!(store.set_policy(next), Err(Error::Credentials));
    assert_eq!(
        fs::read(store.root().join("settings.json")).unwrap(),
        before
    );
    assert!(server.requests().is_empty());
    store.remove().unwrap();
    store
        .connect(connection, Credentials::device("other".into()).unwrap())
        .unwrap();
    store.set_policy(policy()).unwrap();
    assert_eq!(
        store.settings().unwrap().unwrap().publisher.as_deref(),
        Some("publisher-two")
    );
}
