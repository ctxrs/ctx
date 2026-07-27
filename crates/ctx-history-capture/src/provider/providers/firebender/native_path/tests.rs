use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

use super::*;

const TEST_MACHINE: &str = "firebender-nativepath-test";

fn test_context(root: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: TEST_MACHINE.to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: Some(root.to_path_buf()),
        imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
    }
}

fn test_options(capture_work_limit: CaptureWorkLimit, inventory: bool) -> ProviderImportOptions {
    ProviderImportOptions {
        capture_work_limit,
        inventory_observation_token: inventory.then(|| "firebender-inventory".to_owned()),
        ..ProviderImportOptions::default()
    }
}

fn create_test_database(root: &Path, rows: &[(&str, i64, &str)]) -> PathBuf {
    let database = root
        .join(".idea")
        .join("firebender")
        .join("chat_history.db");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    let conn = Connection::open(&database).unwrap();
    conn.execute_batch(
        "create table chat_sessions (
            id text not null,
            name text not null,
            created_at integer not null,
            updated_at integer not null,
            messages_json text not null,
            metadata_json text not null
        );",
    )
    .unwrap();
    for (id, updated_at, messages_json) in rows {
        conn.execute(
            "insert into chat_sessions
             (id, name, created_at, updated_at, messages_json, metadata_json)
             values (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                format!("{id} title"),
                updated_at - 1,
                updated_at,
                messages_json,
                "{}"
            ],
        )
        .unwrap();
    }
    drop(conn);
    database
}

fn replace_messages(database: &Path, id: &str, updated_at: i64, messages: Value) {
    let conn = Connection::open(database).unwrap();
    conn.execute(
        "update chat_sessions set updated_at = ?1, messages_json = ?2 where id = ?3",
        params![updated_at, messages.to_string(), id],
    )
    .unwrap();
}

fn committed_test_cursor(store: &Store, root: &Path) -> FirebenderNativeCursor {
    let identity = firebender_path_identity(root).unwrap();
    let stored = store
        .get_sync_cursor(None, TEST_MACHINE, &identity.cursor_stream)
        .unwrap()
        .unwrap();
    let committed = decode_native_path_committed_cursor(&stored.cursor).unwrap();
    FirebenderNativeCursor::decode(committed.provider_cursor()).unwrap()
}

#[derive(Default)]
struct RecordingOutputSink {
    fail_once: AtomicBool,
    behind: AtomicUsize,
    progress: Mutex<Option<crate::ProOutputProgress>>,
    contents: Mutex<Vec<Vec<u8>>>,
}

impl crate::ProOutputSink for RecordingOutputSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "firebender-nativepath-test-materializer-v1"
    }

    fn observe_source(
        &self,
        _source: &crate::OutputSourceIdentity,
    ) -> std::result::Result<Option<crate::ProOutputProgress>, crate::ProOutputSinkError> {
        Ok(self.progress.lock().unwrap().clone())
    }

    fn materialize_page(
        &self,
        page: crate::ProOutputMaterializationPage,
    ) -> std::result::Result<crate::ProOutputPageResult, crate::ProOutputSinkError> {
        if self.fail_once.swap(false, Ordering::SeqCst) {
            return Err(crate::ProOutputSinkError::new(
                "firebender_test_failure",
                "retry the output page",
            ));
        }
        self.contents.lock().unwrap().extend(
            page.observations
                .iter()
                .map(|observation| observation.content.clone()),
        );
        let committed_cursor = page.next_safe_cursor.clone();
        *self.progress.lock().unwrap() = Some(crate::ProOutputProgress {
            source_epoch: page.source_epoch,
            observed_revision: page.observed_revision.clone(),
            cursor: Some(committed_cursor.clone()),
            parser_revision: page.parser_revision.clone(),
            materializer_revision: page.materializer_revision.clone(),
            terminal: page.terminal,
        });
        Ok(crate::ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor,
            accepted_outputs: u32::try_from(page.observations.len()).unwrap(),
            materialized_facts: 0,
            replayed: false,
        })
    }

    fn mark_behind(&self, _error: crate::ProOutputSinkError) {
        self.behind.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn native_cursor_round_trips() {
    let cursor = FirebenderNativeCursor {
        version: FIREBENDER_NATIVE_CURSOR_VERSION,
        parser_revision: FIREBENDER_NATIVE_PARSER_REVISION,
        policy_revision: FIREBENDER_NATIVE_POLICY_REVISION,
        route_identity: "route".to_owned(),
        canonical_source_identity: "source".to_owned(),
        source_revision: "revision".to_owned(),
        schema_fingerprint: "schema".to_owned(),
        generation: 2,
        rejected_records: 3,
        accepted_sessions: 4,
        accepted_events: 5,
        frontier_accepted_sessions: 4,
        frontier_accepted_events: 5,
        failures: vec![ProviderImportFailure {
            line: 2,
            error: "invalid messages".to_owned(),
        }],
        scan_terminal: false,
        frontier: FirebenderFrontier::initial(),
    };
    let encoded = cursor.encode().expect("encode");
    assert_eq!(
        FirebenderNativeCursor::decode(&encoded).expect("decode"),
        cursor
    );
}

#[test]
fn mixed_invalid_rows_resume_from_pre_rejection_frontier_and_keep_outcome() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("project");
    create_test_database(
        &root,
        &[
            ("invalid", 10, "{not-json"),
            (
                "valid",
                20,
                r#"[{"role":"user","content":"later valid row"}]"#,
            ),
        ],
    );
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();

    let first = import_firebender_nativepath(
        &root,
        &mut store,
        test_context(&root),
        test_options(CaptureWorkLimit::OneSafeGroup, false),
    )
    .unwrap();
    assert_eq!(first.failed, 1);
    assert_eq!(first.imported_events, 0);
    assert!(first.work_remaining);
    assert!(store.list_sessions().unwrap().is_empty());
    assert!(store.export_archive().unwrap().events.is_empty());
    let first_cursor = committed_test_cursor(&store, &root);
    assert_eq!(first_cursor.frontier, FirebenderFrontier::initial());
    assert_eq!(first_cursor.failures, first.failures);
    assert!(!first_cursor.scan_terminal);

    let second = import_firebender_nativepath(
        &root,
        &mut store,
        test_context(&root),
        test_options(CaptureWorkLimit::OneSafeGroup, false),
    )
    .unwrap();
    assert_eq!(second.failed, 1);
    assert_eq!(second.failures, first.failures);
    assert_eq!(second.imported_sessions, 1);
    assert_eq!(second.imported_events, 1);
    assert!(!second.work_remaining);
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    assert_eq!(store.export_archive().unwrap().events.len(), 1);
    let second_cursor = committed_test_cursor(&store, &root);
    assert_eq!(second_cursor.frontier, FirebenderFrontier::initial());
    assert_eq!(second_cursor.rejected_records, 1);
    assert!(second_cursor.scan_terminal);

    let third = import_firebender_nativepath(
        &root,
        &mut store,
        test_context(&root),
        test_options(CaptureWorkLimit::OneSafeGroup, false),
    )
    .unwrap();
    assert_eq!(third.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(third.failed, 1);
    assert_eq!(third.failures, first.failures);
    assert_eq!(third.imported, 0);
    assert_eq!(third.skipped_sessions, 1);
    assert_eq!(third.skipped_events, 1);
    assert!(third.has_accepted_content());
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    assert_eq!(store.export_archive().unwrap().events.len(), 1);
}

#[test]
fn all_invalid_rows_remain_record_rejections_without_orphan_core_rows() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("project");
    create_test_database(
        &root,
        &[("invalid-json", 10, "{"), ("invalid-shape", 20, "{}")],
    );
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();

    let first = import_firebender_nativepath(
        &root,
        &mut store,
        test_context(&root),
        test_options(CaptureWorkLimit::Drain, false),
    )
    .unwrap();
    assert_eq!(first.failed, 2);
    assert_eq!(first.failures.len(), 2);
    assert!(!first.has_accepted_content());
    assert!(store.list_capture_sources().unwrap().is_empty());
    assert!(store.list_sessions().unwrap().is_empty());
    assert!(store.export_archive().unwrap().events.is_empty());

    let second = import_firebender_nativepath(
        &root,
        &mut store,
        test_context(&root),
        test_options(CaptureWorkLimit::Drain, false),
    )
    .unwrap();
    assert_eq!(second.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(second.failed, 2);
    assert_eq!(second.failures, first.failures);
    assert!(!second.has_accepted_content());
    assert!(store.list_capture_sources().unwrap().is_empty());
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn missing_firebender_tables_and_columns_are_typed_unsupported_schema() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    for (name, schema) in [
        ("missing-table", "create table unrelated (id text);"),
        (
            "missing-column",
            "create table chat_sessions (
                id text, name text, created_at integer, updated_at integer,
                messages_json text
            );",
        ),
    ] {
        let root = temp.path().join(name);
        let database = root
            .join(".idea")
            .join("firebender")
            .join("chat_history.db");
        fs::create_dir_all(database.parent().unwrap()).unwrap();
        Connection::open(&database)
            .unwrap()
            .execute_batch(schema)
            .unwrap();
        let mut store = Store::open(temp.path().join(format!("{name}.sqlite"))).unwrap();
        let error = import_firebender_nativepath(
            &root,
            &mut store,
            test_context(&root),
            test_options(CaptureWorkLimit::Drain, false),
        )
        .unwrap_err();
        assert!(matches!(error, CaptureError::UnsupportedSchemaVersion(_)));
    }
}

#[test]
fn missing_source_preserves_not_found_without_inventory_authority() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("missing-project");
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    let error = import_firebender_nativepath(
        &root,
        &mut store,
        test_context(&root),
        test_options(CaptureWorkLimit::Drain, false),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        CaptureError::Io(ref source) if source.kind() == io::ErrorKind::NotFound
    ));
}

#[cfg(unix)]
#[test]
fn inaccessible_source_preserves_permission_denied() {
    use std::os::unix::fs::PermissionsExt;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let locked = temp.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let root = locked.join("project");
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    let result = import_firebender_nativepath(
        &root,
        &mut store,
        test_context(&root),
        test_options(CaptureWorkLimit::Drain, false),
    );
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(
        result,
        Err(CaptureError::Io(ref source))
            if source.kind() == io::ErrorKind::PermissionDenied
    ));
}

#[cfg(unix)]
#[test]
fn relative_and_symlink_aliases_retain_the_live_stream_for_deletion() {
    use std::os::unix::fs::symlink;

    let current = std::env::current_dir().unwrap();
    let relative_temp = tempfile::Builder::new()
        .prefix("firebender-relative-")
        .tempdir_in(&current)
        .unwrap();
    let relative_root = relative_temp
        .path()
        .strip_prefix(&current)
        .unwrap()
        .join("project");
    let relative_database = create_test_database(
        &relative_root,
        &[("relative", 10, r#"[{"role":"user","content":"relative"}]"#)],
    );
    let mut relative_store = Store::open(relative_temp.path().join("relative-ctx.sqlite")).unwrap();
    import_firebender_nativepath(
        &relative_root,
        &mut relative_store,
        test_context(&relative_root),
        test_options(CaptureWorkLimit::Drain, true),
    )
    .unwrap();
    fs::remove_file(&relative_database).unwrap();
    let retired = import_firebender_nativepath(
        &relative_root,
        &mut relative_store,
        test_context(&relative_root),
        test_options(CaptureWorkLimit::Drain, true),
    )
    .unwrap();
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);

    let real_parent = relative_temp.path().join("real");
    let real_root = real_parent.join("project");
    let real_database = create_test_database(
        &real_root,
        &[("alias", 10, r#"[{"role":"user","content":"alias"}]"#)],
    );
    let alias_parent = relative_temp.path().join("alias");
    symlink(&real_parent, &alias_parent).unwrap();
    let alias_root = alias_parent.join("project");
    let mut alias_store = Store::open(relative_temp.path().join("alias-ctx.sqlite")).unwrap();
    import_firebender_nativepath(
        &alias_root,
        &mut alias_store,
        test_context(&alias_root),
        test_options(CaptureWorkLimit::Drain, true),
    )
    .unwrap();
    fs::remove_file(&real_database).unwrap();
    let retired = import_firebender_nativepath(
        &alias_root,
        &mut alias_store,
        test_context(&alias_root),
        test_options(CaptureWorkLimit::Drain, true),
    )
    .unwrap();
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    let noop = import_firebender_nativepath(
        &alias_root,
        &mut alias_store,
        test_context(&alias_root),
        test_options(CaptureWorkLimit::Drain, true),
    )
    .unwrap();
    assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn append_rewrite_and_truncate_are_idempotent_and_keep_historical_rows() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("project");
    let database = create_test_database(
        &root,
        &[("session", 10, r#"[{"role":"user","content":"original"}]"#)],
    );
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();

    let first = import_firebender_nativepath(
        &root,
        &mut store,
        test_context(&root),
        test_options(CaptureWorkLimit::Drain, false),
    )
    .unwrap();
    assert_eq!(first.imported_events, 1);
    replace_messages(
        &database,
        "session",
        20,
        json!([
            {"role": "user", "content": "original"},
            {"role": "assistant", "content": "appended"}
        ]),
    );
    let appended = import_firebender_nativepath(
        &root,
        &mut store,
        test_context(&root),
        test_options(CaptureWorkLimit::Drain, false),
    )
    .unwrap();
    assert_eq!(appended.imported_events, 1);
    assert_eq!(store.export_archive().unwrap().events.len(), 2);

    replace_messages(
        &database,
        "session",
        30,
        json!([
            {"role": "user", "content": "rewritten"},
            {"role": "assistant", "content": "appended"}
        ]),
    );
    import_firebender_nativepath(
        &root,
        &mut store,
        test_context(&root),
        test_options(CaptureWorkLimit::Drain, false),
    )
    .unwrap();
    let rewritten = serde_json::to_string(&store.export_archive().unwrap()).unwrap();
    assert!(rewritten.contains("rewritten"));
    assert!(!rewritten.contains("\"text\":\"original\""));
    assert_eq!(store.export_archive().unwrap().events.len(), 2);

    replace_messages(
        &database,
        "session",
        40,
        json!([{"role": "user", "content": "rewritten"}]),
    );
    import_firebender_nativepath(
        &root,
        &mut store,
        test_context(&root),
        test_options(CaptureWorkLimit::Drain, false),
    )
    .unwrap();
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    assert_eq!(store.list_capture_sources().unwrap().len(), 1);
    assert_eq!(store.export_archive().unwrap().events.len(), 2);
}

#[test]
fn failed_tool_output_has_no_core_result_locator() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("project");
    create_test_database(
        &root,
        &[(
            "session",
            10,
            r#"[{"role":"tool","status":"failed","content":"private result"}]"#,
        )],
    );
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    import_firebender_nativepath(
        &root,
        &mut store,
        test_context(&root),
        test_options(CaptureWorkLimit::Drain, false),
    )
    .unwrap();

    let session = store.list_sessions().unwrap().pop().unwrap();
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::ToolOutput);
    assert!(events[0]
        .sync
        .metadata
        .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .is_none());
    assert!(!events[0].payload.to_string().contains("private result"));
}

#[test]
fn successful_output_is_not_core_eligible() {
    let message = json!({
        "role": "tool",
        "content": "SECRET_OUTPUT",
        "status": "success"
    });
    let evidence = firebender_output_evidence(&message);
    assert!(evidence.success);
    assert!(!evidence.failure);
    assert!(!evidence.timeout);
}

#[test]
fn failure_output_keeps_only_sparse_outcome_authority() {
    let message = json!({
        "role": "tool",
        "content": "SECRET_OUTPUT",
        "status": "failed",
        "exit_code": 9
    });
    let event =
        super::super::firebender_native_event("session", 0, &message, DateTime::<Utc>::UNIX_EPOCH);
    let evidence = firebender_output_evidence(&message);
    assert!(evidence.failure);
    assert_eq!(evidence.exit_code, Some(9));
    assert!(!event.payload.to_string().contains("SECRET_OUTPUT"));
}

#[test]
fn output_failure_keeps_core_success_and_later_replay_catches_up() {
    const SECRET: &str = "firebender-private-output";

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("project");
    let database = root
        .join(".idea")
        .join("firebender")
        .join("chat_history.db");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    let conn = Connection::open(&database).unwrap();
    conn.execute_batch(
        "create table chat_sessions (
            id text not null,
            name text not null,
            created_at integer not null,
            updated_at integer not null,
            messages_json text not null,
            metadata_json text not null
        );",
    )
    .unwrap();
    conn.execute(
        "insert into chat_sessions
         (id, name, created_at, updated_at, messages_json, metadata_json)
         values (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            "firebender-session",
            "test",
            1_785_000_000_i64,
            1_785_000_001_i64,
            json!([
                {"role": "user", "content": "core message"},
                {
                    "role": "tool",
                    "tool_call_id": "call-1",
                    "status": "success",
                    "content": SECRET
                }
            ])
            .to_string(),
            "{}",
        ],
    )
    .unwrap();
    drop(conn);

    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    let sink = Arc::new(RecordingOutputSink::default());
    sink.fail_once.store(true, Ordering::SeqCst);
    let context = ProviderAdapterContext {
        machine_id: "firebender-nativepath-test".to_owned(),
        source_path: Some(root.clone()),
        source_root: Some(root.clone()),
        imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
    };
    let summary = import_firebender_nativepath(
        &root,
        &mut store,
        context.clone(),
        ProviderImportOptions {
            import_profile: crate::ImportProfile::CoreAndPro(sink.clone()),
            ..ProviderImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.imported_events, 1);
    assert_eq!(summary.failed, 1);
    assert_eq!(
        summary.failures[0].error,
        "Firebender Pro output is behind committed Core"
    );
    assert!(sink.behind.load(Ordering::SeqCst) > 0);
    assert!(!serde_json::to_string(&store.export_archive().unwrap())
        .unwrap()
        .contains(SECRET));

    let replay = import_firebender_nativepath(
        &root,
        &mut store,
        context,
        ProviderImportOptions {
            import_profile: crate::ImportProfile::ProReplayOnly(sink.clone()),
            ..ProviderImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.failed, 0);
    assert_eq!(
        sink.contents.lock().unwrap().as_slice(),
        [SECRET.as_bytes()]
    );
}
