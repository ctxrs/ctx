use ctx_history_platform::platform_security::{create_private_file_new, ensure_private_directory};

use super::*;

fn private_file(path: &Path, contents: &[u8]) {
    let mut file = create_private_file_new(path).unwrap();
    file.write_all(contents).unwrap();
    file.sync_all().unwrap();
}

#[test]
fn interrupted_initialization_retries_at_each_marker_boundary() {
    let temp = tempdir().unwrap();
    let archive = archive(
        temp.path(),
        &[record(&source(), "session", 1, "recoverable")],
    );
    let complete = br#"{"archive_root_version":1}"#.as_slice();
    // Model the durable files remaining after interruption: before lock, after
    // lock, during marker staging, after staging sync, and after atomic rename.
    let states = [
        (false, None),
        (true, None),
        (true, Some(("archive-root.json.tmp", b"".as_slice()))),
        (
            true,
            Some(("archive-root.json.tmp", b"{\"archive_root_".as_slice())),
        ),
        (true, Some(("archive-root.json.tmp", complete))),
        (true, Some(("archive-root.json", complete))),
    ];
    for (number, (lock, marker)) in states.into_iter().enumerate() {
        let root = temp.path().join(format!("interrupted-{number}"));
        ensure_private_directory(&root).unwrap();
        if lock {
            private_file(&root.join("restore.lock"), b"");
        }
        if let Some((path, contents)) = marker {
            private_file(&root.join(path), contents);
        }
        let receipt = restore(&archive, &root, &options()).unwrap();
        assert_eq!(receipt.imported_members, 1);
        assert_eq!(
            search_count(
                &VerifiedIndex::open_pinned(root.join("search/lexical")).unwrap(),
                "recoverable"
            ),
            1
        );
        let marker: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("archive-root.json")).unwrap()).unwrap();
        assert_eq!(marker, serde_json::json!({"archive_root_version": 1}));
        assert!(!root.join("archive-root.json.tmp").exists());
        let retry = restore(&archive, &root, &options()).unwrap();
        assert_eq!(retry.generation_id, receipt.generation_id);
        assert_eq!(retry.imported_members, 0);
        assert_eq!(retry.unchanged_members, 1);
    }
}

#[test]
fn initialization_retry_is_locked_and_does_not_claim_unrelated_data() {
    let temp = tempdir().unwrap();
    let archive = archive(temp.path(), &[record(&source(), "session", 1, "retained")]);
    let root = temp.path().join("busy");
    ensure_private_directory(&root).unwrap();
    let lock = create_private_file_new(&root.join("restore.lock")).unwrap();
    fs2::FileExt::try_lock_exclusive(&lock).unwrap();
    private_file(&root.join("archive-root.json.tmp"), b"incomplete");
    assert!(restore(&archive, &root, &options()).is_err());
    assert!(!root.join("archive-root.json").exists());
    assert_eq!(
        fs::read(root.join("archive-root.json.tmp")).unwrap(),
        b"incomplete"
    );
    drop(lock);
    restore(&archive, &root, &options()).unwrap();

    for (name, marker) in [
        ("future", Some(br#"{"archive_root_version":2}"#.as_slice())),
        ("malformed-final", Some(b"{\"archive_root_".as_slice())),
        ("unrelated", None),
    ] {
        let root = temp.path().join(name);
        ensure_private_directory(&root).unwrap();
        if let Some(contents) = marker {
            private_file(&root.join("archive-root.json"), contents);
        } else {
            private_file(&root.join("archive-root.json.tmp"), b"partial");
            private_file(&root.join("unrelated-history"), b"preserve");
        }
        assert!(restore(&archive, &root, &options()).is_err());
        assert!(!root.join("restore.lock").exists());
        assert!(!root.join("archives").exists());
        if let Some(contents) = marker {
            assert_eq!(fs::read(root.join("archive-root.json")).unwrap(), contents);
        } else {
            assert_eq!(
                fs::read(root.join("unrelated-history")).unwrap(),
                b"preserve"
            );
            assert_eq!(
                fs::read(root.join("archive-root.json.tmp")).unwrap(),
                b"partial"
            );
        }
    }
}

#[test]
fn many_small_sessions_replay_add_and_correct_without_changing_other_members() {
    let temp = tempdir().unwrap();
    let source = source();
    let mut records: Vec<_> = (0..257)
        .map(|number| record(&source, &format!("session-{number}"), 1, "original body"))
        .collect();
    let archive = archive(temp.path(), &records);
    let root = temp.path().join("restore");
    let first = restore(&archive, &root, &options()).unwrap();
    assert_eq!(first.imported_members, 257);
    let retry = restore(&archive, &root, &options()).unwrap();
    assert_eq!(retry.snapshot_id, first.snapshot_id);
    assert_eq!(retry.generation_id, first.generation_id);
    assert_eq!(retry.imported_members, 0);
    assert_eq!(retry.unchanged_members, 257);

    let changed_session = records[128].session_id;
    let predecessor = members(&archive)
        .into_iter()
        .find(|m| m.session_id == changed_session)
        .unwrap();
    records[128].content.normalized_body = Some("corrected body".into());
    let added = record(&source, "new session", 1, "added body");
    records.push(added.clone());
    publish(&temp.path().join("producer-index"), &records, 2);
    let selection = Selection {
        sources: Default::default(),
        sessions: [
            hex(&changed_session.digest()),
            hex(&added.session_id.digest()),
        ]
        .into_iter()
        .collect(),
    };
    let update = temp.path().join("update");
    export(
        &VerifiedIndex::open_pinned(temp.path().join("producer-index")).unwrap(),
        &update,
        identity(),
        &selection,
    )
    .unwrap();
    assert!(matches!(
        restore(&update, &root, &options()),
        Err(ArchiveError::Conflict { .. })
    ));
    assert_eq!(
        VerifiedIndex::open_pinned(root.join("search/lexical"))
            .unwrap()
            .generation_id(),
        first.generation_id
    );
    let mut authorized = options();
    authorized
        .expected_predecessors
        .insert(predecessor.path, predecessor.sha256);
    let corrected = restore(&update, &root, &authorized).unwrap();
    assert_eq!(corrected.imported_members, 2);
    assert_eq!(corrected.unchanged_members, 0);
    let retry = restore(&update, &root, &authorized).unwrap();
    assert_eq!(retry.generation_id, corrected.generation_id);
    assert_eq!(retry.imported_members, 0);
    assert_eq!(retry.unchanged_members, 2);
    let index = VerifiedIndex::open_pinned(root.join("search/lexical")).unwrap();
    assert_eq!(index.document_count(), 258);
    for original in records {
        let id = mapped_event(&authorized.binding, original.session_id, original.event_id).unwrap();
        let actual = index.core_record_by_id(id.as_uuid()).unwrap().unwrap();
        assert_eq!(actual.content, original.content);
    }
}
