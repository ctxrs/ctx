use super::*;
use crate::provider::providers::goose::tests::create_goose_tables as create_goose_native_schema;
use crate::{GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT, OPENCODE_SQLITE_SOURCE_FORMAT};

#[test]
fn forgecode_success_result_is_absent_from_core_storage() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_path = temp.path().join("source.forge.db");
    let source = Connection::open(&source_path).unwrap();
    source
        .execute_batch(
            "create table conversations (
                conversation_id text not null, title text, workspace_id integer not null,
                context text, created_at text not null, updated_at text, metrics text
             );",
        )
        .unwrap();
    let secret = "source-only-result-body-9f2d";
    source
        .execute(
            "insert into conversations values ('session', NULL, 1, ?1, \
             '2026-01-01T00:00:00Z', NULL, NULL)",
            [serde_json::to_string(&json!({
                "messages": [{"message": {"tool": {
                    "output": {"values": [{"text": secret}]}
                }}}]
            }))
            .unwrap()],
        )
        .unwrap();
    drop(source);

    let store_path = temp.path().join("ctx.db");
    let mut store = Store::open(&store_path).unwrap();
    let summary = forgecode::import_forgecode_nativepath(
        &source_path,
        &mut store,
        ProviderAdapterContext {
            machine_id: "sqlite-result-import".to_owned(),
            source_path: Some(source_path.clone()),
            source_root: Some(temp.path().to_path_buf()),
            imported_at: DateTime::<Utc>::UNIX_EPOCH,
        },
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 0);

    let stored = Connection::open(&store_path).unwrap();
    let event_count: i64 = stored
        .query_row(
            "select count(*) from events e join sessions s on s.id = e.session_id \
             where s.provider = 'forgecode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event_count, 0);
    let run_count: i64 = stored
        .query_row(
            "select count(*) from runs r join sessions s on s.id = r.session_id \
             where s.provider = 'forgecode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(run_count, 0);
}

#[test]
fn capabilities_account_for_every_sqlite_cohort_without_silent_fallback() {
    let supported = VERIFIED_CONTENT_ROUTES
        .iter()
        .filter(|route| {
            route.role == VerifiedContentRole::MessageBody
                && verified_content_route_supported(
                    route.provider,
                    route.source_format,
                    CompleteContentSourceFamily::Sqlite,
                    route.role,
                )
        })
        .count();
    assert_eq!(supported, 17);
    let trae = VERIFIED_CONTENT_ROUTES
        .iter()
        .find(|route| {
            route.provider == CaptureProvider::Trae
                && route.role == VerifiedContentRole::MessageBody
        })
        .unwrap();
    assert_eq!(trae.source_format, TRAE_STATE_VSCDB_SOURCE_FORMAT);
    assert!(trae.platform_dispositions.iter().all(|disposition| {
        disposition.status == VerifiedContentRouteStatus::Supported && disposition.reason.is_empty()
    }));
}

fn synthetic_event(provider_event_hash: &str, body: &str) -> TestProviderEvent {
    let (_, mut event) = create_event_without_database(body);
    event.provider_event_hash = Some(provider_event_hash.to_owned());
    event
}

fn ordered_rowid_locator(kind: &str, phase: u8, rowid: i64) -> (String, Vec<u8>) {
    let mut value = vec![phase];
    value.extend_from_slice(&((rowid as u64) ^ (1_u64 << 63)).to_be_bytes());
    (kind.to_owned(), value)
}

#[test]
fn goose_message_resolver_reconstructs_long_content_and_persisted_degraded_ids() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("goose-native-message-resolver.db");
    let conn = Connection::open(&path).unwrap();
    create_goose_native_schema(&conn);
    conn.execute("insert into sessions(id) values ('goose-session')", [])
        .unwrap();

    let fixtures = [
        (1_i64, "  stable-id  ".to_owned(), "stable-id"),
        (2, "duplicate-id".to_owned(), "row-2"),
        (3, "duplicate-id".to_owned(), "row-3"),
        (4, "x".repeat(1_025), "row-4"),
        (5, "   ".to_owned(), "row-5"),
    ];
    let bodies = fixtures
        .iter()
        .map(|(id, _, _)| long_body(&format!("Goose complete row {id}")))
        .collect::<Vec<_>>();
    for ((id, message_id, _), body) in fixtures.iter().zip(&bodies) {
        conn.execute(
            "insert into messages(
                id, message_id, session_id, role, content_json, created_timestamp,
                timestamp, tokens, metadata_json
             ) values (
                ?1, ?2, 'goose-session', 'user', ?3, ?4,
                '2026-07-26T00:00:00Z', '{\"input\":1}', '{\"source\":\"test\"}'
             )",
            rusqlite::params![
                id,
                message_id,
                json!([{"type": "text", "text": body}]).to_string(),
                id
            ],
        )
        .unwrap();
    }

    let mut requests = Vec::new();
    for ((id, _, expected_native_record_id), body) in fixtures.iter().zip(&bodies) {
        let values = goose::load_goose_message_values(&conn, *id).unwrap();
        let (session_id, native_record_id, normalized_payload_hash, resolved_body) =
            goose::goose_complete_message_with_normalized_hash(&conn, &values).unwrap();
        assert_eq!(session_id, "goose-session");
        assert_eq!(native_record_id, *expected_native_record_id);
        assert_eq!(resolved_body, *body);
        assert_eq!(normalized_payload_hash.len(), 64);

        let (_, locator) = ordered_rowid_locator(GOOSE_LOCATOR_KIND, 2, *id);
        let event = synthetic_event(&native_record_id, body);
        let mut request = request_for(
            &path,
            CaptureProvider::Goose,
            GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
            "goose-session",
            0,
            GOOSE_LOCATOR_KIND,
            locator,
            &values,
            &event,
            body,
        );
        request.expected_hash_authority = CompleteContentHashAuthority::NormalizedPayloadFallback;
        request.expected_provider_event_hash = normalized_payload_hash;
        requests.push(request);
    }
    drop(conn);

    let resolved = requests
        .iter()
        .enumerate()
        .map(|(index, request)| {
            SqliteCompleteContentResolver::new()
                .resolve(std::slice::from_ref(request))
                .unwrap_or_else(|error| panic!("Goose fixture {index} failed: {error:?}"))
                .pop()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        resolved
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        bodies.iter().map(String::as_str).collect::<Vec<_>>()
    );
}

#[test]
fn newly_supported_sqlite_locators_are_bounded_and_path_free() {
    let body = long_body("path-free locator body");
    let stable_values = vec![NativeSqliteValue::Text("stable logical row".into())];
    let (_, ordered) = ordered_rowid_locator(CRUSH_LOCATOR_KIND, 2, 7);
    let mut raw_phase = vec![2];
    raw_phase.extend_from_slice(&7_i64.to_be_bytes());
    let mut opencode = vec![1];
    opencode.extend_from_slice(&((7_u64) ^ (1_u64 << 63)).to_be_bytes());
    opencode.push(2);
    let routes = [
        (
            CaptureProvider::OpenCode,
            crate::OPENCODE_SQLITE_SOURCE_FORMAT,
            OPENCODE_LOCATOR_KIND,
            opencode.clone(),
        ),
        (
            CaptureProvider::Kilo,
            crate::KILO_SQLITE_SOURCE_FORMAT,
            OPENCODE_LOCATOR_KIND,
            opencode.clone(),
        ),
        (
            CaptureProvider::MiMoCode,
            crate::MIMOCODE_SQLITE_SOURCE_FORMAT,
            OPENCODE_LOCATOR_KIND,
            opencode,
        ),
        (
            CaptureProvider::Crush,
            crate::CRUSH_SQLITE_SOURCE_FORMAT,
            CRUSH_LOCATOR_KIND,
            ordered.clone(),
        ),
        (
            CaptureProvider::Goose,
            crate::GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
            GOOSE_LOCATOR_KIND,
            ordered,
        ),
        (
            CaptureProvider::Hermes,
            crate::HERMES_SQLITE_SOURCE_FORMAT,
            HERMES_LOCATOR_KIND,
            raw_phase,
        ),
        (
            CaptureProvider::ForgeCode,
            crate::FORGECODE_SQLITE_SOURCE_FORMAT,
            FORGECODE_LOCATOR_KIND,
            7_i64.to_be_bytes().to_vec(),
        ),
    ];
    for (provider, source_format, kind, value) in routes {
        let mut event = synthetic_event("native-record", &body);
        let locator = NativeLocator::new(kind, value).unwrap();
        attach_test_sqlite_message_locator(
            &mut event,
            provider,
            source_format,
            &locator,
            &stable_values,
            || body.clone(),
        )
        .unwrap();
        let encoded = event.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY].to_string();
        assert!(!encoded.contains('/') && !encoded.contains(".db"));
        let persisted = VerifiedContentLocatorsV1::from_metadata_value(
            &event.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
        )
        .unwrap();
        assert_eq!(
            persisted
                .locator(VerifiedContentRole::MessageBody)
                .unwrap()
                .kind(),
            kind
        );
    }
}

#[test]
fn newly_supported_sqlite_cohorts_reopen_exact_message_rows() {
    let temp = crate::test_support_paths::tempdir().unwrap();

    // OpenCode-family current session_message layout.
    let body = long_body("OpenCode exact body");
    let path = temp.path().join("opencode.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table session (id text primary key, time_created integer); \
         create table session_message (id text, session_id text, data text);",
    )
    .unwrap();
    let data = serde_json::to_string(&json!({"role":"user", "text": body})).unwrap();
    conn.execute("insert into session values ('oc-session', 1)", [])
        .unwrap();
    conn.execute(
        "insert into session_message values ('oc-message', 'oc-session', ?1)",
        [data],
    )
    .unwrap();
    let values = opencode::load_opencode_message_values(
        &conn,
        &opencode::OPENCODE_SQLITE_DIALECT,
        opencode::OpenCodeCapturedShape::SessionMessage,
        1,
    )
    .unwrap();
    let (_, native_record_id, _, normalized_payload_hash) =
        opencode::opencode_complete_message_with_normalized_hash(
            &values,
            &opencode::OPENCODE_SQLITE_DIALECT,
        )
        .unwrap();
    drop(conn);
    let mut locator = vec![1];
    locator.extend_from_slice(&((1_u64) ^ (1_u64 << 63)).to_be_bytes());
    locator.push(2);
    for (provider, source_format) in [
        (
            CaptureProvider::OpenCode,
            crate::OPENCODE_SQLITE_SOURCE_FORMAT,
        ),
        (CaptureProvider::Kilo, crate::KILO_SQLITE_SOURCE_FORMAT),
        (
            CaptureProvider::MiMoCode,
            crate::MIMOCODE_SQLITE_SOURCE_FORMAT,
        ),
    ] {
        let event = synthetic_event("oc-message", &body);
        let mut request = request_for(
            &path,
            provider,
            source_format,
            "oc-session",
            0,
            OPENCODE_LOCATOR_KIND,
            locator.clone(),
            &values,
            &event,
            &body,
        );
        request.expected_hash_authority = CompleteContentHashAuthority::NormalizedPayloadFallback;
        request.expected_provider_event_hash = normalized_payload_hash.clone();
        request.expected_native_record_id = Some(native_record_id.clone());
        request.expected_record_digest = Some(sqlite_logical_record_digest(&values[1..]));
        assert_eq!(
            SqliteCompleteContentResolver::new()
                .resolve(&[request])
                .unwrap()[0]
                .text,
            body
        );
    }

    // Crush relational message/session join.
    let body = long_body("Crush exact body");
    let path = temp.path().join("crush.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table sessions (id text primary key); \
         create table messages (id text, session_id text, role text, parts text); \
         insert into sessions values ('crush-session');",
    )
    .unwrap();
    let parts = serde_json::to_string(&json!([{"type":"text", "data":{"text":body}}])).unwrap();
    conn.execute(
        "insert into messages values ('crush-message', 'crush-session', 'user', ?1)",
        [parts],
    )
    .unwrap();
    let values = crush::load_crush_message_values(&conn, 1).unwrap();
    drop(conn);
    let (_, locator) = ordered_rowid_locator(CRUSH_LOCATOR_KIND, 2, 1);
    let event = synthetic_event("crush-message", &body);
    let request = request_for(
        &path,
        CaptureProvider::Crush,
        crate::CRUSH_SQLITE_SOURCE_FORMAT,
        "crush-session",
        0,
        CRUSH_LOCATOR_KIND,
        locator,
        &values,
        &event,
        &body,
    );
    assert_eq!(
        SqliteCompleteContentResolver::new()
            .resolve(&[request])
            .unwrap()[0]
            .text,
        body
    );

    // Goose relational message/session join.
    let body = long_body("Goose exact body");
    let path = temp.path().join("goose.db");
    let conn = Connection::open(&path).unwrap();
    create_goose_native_schema(&conn);
    conn.execute("insert into sessions(id) values ('goose-session')", [])
        .unwrap();
    let content = serde_json::to_string(&json!([{"type":"text", "text":body}])).unwrap();
    conn.execute(
        "insert into messages(
            id, message_id, session_id, role, content_json, created_timestamp
         ) values (1, 'goose-message', 'goose-session', 'user', ?1, 1)",
        [content],
    )
    .unwrap();
    let values = goose::load_goose_message_values(&conn, 1).unwrap();
    let (_, native_record_id, normalized_payload_hash, _) =
        goose::goose_complete_message_with_normalized_hash(&conn, &values).unwrap();
    drop(conn);
    let (_, locator) = ordered_rowid_locator(GOOSE_LOCATOR_KIND, 2, 1);
    let event = synthetic_event(&native_record_id, &body);
    let mut request = request_for(
        &path,
        CaptureProvider::Goose,
        crate::GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        "goose-session",
        0,
        GOOSE_LOCATOR_KIND,
        locator,
        &values,
        &event,
        &body,
    );
    request.expected_hash_authority = CompleteContentHashAuthority::NormalizedPayloadFallback;
    request.expected_provider_event_hash = normalized_payload_hash;
    assert_eq!(
        SqliteCompleteContentResolver::new()
            .resolve(&[request])
            .unwrap()[0]
            .text,
        body
    );

    // Hermes visibility-aware message row.
    let body = long_body("Hermes exact body");
    let path = temp.path().join("hermes.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table sessions (id text, source text, started_at real); \
         create table messages (id integer, session_id text, role text, content text, timestamp real); \
         insert into sessions values ('hermes-session', 'cli', 1.0);",
    )
    .unwrap();
    conn.execute(
        "insert into messages values (1, 'hermes-session', 'user', ?1, 1.0)",
        [body.as_str()],
    )
    .unwrap();
    let values = hermes::load_hermes_message_values(&conn, 1).unwrap();
    let (_, _, normalized_payload_hash, _) =
        hermes::hermes_complete_message_with_normalized_hash(&conn, &values).unwrap();
    drop(conn);
    let mut locator = vec![2];
    locator.extend_from_slice(&1_i64.to_be_bytes());
    let event = synthetic_event("message:1", &body);
    let legacy_request = request_for(
        &path,
        CaptureProvider::Hermes,
        crate::HERMES_SQLITE_SOURCE_FORMAT,
        "hermes-session",
        0,
        HERMES_LOCATOR_KIND,
        locator,
        &values,
        &event,
        &body,
    );
    let mut nativepath_request = legacy_request.clone();
    nativepath_request.expected_hash_authority =
        CompleteContentHashAuthority::NormalizedPayloadFallback;
    nativepath_request.expected_provider_event_hash = normalized_payload_hash;
    assert_eq!(
        SqliteCompleteContentResolver::new()
            .resolve(&[legacy_request])
            .unwrap()[0]
            .text,
        body
    );
    assert_eq!(
        SqliteCompleteContentResolver::new()
            .resolve(&[nativepath_request])
            .unwrap()[0]
            .text,
        body
    );

    // ForgeCode embedded message array; the subrecord coordinate selects one entry.
    let body = long_body("ForgeCode exact body");
    let path = temp.path().join("forge.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations (conversation_id text, workspace_id integer, context text, created_at text);",
    )
    .unwrap();
    let context = serde_json::to_string(&json!({
        "messages": [{"message":{"Text":{"role":"user", "content":body}}}]
    }))
    .unwrap();
    conn.execute(
        "insert into conversations values ('forge-session', 1, ?1, '2026-01-01T00:00:00Z')",
        [context],
    )
    .unwrap();
    let values = forgecode::load_forgecode_conversation_values(&conn, 1).unwrap();
    let (_, event_hash, _) = forgecode::forgecode_complete_message(&values, 0).unwrap();
    drop(conn);
    let event = synthetic_event(&event_hash, &body);
    let mut request = request_for(
        &path,
        CaptureProvider::ForgeCode,
        crate::FORGECODE_SQLITE_SOURCE_FORMAT,
        "forge-session",
        0,
        FORGECODE_LOCATOR_KIND,
        1_i64.to_be_bytes().to_vec(),
        &values,
        &event,
        &body,
    );
    assert_eq!(
        SqliteCompleteContentResolver::new()
            .resolve(std::slice::from_ref(&request))
            .unwrap()[0]
            .text,
        body
    );

    // Reusing the same rowid for different logical content must never return a
    // plausible replacement for the historical message.
    let conn = Connection::open(&path).unwrap();
    conn.execute("delete from conversations where rowid = 1", [])
        .unwrap();
    let replacement = serde_json::to_string(&json!({
        "messages": [{"message":{"Text":{"role":"user", "content":"replacement"}}}]
    }))
    .unwrap();
    conn.execute(
        "insert into conversations(rowid, conversation_id, workspace_id, context, created_at) \
         values (1, 'forge-session', 1, ?1, '2026-01-01T00:00:00Z')",
        [replacement],
    )
    .unwrap();
    drop(conn);
    readmit_sqlite(&mut request, &path, SourceSnapshot::default()).unwrap();
    assert_error_kind(
        &request,
        CompleteContentErrorKind::ContentVerificationFailed,
    );
}

#[test]
fn opencode_nativepath_long_messages_hydrate_both_sqlite_shapes_and_mutation_fails_closed() {
    for shape in [
        opencode::OpenCodeCapturedShape::SessionMessage,
        opencode::OpenCodeCapturedShape::MessagePart,
    ] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let shape_name = match shape {
            opencode::OpenCodeCapturedShape::SessionMessage => "session-message",
            opencode::OpenCodeCapturedShape::MessagePart => "message-part",
            _ => unreachable!("test covers the two released hydration shapes"),
        };
        let source_path = temp.path().join(format!("opencode-{shape_name}.db"));
        let conn = Connection::open(&source_path).unwrap();
        conn.execute_batch(
            "create table session (
                 id text primary key,
                 parent_id text,
                 title text,
                 directory text,
                 time_created integer not null,
                 time_updated integer not null
             );",
        )
        .unwrap();
        conn.execute(
            "insert into session values ('long-session', NULL, 'long', '/tmp', 1, 2)",
            [],
        )
        .unwrap();
        let body = format!(
            "long imported body\n{}",
            "x".repeat(PROVIDER_MAX_TEXT_CHARS + 1024)
        );
        assert!(body.len() > 16 * 1024);
        match shape {
            opencode::OpenCodeCapturedShape::SessionMessage => {
                conn.execute_batch(
                    "create table session_message (
                         id text primary key,
                         session_id text not null,
                         type text not null,
                         seq integer not null,
                         time_created integer not null,
                         time_updated integer not null,
                         data text not null
                     );",
                )
                .unwrap();
                conn.execute(
                    "insert into session_message values (
                         'long-message', 'long-session', 'user', 1, 3, 4, ?1
                     )",
                    [json!({"role": "user", "text": body}).to_string()],
                )
                .unwrap();
            }
            opencode::OpenCodeCapturedShape::MessagePart => {
                conn.execute_batch(
                    "create table message (
                         id text primary key,
                         session_id text not null,
                         time_created integer not null,
                         time_updated integer not null,
                         data text not null
                     );
                     create table part (
                         id text primary key,
                         message_id text not null,
                         session_id text not null,
                         type text,
                         time_created integer not null,
                         time_updated integer not null,
                         data text not null
                     );",
                )
                .unwrap();
                conn.execute(
                    "insert into message values (
                         'long-message', 'long-session', 3, 4,
                         '{\"role\":\"user\",\"time\":{\"created\":3}}'
                     )",
                    [],
                )
                .unwrap();
                conn.execute(
                    "insert into part values (
                         'long-part', 'long-message', 'long-session', 'text', 3, 4, ?1
                     )",
                    [json!({"type": "text", "text": body}).to_string()],
                )
                .unwrap();
            }
            _ => unreachable!("test covers the two released hydration shapes"),
        }
        drop(conn);

        let mut store = Store::open(temp.path().join("store.db")).unwrap();
        let summary = opencode::import_opencode_nativepath(
            &source_path,
            &mut store,
            ProviderAdapterContext {
                machine_id: "opencode-long-message-test".to_owned(),
                source_path: Some(source_path.clone()),
                source_root: Some(temp.path().to_path_buf()),
                imported_at: DateTime::<Utc>::UNIX_EPOCH,
            },
            ProviderImportOptions::default(),
            &opencode::OPENCODE_SQLITE_DIALECT,
        )
        .unwrap();
        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        let session = store.list_sessions().unwrap().pop().unwrap();
        let event = store.events_for_session(session.id).unwrap().pop().unwrap();
        let locators = VerifiedContentLocatorsV1::from_metadata_value(
            &event.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
        )
        .unwrap();
        let locator = locators.locator(VerifiedContentRole::MessageBody).unwrap();
        let hash_authority = match event.sync.metadata["provider_event_hash_authority"].as_str() {
            Some("normalized_payload_fallback") => {
                CompleteContentHashAuthority::NormalizedPayloadFallback
            }
            other => panic!("unexpected OpenCode hash authority: {other:?}"),
        };
        let request = CompleteMessageRequest {
            event_id: event.id,
            provider: CaptureProvider::OpenCode,
            source_format: OPENCODE_SQLITE_SOURCE_FORMAT.to_owned(),
            source_access: sqlite_source_access(
                &source_path,
                CaptureProvider::OpenCode,
                OPENCODE_SQLITE_SOURCE_FORMAT,
                event.id,
            ),
            source_family: Some(CompleteContentSourceFamily::Sqlite),
            content_profile: locator.content_profile().to_owned(),
            source_locator: locator.source_locator(),
            provider_session_id: Some("long-session".to_owned()),
            source_record_ordinal: event.sync.metadata["source_record_ordinal"]
                .as_u64()
                .unwrap(),
            source_record_subrecord_index: 0,
            expected_provider_event_hash: event.sync.metadata["provider_event_hash"]
                .as_str()
                .unwrap()
                .to_owned(),
            expected_hash_authority: hash_authority,
            expected_native_record_id: Some(locator.native_record_id().to_owned()),
            expected_record_digest: Some(locator.record_sha256().clone()),
            expected_content_ref: Some(locator.content_ref().clone()),
            indexed_text: event.payload["text"].as_str().unwrap().to_owned(),
            indexed_limit_chars: PROVIDER_MAX_TEXT_CHARS,
        };
        let hydrated = SqliteCompleteContentResolver::new()
            .resolve(std::slice::from_ref(&request))
            .unwrap();
        assert_eq!(hydrated[0].text, body);

        let mutated = format!(
            "mutated imported body\n{}",
            "z".repeat(PROVIDER_MAX_TEXT_CHARS + 1024)
        );
        let conn = Connection::open(&source_path).unwrap();
        match shape {
            opencode::OpenCodeCapturedShape::SessionMessage => {
                conn.execute(
                    "update session_message set data = ?1 where id = 'long-message'",
                    [json!({"role": "user", "text": mutated}).to_string()],
                )
                .unwrap();
            }
            opencode::OpenCodeCapturedShape::MessagePart => {
                conn.execute(
                    "update part set data = ?1 where id = 'long-part'",
                    [json!({"type": "text", "text": mutated}).to_string()],
                )
                .unwrap();
            }
            _ => unreachable!("test covers the two released hydration shapes"),
        }
        drop(conn);
        let mut mutated_request = request.clone();
        mutated_request.source_access = sqlite_source_access(
            &source_path,
            CaptureProvider::OpenCode,
            OPENCODE_SQLITE_SOURCE_FORMAT,
            event.id,
        );
        let error = SqliteCompleteContentResolver::new()
            .resolve(&[mutated_request])
            .unwrap_err();
        assert_eq!(
            error.kind,
            CompleteContentErrorKind::ContentVerificationFailed
        );
    }
}

#[test]
fn lingma_user_prompt_round_trips_and_changed_row_fails_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("lingma.db");
    let body = long_body("Lingma user prompt");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table chat_record (
            session_id text not null, request_id text, chat_prompt text not null,
            summary text, error_result text, gmt_create integer, extra text
        );",
    )
    .unwrap();
    conn.execute(
        "insert into chat_record values (?1, ?2, ?3, ?4, NULL, ?5, ?6)",
        params![
            "lingma-session",
            "lingma-request",
            body,
            "summary is not an original assistant body",
            1_700_000_000_i64,
            "{}",
        ],
    )
    .unwrap();
    let values = lingma::lingma_complete_values(&conn, 1).unwrap().unwrap();
    let (event, complete_text) = lingma::lingma_complete_user_message(&values).unwrap();
    let released_provider_event_hash = event.released_provider_event_hash.clone();
    let mut event = test_provider_event(
        event.provider_event_index,
        Some(event.provider_event_hash),
        Some(event.cursor),
        event.event_type,
        event.payload,
        json!({}),
    );
    let locator_value = ((1_i64 as u64) ^ (1_u64 << 63)).to_be_bytes().to_vec();
    let locator = NativeLocator::new(LINGMA_LOCATOR_KIND, locator_value.clone()).unwrap();
    attach_test_sqlite_message_locator(
        &mut event,
        CaptureProvider::Lingma,
        LINGMA_SQLITE_SOURCE_FORMAT,
        &locator,
        &values,
        || complete_text.clone(),
    )
    .unwrap();
    let persisted = VerifiedContentLocatorsV1::from_metadata_value(
        &event.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let locator_json = serde_json::to_string(&persisted.to_metadata_value()).unwrap();
    assert!(!locator_json.contains("lingma.db"));
    assert!(!locator_json.contains("Lingma user prompt"));

    let mut request = request_for(
        &path,
        CaptureProvider::Lingma,
        LINGMA_SQLITE_SOURCE_FORMAT,
        "lingma-session",
        0,
        LINGMA_LOCATOR_KIND,
        locator_value,
        &values,
        &event,
        &complete_text,
    );
    request.expected_hash_authority = CompleteContentHashAuthority::NormalizedPayloadFallback;
    let messages = SqliteCompleteContentResolver::new()
        .resolve(std::slice::from_ref(&request))
        .unwrap();
    assert_eq!(messages[0].text, body);

    let mut released_request = request.clone();
    released_request.expected_provider_event_hash = released_provider_event_hash.clone();
    released_request.expected_hash_authority = CompleteContentHashAuthority::ProviderSupplied;
    released_request.expected_native_record_id = Some(released_provider_event_hash);
    let released_messages = SqliteCompleteContentResolver::new()
        .resolve(std::slice::from_ref(&released_request))
        .unwrap();
    assert_eq!(released_messages[0].text, body);

    conn.execute(
        "update chat_record set chat_prompt = ?1 where rowid = 1",
        [body.replacen("Lingma", "changed", 1)],
    )
    .unwrap();
    readmit_sqlite(&mut request, &path, SourceSnapshot::default()).unwrap();
    assert_error_kind(
        &request,
        CompleteContentErrorKind::ContentVerificationFailed,
    );
    readmit_sqlite(&mut released_request, &path, SourceSnapshot::default()).unwrap();
    assert_error_kind(
        &released_request,
        CompleteContentErrorKind::ContentVerificationFailed,
    );
}

#[test]
fn trae_nested_itemtable_message_round_trips_without_storing_parent_body() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("state.vscdb");
    let body = long_body("Trae exact message");
    let raw = json!({
        "list": [{
            "id": "trae-session",
            "messages": [{
                "id": "trae-message",
                "role": "user",
                "content": body.clone(),
                "timestamp": "2026-07-18T00:00:00Z"
            }]
        }]
    })
    .to_string();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("create table ItemTable (key text primary key, value);")
        .unwrap();
    conn.execute(
        "insert into ItemTable (key, value) values (?1, ?2)",
        params![crate::provider::providers::trae::TRAE_CHAT_KEYS[0], raw],
    )
    .unwrap();
    let bytes = trae::trae_complete_value(&conn, 0).unwrap().unwrap();
    let provider_session_id = "workspace/trae-session";
    let (event, complete_text) = trae::trae_complete_message(&bytes, 0, 0, 0, provider_session_id)
        .unwrap()
        .unwrap();
    let mut event = test_provider_event(
        event.provider_event_index,
        Some(event.provider_event_hash),
        Some(event.cursor),
        event.event_type,
        event.payload,
        json!({}),
    );
    let locator = trae::trae_complete_message_locator(0, 0, 0).unwrap();
    attach_sqlite_native_content_locator(
        &mut event,
        CaptureProvider::Trae,
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        &locator,
        &CompleteContentBodyDigest::from_bytes(&bytes),
        &complete_text,
    )
    .unwrap();
    let locators = VerifiedContentLocatorsV1::from_metadata_value(
        &event.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let locator_json = serde_json::to_string(&locators.to_metadata_value()).unwrap();
    assert!(!locator_json.contains("state.vscdb"));
    assert!(!locator_json.contains("Trae exact message"));

    let values = [NativeSqliteValue::Blob(bytes.clone())];
    let mut request = request_for(
        &path,
        CaptureProvider::Trae,
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        provider_session_id,
        0,
        locator.kind(),
        locator.value().to_vec(),
        &values,
        &event,
        &complete_text,
    );
    request.expected_record_digest = Some(CompleteContentBodyDigest::from_bytes(&bytes));
    let messages = SqliteCompleteContentResolver::new()
        .resolve(std::slice::from_ref(&request))
        .unwrap();
    assert_eq!(messages[0].text, body);

    let missing_message_locator = trae::trae_complete_message_locator(0, 0, 1).unwrap();
    request.source_locator = CompleteContentSourceLocator::new(
        missing_message_locator.kind(),
        missing_message_locator.value().to_vec(),
    );
    assert_error_kind(&request, CompleteContentErrorKind::SourceRecordMissing);
}

#[test]
fn astrbot_conversation_message_round_trips_and_binds_original_item() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("data_v4.db");
    let body = long_body("AstrBot exact message");
    let content = json!([
        {"id": "checkpoint", "type": "checkpoint", "checkpoint_id": "cp-1"},
        {"id": "message-1", "role": "user", "content": body.clone()}
    ])
    .to_string();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations (
            id integer primary key, conversation_id text, content text,
            created_at integer, updated_at integer
        );",
    )
    .unwrap();
    conn.execute(
        "insert into conversations values (1, 'astrbot-session', ?1, 1000, 2000)",
        [content],
    )
    .unwrap();
    let values = astrbot::astrbot_complete_conversation_values(&conn, 1)
        .unwrap()
        .unwrap();
    let message = astrbot::astrbot_complete_conversation_message(&values, 1)
        .unwrap()
        .unwrap();
    let complete_text = message.text;
    let provider_session_id = message.provider_session_id;
    let mut event = test_provider_event(
        message.provider_event_index,
        message.provider_event_hash,
        Some(message.cursor),
        message.event_type,
        message.payload,
        json!({}),
    );
    let locator = astrbot::astrbot_complete_message_locator(1, 1).unwrap();
    attach_test_sqlite_message_locator(
        &mut event,
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        &locator,
        &values,
        || complete_text.clone(),
    )
    .unwrap();
    let locators = VerifiedContentLocatorsV1::from_metadata_value(
        &event.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let locator_json = serde_json::to_string(&locators.to_metadata_value()).unwrap();
    assert!(!locator_json.contains("data_v4.db"));
    assert!(!locator_json.contains("AstrBot exact message"));

    let mut request = request_for(
        &path,
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        &provider_session_id,
        0,
        locator.kind(),
        locator.value().to_vec(),
        &values,
        &event,
        &complete_text,
    );
    let messages = SqliteCompleteContentResolver::new()
        .resolve(std::slice::from_ref(&request))
        .unwrap();
    assert_eq!(messages[0].text, body);

    let missing_item_locator = astrbot::astrbot_complete_message_locator(1, 2).unwrap();
    request.source_locator = CompleteContentSourceLocator::new(
        missing_item_locator.kind(),
        missing_item_locator.value().to_vec(),
    );
    assert_error_kind(&request, CompleteContentErrorKind::SourceRecordMissing);
}
