use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use ctx_history_store::Store;
use rusqlite::Connection;

use crate::{
    test_support_paths::tempdir, CaptureWorkLimit, ImportProfile, OutputSourceIdentity,
    ProOutputMaterializationPage, ProOutputPageResult, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProviderAdapterContext, ProviderImportOptions, ProviderImportWorkResult,
};

use super::import_astrbot_nativepath;

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
            1_780_000_000_000_i64.saturating_add(id),
            1_780_000_001_000_i64.saturating_add(id),
        ],
    )
    .expect("conversation");
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
}
