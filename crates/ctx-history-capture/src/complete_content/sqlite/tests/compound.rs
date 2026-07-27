use super::*;

fn create_deepagents_tables(conn: &Connection) {
    conn.execute_batch(
        "create table checkpoints (
            thread_id text not null, checkpoint_ns text not null default '',
            checkpoint_id text not null, checkpoint blob, metadata blob,
            primary key (thread_id, checkpoint_ns, checkpoint_id)
        );
        create table writes (
            thread_id text not null, checkpoint_ns text not null default '',
            checkpoint_id text not null, task_id text not null, idx integer not null,
            channel text not null, type text, value blob,
            primary key (thread_id, checkpoint_ns, checkpoint_id, task_id, idx)
        );",
    )
    .unwrap();
}

fn deepagents_message_blob(role: &str, text: &str, message_id: &str) -> Vec<u8> {
    let message = MsgpackValue::Map(vec![
        (
            MsgpackValue::String("type".into()),
            MsgpackValue::String(role.into()),
        ),
        (
            MsgpackValue::String("content".into()),
            MsgpackValue::String(text.into()),
        ),
        (
            MsgpackValue::String("id".into()),
            MsgpackValue::String(message_id.into()),
        ),
    ]);
    let mut bytes = Vec::new();
    write_msgpack_value(&mut bytes, &MsgpackValue::Array(vec![message])).unwrap();
    bytes
}

fn insert_deepagents_checkpoint(conn: &Connection, checkpoint: &str) {
    conn.execute(
        "insert into checkpoints
         (thread_id, checkpoint_ns, checkpoint_id, checkpoint, metadata)
         values ('thread-a', '', ?1, x'00', ?2)",
        params![
            checkpoint,
            serde_json::to_vec(&json!({"updated_at": "2026-07-22T12:00:00Z"})).unwrap()
        ],
    )
    .unwrap();
}

fn insert_deepagents_write(
    conn: &Connection,
    checkpoint: &str,
    task: &str,
    idx: i64,
    role: &str,
    text: &str,
    message_id: &str,
) {
    conn.execute(
        "insert into writes
         (thread_id, checkpoint_ns, checkpoint_id, task_id, idx, channel, type, value)
         values ('thread-a', '', ?1, ?2, ?3, 'messages', 'msgpack', ?4)",
        params![
            checkpoint,
            task,
            idx,
            deepagents_message_blob(role, text, message_id)
        ],
    )
    .unwrap();
}

fn deepagents_address(
    checkpoint: &str,
    task: &str,
    idx: i64,
) -> deepagents::DeepAgentsContentAddress {
    deepagents::DeepAgentsContentAddress {
        thread_id: "thread-a".to_owned(),
        checkpoint_id: checkpoint.to_owned(),
        task_id: task.to_owned(),
        write_idx: idx,
        message_offset: 0,
    }
}

fn deepagents_message_request(
    path: &Path,
    address: &deepagents::DeepAgentsContentAddress,
    ordinal: u64,
    indexed_limit_chars: usize,
) -> CompleteMessageRequest {
    let conn = Connection::open(path).unwrap();
    let resolved = deepagents::resolve_deepagents_content(&conn, address)
        .unwrap()
        .unwrap();
    drop(conn);
    let event_id = Uuid::new_v4();
    CompleteMessageRequest {
        event_id,
        provider: CaptureProvider::DeepAgents,
        source_format: crate::DEEPAGENTS_SQLITE_SOURCE_FORMAT.to_owned(),
        source_access: sqlite_source_access(
            path,
            CaptureProvider::DeepAgents,
            crate::DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            event_id,
        ),
        source_family: Some(CompleteContentSourceFamily::Sqlite),
        content_profile: verified_content_profile(
            CaptureProvider::DeepAgents,
            crate::DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            CompleteContentSourceFamily::Sqlite,
            VerifiedContentRole::MessageBody,
        )
        .unwrap()
        .to_owned(),
        source_locator: CompleteContentSourceLocator::new(
            deepagents::DEEPAGENTS_CONTENT_LOCATOR_KIND,
            address.encode().unwrap(),
        ),
        provider_session_id: Some("thread-a".to_owned()),
        source_record_ordinal: ordinal,
        source_record_subrecord_index: 0,
        expected_provider_event_hash: resolved.event.provider_event_hash.clone().unwrap(),
        expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
        expected_native_record_id: Some(native_record_id(
            resolved.event.provider_event_index,
            resolved.event.provider_event_hash.as_deref(),
            Some(resolved.event.cursor.as_str()),
        )),
        expected_record_digest: Some(resolved.record_digest),
        expected_content_ref: ContentRef::from_bytes(resolved.text.as_bytes()),
        indexed_text: resolved.text.chars().take(indexed_limit_chars).collect(),
        indexed_limit_chars,
    }
}

fn deepagents_result_request(
    path: &Path,
    address: &deepagents::DeepAgentsContentAddress,
    ordinal: u64,
) -> ResultContentRequest {
    let conn = Connection::open(path).unwrap();
    let resolved = deepagents::resolve_deepagents_content(&conn, address)
        .unwrap()
        .unwrap();
    drop(conn);
    let event_id = Uuid::new_v4();
    ResultContentRequest {
        event_id,
        provider: CaptureProvider::DeepAgents,
        source_format: crate::DEEPAGENTS_SQLITE_SOURCE_FORMAT.to_owned(),
        source_access: sqlite_source_access(
            path,
            CaptureProvider::DeepAgents,
            crate::DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            event_id,
        ),
        source_family: CompleteContentSourceFamily::Sqlite,
        content_profile: verified_content_profile(
            CaptureProvider::DeepAgents,
            crate::DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            CompleteContentSourceFamily::Sqlite,
            VerifiedContentRole::ResultBody,
        )
        .unwrap()
        .to_owned(),
        source_locator: CompleteContentSourceLocator::new(
            deepagents::DEEPAGENTS_CONTENT_LOCATOR_KIND,
            address.encode().unwrap(),
        )
        .unwrap(),
        source_record_ordinal: ordinal,
        source_record_subrecord_index: 0,
        expected_native_record_id: native_record_id(
            resolved.event.provider_event_index,
            resolved.event.provider_event_hash.as_deref(),
            Some(resolved.event.cursor.as_str()),
        ),
        expected_record_digest: resolved.record_digest,
        expected_content_ref: ContentRef::from_bytes(resolved.text.as_bytes()).unwrap(),
    }
}

#[test]
fn deepagents_compound_addresses_recover_message_and_result_across_checkpoints() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let conn = Connection::open(&path).unwrap();
    create_deepagents_tables(&conn);
    insert_deepagents_checkpoint(&conn, "checkpoint-a");
    insert_deepagents_checkpoint(&conn, "checkpoint-b");
    let message = long_body("DeepAgents complete message");
    let result = "DeepAgents exact tool result\nwith unicode 🦀 and escapes \\\"";
    insert_deepagents_write(
        &conn,
        "checkpoint-a",
        "task-a",
        0,
        "human",
        &message,
        "message-a",
    );
    insert_deepagents_write(
        &conn,
        "checkpoint-b",
        "task-b",
        4,
        "tool",
        result,
        "result-b",
    );
    drop(conn);

    let message_request = deepagents_message_request(
        &path,
        &deepagents_address("checkpoint-a", "task-a", 0),
        0,
        PROVIDER_MAX_TEXT_CHARS,
    );
    let messages = SqliteCompleteContentResolver::new()
        .resolve(&[message_request])
        .unwrap();
    assert_eq!(messages[0].text, message);
    assert!(messages[0].verification.is_verified());

    let result_request =
        deepagents_result_request(&path, &deepagents_address("checkpoint-b", "task-b", 4), 1);
    let results = ResultContentResolver::resolve_results(
        &SqliteCompleteContentResolver::new(),
        &[result_request],
    );
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].as_ref().unwrap().content, result);
    assert!(results[0].as_ref().unwrap().verification.is_verified());
}

#[test]
fn deepagents_result_batch_is_coordinate_ordered_and_row_mutation_fails_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let conn = Connection::open(&path).unwrap();
    create_deepagents_tables(&conn);
    insert_deepagents_checkpoint(&conn, "checkpoint-a");
    insert_deepagents_write(
        &conn,
        "checkpoint-a",
        "task-a",
        0,
        "tool",
        "first exact result",
        "result-1",
    );
    insert_deepagents_write(
        &conn,
        "checkpoint-a",
        "task-a",
        1,
        "tool",
        "second exact result",
        "result-2",
    );
    drop(conn);
    let mut first =
        deepagents_result_request(&path, &deepagents_address("checkpoint-a", "task-a", 0), 0);
    let mut second =
        deepagents_result_request(&path, &deepagents_address("checkpoint-a", "task-a", 1), 1);
    second.source_access = first.source_access.clone();
    let resolved = ResultContentResolver::resolve_results(
        &SqliteCompleteContentResolver::new(),
        &[first.clone(), second.clone()],
    );
    assert_eq!(resolved[0].as_ref().unwrap().content, "first exact result");
    assert_eq!(resolved[1].as_ref().unwrap().content, "second exact result");

    let reversed = ResultContentResolver::resolve_results(
        &SqliteCompleteContentResolver::new(),
        &[second, first.clone()],
    );
    assert!(reversed.iter().all(|item| item
        .as_ref()
        .is_err_and(|error| error.kind == CompleteContentErrorKind::ContentVerificationFailed)));

    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "update writes set value = ?1
         where thread_id = 'thread-a' and checkpoint_id = 'checkpoint-a'
           and task_id = 'task-a' and idx = 0",
        [deepagents_message_blob(
            "tool",
            "mutated result",
            "result-1",
        )],
    )
    .unwrap();
    drop(conn);
    first.source_access = sqlite_source_access(
        &path,
        CaptureProvider::DeepAgents,
        crate::DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        first.event_id,
    );
    let mutated =
        ResultContentResolver::resolve_results(&SqliteCompleteContentResolver::new(), &[first]);
    assert!(mutated[0]
        .as_ref()
        .is_err_and(|error| error.kind == CompleteContentErrorKind::ContentVerificationFailed));
}

#[test]
fn deepagents_wal_snapshot_resolution_does_not_mutate_provider_components() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    create_deepagents_tables(&writer);
    insert_deepagents_checkpoint(&writer, "checkpoint-a");
    insert_deepagents_write(
        &writer,
        "checkpoint-a",
        "task-a",
        0,
        "tool",
        "WAL-backed exact result",
        "result-wal",
    );
    let request =
        deepagents_result_request(&path, &deepagents_address("checkpoint-a", "task-a", 0), 0);
    let before = sqlite_components(&path);
    assert!(before
        .iter()
        .any(|(component, _)| component.to_string_lossy().ends_with("-wal")));
    let resolved =
        ResultContentResolver::resolve_results(&SqliteCompleteContentResolver::new(), &[request]);
    assert_eq!(
        resolved[0].as_ref().unwrap().content,
        "WAL-backed exact result"
    );
    assert_eq!(sqlite_components(&path), before);
    drop(writer);
}
