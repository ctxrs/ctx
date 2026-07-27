use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, Event, EventRole, EventType, Fidelity, Session, SyncCursor,
};
use ctx_history_store::{RawSqlOptions, RawSqlValue, Store};
use rusqlite::Connection;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    complete_content::{
        sqlite::SqliteCompleteContentResolver, AuthorizedSourceRoute, CompleteContentHashAuthority,
        CompleteContentResolver, CompleteContentSourceFamily, CompleteMessageRequest,
        SourceAccessBroker, SourceSnapshot, VerifiedContentLocatorsV1, VerifiedContentRole,
        COMPLETE_CONTENT_INDEXED_MESSAGE_LIMIT_CHARS, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    provider::importer::{
        provider_path_identity, provider_source_cursor_stream_for_path,
        provider_source_event_import_identity, provider_sync_metadata, timestamps,
    },
    test_support_paths::tempdir,
    CaptureWorkLimit, ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage,
    ProOutputPageResult, ProOutputProgress, ProOutputSink, ProOutputSinkError,
    ProviderAdapterContext, ProviderImportOptions, ProviderImportWorkResult,
    ASTRBOT_SQLITE_SOURCE_FORMAT,
};

use super::{import_astrbot_nativepath, native_path::released_v025_message_payload};

fn create_database(path: &Path) {
    let conn = Connection::open(path).expect("open fixture");
    conn.execute_batch(
        "pragma user_version = 4;
         create table conversations (
             id integer primary key,
             inner_conversation_id text,
             conversation_id text,
             platform_id text,
             user_id text,
             content text not null,
             title text,
             persona_id text,
             token_usage text,
             created_at integer,
             updated_at integer
         );
         create table platform_message_history (
             id integer primary key,
             platform_id text,
             user_id text,
             sender_id text,
             sender_name text,
             content text,
             llm_checkpoint_id text,
             created_at integer
         );
         create table preferences (scope text, key text, value text);",
    )
    .expect("schema");
}

fn insert_conversation(conn: &Connection, id: i64, session: &str, content: &str) {
    insert_conversation_at(
        conn,
        id,
        session,
        content,
        1_780_000_000_000_i64.saturating_add(id),
    );
}

fn insert_conversation_at(
    conn: &Connection,
    id: i64,
    session: &str,
    content: &str,
    created_at: i64,
) {
    conn.execute(
        "insert into conversations (
             id, inner_conversation_id, conversation_id, platform_id, user_id,
             content, title, persona_id, token_usage, created_at, updated_at
         ) values (?1, ?2, ?3, 'webchat', 'user', ?4, 'title', 'persona',
                   '{\"prompt\":1,\"completion\":2}', ?5, ?6)",
        rusqlite::params![
            id,
            session,
            format!("conversation-{id}"),
            content,
            created_at,
            created_at.saturating_add(1_000),
        ],
    )
    .expect("conversation");
}

fn session_by_external_id(store: &Store, external_session_id: &str) -> Session {
    store
        .list_sessions()
        .expect("sessions")
        .into_iter()
        .find(|session| session.external_session_id.as_deref() == Some(external_session_id))
        .unwrap_or_else(|| panic!("missing session {external_session_id}"))
}

#[allow(clippy::too_many_arguments)]
fn insert_released_v025_event(
    store: &Store,
    session: &Session,
    provider_event_index: u64,
    provider_event_hash: &str,
    cursor: &str,
    role: Option<EventRole>,
    occurred_at_ms: i64,
    released_payload: Value,
    metadata: Value,
) -> Uuid {
    let source_id = session.capture_source_id.expect("capture source");
    let identity =
        provider_source_event_import_identity(source_id, provider_event_index, provider_event_hash);
    let occurred_at = DateTime::<Utc>::from_timestamp_millis(occurred_at_ms).expect("event time");
    let event = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: None,
        session_id: Some(session.id),
        run_id: None,
        event_type: EventType::Message,
        role,
        occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::AstrBot.as_str(),
            "provider_session_id": session.external_session_id,
            "provider_event_index": provider_event_index,
            "provider_event_hash": provider_event_hash,
            "cursor": cursor,
            "artifacts": [],
            "body": released_payload,
        }),
        payload_blob_id: None,
        dedupe_key: Some(identity.dedupe_key),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.external_session_id,
                "provider_event_index": provider_event_index,
                "provider_event_hash": provider_event_hash,
                "cursor": cursor,
                "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "fixture_line": provider_event_index.saturating_add(1),
                "imported_at": occurred_at,
                "event_idempotency_key": format!(
                    "provider-event:{}:{}:{provider_event_index}",
                    CaptureProvider::AstrBot.as_str(),
                    session.external_session_id.as_deref().unwrap_or_default(),
                ),
                "metadata": metadata,
            }),
        ),
    };
    assert!(store
        .insert_event_if_absent(&event)
        .expect("insert released event"));
    event.id
}

fn seed_released_cursor(store: &Store, source: &Path, at: DateTime<Utc>) {
    let canonical = fs::canonicalize(source).expect("canonical source");
    let locator = provider_path_identity(&canonical).expect("locator identity");
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        &locator,
    );
    store
        .upsert_sync_cursor(&SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: "astrbot-nativepath-test".to_owned(),
            stream,
            cursor: "released-v025-opaque-cursor".to_owned(),
            last_synced_at: Some(at),
            timestamps: timestamps(at),
        })
        .expect("released cursor");
}

fn sql_count(store: &Store, sql: &str) -> i64 {
    let result = store
        .raw_sql_query(sql, RawSqlOptions::default())
        .expect("SQL query");
    match result.rows.as_slice() {
        [row] => match row.as_slice() {
            [RawSqlValue::Integer(value)] => *value,
            values => panic!("unexpected SQL values: {values:?}"),
        },
        rows => panic!("unexpected SQL rows: {rows:?}"),
    }
}

fn relative_path(from: &Path, to: &Path) -> PathBuf {
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in common..from.len() {
        relative.push("..");
    }
    for component in &to[common..] {
        relative.push(component.as_os_str());
    }
    relative
}

fn context(path: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "astrbot-nativepath-test".to_owned(),
        source_path: Some(path.to_path_buf()),
        source_root: None,
        imported_at: DateTime::<Utc>::from_timestamp_millis(1_790_000_000_000).expect("timestamp"),
    }
}

fn options(profile: ImportProfile) -> ProviderImportOptions {
    ProviderImportOptions {
        history_record_id: None,
        capture_work_limit: CaptureWorkLimit::Drain,
        inventory_observation_token: None,
        import_profile: profile,
    }
}

#[derive(Default)]
struct RecordingSink {
    state: Mutex<Option<ProOutputProgress>>,
    content: Mutex<Vec<Vec<u8>>>,
    behind: Mutex<Vec<&'static str>>,
    fail_materialization: bool,
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        7
    }

    fn materializer_revision(&self) -> &str {
        "astrbot-test-materializer-v1"
    }

    fn observe_source(
        &self,
        _source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(self.state.lock().expect("state").clone())
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        if self.fail_materialization {
            return Err(ProOutputSinkError::new(
                "astrbot_test_output_failure",
                "retry this output page",
            ));
        }
        let accepted_outputs = u32::try_from(page.observations.len()).unwrap_or(u32::MAX);
        self.content.lock().expect("content").extend(
            page.observations
                .iter()
                .map(|output| output.content.clone()),
        );
        *self.state.lock().expect("state") = Some(ProOutputProgress {
            source_epoch: page.source_epoch,
            observed_revision: page.observed_revision.clone(),
            cursor: Some(page.next_safe_cursor.clone()),
            parser_revision: page.parser_revision.clone(),
            materializer_revision: page.materializer_revision.clone(),
            terminal: page.terminal,
        });
        Ok(ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor: page.next_safe_cursor,
            accepted_outputs,
            materialized_facts: accepted_outputs,
            replayed: false,
        })
    }

    fn mark_behind(&self, error: ProOutputSinkError) {
        self.behind.lock().expect("behind").push(error.code);
    }
}

#[test]
fn astrbot_nativepath_core_is_idempotent_and_replays_outputs_later() {
    let temp = tempdir().expect("temp");
    let source = temp.path().join("data_v4.db");
    create_database(&source);
    let conn = Connection::open(&source).expect("open");
    insert_conversation(
        &conn,
        1,
        "session-1",
        r#"[
            {"role":"user","content":"stable-user-text"},
            {"role":"tool","id":"tool-1","success":true,"content":"PRO_OUTPUT_SECRET"},
            {"role":"assistant","content":"stable-assistant-text"}
        ]"#,
    );
    drop(conn);

    let mut store = Store::open(temp.path().join("work.sqlite")).expect("store");
    let first = import_astrbot_nativepath(
        &source,
        &mut store,
        context(&source),
        options(ImportProfile::CoreOnly),
    )
    .expect("core import");
    assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
    let sessions = store.list_sessions().expect("sessions");
    assert_eq!(sessions.len(), 1);
    let events = store.events_for_session(sessions[0].id).expect("events");
    assert_eq!(events.len(), 2, "successful output must not enter Core");
    assert!(events
        .iter()
        .all(|event| !event.payload.to_string().contains("PRO_OUTPUT_SECRET")));

    let replay = import_astrbot_nativepath(
        &source,
        &mut store,
        context(&source),
        options(ImportProfile::CoreOnly),
    )
    .expect("idempotent replay");
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);

    let sink = Arc::new(RecordingSink::default());
    import_astrbot_nativepath(
        &source,
        &mut store,
        context(&source),
        options(ImportProfile::ProReplayOnly(sink.clone())),
    )
    .expect("Pro replay");
    assert_eq!(
        sink.content.lock().expect("content").as_slice(),
        &[b"PRO_OUTPUT_SECRET".to_vec()]
    );

    let failing_sink = Arc::new(RecordingSink {
        fail_materialization: true,
        ..RecordingSink::default()
    });
    import_astrbot_nativepath(
        &source,
        &mut store,
        context(&source),
        options(ImportProfile::ProReplayOnly(failing_sink.clone())),
    )
    .expect("Pro failure must not fail committed Core");
    assert_eq!(
        failing_sink.behind.lock().expect("behind").as_slice(),
        &["astrbot_test_output_failure"]
    );
    assert_eq!(store.list_sessions().expect("Core retained").len(), 1);
}

#[test]
fn astrbot_nativepath_retains_typed_failures_without_output_content() {
    const FAILURE_SECRET: &str = "ASTRBOT_CORE_FAILURE_SECRET";

    let temp = tempdir().expect("temp");
    let source = temp.path().join("data_v4.db");
    create_database(&source);
    let conn = Connection::open(&source).expect("open");
    insert_conversation(
        &conn,
        1,
        "failure-session",
        &json!([
            {"role": "user", "content": "before failure"},
            {
                "role": "tool",
                "id": "failed-tool",
                "success": false,
                "content": FAILURE_SECRET,
            },
        ])
        .to_string(),
    );
    drop(conn);

    let mut store = Store::open(temp.path().join("work.sqlite")).expect("store");
    let summary = import_astrbot_nativepath(
        &source,
        &mut store,
        context(&source),
        options(ImportProfile::CoreOnly),
    )
    .expect("core import");
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);

    let session = session_by_external_id(&store, "failure-session");
    let events = store.events_for_session(session.id).expect("events");
    let failure = events
        .iter()
        .find(|event| event.event_type == EventType::ToolOutput)
        .expect("typed failure event");
    assert_eq!(failure.role, Some(EventRole::Tool));
    assert_eq!(failure.payload["body"]["result_outcome"], json!("failure"));
    assert!(!failure.payload.to_string().contains(FAILURE_SECRET));
    assert_eq!(
        store
            .search_event_hits(FAILURE_SECRET, 10)
            .expect("search")
            .len(),
        0
    );
}

#[test]
fn astrbot_nativepath_restarts_from_safe_pages_without_duplicate_events() {
    let temp = tempdir().expect("temp");
    let source = temp.path().join("data_v4.db");
    let store_path = temp.path().join("work.sqlite");
    create_database(&source);
    let conn = Connection::open(&source).expect("open");
    for id in 1..=129 {
        insert_conversation(
            &conn,
            id,
            &format!("restart-session-{id}"),
            &json!([{"role": "user", "content": format!("restart-message-{id}")}]).to_string(),
        );
    }
    drop(conn);

    let mut import_options = options(ImportProfile::CoreOnly);
    import_options.capture_work_limit = CaptureWorkLimit::OneSafeGroup;
    let mut store = Store::open(&store_path).expect("initial store");
    let first = import_astrbot_nativepath(
        &source,
        &mut store,
        context(&source),
        import_options.clone(),
    )
    .expect("first page");
    assert!(first.work_remaining);
    drop(store);

    let mut store = Store::open(&store_path).expect("restart store");
    let mut drained = false;
    for attempt in 0..4 {
        let summary = import_astrbot_nativepath(
            &source,
            &mut store,
            context(&source),
            import_options.clone(),
        )
        .expect("restart page");
        if !summary.work_remaining {
            drained = true;
            break;
        }
        assert!(attempt < 3, "restart did not drain bounded pages");
    }
    assert!(drained, "restart did not reach a terminal cursor");
    assert_eq!(store.list_sessions().expect("sessions").len(), 129);
    assert_eq!(
        sql_count(&store, "select count(*) from events"),
        129,
        "safe-page restart must neither omit nor duplicate events"
    );
    let replay = import_astrbot_nativepath(
        &source,
        &mut store,
        context(&source),
        options(ImportProfile::CoreOnly),
    )
    .expect("terminal replay");
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn astrbot_nativepath_rejects_a_core_unit_that_exceeds_page_bound() {
    let temp = tempdir().expect("temp");
    let source = temp.path().join("data_v4.db");
    create_database(&source);
    let conn = Connection::open(&source).expect("open");
    insert_conversation(
        &conn,
        1,
        "oversize-core-unit",
        r#"[{"role":"user","content":"discarded-with-oversize-metadata"}]"#,
    );
    conn.execute(
        "update conversations set token_usage = ?1 where id = 1",
        ["x".repeat(9 * 1024 * 1024)],
    )
    .expect("oversize metadata");
    insert_conversation(
        &conn,
        2,
        "retained-after-rejection",
        r#"[{"role":"user","content":"bounded-following-message"}]"#,
    );
    drop(conn);

    let mut store = Store::open(temp.path().join("work.sqlite")).expect("store");
    let summary = import_astrbot_nativepath(
        &source,
        &mut store,
        context(&source),
        options(ImportProfile::CoreOnly),
    )
    .expect("bounded rejection import");
    assert_eq!(summary.failed, 1);
    assert!(summary.failures.iter().any(|failure| {
        failure.error == "AstrBot conversation record exceeds the bounded Core publication page"
    }));
    assert!(store
        .list_sessions()
        .expect("sessions")
        .iter()
        .all(|session| session.external_session_id.as_deref() != Some("oversize-core-unit")));
    assert_eq!(
        store
            .search_event_hits("bounded-following-message", 10)
            .expect("search")
            .len(),
        1
    );
}

#[test]
fn astrbot_nativepath_append_and_rewrite_keep_stable_session_identity() {
    let temp = tempdir().expect("temp");
    let source = temp.path().join("data_v4.db");
    create_database(&source);
    let conn = Connection::open(&source).expect("open");
    insert_conversation(
        &conn,
        1,
        "session-1",
        r#"[{"role":"user","content":"before"}]"#,
    );
    drop(conn);
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("store");
    import_astrbot_nativepath(
        &source,
        &mut store,
        context(&source),
        options(ImportProfile::CoreOnly),
    )
    .expect("first");
    let original_session = store.list_sessions().expect("sessions")[0].id;

    let conn = Connection::open(&source).expect("open append");
    insert_conversation(
        &conn,
        2,
        "session-2",
        r#"[{"role":"assistant","content":"appended"}]"#,
    );
    drop(conn);
    import_astrbot_nativepath(
        &source,
        &mut store,
        context(&source),
        options(ImportProfile::CoreOnly),
    )
    .expect("append");
    assert_eq!(store.list_sessions().expect("sessions").len(), 2);

    let conn = Connection::open(&source).expect("open rewrite");
    conn.execute(
        "update conversations set content = ?1 where id = 1",
        [r#"[{"role":"user","content":"after-rewrite"}]"#],
    )
    .expect("rewrite");
    drop(conn);
    import_astrbot_nativepath(
        &source,
        &mut store,
        context(&source),
        options(ImportProfile::CoreOnly),
    )
    .expect("rewrite import");
    let session = store.get_session(original_session).expect("stable session");
    let events = store.events_for_session(session.id).expect("events");
    assert!(events
        .iter()
        .any(|event| event.payload.to_string().contains("after-rewrite")));
}

#[test]
fn astrbot_nativepath_retires_a_missing_source_route_without_deleting_core() {
    let temp = tempdir().expect("temp");
    let source = temp.path().join("data_v4.db");
    create_database(&source);
    let conn = Connection::open(&source).expect("open");
    insert_conversation(
        &conn,
        1,
        "session-1",
        r#"[{"role":"user","content":"retained-history"}]"#,
    );
    drop(conn);
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("store");
    import_astrbot_nativepath(
        &source,
        &mut store,
        context(&source),
        options(ImportProfile::CoreOnly),
    )
    .expect("import");
    let moved = temp.path().join("removed.db");
    fs::rename(&source, moved).expect("remove source");
    let retirement = import_astrbot_nativepath(
        &source,
        &mut store,
        context(&source),
        options(ImportProfile::CoreOnly),
    )
    .expect("retire");
    assert_eq!(retirement.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(store.list_sessions().expect("history retained").len(), 1);
    let retained_session = store.list_sessions().expect("retained sessions")[0].id;
    let event = store
        .events_for_session(retained_session)
        .expect("retained events")[0]
        .clone();
    assert!(store.authorized_source_route_for_event(event.id).is_err());
    assert!(store.get_event(event.id).is_ok());
}

#[test]
fn astrbot_nativepath_upgrades_exact_v025_hashes_and_scrubs_released_output_everywhere() {
    const SECRET: &str = "astrbotv025outputsecret";
    const CHANGED_SECRET: &str = "astrbotv025changedoutputsecret";

    let temp = tempdir().expect("temp");
    let source = temp.path().join("data_v4.db");
    create_database(&source);
    let user_item = json!({
        "id": "array-message-1",
        "role": "user",
        "content": "released-array-message",
    });
    let output_item = json!({
        "id": "tool-message-1",
        "role": "tool",
        "success": true,
        "content": SECRET,
    });
    let conn = Connection::open(&source).expect("open source");
    insert_conversation_at(
        &conn,
        1,
        "session-array",
        &json!([user_item, output_item]).to_string(),
        1_780_000_000_100,
    );
    insert_conversation_at(
        &conn,
        2,
        "session-scalar",
        "released-scalar-message",
        1_780_000_000_200,
    );
    conn.execute(
        "insert into platform_message_history (
             id, platform_id, user_id, sender_id, sender_name, content,
             llm_checkpoint_id, created_at
         ) values (7, 'webchat', 'platform-user', 'platform-user', 'User',
                   'released-platform-message', null, 1780000000300)",
        [],
    )
    .expect("platform row");
    drop(conn);

    let mut template = Store::open(temp.path().join("template.sqlite")).expect("template store");
    import_astrbot_nativepath(
        &source,
        &mut template,
        context(&source),
        options(ImportProfile::CoreOnly),
    )
    .expect("template import");

    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).expect("historical store");
    for source in template.list_capture_sources().expect("template sources") {
        store.upsert_capture_source(&source).expect("copy source");
    }
    for session in template.list_sessions().expect("template sessions") {
        store.upsert_session(&session).expect("copy session");
    }

    let array_session = session_by_external_id(&store, "session-array");
    let scalar_session = session_by_external_id(&store, "session-scalar");
    let platform_session = session_by_external_id(&store, "platform/webchat/platform-user");
    let user_item = json!({
        "id": "array-message-1",
        "role": "user",
        "content": "released-array-message",
    });
    let output_item = json!({
        "id": "tool-message-1",
        "role": "tool",
        "success": true,
        "content": SECRET,
    });
    let array_id = insert_released_v025_event(
        &store,
        &array_session,
        0,
        "conversation:array-message-1",
        "conversation:conversation-1:item:0",
        Some(EventRole::User),
        1_780_000_000_100,
        released_v025_message_payload("released-array-message", &user_item),
        json!({
            "source": "astrbot_conversations",
            "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
            "conversation_id": "conversation-1",
            "inner_conversation_id": "session-array",
            "item_index": 0,
        }),
    );
    let output_id = insert_released_v025_event(
        &store,
        &array_session,
        1,
        "conversation:tool-message-1",
        "conversation:conversation-1:item:1",
        Some(EventRole::Tool),
        1_780_000_000_100,
        released_v025_message_payload(SECRET, &output_item),
        json!({
            "source": "astrbot_conversations",
            "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
            "conversation_id": "conversation-1",
            "inner_conversation_id": "session-array",
            "item_index": 1,
        }),
    );
    let scalar_body = Value::String("released-scalar-message".to_owned());
    let scalar_id = insert_released_v025_event(
        &store,
        &scalar_session,
        0,
        "conversation-row:2",
        "conversation:conversation-2:content",
        None,
        1_780_000_000_200,
        released_v025_message_payload("released-scalar-message", &scalar_body),
        json!({
            "source": "astrbot_conversations",
            "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
            "conversation_id": "conversation-2",
        }),
    );
    let platform_body = json!({
        "message_id": 7,
        "platform_id": "webchat",
        "user_id": "platform-user",
        "sender_id": "platform-user",
        "sender_name": "User",
        "content": "released-platform-message",
        "llm_checkpoint_id": Value::Null,
    });
    let platform_id = insert_released_v025_event(
        &store,
        &platform_session,
        1_000_007,
        "platform-message:7",
        "platform_message_history:id:7",
        Some(EventRole::User),
        1_780_000_000_300,
        released_v025_message_payload("released-platform-message", &platform_body),
        json!({
            "source": "astrbot_platform_message_history",
            "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
            "message_id": 7,
        }),
    );
    seed_released_cursor(&store, &source, context(&source).imported_at);
    assert_eq!(
        sql_count(
            &store,
            "select count(*) from ctx_events
             where payload_json like '%astrbotv025outputsecret%'"
        ),
        1,
        "the exact released event must contain the privacy leak before upgrade"
    );

    let upgraded = import_astrbot_nativepath(
        &source,
        &mut store,
        context(&source),
        options(ImportProfile::CoreOnly),
    )
    .expect("upgrade released store");
    assert_eq!(upgraded.failed, 0);

    for (id, expected) in [
        (array_id, "released-array-message"),
        (scalar_id, "released-scalar-message"),
        (platform_id, "released-platform-message"),
    ] {
        let event = store.get_event(id).expect("migrated event keeps identity");
        assert!(event.payload.to_string().contains(expected));
        assert_eq!(
            event.sync.metadata["provider_event_hash_authority"],
            json!("normalized_payload_fallback"),
            "{expected}"
        );
        assert!(event.sync.deleted_at.is_none());
    }
    let scrubbed = store
        .get_event(output_id)
        .expect("scrubbed output identity");
    assert_eq!(scrubbed.event_type, EventType::Message);
    assert!(scrubbed.sync.deleted_at.is_some());
    assert_eq!(
        scrubbed.sync.metadata["retired_by"],
        json!("astrbot_v025_output_scrub")
    );
    assert!(!scrubbed.payload.to_string().contains(SECRET));
    assert_eq!(
        store
            .search_event_hits(SECRET, 10)
            .expect("new search")
            .len(),
        0
    );
    assert_eq!(
        sql_count(
            &store,
            "select count(*) from events
             where payload_json like '%astrbotv025outputsecret%'
                or metadata_json like '%astrbotv025outputsecret%'"
        ),
        0
    );

    let replay = import_astrbot_nativepath(
        &source,
        &mut store,
        context(&source),
        options(ImportProfile::CoreOnly),
    )
    .expect("unchanged replay");
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);

    let changed_output = json!({
        "id": "tool-message-1",
        "role": "tool",
        "success": true,
        "content": CHANGED_SECRET,
    });
    let appended = json!({
        "id": "array-message-2",
        "role": "assistant",
        "content": "post-upgrade-append",
    });
    let conn = Connection::open(&source).expect("open changed source");
    conn.execute(
        "update conversations set content = ?1 where id = 1",
        [json!([user_item, changed_output, appended]).to_string()],
    )
    .expect("change and append");
    drop(conn);
    import_astrbot_nativepath(
        &source,
        &mut store,
        context(&source),
        options(ImportProfile::CoreOnly),
    )
    .expect("changed replay");
    assert_eq!(
        store
            .search_event_hits(CHANGED_SECRET, 10)
            .expect("changed secret search")
            .len(),
        0
    );
    assert_eq!(
        store
            .search_event_hits("post-upgrade-append", 10)
            .expect("append search")
            .len(),
        1
    );
    assert_eq!(
        sql_count(
            &store,
            "select count(*) from events
             where payload_json like '%astrbotv025outputsecret%'
                or payload_json like '%astrbotv025changedoutputsecret%'
                or metadata_json like '%astrbotv025outputsecret%'
                or metadata_json like '%astrbotv025changedoutputsecret%'"
        ),
        0
    );
    assert_eq!(store.get_event(output_id).expect("tombstone").id, output_id);
}

#[test]
fn astrbot_nativepath_accepts_timestamp_inversions_with_physical_keysets() {
    let temp = tempdir().expect("temp");
    let source = temp.path().join("data_v4.db");
    create_database(&source);
    let conn = Connection::open(&source).expect("open");
    insert_conversation_at(
        &conn,
        1,
        "newer-physical-first",
        r#"[{"role":"user","content":"newer-conversation"}]"#,
        2_000,
    );
    insert_conversation_at(
        &conn,
        2,
        "older-backfill-second",
        r#"[{"role":"user","content":"older-conversation"}]"#,
        1_000,
    );
    conn.execute_batch(
        "insert into platform_message_history values
             (1, 'webchat', 'same-user', 'same-user', 'User',
              'newer-platform', null, 4000),
             (2, 'webchat', 'same-user', 'same-user', 'User',
              'older-platform', null, 3000);",
    )
    .expect("platform inversions");
    drop(conn);

    let mut store = Store::open(temp.path().join("work.sqlite")).expect("store");
    let summary = import_astrbot_nativepath(
        &source,
        &mut store,
        context(&source),
        options(ImportProfile::CoreOnly),
    )
    .expect("inversion import");
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    for text in [
        "newer-conversation",
        "older-conversation",
        "newer-platform",
        "older-platform",
    ] {
        assert_eq!(
            store.search_event_hits(text, 10).expect("search").len(),
            1,
            "{text}"
        );
    }
    let sessions = store.list_sessions().expect("sessions");
    let ordered = sessions
        .iter()
        .filter_map(|session| session.external_session_id.as_deref())
        .collect::<Vec<_>>();
    assert!(
        ordered.iter().position(|id| *id == "older-backfill-second")
            < ordered.iter().position(|id| *id == "newer-physical-first"),
        "released timestamp presentation order must not follow rowid: {ordered:?}"
    );
    let platform = session_by_external_id(&store, "platform/webchat/same-user");
    let platform_events = store
        .events_for_session(platform.id)
        .expect("platform events");
    assert_eq!(platform_events.len(), 2);
    assert!(platform_events[0].seq < platform_events[1].seq);

    let replay = import_astrbot_nativepath(
        &source,
        &mut store,
        context(&source),
        options(ImportProfile::CoreOnly),
    )
    .expect("inversion replay");
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn astrbot_nativepath_canonicalizes_relative_source_authority() {
    let temp = tempdir().expect("temp");
    let source = temp.path().join("relative/data_v4.db");
    fs::create_dir_all(source.parent().expect("parent")).expect("source parent");
    create_database(&source);
    let complete_body = format!(
        "relative-hydration-message{}",
        "x".repeat(COMPLETE_CONTENT_INDEXED_MESSAGE_LIMIT_CHARS + 32)
    );
    let conn = Connection::open(&source).expect("open");
    insert_conversation(
        &conn,
        1,
        "relative-session",
        &json!([{
            "id": "relative-message",
            "role": "user",
            "content": complete_body.clone(),
        }])
        .to_string(),
    );
    drop(conn);
    let current_dir = std::env::current_dir().expect("current directory");
    let relative = relative_path(&current_dir, &source);
    assert!(!relative.is_absolute());

    let mut store = Store::open(temp.path().join("work.sqlite")).expect("store");
    import_astrbot_nativepath(
        &relative,
        &mut store,
        context(&relative),
        options(ImportProfile::CoreOnly),
    )
    .expect("relative import");
    let canonical = fs::canonicalize(&source).expect("canonical source");
    let session = session_by_external_id(&store, "relative-session");
    let source_id = session.capture_source_id.expect("source id");
    let captured = store.get_capture_source(source_id).expect("capture source");
    assert_eq!(
        captured.descriptor.raw_source_path.as_deref(),
        Some(canonical.to_string_lossy().as_ref())
    );
    assert!(Path::new(
        captured
            .descriptor
            .source_root
            .as_deref()
            .expect("source root")
    )
    .is_absolute());
    let event = store.events_for_session(session.id).expect("events")[0].clone();
    assert_eq!(
        store
            .authorized_source_route_for_event(event.id)
            .expect("authorized route")
            .path(),
        canonical
    );

    let original_source_id = source_id;
    let original_session_id = session.id;
    let original_event_id = event.id;
    import_astrbot_nativepath(
        &canonical,
        &mut store,
        context(&canonical),
        options(ImportProfile::CoreOnly),
    )
    .expect("absolute reimport");
    let absolute_session = session_by_external_id(&store, "relative-session");
    assert_eq!(absolute_session.capture_source_id, Some(original_source_id));
    assert_eq!(absolute_session.id, original_session_id);
    let absolute_event = store
        .events_for_session(absolute_session.id)
        .expect("events")[0]
        .clone();
    assert_eq!(absolute_event.id, original_event_id);

    let locators = VerifiedContentLocatorsV1::from_metadata_value(
        absolute_event
            .sync
            .metadata
            .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
            .expect("message locator metadata"),
    )
    .expect("valid message locators");
    let persisted = locators
        .locator(VerifiedContentRole::MessageBody)
        .expect("message body locator")
        .clone();
    let route = store
        .authorized_source_route_for_event(absolute_event.id)
        .expect("absolute authorized route");
    let source_access = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: route.capture_source_id(),
                provider: route.provider(),
                source_format: route.source_format().to_owned(),
                family: CompleteContentSourceFamily::Sqlite,
                raw_source_path: route.path().to_path_buf(),
                source_root: captured
                    .descriptor
                    .source_root
                    .as_deref()
                    .map(PathBuf::from),
                source_identity: Some(route.canonical_source_identity().to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            absolute_event.id,
        )
        .expect("admit canonical source");
    let request = CompleteMessageRequest {
        event_id: absolute_event.id,
        provider: route.provider(),
        source_format: route.source_format().to_owned(),
        source_access,
        source_family: Some(persisted.family()),
        content_profile: persisted.content_profile().to_owned(),
        source_locator: persisted.source_locator(),
        provider_session_id: absolute_session.external_session_id.clone(),
        source_record_ordinal: absolute_event.sync.metadata["source_record_ordinal"]
            .as_u64()
            .expect("source ordinal"),
        source_record_subrecord_index: u32::try_from(
            absolute_event.sync.metadata["source_record_subrecord_index"]
                .as_u64()
                .expect("source subrecord"),
        )
        .expect("bounded subrecord"),
        expected_provider_event_hash: absolute_event.sync.metadata["provider_event_hash"]
            .as_str()
            .expect("provider event hash")
            .to_owned(),
        expected_hash_authority: CompleteContentHashAuthority::NormalizedPayloadFallback,
        expected_native_record_id: Some(persisted.native_record_id().to_owned()),
        expected_record_digest: Some(persisted.record_sha256().clone()),
        expected_content_ref: Some(persisted.content_ref().clone()),
        indexed_text: absolute_event.payload["text"]
            .as_str()
            .expect("indexed text")
            .to_owned(),
        indexed_limit_chars: COMPLETE_CONTENT_INDEXED_MESSAGE_LIMIT_CHARS,
    };
    let hydrated = SqliteCompleteContentResolver::new()
        .resolve(&[request])
        .expect("hydrate after relative-to-absolute reimport");
    assert_eq!(hydrated.len(), 1);
    assert_eq!(hydrated[0].text, complete_body);
}
