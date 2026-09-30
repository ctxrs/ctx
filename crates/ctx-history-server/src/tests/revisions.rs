use super::*;
use rusqlite::params;

#[test]
fn content_reversion_reuses_citations_and_rebuilds_every_accepted_prefix() {
    let root = tempfile::tempdir().unwrap();
    let a = fixture(&root.path().join("a"), "reversion", &["original almond"]);
    let b = fixture(&root.path().join("b"), "reversion", &["corrected walnut"]);
    let (server, admin) = bootstrap(root.path());
    let first = stage(
        &server,
        &admin.credential.secret,
        &admin.collection,
        &a,
        "pub",
        None,
        "a1",
    );
    server
        .publish(&admin.credential.secret, &admin.collection, first.clone())
        .unwrap();
    server.index_pending(&admin.collection, 1).unwrap();
    let old_a = find(
        &server,
        &admin.credential.secret,
        &admin.collection,
        "almond",
    )
    .remove(0);
    let second = stage(
        &server,
        &admin.credential.secret,
        &admin.collection,
        &b,
        "pub",
        Some(a.member.sha256.clone()),
        "b2",
    );
    server
        .publish(&admin.credential.secret, &admin.collection, second.clone())
        .unwrap();
    server.index_pending(&admin.collection, 1).unwrap();
    let old_b = find(
        &server,
        &admin.credential.secret,
        &admin.collection,
        "walnut",
    )
    .remove(0);
    let third = stage(
        &server,
        &admin.credential.secret,
        &admin.collection,
        &a,
        "pub",
        Some(b.member.sha256.clone()),
        "a3",
    );
    assert_eq!(
        server
            .publish(&admin.credential.secret, &admin.collection, third.clone())
            .unwrap()
            .sequence,
        3
    );
    server.index_pending(&admin.collection, 1).unwrap();
    let restored_a = find(
        &server,
        &admin.credential.secret,
        &admin.collection,
        "almond",
    )
    .remove(0);
    assert_eq!(restored_a.citation, old_a.citation);
    assert!(find(
        &server,
        &admin.credential.secret,
        &admin.collection,
        "walnut"
    )
    .is_empty());
    assert_eq!(
        server
            .read_event(&admin.credential.secret, &admin.collection, &old_b.citation)
            .unwrap()
            .record
            .content
            .meaningful_text(),
        "corrected walnut"
    );
    assert_eq!(
        server
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM revisions", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        server
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM event_refs", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        2
    );
    // Historical retries acknowledge their original transition and never
    // activate its bytes again, even after the content has cycled.
    assert_eq!(
        server
            .publish(&admin.credential.secret, &admin.collection, second)
            .unwrap()
            .sequence,
        2
    );
    assert_eq!(
        server
            .status(&admin.credential.secret, &admin.collection)
            .unwrap()
            .stored_sequence,
        3
    );
    let mut stale = third;
    stale.operation.idempotency_key = "stale-predecessor".into();
    assert!(matches!(
        server.publish(&admin.credential.secret, &admin.collection, stale),
        Err(Error::Conflict)
    ));
    drop(server);

    let server = HistoryServer::open(ServerConfig::new(root.path().join("server"))).unwrap();
    assert_eq!(
        server
            .publication_state(&admin.credential.secret, &admin.collection, "pub")
            .unwrap()
            .sequence,
        3
    );
    server.rebuild_collection(&admin.collection).unwrap();
    for expected in 1..=3 {
        let status = server.index_pending(&admin.collection, 1).unwrap();
        assert_eq!(status.searchable_sequence, expected);
        assert_eq!(status.reads_available, expected == 3);
    }
    assert_eq!(
        find(
            &server,
            &admin.credential.secret,
            &admin.collection,
            "almond"
        )[0]
        .citation,
        old_a.citation
    );
    server.rebuild_collection(&admin.collection).unwrap();
    assert_eq!(
        server
            .index_pending(&admin.collection, 16)
            .unwrap()
            .searchable_sequence,
        3
    );
    assert_eq!(
        find(
            &server,
            &admin.credential.secret,
            &admin.collection,
            "almond"
        )[0]
        .citation,
        old_a.citation
    );
    server
        .withdraw(
            &admin.credential.secret,
            &admin.collection,
            WithdrawRequest {
                operation: Operation {
                    idempotency_key: "withdraw".into(),
                    publication: "pub".into(),
                    writer_epoch: 1,
                    policy_revision: 1,
                    expected_revision: Some(a.member.sha256.clone()),
                    expected_sequence: Some(3),
                    revision: "gone".into(),
                },
            },
        )
        .unwrap();
    let rejected = stage(
        &server,
        &admin.credential.secret,
        &admin.collection,
        &b,
        "pub",
        Some("gone".into()),
        "revive",
    );
    assert!(matches!(
        server.publish(&admin.credential.secret, &admin.collection, rejected),
        Err(Error::Conflict)
    ));
    assert_eq!(
        server
            .publish(&admin.credential.secret, &admin.collection, first)
            .unwrap()
            .sequence,
        1
    );
    server.index_pending(&admin.collection, 16).unwrap();
    assert!(matches!(
        server.read_event(&admin.credential.secret, &admin.collection, &old_a.citation),
        Err(Error::NotFound)
    ));
}

#[test]
fn append_backlog_reports_incomplete_without_closing_the_safe_generation() {
    let root = tempfile::tempdir().unwrap();
    let a = fixture(&root.path().join("a"), "first", &["searchable olive"]);
    let b = fixture(&root.path().join("b"), "next", &["pending pistachio"]);
    let (server, admin) = bootstrap(root.path());
    let first = stage(
        &server,
        &admin.credential.secret,
        &admin.collection,
        &a,
        "a",
        None,
        "a",
    );
    server
        .publish(&admin.credential.secret, &admin.collection, first)
        .unwrap();
    server.index_pending(&admin.collection, 16).unwrap();
    let second = stage(
        &server,
        &admin.credential.secret,
        &admin.collection,
        &b,
        "b",
        None,
        "b",
    );
    server
        .publish(&admin.credential.secret, &admin.collection, second)
        .unwrap();
    let query = SearchRequest {
        q: "pistachio".into(),
        limit: 20,
    };
    let pending = server
        .search(&admin.credential.secret, &admin.collection, query.clone())
        .unwrap();
    assert_eq!(
        (
            pending.status.stored_sequence,
            pending.status.searchable_sequence
        ),
        (2, 1)
    );
    assert!(pending.status.reads_available);
    assert!(pending.results.is_empty());
    assert!(!pending.complete && !pending.exhaustive);
    assert_eq!(
        find(
            &server,
            &admin.credential.secret,
            &admin.collection,
            "olive"
        )
        .len(),
        1
    );
    server.index_pending(&admin.collection, 16).unwrap();
    let complete = server
        .search(&admin.credential.secret, &admin.collection, query)
        .unwrap();
    assert!(complete.complete && complete.exhaustive);
    assert_eq!(complete.results[0].snippet, "pending pistachio");
}

#[test]
fn failed_collection_does_not_block_other_collections_or_skip_its_own_hole() {
    let root = tempfile::tempdir().unwrap();
    let input = fixture(
        &root.path().join("input"),
        "isolation",
        &["available cashew"],
    );
    let (server, admin) = bootstrap(root.path());
    let other = server.create_collection("other").unwrap();
    server
        .set_grants(
            &admin.principal,
            &other,
            Grants {
                read: true,
                publish: true,
                manage: true,
            },
        )
        .unwrap();
    let mut collections = [admin.collection.clone(), other];
    collections.sort();
    for collection in &collections {
        let request = stage(
            &server,
            &admin.credential.secret,
            collection,
            &input,
            "pub",
            None,
            "first",
        );
        server
            .publish(&admin.credential.secret, collection, request)
            .unwrap();
    }
    let bad_payload = server
        .collection_root(&collections[0])
        .join("payloads")
        .join(&input.member.sha256);
    fs::remove_file(&bad_payload).unwrap();
    let mut cursor = String::new();
    assert!(matches!(
        server.index_sweep(&mut cursor),
        Err(Error::Unavailable)
    ));
    assert_eq!(
        server
            .status(&admin.credential.secret, &collections[0])
            .unwrap()
            .searchable_sequence,
        0
    );
    assert_eq!(
        find(&server, &admin.credential.secret, &collections[1], "cashew").len(),
        1
    );
    assert_eq!(
        server
            .status(&admin.credential.secret, &collections[1])
            .unwrap()
            .searchable_sequence,
        1
    );
    // Explicitly repair the synthetic immutable input; the original receipt
    // remains authoritative and the original pending operation is retried.
    fs::write(bad_payload, &input.bytes).unwrap();
    server.index_sweep(&mut cursor).unwrap();
    assert_eq!(
        find(&server, &admin.credential.secret, &collections[0], "cashew").len(),
        1
    );
    assert_eq!(
        server
            .receipt(&admin.credential.secret, &collections[0], "first")
            .unwrap()
            .sequence,
        1
    );
}

#[test]
fn index_failure_page_does_not_become_a_collection_lifetime_cap() {
    let root = tempfile::tempdir().unwrap();
    let (server, _) = bootstrap(root.path());
    // Missing receipt closure fails before any Core build. Sixty-five small
    // catalog entries exercise scheduler paging without sixty-five indexes.
    let mut ids = Vec::new();
    for i in 0..65 {
        ids.push(
            server
                .create_collection(&format!("collection-{i}"))
                .unwrap(),
        );
    }
    ids.sort();
    {
        let mut connection = server.lock().unwrap();
        let tx = connection.transaction().unwrap();
        for id in &ids {
            tx.execute("UPDATE collections SET sequence=1 WHERE id=?1", [id])
                .unwrap();
            tx.execute("INSERT INTO pending VALUES (?1,1)", [id])
                .unwrap();
        }
        // The final collection has no accepted work, so reconciliation can
        // safely discard a leftover pending marker without constructing Core.
        tx.execute("UPDATE collections SET sequence=0 WHERE id=?1", [&ids[64]])
            .unwrap();
        tx.execute(
            "UPDATE pending SET sequence=0 WHERE collection=?1",
            [&ids[64]],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let mut cursor = String::new();
    assert!(server.index_sweep(&mut cursor).is_err());
    assert_eq!(cursor, ids[63]);
    server.index_sweep(&mut cursor).unwrap();
    assert!(cursor.is_empty());
    let remaining: u64 = server
        .lock()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM pending WHERE collection=?1",
            params![ids[64]],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0);
}
