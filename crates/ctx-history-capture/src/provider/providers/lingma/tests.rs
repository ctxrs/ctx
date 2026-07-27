use std::sync::{Arc, Mutex};

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use rusqlite::Connection;

use crate::{
    import_lingma_sqlite, CaptureWorkLimit, ImportProfile, LingmaSqliteImportOptions,
    OutputSourceIdentity, ProOutputMaterializationPage, ProOutputPageResult, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProviderImportWorkResult,
};

const MACHINE: &str = "lingma-nativepath-test-machine";

fn create_db(path: &std::path::Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table chat_record (
                session_id text not null,
                request_id text,
                chat_prompt text,
                summary text,
                error_result text,
                gmt_create integer,
                extra text
             );",
        )
        .unwrap();
    connection
}

#[allow(clippy::too_many_arguments)]
fn insert_row(
    connection: &Connection,
    session_id: &str,
    request_id: &str,
    prompt: &str,
    summary: Option<&str>,
    error: Option<&str>,
    timestamp: i64,
    extra: Option<&str>,
) {
    connection
        .execute(
            "insert into chat_record (
                session_id, request_id, chat_prompt, summary, error_result, gmt_create, extra
             ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![session_id, request_id, prompt, summary, error, timestamp, extra],
        )
        .unwrap();
}

fn options(profile: ImportProfile) -> LingmaSqliteImportOptions {
    LingmaSqliteImportOptions {
        machine_id: MACHINE.to_owned(),
        import_profile: profile,
        ..LingmaSqliteImportOptions::default()
    }
}

fn lingma_events(store: &Store) -> Vec<ctx_history_core::Event> {
    let mut events = Vec::new();
    for session in store
        .list_sessions()
        .unwrap()
        .into_iter()
        .filter(|session| session.provider == CaptureProvider::Lingma)
    {
        events.extend(store.events_for_session(session.id).unwrap());
    }
    events.sort_by_key(|event| event.seq);
    events
}

#[test]
fn nativepath_lifecycle_is_idempotent_and_excludes_unclassified_extra_from_core() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let db = temp.path().join("local.db");
    let connection = create_db(&db);
    insert_row(
        &connection,
        "session-a",
        "request-a",
        "first prompt",
        Some("first assistant summary"),
        None,
        1_700_000_000,
        Some("CTX_LINGMA_PRIVATE_OUTPUT_BODY"),
    );
    drop(connection);
    let store_path = temp.path().join("work.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let fresh = import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(fresh.imported_events, 2);
    let original = lingma_events(&store);
    assert_eq!(original.len(), 2);
    assert!(original.iter().all(|event| {
        !serde_json::to_string(&(event.payload.clone(), event.sync.metadata.clone()))
            .unwrap()
            .contains("CTX_LINGMA_PRIVATE_OUTPUT_BODY")
    }));
    let routed_event = original[0].id;
    store
        .authorized_source_route_for_event(routed_event)
        .unwrap();

    let noop = import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);
    drop(store);
    let mut store = Store::open(&store_path).unwrap();
    assert_eq!(
        import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly))
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::NoOp
    );

    let connection = Connection::open(&db).unwrap();
    insert_row(
        &connection,
        "session-a",
        "request-b",
        "appended prompt",
        Some("appended summary"),
        None,
        1_700_000_001,
        None,
    );
    drop(connection);
    let append = import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(lingma_events(&store).len(), 4);

    let connection = Connection::open(&db).unwrap();
    connection
        .execute(
            "update chat_record set chat_prompt = 'rewritten prompt' where rowid = 1",
            [],
        )
        .unwrap();
    drop(connection);
    let rewrite = import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    assert_eq!(rewrite.work_result(), ProviderImportWorkResult::Changed);
    assert!(serde_json::to_string(&lingma_events(&store))
        .unwrap()
        .contains("rewritten prompt"));

    let connection = Connection::open(&db).unwrap();
    connection
        .execute("delete from chat_record where rowid = 2", [])
        .unwrap();
    drop(connection);
    assert_eq!(
        import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly))
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::Changed
    );

    let replacement = temp.path().join("replacement.db");
    let replacement_connection = create_db(&replacement);
    insert_row(
        &replacement_connection,
        "session-replacement",
        "request-replacement",
        "replacement prompt",
        Some("replacement summary"),
        None,
        1_700_000_100,
        None,
    );
    drop(replacement_connection);
    std::fs::rename(&replacement, &db).unwrap();
    assert_eq!(
        import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly))
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(serde_json::to_string(&lingma_events(&store))
        .unwrap()
        .contains("replacement prompt"));

    std::fs::remove_file(&db).unwrap();
    let retired = import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    assert!(store
        .authorized_source_route_for_event(routed_event)
        .is_err());
    assert_eq!(
        import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly))
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[test]
fn one_safe_group_resumes_without_replaying_the_committed_prefix() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let db = temp.path().join("local.db");
    let connection = create_db(&db);
    for index in 0..70 {
        insert_row(
            &connection,
            "session",
            &format!("request-{index}"),
            &format!("prompt-{index}"),
            Some(&format!("summary-{index}")),
            None,
            1_700_000_000 + index,
            None,
        );
    }
    drop(connection);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let mut first_options = options(ImportProfile::CoreOnly);
    first_options.capture_work_limit = CaptureWorkLimit::OneSafeGroup;
    let first = import_lingma_sqlite(&db, &mut store, first_options).unwrap();
    assert!(first.work_remaining);
    assert_eq!(lingma_events(&store).len(), 128);

    let replay = Arc::new(RecordingSink::default());
    import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert!(replay.pages.lock().unwrap().is_empty());

    let second = import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    assert!(!second.work_remaining);
    assert_eq!(lingma_events(&store).len(), 140);
    import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert_eq!(replay.pages.lock().unwrap().len(), 1);
    assert_eq!(
        import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly))
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::NoOp
    );
}

#[derive(Default)]
struct RecordingSink {
    fail: bool,
    pages: Mutex<Vec<ProOutputMaterializationPage>>,
    progress: Mutex<Option<ProOutputProgress>>,
}

impl RecordingSink {
    fn failing() -> Self {
        Self {
            fail: true,
            ..Self::default()
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        7
    }

    fn materializer_revision(&self) -> &str {
        "test-materializer-v1"
    }

    fn observe_source(
        &self,
        _source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(self.progress.lock().unwrap().clone())
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        if self.fail {
            return Err(ProOutputSinkError::new("injected", "injected Pro failure"));
        }
        assert!(page.observations.is_empty());
        assert!(page.terminal);
        let result = ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor: page.next_safe_cursor.clone(),
            accepted_outputs: 0,
            materialized_facts: 0,
            replayed: false,
        };
        *self.progress.lock().unwrap() = Some(ProOutputProgress {
            source_epoch: page.source_epoch,
            observed_revision: page.observed_revision.clone(),
            cursor: Some(page.next_safe_cursor.clone()),
            parser_revision: page.parser_revision.clone(),
            materializer_revision: page.materializer_revision.clone(),
            terminal: page.terminal,
        });
        self.pages.lock().unwrap().push(page);
        Ok(result)
    }
}

#[test]
fn pro_failure_never_blocks_core_and_later_activation_replays_independently() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let db = temp.path().join("local.db");
    let connection = create_db(&db);
    insert_row(
        &connection,
        "session",
        "request",
        "prompt",
        Some("summary"),
        None,
        1_700_000_000,
        Some("private unclassified result"),
    );
    drop(connection);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let mut empty_store = Store::open(temp.path().join("empty.sqlite")).unwrap();
    let empty_replay = Arc::new(RecordingSink::default());
    let empty_summary = import_lingma_sqlite(
        &db,
        &mut empty_store,
        options(ImportProfile::ProReplayOnly(empty_replay.clone())),
    )
    .unwrap();
    assert_eq!(empty_summary.work_result(), ProviderImportWorkResult::NoOp);
    assert!(empty_store.list_sessions().unwrap().is_empty());
    assert!(empty_replay.pages.lock().unwrap().is_empty());

    let failing = Arc::new(RecordingSink::failing());
    let summary =
        import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreAndPro(failing))).unwrap();
    assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(lingma_events(&store).len(), 2);

    let replay = Arc::new(RecordingSink::default());
    let summary = import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert_eq!(summary.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(replay.pages.lock().unwrap().len(), 1);
    assert_eq!(lingma_events(&store).len(), 2);

    import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert_eq!(replay.pages.lock().unwrap().len(), 1);
}

#[test]
fn pro_replay_waits_for_lingma_append_rewrite_and_replacement_core() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let db = temp.path().join("local.db");
    let connection = create_db(&db);
    insert_row(
        &connection,
        "session",
        "initial",
        "initial prompt",
        Some("initial summary"),
        None,
        1_700_000_000,
        None,
    );
    drop(connection);
    let mut store = Store::open(temp.path().join("core.sqlite")).unwrap();
    assert_eq!(
        import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly))
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::Changed
    );
    let replay = Arc::new(RecordingSink::default());

    let connection = Connection::open(&db).unwrap();
    insert_row(
        &connection,
        "session",
        "append",
        "append prompt",
        Some("append summary"),
        None,
        1_700_000_001,
        None,
    );
    drop(connection);
    import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert!(replay.pages.lock().unwrap().is_empty());
    import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert_eq!(replay.pages.lock().unwrap().len(), 1);

    let connection = Connection::open(&db).unwrap();
    connection
        .execute(
            "update chat_record set chat_prompt = 'rewrite prompt' where rowid = 1",
            [],
        )
        .unwrap();
    drop(connection);
    import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert_eq!(replay.pages.lock().unwrap().len(), 1);
    import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert_eq!(replay.pages.lock().unwrap().len(), 2);

    let replacement = temp.path().join("replacement.db");
    let replacement_connection = create_db(&replacement);
    insert_row(
        &replacement_connection,
        "replacement",
        "replacement",
        "replacement prompt",
        Some("replacement summary"),
        None,
        1_700_000_100,
        None,
    );
    drop(replacement_connection);
    std::fs::remove_file(&db).unwrap();
    std::fs::rename(&replacement, &db).unwrap();
    import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert_eq!(replay.pages.lock().unwrap().len(), 2);
    import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    import_lingma_sqlite(
        &db,
        &mut store,
        options(ImportProfile::ProReplayOnly(replay.clone())),
    )
    .unwrap();
    assert_eq!(replay.pages.lock().unwrap().len(), 3);
}

#[test]
fn malformed_text_is_row_local_and_valid_siblings_commit() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let db = temp.path().join("local.db");
    let connection = create_db(&db);
    connection
        .execute_batch(
            "insert into chat_record (
                session_id, request_id, chat_prompt, summary, gmt_create
             ) values ('bad-session', 'bad-request', cast(x'80' as text), null, 1700000000);",
        )
        .unwrap();
    insert_row(
        &connection,
        "good-session",
        "good-request",
        "good prompt",
        Some("good summary"),
        None,
        1_700_000_001,
        None,
    );
    drop(connection);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly)).unwrap();
    assert_eq!(summary.failed, 1);
    assert_eq!(summary.imported_events, 2);
    assert_eq!(lingma_events(&store).len(), 2);
    assert_eq!(
        import_lingma_sqlite(&db, &mut store, options(ImportProfile::CoreOnly))
            .unwrap()
            .work_result(),
        ProviderImportWorkResult::NoOp
    );
}
