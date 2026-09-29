use super::*;
use std::{
    sync::{mpsc, Mutex},
    thread,
    time::Duration,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    Validation,
    Projection,
}

type Hook = Arc<dyn Fn(Stage) + Send + Sync>;

#[derive(Default)]
pub(crate) struct Hooks(Mutex<Option<Hook>>);

impl Hooks {
    pub(crate) fn run(&self, stage: Stage) {
        let hook = self.0.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(stage);
        }
    }
}

// A one-shot pause inside the actual validator/indexer replaces a slow large
// input. Bounded channels prove authority progresses without timing a fixture.
pub(super) struct Pause {
    entered: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
}

impl Pause {
    pub(super) fn at(server: &HistoryServer, point: Stage) -> Self {
        let (arrived, entered) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let resume = Mutex::new(Some(resume));
        *server.hooks.0.lock().unwrap() = Some(Arc::new(move |stage| {
            if stage == point {
                let receiver = resume.lock().unwrap().take();
                if let Some(receiver) = receiver {
                    arrived.send(()).unwrap();
                    receiver.recv_timeout(Duration::from_secs(30)).unwrap();
                }
            }
        }));
        Self { entered, release }
    }

    pub(super) fn wait(&self) {
        self.entered.recv_timeout(Duration::from_secs(10)).unwrap();
    }
}

impl Drop for Pause {
    fn drop(&mut self) {
        let _ = self.release.send(());
    }
}

fn all_grants() -> Grants {
    Grants {
        read: true,
        publish: true,
        manage: true,
    }
}

fn withdraw_request(publication: &str, revision: &str, sequence: u64) -> WithdrawRequest {
    WithdrawRequest {
        operation: Operation {
            idempotency_key: "withdraw-during-index".into(),
            publication: publication.into(),
            writer_epoch: 1,
            policy_revision: 1,
            expected_revision: Some(revision.into()),
            expected_sequence: Some(sequence),
            revision: "withdrawn".into(),
        },
    }
}

#[test]
fn validation_releases_authority_and_rechecks_revoke_grants_and_expiry() {
    let root = tempfile::tempdir().unwrap();
    let input = fixture(
        &root.path().join("input"),
        "late-auth",
        &["private nectarine"],
    );
    for action in ["credential", "grants", "expiry"] {
        let case = root.path().join(action);
        fs::create_dir(&case).unwrap();
        let (server, admin) = bootstrap(&case);
        let server = Arc::new(server);
        let other = server.create_collection("readable neighbor").unwrap();
        server
            .set_grants(&admin.principal, &other, all_grants())
            .unwrap();
        let seed = stage(
            &server,
            &admin.credential.secret,
            &other,
            &input,
            "neighbor",
            None,
            "seed",
        );
        server
            .publish(&admin.credential.secret, &other, seed)
            .unwrap();
        server.index_pending(&other, 16).unwrap();
        let publisher = server.create_principal("publisher").unwrap();
        server
            .set_grants(
                &publisher,
                &admin.collection,
                Grants {
                    read: true,
                    publish: true,
                    manage: false,
                },
            )
            .unwrap();
        let device = server
            .issue_credential(
                &publisher,
                &admin.collection,
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
            &device.secret,
            &admin.collection,
            &input,
            "pending",
            None,
            "pending",
        );
        let pause = Pause::at(&server, Stage::Validation);
        let publishing = {
            let server = server.clone();
            let token = device.secret.clone();
            let collection = admin.collection.clone();
            thread::spawn(move || server.publish(&token, &collection, request))
        };
        pause.wait();
        let (sent, received) = mpsc::channel();
        let authority = {
            let server = server.clone();
            let token = admin.credential.secret.clone();
            let collection = admin.collection.clone();
            thread::spawn(move || {
                let found = find(&server, &token, &other, "nectarine");
                match action {
                    "credential" => server.revoke_credential(&device.id).unwrap(),
                    "grants" => server
                        .set_grants(
                            &publisher,
                            &collection,
                            Grants {
                                read: true,
                                publish: false,
                                manage: false,
                            },
                        )
                        .unwrap(),
                    "expiry" => {
                        server
                            .lock()
                            .unwrap()
                            .execute("UPDATE credentials SET expires=1 WHERE id=?1", [&device.id])
                            .unwrap();
                    }
                    _ => unreachable!(),
                }
                sent.send(found[0].snippet.clone()).unwrap();
            })
        };
        let progress = received.recv_timeout(Duration::from_secs(10));
        drop(pause);
        authority.join().unwrap();
        assert_eq!(progress.unwrap(), "private nectarine");
        assert!(matches!(publishing.join().unwrap(), Err(Error::Forbidden)));
        assert_eq!(
            server
                .status(&admin.credential.secret, &admin.collection)
                .unwrap()
                .stored_sequence,
            0
        );
        assert!(!server
            .collection_root(&admin.collection)
            .join("payloads")
            .join(&input.member.sha256)
            .exists());
    }
}

#[test]
fn validation_rechecks_a_competing_predecessor_and_identical_retry() {
    let root = tempfile::tempdir().unwrap();
    let a = fixture(&root.path().join("a"), "race", &["first coconut"]);
    let b = fixture(&root.path().join("b"), "race", &["second papaya"]);
    let c = fixture(&root.path().join("c"), "race", &["winning guava"]);
    let (server, admin) = bootstrap(root.path());
    let server = Arc::new(server);
    let first = stage(
        &server,
        &admin.credential.secret,
        &admin.collection,
        &a,
        "pub",
        None,
        "first",
    );
    server
        .publish(&admin.credential.secret, &admin.collection, first)
        .unwrap();
    let losing = stage(
        &server,
        &admin.credential.secret,
        &admin.collection,
        &b,
        "pub",
        Some(a.member.sha256.clone()),
        "losing",
    );
    let winner = stage(
        &server,
        &admin.credential.secret,
        &admin.collection,
        &c,
        "pub",
        Some(a.member.sha256.clone()),
        "winner",
    );
    let pause = Pause::at(&server, Stage::Validation);
    let publishing = {
        let server = server.clone();
        let token = admin.credential.secret.clone();
        let collection = admin.collection.clone();
        thread::spawn(move || server.publish(&token, &collection, losing))
    };
    pause.wait();
    let accepted = server
        .publish(&admin.credential.secret, &admin.collection, winner)
        .unwrap();
    drop(pause);
    assert_eq!(accepted.sequence, 2);
    assert!(matches!(publishing.join().unwrap(), Err(Error::Conflict)));
    assert!(!server
        .collection_root(&admin.collection)
        .join("payloads")
        .join(&b.member.sha256)
        .exists());

    let same = stage(
        &server,
        &admin.credential.secret,
        &admin.collection,
        &b,
        "pub",
        Some(c.member.sha256.clone()),
        "same",
    );
    let pause = Pause::at(&server, Stage::Validation);
    let publishing = {
        let server = server.clone();
        let token = admin.credential.secret.clone();
        let collection = admin.collection.clone();
        let same = same.clone();
        thread::spawn(move || server.publish(&token, &collection, same))
    };
    pause.wait();
    let accepted = server
        .publish(&admin.credential.secret, &admin.collection, same)
        .unwrap();
    drop(pause);
    let retry = publishing.join().unwrap().unwrap();
    assert_eq!(accepted.sequence, 3);
    assert_eq!(
        serde_json::to_value(accepted).unwrap(),
        serde_json::to_value(retry).unwrap()
    );
}

#[test]
fn indexing_releases_authority_and_cannot_reopen_a_withdrawn_prefix() {
    let root = tempfile::tempdir().unwrap();
    let a = fixture(&root.path().join("a"), "index-race", &["old fig"]);
    let b = fixture(&root.path().join("b"), "index-race", &["new date"]);
    let (server, admin) = bootstrap(root.path());
    let server = Arc::new(server);
    let other = server.create_collection("neighbor").unwrap();
    server
        .set_grants(&admin.principal, &other, all_grants())
        .unwrap();
    for collection in [&admin.collection, &other] {
        let seed = stage(
            &server,
            &admin.credential.secret,
            collection,
            &a,
            "pub",
            None,
            "seed",
        );
        server
            .publish(&admin.credential.secret, collection, seed)
            .unwrap();
        server.index_pending(collection, 16).unwrap();
    }
    let old = find(&server, &admin.credential.secret, &admin.collection, "fig").remove(0);
    let correction = stage(
        &server,
        &admin.credential.secret,
        &admin.collection,
        &b,
        "pub",
        Some(a.member.sha256.clone()),
        "correction",
    );
    server
        .publish(&admin.credential.secret, &admin.collection, correction)
        .unwrap();
    let device = server
        .issue_credential(&admin.principal, &admin.collection, all_grants(), 3600)
        .unwrap();
    let pause = Pause::at(&server, Stage::Projection);
    let indexing = {
        let server = server.clone();
        let collection = admin.collection.clone();
        thread::spawn(move || server.index_pending(&collection, 16))
    };
    pause.wait();
    let (sent, received) = mpsc::channel();
    let authority = {
        let server = server.clone();
        let token = admin.credential.secret.clone();
        let collection = admin.collection.clone();
        let revision = b.member.sha256.clone();
        thread::spawn(move || {
            assert_eq!(find(&server, &token, &other, "fig").len(), 1);
            server.revoke_credential(&device.id).unwrap();
            let receipt = server
                .withdraw(&token, &collection, withdraw_request("pub", &revision, 2))
                .unwrap();
            sent.send(receipt.sequence).unwrap();
        })
    };
    let progress = received.recv_timeout(Duration::from_secs(10));
    drop(pause);
    authority.join().unwrap();
    assert_eq!(progress.unwrap(), 3);
    let status = indexing.join().unwrap().unwrap();
    assert_eq!((status.searchable_sequence, status.stored_sequence), (2, 3));
    assert!(!status.reads_available);
    assert!(matches!(
        server.read_event(&admin.credential.secret, &admin.collection, &old.citation),
        Err(Error::Unavailable)
    ));
    assert!(
        server
            .index_pending(&admin.collection, 16)
            .unwrap()
            .reads_available
    );
    assert!(matches!(
        server.read_event(&admin.credential.secret, &admin.collection, &old.citation),
        Err(Error::NotFound)
    ));
    assert!(find(&server, &admin.credential.secret, &admin.collection, "date").is_empty());
}

#[test]
fn malformed_members_never_leave_retained_payloads_and_restart_discards_scratch() {
    let root = tempfile::tempdir().unwrap();
    let input = fixture(
        &root.path().join("input"),
        "validation",
        &["valid mulberry"],
    );
    let (server, admin) = bootstrap(root.path());
    for i in 0..3 {
        let bytes = format!("not a Core record {i}\n").into_bytes();
        let mut member = input.member.clone();
        member.sha256 = hex::encode(Sha256::digest(&bytes));
        member.bytes = bytes.len() as u64;
        let malformed = Fixture {
            identity: input.identity.clone(),
            member,
            bytes,
        };
        let request = stage(
            &server,
            &admin.credential.secret,
            &admin.collection,
            &malformed,
            "pub",
            None,
            &format!("bad-{i}"),
        );
        let upload = request.upload.clone();
        assert!(matches!(
            server.publish(&admin.credential.secret, &admin.collection, request),
            Err(Error::Archive(_))
        ));
        server
            .lock()
            .unwrap()
            .execute("UPDATE uploads SET expires=0 WHERE id=?1", [upload])
            .unwrap();
    }
    let request = stage(
        &server,
        &admin.credential.secret,
        &admin.collection,
        &input,
        "pub",
        None,
        "valid",
    );
    let collection_root = server.collection_root(&admin.collection);
    assert_eq!(
        fs::read_dir(collection_root.join("payloads"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(
        server
            .status(&admin.credential.secret, &admin.collection)
            .unwrap()
            .stored_sequence,
        0
    );
    server
        .publish(&admin.credential.secret, &admin.collection, request)
        .unwrap();
    assert!(collection_root
        .join("payloads")
        .join(&input.member.sha256)
        .is_file());
    let abandoned = collection_root.join("staging/.validate-interrupted");
    fs::create_dir(&abandoned).unwrap();
    fs::write(abandoned.join("references.sqlite"), b"incomplete scratch").unwrap();
    drop(server);
    let server = HistoryServer::open(ServerConfig::new(root.path().join("server"))).unwrap();
    assert!(!abandoned.exists());
    server.index_pending(&admin.collection, 16).unwrap();
    assert_eq!(
        find(
            &server,
            &admin.credential.secret,
            &admin.collection,
            "mulberry"
        )[0]
        .snippet,
        "valid mulberry"
    );
}

#[test]
fn final_transaction_checks_expiry_and_cleans_only_uncommitted_promotion() {
    let root = tempfile::tempdir().unwrap();
    let input = fixture(
        &root.path().join("input"),
        "final-expiry",
        &["fresh gooseberry"],
    );
    let (server, admin) = bootstrap(root.path());
    let request = stage(
        &server,
        &admin.credential.secret,
        &admin.collection,
        &input,
        "pub",
        None,
        "final-expiry",
    );
    // This deterministic expiry occurs after the final lock has been acquired
    // and its initial admission check has passed, during the metadata merge.
    server
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TEMP TRIGGER expire_during_merge AFTER INSERT ON main.event_refs
         BEGIN UPDATE credentials SET expires=1; END;",
        )
        .unwrap();
    assert!(matches!(
        server.publish(&admin.credential.secret, &admin.collection, request.clone()),
        Err(Error::Forbidden)
    ));
    let retained = server
        .collection_root(&admin.collection)
        .join("payloads")
        .join(&input.member.sha256);
    assert!(!retained.exists());
    assert_eq!(
        server
            .status(&admin.credential.secret, &admin.collection)
            .unwrap()
            .stored_sequence,
        0
    );
    assert!(matches!(
        server.receipt(&admin.credential.secret, &admin.collection, "final-expiry"),
        Err(Error::NotFound)
    ));
    server
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER expire_during_merge;")
        .unwrap();
    assert_eq!(
        server
            .publish(&admin.credential.secret, &admin.collection, request)
            .unwrap()
            .sequence,
        1
    );
    assert!(retained.is_file());
}

#[test]
fn catalog_failure_after_promotion_preserves_existing_payloads_and_allows_retry() {
    let root = tempfile::tempdir().unwrap();
    let a = fixture(&root.path().join("a"), "stable", &["stable pecan"]);
    let b = fixture(&root.path().join("b"), "new", &["new macadamia"]);
    let (server, admin) = bootstrap(root.path());
    let first = stage(
        &server,
        &admin.credential.secret,
        &admin.collection,
        &a,
        "a",
        None,
        "first",
    );
    server
        .publish(&admin.credential.secret, &admin.collection, first)
        .unwrap();
    let next = stage(
        &server,
        &admin.credential.secret,
        &admin.collection,
        &b,
        "b",
        None,
        "next",
    );
    server
        .lock()
        .unwrap()
        .execute_batch(
            "CREATE TEMP TRIGGER reject_receipt BEFORE INSERT ON main.operations
         BEGIN SELECT RAISE(ABORT,'synthetic receipt failure'); END;",
        )
        .unwrap();
    assert!(matches!(
        server.publish(&admin.credential.secret, &admin.collection, next.clone()),
        Err(Error::Sql(_))
    ));
    let payloads = server.collection_root(&admin.collection).join("payloads");
    assert!(payloads.join(&a.member.sha256).is_file());
    assert!(!payloads.join(&b.member.sha256).exists());
    assert_eq!(
        server
            .status(&admin.credential.secret, &admin.collection)
            .unwrap()
            .stored_sequence,
        1
    );
    server
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_receipt;")
        .unwrap();
    assert_eq!(
        server
            .publish(&admin.credential.secret, &admin.collection, next)
            .unwrap()
            .sequence,
        2
    );
    server.index_pending(&admin.collection, 16).unwrap();
    assert_eq!(
        find(
            &server,
            &admin.credential.secret,
            &admin.collection,
            "pecan"
        )
        .len(),
        1
    );
    assert_eq!(
        find(
            &server,
            &admin.credential.secret,
            &admin.collection,
            "macadamia"
        )
        .len(),
        1
    );
}
