use super::*;

#[test]
fn active_wal_scan_and_hydration_are_read_only_and_recover_committed_content() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("active-wal.sqlite");
    create_opencode_session_message_database(&path, &["before WAL"]);

    let writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute_batch("pragma wal_checkpoint(truncate)")
        .unwrap();
    let body = long_body("committed active WAL body");
    writer
        .execute(
            "update session_message
             set data = ?1, time_updated = time_updated + 1
             where id = 'message-0'",
            [json!({"role": "user", "text": body}).to_string()],
        )
        .unwrap();
    let before = sqlite_persistent_bytes(&path);
    assert!(before
        .iter()
        .any(|(path, _)| path.to_string_lossy().ends_with("-wal")));

    let registration = opencode::opencode_source_backed_registration();
    let documents = collect_opencode_documents(registration, &path);
    assert_eq!(documents[0].body, body);
    let hydrated = registration
        .exact_resolver(&path)
        .hydrate_event(&event_request(&documents[0]))
        .unwrap();
    assert_eq!(hydrated.provider_bytes, body.as_bytes());
    assert_eq!(sqlite_persistent_bytes(&path), before);
    drop(writer);
}

#[test]
fn typed_native_key_row_version_and_digest_are_all_verified() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("typed-evidence.sqlite");
    let body = long_body("typed evidence body");
    create_opencode_session_message_database(&path, &[&body]);
    let registration = opencode::opencode_source_backed_registration();
    let documents = collect_opencode_documents(registration, &path);
    let document = &documents[0];
    let resolver = registration.exact_resolver(&path);
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = document.locator.coordinate()
    else {
        panic!("expected provider SQLite locator")
    };

    let wrong_key = locator_with_evidence(
        document,
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: logical_relation.clone(),
            primary_key: TypedKey::utf8("missing-message").unwrap(),
            row_version: row_version.clone(),
        },
        *document.locator.record_digest(),
    );
    let failure = resolver
        .hydrate_event(&EventHydrationRequest::new(document.event_id, wrong_key).unwrap())
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::MissingRecord);

    let wrong_version = locator_with_evidence(
        document,
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: logical_relation.clone(),
            primary_key: primary_key.clone(),
            row_version: Some(
                TypedKey::composite(vec![
                    TypedKey::I64(99),
                    TypedKey::utf8("wrong-semantic-digest").unwrap(),
                ])
                .unwrap(),
            ),
        },
        *document.locator.record_digest(),
    );
    let failure = resolver
        .hydrate_event(&EventHydrationRequest::new(document.event_id, wrong_version).unwrap())
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleRecordEvidence);

    let wrong_digest =
        locator_with_evidence(document, document.locator.coordinate().clone(), [0x5a; 32]);
    let failure = resolver
        .hydrate_event(&EventHydrationRequest::new(document.event_id, wrong_digest).unwrap())
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleRecordEvidence);
}

#[test]
fn mutation_and_concurrent_leaf_replacement_fail_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("mutable.sqlite");
    let moved = temp.path().join("moved.sqlite");
    let replacement = temp.path().join("replacement.sqlite");
    let body = long_body("source before replacement");
    let attacker = long_body("attacker replacement must not hydrate");
    create_opencode_session_message_database(&path, &[&body]);
    create_opencode_session_message_database(&replacement, &[&attacker]);

    let registration = opencode::opencode_source_backed_registration();
    let documents = collect_opencode_documents(registration, &path);
    let request = event_request(&documents[0]);
    let resolver = registration.exact_resolver(&path);
    let start = std::sync::Arc::new(std::sync::Barrier::new(2));
    let replacement_thread = std::thread::spawn({
        let path = path.clone();
        let start = std::sync::Arc::clone(&start);
        move || {
            start.wait();
            fs::rename(&path, moved).unwrap();
            fs::rename(replacement, path).unwrap();
        }
    });

    start.wait();
    match resolver.hydrate_event(&request) {
        Ok(hydrated) => assert_eq!(hydrated.provider_bytes, body.as_bytes()),
        Err(failure) => {
            assert!(matches!(
                failure.kind,
                HydrationFailureKind::StaleSourceEvidence
                    | HydrationFailureKind::TemporarilyUnavailable
            ));
            assert!(!failure.detail.contains(&attacker));
        }
    }
    replacement_thread.join().unwrap();

    let failure = resolver.hydrate_event(&request).unwrap_err();
    assert!(matches!(
        failure.kind,
        HydrationFailureKind::StaleSourceEvidence | HydrationFailureKind::TemporarilyUnavailable
    ));
    assert!(!failure.detail.contains(&attacker));
}

#[cfg(unix)]
#[test]
fn symlinked_leaf_and_ancestor_routes_are_rejected_before_projection() {
    use std::os::unix::fs::symlink;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let real_parent = temp.path().join("real-parent");
    fs::create_dir(&real_parent).unwrap();
    let path = real_parent.join("source.sqlite");
    create_opencode_session_message_database(&path, &["inside source root"]);
    let registration = opencode::opencode_source_backed_registration();

    let leaf_link = temp.path().join("leaf-link.sqlite");
    symlink(&path, &leaf_link).unwrap();
    assert!(registration.scan(&leaf_link, &mut |_| Ok(())).is_err());

    let parent_link = temp.path().join("parent-link");
    symlink(&real_parent, &parent_link).unwrap();
    assert!(registration
        .scan(&parent_link.join("source.sqlite"), &mut |_| Ok(()))
        .is_err());
}
