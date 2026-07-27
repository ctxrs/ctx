use super::*;

#[test]
fn sqlite_result_wal_append_survives_but_addressed_mutation_fails_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("goose-wal.db");
    let writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute_batch(
            "create table sessions (id text primary key);
             create table messages (
                session_id text not null, role text not null, content_json text not null
             );
             insert into sessions values ('session');",
        )
        .unwrap();
    writer
        .execute(
            "insert into messages values ('session', 'tool', ?1)",
            [serde_json::to_string(&json!([{
                "type": "toolResponse", "result": "stable result"
            }]))
            .unwrap()],
        )
        .unwrap();
    let record = goose::goose_result_record(&writer, 1).unwrap().unwrap();
    let mut locator = vec![2];
    locator.extend_from_slice(&ordered_rowid(1));
    let request = result_request_for(
        &path,
        CaptureProvider::Goose,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        GOOSE_LOCATOR_KIND,
        locator,
        0,
        &record,
    );

    writer
        .execute(
            "insert into messages values ('session', 'tool', ?1)",
            [serde_json::to_string(&json!([{
                "type": "toolResponse", "result": "unrelated append"
            }]))
            .unwrap()],
        )
        .unwrap();
    assert_eq!(resolve_result(&request).unwrap().content, "stable result");

    let mut wrong_native = request.clone();
    wrong_native.expected_native_record_id = "other-native-id".to_owned();
    assert_eq!(
        resolve_result(&wrong_native).unwrap_err().kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );
    let mut wrong_profile = request.clone();
    wrong_profile.content_profile = "crush-sqlite.result-body.v1".to_owned();
    assert_eq!(
        resolve_result(&wrong_profile).unwrap_err().kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );

    writer
        .execute(
            "update messages set content_json = ?1 where rowid = 1",
            [serde_json::to_string(&json!([{
                "type": "toolResponse", "result": "mutated result"
            }]))
            .unwrap()],
        )
        .unwrap();
    let mut changed_request = request.clone();
    changed_request.source_access = sqlite_source_access(
        &path,
        CaptureProvider::Goose,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        changed_request.event_id,
    );
    assert_eq!(
        resolve_result(&changed_request).unwrap_err().kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );
}

#[test]
fn source_move_under_current_root_and_append_only_growth_preserve_exact_row() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let original_root = temp.path().join("original");
    let moved_root = temp.path().join("moved");
    fs::create_dir(&original_root).unwrap();
    fs::create_dir(&moved_root).unwrap();
    let original = original_root.join("chat_history.db");
    let moved = moved_root.join("chat_history.db");
    let body = long_body("moved body");
    let (values, event) = create_firebender_database(&original, &body);
    let mut request = firebender_request(&original, &body, &values, &event);
    let original_snapshot = source_snapshot(&original);

    fs::rename(&original, &moved).unwrap();
    readmit_sqlite(&mut request, &moved, original_snapshot.clone()).unwrap();
    let messages = SqliteCompleteContentResolver::new()
        .resolve(&[request.clone()])
        .unwrap();
    assert_eq!(messages[0].text, body);

    let conn = Connection::open(&moved).unwrap();
    let other_messages = serde_json::to_string(&json!([{
        "id": "unrelated",
        "role": "user",
        "content": "append"
    }]))
    .unwrap();
    conn.execute(
        "insert into chat_sessions values (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            "other",
            "Other",
            CREATED_AT,
            CREATED_AT,
            other_messages,
            "{}"
        ],
    )
    .unwrap();
    drop(conn);
    assert!(fs::metadata(&moved).unwrap().len() >= original_snapshot.size_bytes.unwrap());
    readmit_sqlite(&mut request, &moved, original_snapshot).unwrap();
    let messages = SqliteCompleteContentResolver::new()
        .resolve(&[request])
        .unwrap();
    assert_eq!(messages[0].text, body);
}
#[test]
fn wal_snapshot_reads_committed_append_without_mutating_provider_components() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("wal.db");
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "journal_mode", "wal").unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    conn.execute_batch(
        "create table chat_sessions (
            id text not null, name text not null, created_at integer not null,
            updated_at integer not null, messages_json text not null,
            metadata_json text not null
        );",
    )
    .unwrap();
    let body = long_body("WAL body");
    let message = json!({
        "id": "native-message-1", "role": "user", "timestamp": CREATED_AT,
        "content": { "type": "text", "text": body }
    });
    let messages_json = serde_json::to_string(&json!([message.clone()])).unwrap();
    conn.execute(
        "insert into chat_sessions values (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            SESSION_ID,
            "Complete content fixture",
            CREATED_AT,
            CREATED_AT + 1,
            messages_json,
            "{}"
        ],
    )
    .unwrap();
    conn.execute(
        "insert into chat_sessions values ('append', 'Append', 1, 1, '[]', '{}')",
        [],
    )
    .unwrap();
    let values = firebender_values(&messages_json);
    let event = firebender::firebender_event(
        SESSION_ID,
        0,
        &message,
        DateTime::<Utc>::from_timestamp_millis(CREATED_AT).unwrap(),
    );
    let request = firebender_request(&path, &body, &values, &event);
    let before = sqlite_components(&path);
    assert!(before
        .iter()
        .any(|(path, _)| path.to_string_lossy().ends_with("-wal")));

    let messages = SqliteCompleteContentResolver::new()
        .resolve(&[request])
        .unwrap();
    assert_eq!(messages[0].text, body);
    assert_eq!(sqlite_components(&path), before);
    drop(conn);
}

#[test]
fn rollback_journal_snapshot_never_recovers_into_provider_database() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("rollback.db");
    let body = long_body("rollback body");
    let (values, event) = create_firebender_database(&path, &body);
    let request = firebender_request(&path, &body, &values, &event);
    let writer = Connection::open(&path).unwrap();
    writer
        .pragma_update(None, "journal_mode", "delete")
        .unwrap();
    writer.execute_batch("begin immediate").unwrap();
    writer
        .execute(
            "update chat_sessions set name = 'uncommitted' where rowid = 1",
            [],
        )
        .unwrap();
    let before = sqlite_components(&path);
    assert!(before
        .iter()
        .any(|(path, _)| path.to_string_lossy().ends_with("-journal")));

    let messages = SqliteCompleteContentResolver::new()
        .resolve(&[request])
        .unwrap();
    assert_eq!(messages[0].text, body);
    assert_eq!(sqlite_components(&path), before);
    writer.execute_batch("rollback").unwrap();
}

#[test]
fn wrong_coordinates_and_digests_fail_without_plausible_content() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("wrong.db");
    let body = long_body("wrong identity body");
    let (values, event) = create_firebender_database(&path, &body);
    let request = firebender_request(&path, &body, &values, &event);

    let mut wrong_row = request.clone();
    wrong_row.source_locator =
        CompleteContentSourceLocator::new(FIREBENDER_LOCATOR_KIND, 99_i64.to_be_bytes().to_vec());
    assert_error_kind(&wrong_row, CompleteContentErrorKind::SourceRecordMissing);

    let mut wrong_kind = request.clone();
    wrong_kind.source_locator =
        CompleteContentSourceLocator::new("arbitrary-table-row-v1", 1_i64.to_be_bytes().to_vec());
    assert_error_kind(
        &wrong_kind,
        CompleteContentErrorKind::ContentVerificationFailed,
    );

    let mut wrong_native = request.clone();
    wrong_native.expected_native_record_id = Some("other-native-id".to_owned());
    assert_error_kind(
        &wrong_native,
        CompleteContentErrorKind::ContentVerificationFailed,
    );

    let mut wrong_record = request.clone();
    wrong_record.expected_record_digest = Some(CompleteContentBodyDigest::from_text("other row"));
    assert_error_kind(
        &wrong_record,
        CompleteContentErrorKind::ContentVerificationFailed,
    );

    let mut wrong_body = request.clone();
    wrong_body.expected_content_ref = ContentRef::from_bytes(b"other body");
    assert_error_kind(
        &wrong_body,
        CompleteContentErrorKind::ContentVerificationFailed,
    );

    let mut wrong_subrecord = request.clone();
    wrong_subrecord.source_record_subrecord_index = 1;
    assert_error_kind(
        &wrong_subrecord,
        CompleteContentErrorKind::SourceRecordMissing,
    );

    let mut wrong_family = request;
    wrong_family.source_family = Some(CompleteContentSourceFamily::Jsonl);
    assert_error_kind(
        &wrong_family,
        CompleteContentErrorKind::ContentVerificationFailed,
    );
}

#[test]
fn mutation_replacement_deletion_and_permission_loss_are_typed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("mutable.db");
    let body = long_body("mutable body");
    let (values, event) = create_firebender_database(&path, &body);
    let request = firebender_request(&path, &body, &values, &event);

    let changed_body = body.replacen("mutable", "mutated", 1);
    let changed_message = json!({
        "id": "native-message-1", "role": "user", "timestamp": CREATED_AT,
        "content": { "type": "text", "text": changed_body }
    });
    let changed_json = serde_json::to_string(&json!([changed_message])).unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "update chat_sessions set messages_json = ?1 where rowid = 1",
        [changed_json],
    )
    .unwrap();
    drop(conn);
    let mut request = request;
    readmit_sqlite(&mut request, &path, SourceSnapshot::default()).unwrap();
    assert_error_kind(
        &request,
        CompleteContentErrorKind::ContentVerificationFailed,
    );

    fs::remove_file(&path).unwrap();
    let error = readmit_sqlite(&mut request, &path, SourceSnapshot::default()).unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceMissing);

    let (values, event) = create_firebender_database(&path, &body);
    let mut replacement_request = firebender_request(&path, &body, &values, &event);
    let replacement_snapshot = source_snapshot(&path);
    let replacement = temp.path().join("replacement.db");
    create_firebender_database(&replacement, &body.replacen("mutable", "replaced", 1));
    fs::rename(&replacement, &path).unwrap();
    let error = readmit_sqlite(&mut replacement_request, &path, replacement_snapshot).unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o000);
        fs::set_permissions(&path, permissions).unwrap();
        let mut permission_request = replacement_request;
        let error =
            readmit_sqlite(&mut permission_request, &path, SourceSnapshot::default()).unwrap_err();
        assert_eq!(error.kind, CompleteContentErrorKind::SourceUnreadable);
    }
}

#[test]
fn symlink_schema_and_request_bounds_are_enforced_before_hydration() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("real.db");
    let body = long_body("bounded body");
    let (values, event) = create_firebender_database(&path, &body);
    let request = firebender_request(&path, &body, &values, &event);

    let mut oversized_batch = Vec::new();
    for index in 0..=MAX_SQLITE_COMPLETE_REQUESTS {
        let mut item = request.clone();
        item.event_id = Uuid::new_v4();
        item.source_record_ordinal = index as u64;
        oversized_batch.push(item);
    }
    let error = SqliteCompleteContentResolver::new()
        .resolve(&oversized_batch)
        .unwrap_err();
    assert_eq!(error.kind, CompleteContentErrorKind::ContentTooLarge);

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let link = temp.path().join("leaf-link.db");
        symlink(&path, &link).unwrap();
        let mut linked = request.clone();
        let error = readmit_sqlite(&mut linked, &link, SourceSnapshot::default()).unwrap_err();
        assert_eq!(error.kind, CompleteContentErrorKind::SourceUnreadable);

        let real_parent = temp.path().join("real-parent");
        fs::create_dir(&real_parent).unwrap();
        let parent_db = real_parent.join("nested.db");
        fs::copy(&path, &parent_db).unwrap();
        let linked_parent = temp.path().join("linked-parent");
        symlink(&real_parent, &linked_parent).unwrap();
        let mut parent_linked = request.clone();
        let error = readmit_sqlite(
            &mut parent_linked,
            &linked_parent.join("nested.db"),
            SourceSnapshot::default(),
        )
        .unwrap_err();
        assert_eq!(error.kind, CompleteContentErrorKind::SourceUnreadable);
    }

    let invalid_schema = temp.path().join("invalid-schema.db");
    Connection::open(&invalid_schema)
        .unwrap()
        .execute("create table unrelated (value text)", [])
        .unwrap();
    let mut invalid = request;
    readmit_sqlite(&mut invalid, &invalid_schema, SourceSnapshot::default()).unwrap();
    assert_error_kind(
        &invalid,
        CompleteContentErrorKind::ContentVerificationFailed,
    );
}
