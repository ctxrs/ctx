use ctx_history_core::{AgentType, LocatorRevisionPolicy, NativeRecordCoordinate, TypedKey};
use rmpv::{encode::write_value as write_msgpack_value, Value as MsgpackValue};
use rusqlite::{params, Connection};

use super::*;

fn create_database(path: &Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "create table checkpoints (
            thread_id text not null,
            checkpoint_ns text not null default '',
            checkpoint_id text not null,
            parent_checkpoint_id text,
            type text,
            checkpoint blob,
            metadata blob,
            primary key (thread_id, checkpoint_ns, checkpoint_id)
        );
        create table writes (
            thread_id text not null,
            checkpoint_ns text not null default '',
            checkpoint_id text not null,
            task_id text not null,
            idx integer not null,
            channel text not null,
            type text,
            value blob,
            primary key (thread_id, checkpoint_ns, checkpoint_id, task_id, idx)
        );",
    )
    .unwrap();
    conn
}

#[cfg(target_os = "linux")]
#[test]
fn source_backed_open_does_not_follow_leaf_swap_after_authorization() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let attacker = temp.path().join("attacker.db");
    let original = temp.path().join("original.db");
    let expected = create_database(&path);
    expected.pragma_update(None, "user_version", 41).unwrap();
    drop(expected);
    let attacker_database = create_database(&attacker);
    attacker_database
        .pragma_update(None, "user_version", 99)
        .unwrap();
    drop(attacker_database);

    let result = open_root_authorized_snapshot_with_hook(
        crate::test_provider_sqlite_data_root(),
        &path,
        || {
            fs::rename(&path, &original).unwrap();
            fs::rename(&attacker, &path).unwrap();
        },
    );
    assert!(matches!(
        result,
        Err(DeepAgentsSourceBackedErrorV0::SqliteSource(
            SqliteSourceAccessError::SourceChanged,
        ))
    ));
}

#[test]
fn active_wal_scan_reads_latest_rows_without_persistent_source_writes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let writer = create_database(&path);
    writer.pragma_update(None, "journal_mode", "wal").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute_batch("pragma wal_checkpoint(truncate)")
        .unwrap();
    insert_checkpoint(&writer, b"opaque checkpoint state");
    insert_write(
        &writer,
        "task-a",
        0,
        "messages",
        Some("msgpack"),
        &message_blob(vec![message(
            "human",
            "DeepAgents active WAL sentinel",
            "message-wal",
        )]),
    );
    let before = sqlite_persistent_bytes(&path);
    let (documents, _, _) = scan(DeepAgentsDatabaseSelectionV0::explicit(
        crate::test_provider_sqlite_data_root(),
        &path,
    ));
    assert!(documents
        .iter()
        .any(|document| document.body.contains("DeepAgents active WAL sentinel")));
    assert_eq!(sqlite_persistent_bytes(&path), before);
    drop(writer);
}

fn insert_checkpoint(conn: &Connection, checkpoint_state: &[u8]) {
    let metadata = serde_json::to_vec(&serde_json::json!({
        "updated_at": "2026-07-28T20:00:00Z",
        "cwd": "/workspace/deepagents",
        "agent_name": "deepagents-test",
        "git_branch": "feature/source-backed",
    }))
    .unwrap();
    conn.execute(
        "insert into checkpoints
         (thread_id, checkpoint_ns, checkpoint_id, checkpoint, metadata)
         values ('thread-a', '', 'checkpoint-a', ?1, ?2)",
        params![checkpoint_state, metadata],
    )
    .unwrap();
}

fn message(role: &str, text: &str, id: &str) -> MsgpackValue {
    MsgpackValue::Map(vec![
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
            MsgpackValue::String(id.into()),
        ),
    ])
}

fn message_blob(messages: Vec<MsgpackValue>) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_msgpack_value(&mut bytes, &MsgpackValue::Array(messages)).unwrap();
    bytes
}

fn insert_write(
    conn: &Connection,
    task_id: &str,
    idx: i64,
    channel: &str,
    value_type: Option<&str>,
    value: &[u8],
) {
    conn.execute(
        "insert into writes
         (thread_id, checkpoint_ns, checkpoint_id, task_id, idx, channel, type, value)
         values ('thread-a', '', 'checkpoint-a', ?1, ?2, ?3, ?4, ?5)",
        params![task_id, idx, channel, value_type, value],
    )
    .unwrap();
}

fn replace_messages(conn: &Connection, messages: Vec<MsgpackValue>) {
    conn.execute(
        "update writes set value = ?1
         where thread_id = 'thread-a' and checkpoint_ns = ''
           and checkpoint_id = 'checkpoint-a' and task_id = 'task-a' and idx = 0",
        [message_blob(messages)],
    )
    .unwrap();
}

fn sqlite_persistent_bytes(path: &Path) -> Vec<Vec<u8>> {
    // Stock WAL readers may update volatile SHM reader marks.
    ["", "-wal"]
        .into_iter()
        .map(|suffix| {
            let mut component = path.as_os_str().to_os_string();
            component.push(suffix);
            fs::read(PathBuf::from(component)).unwrap()
        })
        .collect()
}

fn scan(
    selection: DeepAgentsDatabaseSelectionV0,
) -> (
    Vec<LexicalDocument>,
    DeepAgentsSourceBackedScanV0,
    Vec<usize>,
) {
    let mut scanner =
        DeepAgentsSourceBackedScannerV0::open(selection, DateTime::<Utc>::UNIX_EPOCH).unwrap();
    let mut documents = Vec::new();
    let mut page_lengths = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        page_lengths.push(page.len());
        documents.extend(page);
    }
    let scan = scanner.finish().unwrap();
    (documents, scan, page_lengths)
}

#[test]
fn current_database_wins_and_legacy_is_only_a_missing_current_fallback() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let current = temp.path().join(".deepagents/.state/sessions.db");
    let legacy = temp.path().join(".deepagents/sessions.db");
    fs::create_dir_all(current.parent().unwrap()).unwrap();
    fs::write(&current, b"current").unwrap();
    fs::write(&legacy, b"legacy").unwrap();

    let selected = DeepAgentsDatabaseSelectionV0::from_home(
        crate::test_provider_sqlite_data_root(),
        temp.path(),
    );
    assert_eq!(selected.path(), current);
    assert_eq!(selected.route(), DeepAgentsDatabaseRouteV0::Current);
    assert_eq!(
        DeepAgentsLocatorResolverV0::from_home(
            crate::test_provider_sqlite_data_root(),
            temp.path()
        )
        .selection
        .path(),
        current
    );

    fs::remove_file(&current).unwrap();
    let selected = DeepAgentsDatabaseSelectionV0::from_home(
        crate::test_provider_sqlite_data_root(),
        temp.path(),
    );
    assert_eq!(selected.path(), legacy);
    assert_eq!(selected.route(), DeepAgentsDatabaseRouteV0::Legacy);

    fs::remove_file(&legacy).unwrap();
    let selected = DeepAgentsDatabaseSelectionV0::from_home(
        crate::test_provider_sqlite_data_root(),
        temp.path(),
    );
    assert_eq!(selected.path(), current);
    assert_eq!(selected.route(), DeepAgentsDatabaseRouteV0::Current);
}

#[test]
fn bounded_cold_scan_emits_compound_exact_row_locators() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let conn = create_database(&path);
    insert_checkpoint(&conn, b"opaque checkpoint state");
    let long_message = format!("deepagents-head-{}-deepagents-tail", "x".repeat(3_000));
    let mut messages = vec![message("human", &long_message, "message-0")];
    messages.extend((1..130).map(|index| {
        message(
            if index % 2 == 0 { "human" } else { "ai" },
            &format!("bounded message {index}"),
            &format!("message-{index}"),
        )
    }));
    insert_write(
        &conn,
        "task-a",
        0,
        "messages",
        Some("msgpack"),
        &message_blob(messages),
    );
    drop(conn);

    let selection =
        DeepAgentsDatabaseSelectionV0::explicit(crate::test_provider_sqlite_data_root(), &path);
    let mut scanner =
        DeepAgentsSourceBackedScannerV0::open(selection.clone(), DateTime::<Utc>::UNIX_EPOCH)
            .unwrap();
    let scanner_source = scanner.source().clone();
    let first_page = scanner.next_page().unwrap().unwrap();
    assert!(scanner.sqlite_snapshot.is_some());
    let mut page_lengths = vec![first_page.len()];
    let mut documents = first_page;
    while let Some(page) = scanner.next_page().unwrap() {
        page_lengths.push(page.len());
        documents.extend(page);
    }
    let result = scanner.finish().unwrap();

    assert_eq!(page_lengths, [64, 64, 2]);
    assert_eq!(documents.len(), 130);
    assert_eq!(result.source, scanner_source);
    assert_eq!(result.certificate.counts().complete_records, 130);
    assert_eq!(result.certificate.counts().retained_records, 130);
    assert_eq!(result.certificate.counts().indexed_documents, 130);
    assert_eq!(result.selected_path, path);
    assert_eq!(result.selected_route, DeepAgentsDatabaseRouteV0::Explicit);
    assert_eq!(documents[0].body, long_message);
    assert!(documents[0].body.ends_with("deepagents-tail"));
    assert_eq!(documents[0].parent_session_id, None);
    assert_eq!(documents[0].root_session_id, documents[0].session_id);
    assert_eq!(
        documents[0].provider_session_id.as_deref(),
        Some("thread-a")
    );
    assert_eq!(
        documents[0].branch.as_deref(),
        Some("feature/source-backed")
    );
    assert_eq!(documents[0].source_path.as_deref(), path.to_str());
    assert_eq!(documents[0].agent_type, AgentType::Primary.as_str());
    assert!(documents[0].is_primary);
    assert_eq!(
        documents[0].locator.revision_policy(),
        LocatorRevisionPolicy::StableRecordEvidence
    );
    assert!(documents[0]
        .locator
        .certified_source_revision_digest()
        .is_none());
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = documents[0].locator.coordinate()
    else {
        panic!("expected SQLite locator");
    };
    assert_eq!(logical_relation, DEEPAGENTS_LOGICAL_RELATION);
    assert_eq!(
        primary_key,
        &TypedKey::Composite(vec![
            TypedKey::Utf8("thread-a".to_owned()),
            TypedKey::Utf8("checkpoint-a".to_owned()),
            TypedKey::Utf8("task-a".to_owned()),
            TypedKey::I64(0),
            TypedKey::U64(0),
        ])
    );
    assert_eq!(
        row_version,
        &Some(TypedKey::Bytes(
            documents[0].locator.record_digest().to_vec()
        ))
    );

    let hydrated =
        DeepAgentsLocatorResolverV0::explicit(crate::test_provider_sqlite_data_root(), &path)
            .hydrate(&documents[0].locator)
            .unwrap();
    assert_eq!(hydrated.text, long_message);
    assert_eq!(
        &hydrated.record_digest,
        documents[0].locator.record_digest()
    );
}

#[test]
fn checkpoint_state_and_non_message_writes_never_become_chat() {
    const OPAQUE_STATE_SECRET: &str = "OPAQUE_CHECKPOINT_STATE_MUST_NOT_BE_CHAT";
    const OPAQUE_CHANNEL_SECRET: &str = "OPAQUE_WRITE_CHANNEL_MUST_NOT_BE_CHAT";
    const SYSTEM_SECRET: &str = "SYSTEM_STATE_MUST_NOT_BE_CHAT";

    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let conn = create_database(&path);
    insert_checkpoint(&conn, OPAQUE_STATE_SECRET.as_bytes());
    insert_write(
        &conn,
        "opaque-task",
        0,
        "state",
        Some("msgpack"),
        OPAQUE_CHANNEL_SECRET.as_bytes(),
    );
    insert_write(
        &conn,
        "task-a",
        0,
        "messages",
        Some("msgpack"),
        &message_blob(vec![
            message("system", SYSTEM_SECRET, "system-message"),
            message("human", "visible chat message", "message-a"),
        ]),
    );
    drop(conn);

    let (documents, result, _) = scan(DeepAgentsDatabaseSelectionV0::explicit(
        crate::test_provider_sqlite_data_root(),
        &path,
    ));
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].body, "visible chat message");
    assert_eq!(result.certificate.counts().complete_records, 2);
    assert_eq!(result.certificate.counts().retained_records, 1);
    assert_eq!(result.certificate.counts().ignored_records, 1);
    assert_eq!(result.certificate.counts().indexed_documents, 1);
    let projected = format!("{documents:?}");
    assert!(!projected.contains(OPAQUE_STATE_SECRET));
    assert!(!projected.contains(OPAQUE_CHANNEL_SECRET));
    assert!(!projected.contains(SYSTEM_SECRET));
}

#[test]
fn replacement_preserves_ids_and_invalidates_old_snapshot_evidence() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("sessions.db");
    let conn = create_database(&path);
    insert_checkpoint(&conn, b"opaque");
    insert_write(
        &conn,
        "task-a",
        0,
        "messages",
        Some("msgpack"),
        &message_blob(vec![message("human", "before replacement", "stable-id")]),
    );
    drop(conn);

    let (before, before_scan, _) = scan(DeepAgentsDatabaseSelectionV0::explicit(
        crate::test_provider_sqlite_data_root(),
        &path,
    ));
    assert_eq!(before.len(), 1);
    let before_event_id = before[0].event_id;
    let before_session_id = before[0].session_id;
    let before_locator = before[0].locator.clone();
    let before_source = before_scan.source;

    let conn = Connection::open(&path).unwrap();
    replace_messages(
        &conn,
        vec![message("human", "after replacement", "stable-id")],
    );
    drop(conn);

    assert!(matches!(
        DeepAgentsLocatorResolverV0::explicit(crate::test_provider_sqlite_data_root(), &path)
            .hydrate(&before_locator),
        Err(DeepAgentsSourceBackedErrorV0::StaleRecordEvidence)
    ));

    let (after, after_scan, _) = scan(DeepAgentsDatabaseSelectionV0::explicit(
        crate::test_provider_sqlite_data_root(),
        &path,
    ));
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].event_id, before_event_id);
    assert_eq!(after[0].session_id, before_session_id);
    assert_eq!(after_scan.source, before_source);
    assert_ne!(
        after[0].locator.record_digest(),
        before_locator.record_digest()
    );
    assert_eq!(
        after[0].locator.certified_source_revision_digest(),
        before_locator.certified_source_revision_digest()
    );
    let hydrated =
        DeepAgentsLocatorResolverV0::explicit(crate::test_provider_sqlite_data_root(), &path)
            .hydrate(&after[0].locator)
            .unwrap();
    assert_eq!(hydrated.text, "after replacement");
}
