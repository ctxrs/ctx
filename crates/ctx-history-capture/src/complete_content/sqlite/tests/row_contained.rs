use super::*;

#[test]
fn kiro_and_zed_row_contained_cohorts_recover_exact_message_text() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let kiro_path = temp.path().join("kiro.db");
    let kiro_user_body = long_body("Kiro user body");
    let kiro_assistant_body = long_body("Kiro assistant body");
    let kiro_tool_fallback_body = long_body("Kiro tool fallback body");
    let kiro_value = json!({
        "history": [
            {"unrecognized": true},
            {
                "assistant": {
                    "ToolUse": {"tool_uses": [{"name": "shell"}]},
                    "timestamp": "2026-07-21T11:59:59Z"
                }
            },
            {
                "user": {
                    "timestamp": "2026-07-21T12:00:00Z",
                    "content": { "Prompt": { "prompt": kiro_user_body } }
                },
                "assistant": {
                    "timestamp": "2026-07-21T12:00:01Z",
                    "Response": {"content": kiro_assistant_body}
                }
            },
            {
                "user": {"content": {"Prompt": {"prompt": "   "}}},
                "assistant": {"ToolUse": {"content": kiro_tool_fallback_body}}
            }
        ]
    });
    let kiro_json = serde_json::to_string(&kiro_value).unwrap();
    let conn = Connection::open(&kiro_path).unwrap();
    conn.execute_batch(
        "create table conversations_v2 (
            key text not null, conversation_id text not null, value text not null,
            created_at integer, updated_at integer
        );",
    )
    .unwrap();
    conn.execute(
        "insert into conversations_v2 values (?1, ?2, ?3, ?4, ?5)",
        params![
            "/workspace",
            "kiro-session",
            kiro_json,
            CREATED_AT,
            CREATED_AT + 1
        ],
    )
    .unwrap();
    drop(conn);
    let kiro_values = vec![
        CapturedSqliteValue::Integer(1),
        CapturedSqliteValue::Text("/workspace".to_owned()),
        CapturedSqliteValue::Text("kiro-session".to_owned()),
        CapturedSqliteValue::Text(kiro_json),
        CapturedSqliteValue::Integer(CREATED_AT),
        CapturedSqliteValue::Integer(CREATED_AT + 1),
    ];
    let kiro_row =
        kiro::decode_kiro_conversation_for_complete("conversations_v2", &kiro_values).unwrap();
    let started_at =
        kiro::kiro_session_started_at(&kiro_row, &kiro_value, DateTime::<Utc>::UNIX_EPOCH);
    let decoded = kiro::kiro_history_events(&kiro_row, "kiro-session", &kiro_value, started_at)
        .map(|decoded| {
            let text = decoded.complete_text();
            (decoded.event, text)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        decoded
            .iter()
            .map(|(event, _)| (event.provider_event_index, event.event_type))
            .collect::<Vec<_>>(),
        vec![
            (3, EventType::ToolCall),
            (4, EventType::Message),
            (5, EventType::Message),
            (7, EventType::Message),
        ]
    );
    let mut kiro_locator = vec![1_u8];
    kiro_locator.extend_from_slice(&(1_u64 ^ (1_u64 << 63)).to_be_bytes());
    let tool_call_request = request_for(
        &kiro_path,
        CaptureProvider::KiroCli,
        KIRO_SQLITE_SOURCE_FORMAT,
        "kiro-session",
        0,
        KIRO_LOCATOR_KIND,
        kiro_locator.clone(),
        &kiro_values,
        &decoded[0].0,
        &decoded[0].1,
    );
    assert_error_kind(
        &tool_call_request,
        CompleteContentErrorKind::HydrationUnsupported,
    );
    let mut kiro_requests = decoded
        .iter()
        .enumerate()
        .skip(1)
        .map(|(subrecord, (event, body))| {
            assert_eq!(event.payload["text_retention"]["truncated"], true);
            request_for(
                &kiro_path,
                CaptureProvider::KiroCli,
                KIRO_SQLITE_SOURCE_FORMAT,
                "kiro-session",
                u32::try_from(subrecord).unwrap(),
                KIRO_LOCATOR_KIND,
                kiro_locator.clone(),
                &kiro_values,
                event,
                body,
            )
        })
        .collect::<Vec<_>>();
    if let Some(first) = kiro_requests
        .first()
        .map(|request| request.source_access.clone())
    {
        for request in &mut kiro_requests {
            request.source_access = first.clone();
        }
    }
    let result = SqliteCompleteContentResolver::new()
        .resolve(&kiro_requests)
        .unwrap();
    assert_eq!(
        result
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        vec![
            kiro_user_body.as_str(),
            kiro_assistant_body.as_str(),
            kiro_tool_fallback_body.as_str(),
        ]
    );

    let zed_path = temp.path().join("zed.db");
    let zed_body = long_body("Zed body");
    let zed_message = json!({ "User": { "content": [{ "Text": zed_body }] } });
    let zed_thread = json!({
        "messages": [zed_message.clone()],
        "updated_at": "2026-07-21T12:00:00Z"
    });
    let zed_data = serde_json::to_vec(&zed_thread).unwrap();
    let conn = Connection::open(&zed_path).unwrap();
    conn.execute_batch(
        "create table threads (
            id text not null, summary text not null, updated_at text not null,
            data_type text not null, data blob not null
        );",
    )
    .unwrap();
    conn.execute(
        "insert into threads values (?1, ?2, ?3, ?4, ?5)",
        params![
            "zed-session",
            "Zed fixture",
            "2026-07-21T12:00:00Z",
            "json",
            zed_data
        ],
    )
    .unwrap();
    drop(conn);
    let zed_values = vec![
        CapturedSqliteValue::Integer(1),
        CapturedSqliteValue::Text("zed-session".to_owned()),
        CapturedSqliteValue::Null,
        CapturedSqliteValue::Null,
        CapturedSqliteValue::Null,
        CapturedSqliteValue::Text("Zed fixture".to_owned()),
        CapturedSqliteValue::Text("2026-07-21T12:00:00Z".to_owned()),
        CapturedSqliteValue::Text("json".to_owned()),
        CapturedSqliteValue::Blob(zed_data),
        CapturedSqliteValue::Null,
    ];
    let zed_row = zed::decode_zed_thread_for_complete(&zed_values).unwrap();
    let zed_decoded = zed::decode_zed_thread_events(&zed_row).unwrap();
    let zed_event = zed_decoded
        .event_at("zed-session", 0)
        .unwrap()
        .unwrap()
        .event;
    let zed_request = request_for(
        &zed_path,
        CaptureProvider::Zed,
        ZED_THREADS_SQLITE_SOURCE_FORMAT,
        "zed-session",
        0,
        ZED_LOCATOR_KIND,
        1_i64.to_be_bytes().to_vec(),
        &zed_values,
        &zed_event,
        &zed_body,
    );
    let result = SqliteCompleteContentResolver::new()
        .resolve(&[zed_request])
        .unwrap();
    assert_eq!(result[0].text, zed_body);
}

#[test]
fn zed_result_locator_reopens_exact_row_without_persisting_output() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("zed-result.db");
    let content = "first\nsecond";
    let message = json!({"Agent": {"tool_results": {
        "call-1": {
            "tool_name": "shell",
            "content": [{"Text": "first"}, {"Image": {"source": "ignored"}}],
            "output": "second"
        }
    }}});
    let thread = json!({
        "messages": [message],
        "updated_at": "2026-07-21T12:00:00Z"
    });
    let data = serde_json::to_vec(&thread).unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table threads (
            id text not null, summary text not null, updated_at text not null,
            data_type text not null, data blob not null
        );",
    )
    .unwrap();
    conn.execute(
        "insert into threads values (?1, ?2, ?3, ?4, ?5)",
        params![
            "zed-result-session",
            "Zed result fixture",
            "2026-07-21T12:00:00Z",
            "json",
            data
        ],
    )
    .unwrap();
    drop(conn);
    let values = vec![
        CapturedSqliteValue::Integer(1),
        CapturedSqliteValue::Text("zed-result-session".to_owned()),
        CapturedSqliteValue::Null,
        CapturedSqliteValue::Null,
        CapturedSqliteValue::Null,
        CapturedSqliteValue::Text("Zed result fixture".to_owned()),
        CapturedSqliteValue::Text("2026-07-21T12:00:00Z".to_owned()),
        CapturedSqliteValue::Text("json".to_owned()),
        CapturedSqliteValue::Blob(data),
        CapturedSqliteValue::Null,
    ];
    let row = zed::decode_zed_thread_for_complete(&values).unwrap();
    let decoded = zed::decode_zed_thread_events(&row).unwrap();
    let mut event = decoded
        .event_at("zed-result-session", 0)
        .unwrap()
        .unwrap()
        .event;
    assert_eq!(event.event_type, EventType::ToolOutput);
    let locator = NativeLocator::new(ZED_LOCATOR_KIND, 1_i64.to_be_bytes().to_vec()).unwrap();
    attach_sqlite_result_content_locator(
        &mut event,
        CaptureProvider::Zed,
        ZED_THREADS_SQLITE_SOURCE_FORMAT,
        &locator,
        &values,
        Some(content.to_owned()),
    )
    .unwrap();
    assert!(!event.payload.to_string().contains(content));
    let persisted = VerifiedContentLocatorsV1::from_metadata_value(
        &event.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let persisted = persisted.locator(VerifiedContentRole::ResultBody).unwrap();
    let event_id = Uuid::new_v4();
    let source_access = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider: CaptureProvider::Zed,
                source_format: ZED_THREADS_SQLITE_SOURCE_FORMAT.to_owned(),
                family: CompleteContentSourceFamily::Sqlite,
                raw_source_path: path.clone(),
                source_root: path.parent().map(Path::to_path_buf),
                source_identity: Some("zed-result-source".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            event_id,
        )
        .unwrap();
    let mut request = ResultContentRequest {
        event_id,
        provider: CaptureProvider::Zed,
        source_format: ZED_THREADS_SQLITE_SOURCE_FORMAT.to_owned(),
        source_access,
        source_family: CompleteContentSourceFamily::Sqlite,
        content_profile: persisted.content_profile().to_owned(),
        source_locator: persisted.source_locator().unwrap(),
        source_record_ordinal: 0,
        source_record_subrecord_index: 0,
        expected_native_record_id: persisted.native_record_id().to_owned(),
        expected_record_digest: persisted.record_sha256().clone(),
        expected_content_ref: persisted.content_ref().clone(),
    };
    let resolved = ResultContentResolver::resolve_results(
        &SqliteCompleteContentResolver::new(),
        std::slice::from_ref(&request),
    );
    assert_eq!(resolved[0].as_ref().unwrap().content, content);

    let conn = Connection::open(&path).unwrap();
    conn.execute("update threads set summary = 'mutated' where rowid = 1", [])
        .unwrap();
    drop(conn);
    request.source_access = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider: request.provider,
                source_format: request.source_format.clone(),
                family: CompleteContentSourceFamily::Sqlite,
                raw_source_path: path.clone(),
                source_root: path.parent().map(Path::to_path_buf),
                source_identity: Some("zed-result-source".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            request.event_id,
        )
        .unwrap();
    assert_eq!(
        ResultContentResolver::resolve_results(&SqliteCompleteContentResolver::new(), &[request])
            [0]
        .as_ref()
        .unwrap_err()
        .kind,
        CompleteContentErrorKind::SourceChanged
    );
}

#[test]
fn malformed_and_truncated_kiro_records_fail_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("malformed-kiro.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations_v2 (
            key text not null, conversation_id text not null, value text not null,
            created_at integer, updated_at integer
        );",
    )
    .unwrap();
    let truncated_json = r#"{"history":["#;
    let malformed_history = r#"{"history":{"not":"an array"}}"#;
    conn.execute(
        "insert into conversations_v2 values (?1, ?2, ?3, ?4, ?5)",
        params![
            "/truncated",
            "kiro-truncated",
            truncated_json,
            CREATED_AT,
            CREATED_AT
        ],
    )
    .unwrap();
    conn.execute(
        "insert into conversations_v2 values (?1, ?2, ?3, ?4, ?5)",
        params![
            "/malformed-history",
            "kiro-malformed-history",
            malformed_history,
            CREATED_AT,
            CREATED_AT
        ],
    )
    .unwrap();
    drop(conn);

    for (rowid, key, session_id, stored_value, expected) in [
        (
            1,
            "/truncated",
            "kiro-truncated",
            truncated_json,
            CompleteContentErrorKind::ContentVerificationFailed,
        ),
        (
            2,
            "/malformed-history",
            "kiro-malformed-history",
            malformed_history,
            CompleteContentErrorKind::SourceRecordMissing,
        ),
    ] {
        let values = vec![
            CapturedSqliteValue::Integer(rowid),
            CapturedSqliteValue::Text(key.to_owned()),
            CapturedSqliteValue::Text(session_id.to_owned()),
            CapturedSqliteValue::Text(stored_value.to_owned()),
            CapturedSqliteValue::Integer(CREATED_AT),
            CapturedSqliteValue::Integer(CREATED_AT),
        ];
        let row = kiro::decode_kiro_conversation_for_complete("conversations_v2", &values).unwrap();
        let body = long_body("untrusted fallback must not be returned");
        let reference = json!({
            "history": [{"user": {"content": {"Prompt": {"prompt": body}}}}]
        });
        let decoded =
            kiro::kiro_history_events(&row, session_id, &reference, DateTime::<Utc>::UNIX_EPOCH)
                .next()
                .unwrap();
        let mut locator = vec![1_u8];
        locator.extend_from_slice(&((rowid as u64) ^ (1_u64 << 63)).to_be_bytes());
        let request = request_for(
            &path,
            CaptureProvider::KiroCli,
            KIRO_SQLITE_SOURCE_FORMAT,
            session_id,
            0,
            KIRO_LOCATOR_KIND,
            locator,
            &values,
            &decoded.event,
            &body,
        );
        assert_error_kind(&request, expected);
    }
}

#[test]
fn legacy_kiro_row_preserves_decoder_identity_locator_and_content() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("legacy-kiro.db");
    let body = long_body("Legacy Kiro body");
    let value = json!({
        "conversation_id": "kiro-legacy-session",
        "history": [{
            "user": {
                "timestamp": "2026-07-21T12:00:00Z",
                "content": {"Prompt": {"prompt": body}}
            }
        }]
    });
    let encoded = serde_json::to_string(&value).unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("create table conversations (key text not null, value text not null);")
        .unwrap();
    conn.execute(
        "insert into conversations values (?1, ?2)",
        params!["/legacy", encoded],
    )
    .unwrap();
    drop(conn);
    let values = vec![
        CapturedSqliteValue::Integer(1),
        CapturedSqliteValue::Text("/legacy".to_owned()),
        CapturedSqliteValue::Text(encoded),
    ];
    let row = kiro::decode_kiro_conversation_for_complete("conversations", &values).unwrap();
    let provider_session_id = kiro::kiro_provider_session_id(&row, &value);
    let started_at = kiro::kiro_session_started_at(&row, &value, DateTime::<Utc>::UNIX_EPOCH);
    let decoded = kiro::kiro_history_events(&row, &provider_session_id, &value, started_at)
        .next()
        .unwrap();
    assert_eq!(
        decoded.event.provider_event_hash.as_deref(),
        Some("conversations:kiro-legacy-session:0:user")
    );
    assert_eq!(
        decoded.event.cursor.as_deref(),
        Some("conversations:kiro-legacy-session:history:0:user")
    );
    let mut locator = vec![2_u8];
    locator.extend_from_slice(&(1_u64 ^ (1_u64 << 63)).to_be_bytes());
    let request = request_for(
        &path,
        CaptureProvider::KiroCli,
        KIRO_SQLITE_SOURCE_FORMAT,
        &provider_session_id,
        0,
        KIRO_LOCATOR_KIND,
        locator,
        &values,
        &decoded.event,
        &body,
    );
    let message = SqliteCompleteContentResolver::new()
        .resolve(&[request])
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(message.text, body);
}

#[test]
fn oversized_kiro_record_fails_before_json_decode() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("oversized-kiro.db");
    let oversized_value = "x".repeat(COMPLETE_CONTENT_MAX_BODY_BYTES + 1);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations_v2 (
            key text not null, conversation_id text not null, value text not null,
            created_at integer, updated_at integer
        );",
    )
    .unwrap();
    conn.execute(
        "insert into conversations_v2 values (?1, ?2, ?3, ?4, ?5)",
        params![
            "/oversized",
            "kiro-oversized",
            oversized_value,
            CREATED_AT,
            CREATED_AT
        ],
    )
    .unwrap();
    drop(conn);
    let values = vec![
        CapturedSqliteValue::Integer(1),
        CapturedSqliteValue::Text("/oversized".to_owned()),
        CapturedSqliteValue::Text("kiro-oversized".to_owned()),
        CapturedSqliteValue::Text(oversized_value),
        CapturedSqliteValue::Integer(CREATED_AT),
        CapturedSqliteValue::Integer(CREATED_AT),
    ];
    let row = kiro::decode_kiro_conversation_for_complete("conversations_v2", &values).unwrap();
    let body = long_body("oversized row fallback");
    let reference = json!({
        "history": [{"user": {"content": {"Prompt": {"prompt": body}}}}]
    });
    let decoded = kiro::kiro_history_events(
        &row,
        "kiro-oversized",
        &reference,
        DateTime::<Utc>::UNIX_EPOCH,
    )
    .next()
    .unwrap();
    let mut locator = vec![1_u8];
    locator.extend_from_slice(&(1_u64 ^ (1_u64 << 63)).to_be_bytes());
    let request = request_for(
        &path,
        CaptureProvider::KiroCli,
        KIRO_SQLITE_SOURCE_FORMAT,
        "kiro-oversized",
        0,
        KIRO_LOCATOR_KIND,
        locator,
        &values,
        &decoded.event,
        &body,
    );
    assert_error_kind(&request, CompleteContentErrorKind::ContentTooLarge);
}

#[test]
fn oversized_sqlite_record_returns_content_too_large() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("oversized.db");
    let body = "z".repeat(COMPLETE_CONTENT_MAX_BODY_BYTES + 1);
    let (values, event) = create_firebender_database(&path, &body);
    let request = firebender_request(&path, &body, &values, &event);
    assert_error_kind(&request, CompleteContentErrorKind::ContentTooLarge);
}
