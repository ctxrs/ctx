use super::*;

fn proto_varint(mut value: u64) -> Vec<u8> {
    let mut encoded = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        encoded.push(byte);
        if value == 0 {
            return encoded;
        }
    }
}

fn proto_field(field: u32, payload: &[u8]) -> Vec<u8> {
    let mut encoded = proto_varint(u64::from(field) << 3 | 2);
    encoded.extend(proto_varint(payload.len() as u64));
    encoded.extend_from_slice(payload);
    encoded
}

fn warp_task_fixture(user_body: &str, result_body: &str) -> Vec<u8> {
    let user_query = proto_field(1, user_body.as_bytes());
    let mut user_message = proto_field(1, b"warp-user-message");
    user_message.extend(proto_field(2, &user_query));

    let finished = proto_field(1, result_body.as_bytes());
    let run_shell = proto_field(5, &finished);
    let mut tool_result = proto_field(1, b"warp-call-1");
    tool_result.extend(proto_field(2, &run_shell));
    let mut result_message = proto_field(1, b"warp-result-message");
    result_message.extend(proto_field(5, &tool_result));

    let mut task = proto_field(1, b"warp-task-1");
    task.extend(proto_field(5, &user_message));
    task.extend(proto_field(5, &result_message));
    task
}

fn create_warp_database(
    path: &Path,
    user_body: &str,
    result_body: &str,
    wal: bool,
) -> (Connection, Vec<NativeSqliteValue>) {
    let conn = Connection::open(path).unwrap();
    if wal {
        conn.pragma_update(None, "journal_mode", "wal").unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    }
    conn.execute_batch(
        "create table agent_tasks (
            id integer primary key,
            conversation_id text not null,
            task_id text not null unique,
            task blob not null,
            last_modified_at text not null
        );",
    )
    .unwrap();
    let task = warp_task_fixture(user_body, result_body);
    conn.execute(
        "insert into agent_tasks
         (conversation_id, task_id, task, last_modified_at)
         values (?1, ?2, ?3, ?4)",
        params![
            "warp-conversation-1",
            "warp-task-1",
            task,
            "2026-07-22 12:00:00"
        ],
    )
    .unwrap();
    let values = vec![
        NativeSqliteValue::Integer(1),
        NativeSqliteValue::Text("warp-conversation-1".to_owned()),
        NativeSqliteValue::Text("warp-task-1".to_owned()),
        NativeSqliteValue::Blob(task),
        NativeSqliteValue::Text("2026-07-22 12:00:00".to_owned()),
    ];
    (conn, values)
}

fn warp_locator(rowid: i64, message_index: u32) -> Vec<u8> {
    let mut value = rowid.to_be_bytes().to_vec();
    value.extend_from_slice(&message_index.to_be_bytes());
    value
}

fn warp_event(event_type: EventType, native_id: &str) -> TestProviderEvent {
    TestProviderEvent {
        provider_event_index: 0,
        provider_event_hash: Some(native_id.to_owned()),
        cursor: Some(format!("warp:{native_id}")),
        event_type,
        payload: json!({
            "text": "",
            "text_retention": {
                "mode": if event_type == EventType::Message { "bounded" } else { "none" },
                "limit_chars": if event_type == EventType::Message { json!(PROVIDER_MAX_TEXT_CHARS) } else { Value::Null },
                "truncated": event_type == EventType::Message,
            },
            "body": {},
        }),
        metadata: json!({"source": WARP_SQLITE_SOURCE_FORMAT}),
    }
}

fn warp_result_request(
    path: &Path,
    values: &[NativeSqliteValue],
    result_body: &str,
) -> ResultContentRequest {
    let event_id = Uuid::new_v4();
    ResultContentRequest {
        event_id,
        provider: CaptureProvider::Warp,
        source_format: WARP_SQLITE_SOURCE_FORMAT.to_owned(),
        source_access: sqlite_source_access(
            path,
            CaptureProvider::Warp,
            WARP_SQLITE_SOURCE_FORMAT,
            event_id,
        ),
        source_family: CompleteContentSourceFamily::Sqlite,
        content_profile: verified_content_profile(
            CaptureProvider::Warp,
            WARP_SQLITE_SOURCE_FORMAT,
            CompleteContentSourceFamily::Sqlite,
            VerifiedContentRole::ResultBody,
        )
        .unwrap()
        .to_owned(),
        source_locator: CompleteContentSourceLocator::new(WARP_LOCATOR_KIND, warp_locator(1, 1))
            .unwrap(),
        source_record_ordinal: 0,
        source_record_subrecord_index: 1,
        expected_native_record_id: "warp-result-message".to_owned(),
        expected_record_digest: sqlite_logical_record_digest(values),
        expected_content_ref: ContentRef::from_bytes(result_body.as_bytes()).unwrap(),
    }
}

#[test]
fn warp_recovers_verified_message_and_result_without_persisting_raw_result() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("warp.db");
    let user_body = long_body("Warp exact message");
    let result_body = "exact Warp shell output\nUnicode: 🦀";
    let (conn, values) = create_warp_database(&path, &user_body, result_body, false);
    drop(conn);

    let locator = NativeLocator::new(WARP_LOCATOR_KIND, warp_locator(1, 1)).unwrap();
    let mut event = warp_event(EventType::ToolOutput, "warp-result-message");
    attach_sqlite_result_content_locator(
        &mut event,
        CaptureProvider::Warp,
        WARP_SQLITE_SOURCE_FORMAT,
        &locator,
        &values,
        Some(result_body.to_owned()),
    )
    .unwrap();
    let serialized = serde_json::to_string(&event).unwrap();
    assert!(!serialized.contains(result_body));
    assert!(event.payload.get("result_content_ref").is_some());
    let locators = VerifiedContentLocatorsV1::from_metadata_value(
        &event.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    assert_eq!(
        locators
            .locator(VerifiedContentRole::ResultBody)
            .unwrap()
            .kind(),
        WARP_LOCATOR_KIND
    );

    let mut result_request = warp_result_request(&path, &values, result_body);
    let results = ResultContentResolver::resolve_results(
        &SqliteCompleteContentResolver::new(),
        std::slice::from_ref(&result_request),
    );
    let resolved = results.into_iter().next().unwrap().unwrap();
    assert_eq!(resolved.content.as_bytes(), result_body.as_bytes());
    assert!(resolved.verification.is_verified());

    let message_event = warp_event(EventType::Message, "warp-user-message");
    let message_request = request_for(
        &path,
        CaptureProvider::Warp,
        WARP_SQLITE_SOURCE_FORMAT,
        "warp-conversation-1",
        0,
        WARP_LOCATOR_KIND,
        warp_locator(1, 0),
        &values,
        &message_event,
        &user_body,
    );
    let messages =
        CompleteContentResolver::resolve(&SqliteCompleteContentResolver::new(), &[message_request])
            .unwrap();
    assert_eq!(messages[0].text, user_body);

    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "update agent_tasks set last_modified_at = '2026-07-22 12:00:01' where rowid = 1",
        [],
    )
    .unwrap();
    drop(conn);
    result_request.source_access = sqlite_source_access(
        &path,
        CaptureProvider::Warp,
        WARP_SQLITE_SOURCE_FORMAT,
        result_request.event_id,
    );
    let changed = ResultContentResolver::resolve_results(
        &SqliteCompleteContentResolver::new(),
        &[result_request],
    );
    assert_eq!(
        changed.into_iter().next().unwrap().unwrap_err().kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );
}

#[test]
fn warp_result_resolution_uses_readonly_wal_snapshot() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("warp-wal.db");
    let result_body = "Warp WAL exact output";
    let (conn, values) = create_warp_database(&path, "short prompt", result_body, true);
    let before = sqlite_components(&path);
    assert!(before
        .iter()
        .any(|(path, _)| path.to_string_lossy().ends_with("-wal")));
    let request = warp_result_request(&path, &values, result_body);
    let resolved =
        ResultContentResolver::resolve_results(&SqliteCompleteContentResolver::new(), &[request]);
    assert_eq!(
        resolved.into_iter().next().unwrap().unwrap().content,
        result_body
    );
    assert_eq!(sqlite_components(&path), before);
    drop(conn);
}

#[test]
fn firebender_recovers_unicode_escaped_multiline_bytes_and_retains_only_truncated_locator() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("chat_history.db");
    let body = long_body("Firebender exact body");
    let (values, mut event) = create_firebender_database(&path, &body);
    assert_eq!(event.payload["text_retention"]["truncated"], true);

    let locator =
        NativeLocator::new(FIREBENDER_LOCATOR_KIND, 1_i64.to_be_bytes().to_vec()).unwrap();
    attach_test_sqlite_message_locator(
        &mut event,
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        &locator,
        &values,
        || body.clone(),
    )
    .unwrap();
    let persisted = VerifiedContentLocatorsV1::from_metadata_value(
        &event.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let persisted = persisted.locator(VerifiedContentRole::MessageBody).unwrap();
    assert_eq!(persisted.family(), CompleteContentSourceFamily::Sqlite);
    assert_eq!(persisted.kind(), FIREBENDER_LOCATOR_KIND);
    assert_eq!(persisted.native_record_id(), "native-message-1");
    assert_eq!(
        persisted.record_sha256(),
        &sqlite_logical_record_digest(&values)
    );
    assert_eq!(
        persisted.content_ref(),
        &ContentRef::from_bytes(body.as_bytes()).unwrap()
    );

    let request = firebender_request(&path, &body, &values, &event);
    let messages = SqliteCompleteContentResolver::new()
        .resolve(&[request])
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text.as_bytes(), body.as_bytes());
    assert!(messages[0].verification.is_verified());

    let short = "ordinary short message";
    let (_, mut short_event) = create_event_without_database(short);
    attach_test_sqlite_message_locator(
        &mut short_event,
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        &locator,
        &values,
        || short.to_owned(),
    )
    .unwrap();
    assert!(short_event
        .metadata
        .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .is_none());
}

#[test]
fn firebender_result_retains_only_a_verified_reference_and_reopens_exact_bytes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("chat_history.db");
    let body = "Firebender tool output: unicode 雪\nexact bytes";
    let (values, mut event) = create_firebender_result_database(&path, body);
    assert_eq!(event.event_type, EventType::ToolOutput);
    assert!(!event.payload.to_string().contains(body));

    let locator =
        NativeLocator::new(FIREBENDER_LOCATOR_KIND, 1_i64.to_be_bytes().to_vec()).unwrap();
    attach_sqlite_result_content_locator(
        &mut event,
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        &locator,
        &values,
        firebender::firebender_result_content(&json!({
            "role": "tool",
            "name": "ignored display name",
            "content": {"text": body},
            "tool_calls": [{"name": "ignored display call"}],
        })),
    )
    .unwrap();
    let persisted = VerifiedContentLocatorsV1::from_metadata_value(
        &event.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let persisted = persisted.locator(VerifiedContentRole::ResultBody).unwrap();
    let content_ref = ContentRef::from_bytes(body.as_bytes()).unwrap();
    assert_eq!(persisted.content_ref(), &content_ref);
    assert_eq!(
        serde_json::from_value::<ContentRef>(event.payload["result_content_ref"].clone()).unwrap(),
        content_ref
    );
    assert!(!event.payload.to_string().contains(body));

    let event_id = Uuid::new_v4();
    let mut request = ResultContentRequest {
        event_id,
        provider: CaptureProvider::Firebender,
        source_format: FIREBENDER_SQLITE_SOURCE_FORMAT.to_owned(),
        source_access: sqlite_source_access(
            &path,
            CaptureProvider::Firebender,
            FIREBENDER_SQLITE_SOURCE_FORMAT,
            event_id,
        ),
        source_family: CompleteContentSourceFamily::Sqlite,
        content_profile: persisted.content_profile().to_owned(),
        source_locator: persisted.source_locator().unwrap(),
        source_record_ordinal: 0,
        source_record_subrecord_index: 0,
        expected_native_record_id: persisted.native_record_id().to_owned(),
        expected_record_digest: persisted.record_sha256().clone(),
        expected_content_ref: content_ref,
    };
    let resolved = ResultContentResolver::resolve_results(
        &SqliteCompleteContentResolver::new(),
        std::slice::from_ref(&request),
    );
    let resolved = resolved[0].as_ref().unwrap();
    assert_eq!(resolved.content.as_bytes(), body.as_bytes());
    assert!(resolved.verification.is_verified());

    let mut wrong = request.clone();
    wrong.expected_content_ref = ContentRef::from_bytes(b"different").unwrap();
    let failed = ResultContentResolver::resolve_results(
        &SqliteCompleteContentResolver::new(),
        std::slice::from_ref(&wrong),
    );
    assert_eq!(
        failed[0].as_ref().unwrap_err().kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );

    let changed_message = serde_json::to_string(&json!([{
        "id": "tool-result-1",
        "role": "tool",
        "content": {"type": "text", "text": "mutated result bytes"},
    }]))
    .unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "update chat_sessions set messages_json = ?1 where rowid = 1",
        [&changed_message],
    )
    .unwrap();
    drop(conn);
    request.source_access = sqlite_source_access(
        &path,
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        event_id,
    );
    let failed = ResultContentResolver::resolve_results(
        &SqliteCompleteContentResolver::new(),
        std::slice::from_ref(&request),
    );
    assert_eq!(
        failed[0].as_ref().unwrap_err().kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );

    let reused_message = serde_json::to_string(&json!([{
        "id": "tool-result-1",
        "role": "tool",
        "content": {"type": "text", "text": body},
    }]))
    .unwrap();
    let mut conn = Connection::open(&path).unwrap();
    let transaction = conn.transaction().unwrap();
    transaction
        .execute("delete from chat_sessions where rowid = 1", [])
        .unwrap();
    transaction
        .execute(
            "insert into chat_sessions (
                rowid, id, name, created_at, updated_at, messages_json, metadata_json
             ) values (1, ?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                "reused-session",
                "Reused row",
                CREATED_AT,
                CREATED_AT + 1,
                reused_message,
                "{}",
            ],
        )
        .unwrap();
    transaction.commit().unwrap();
    drop(conn);
    request.source_access = sqlite_source_access(
        &path,
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        event_id,
    );
    let failed = ResultContentResolver::resolve_results(
        &SqliteCompleteContentResolver::new(),
        std::slice::from_ref(&request),
    );
    assert_eq!(
        failed[0].as_ref().unwrap_err().kind,
        CompleteContentErrorKind::ContentVerificationFailed
    );
}

#[test]
fn firebender_name_only_tool_message_never_gets_result_evidence() {
    let message = json!({
        "id": "name-only-result",
        "role": "tool",
        "name": "display-only-name",
        "tool_calls": [{"name": "display-only-tool-call"}],
    });
    assert!(firebender::firebender_result_content(&message).is_none());
    let mut event = firebender_event(SESSION_ID, 0, &message, DateTime::<Utc>::UNIX_EPOCH);
    let locator =
        NativeLocator::new(FIREBENDER_LOCATOR_KIND, 1_i64.to_be_bytes().to_vec()).unwrap();
    attach_sqlite_result_content_locator(
        &mut event,
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        &locator,
        &firebender_values("[]"),
        firebender::firebender_result_content(&message),
    )
    .unwrap();
    assert!(event.payload.get("result_content_ref").is_none());
    assert!(event
        .metadata
        .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .is_none());
}
