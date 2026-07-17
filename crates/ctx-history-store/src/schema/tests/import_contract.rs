const FRESH_ONLY_OPTIMIZED_INDEX_NAMES: &[&str] = &[
    "idx_capture_sources_provider_material_owner",
    "idx_provider_file_publications_owner",
    "idx_provider_file_publications_fence",
    "idx_session_aliases_session_id",
    "idx_event_aliases_event_id",
    "idx_reconcile_history_record_links_source_id",
    "idx_reconcile_files_touched_source_id",
    "idx_reconcile_files_touched_event_id",
    "idx_reconcile_files_touched_run_id",
    "idx_reconcile_session_edges_source_id",
    "idx_reconcile_session_edges_from_session_id",
    "idx_reconcile_session_edges_to_session_id",
    "idx_reconcile_summaries_source_id",
    "idx_reconcile_events_capture_source_id",
    "idx_reconcile_events_session_id",
    "idx_reconcile_events_run_id",
    "idx_reconcile_runs_source_id",
    "idx_reconcile_runs_session_id",
    "idx_reconcile_sessions_capture_source_id",
    "idx_reconcile_vcs_changes_source_id",
    "idx_reconcile_artifacts_source_id",
    "idx_reconcile_record_edges_source_id",
    "idx_reconcile_history_records_source_id",
    "idx_reconcile_vcs_workspaces_source_id",
    "idx_reconcile_audit_log_source_id",
    "idx_catalog_sessions_pending_fresh_attempt",
    "idx_catalog_sessions_pending_recovery_attempt",
    "idx_source_import_files_pending_fresh_attempt",
    "idx_source_import_files_pending_recovery_attempt",
    "idx_sessions_unique_capture_source_external_session",
];

#[test]
fn fresh_store_installs_all_optimized_indexes() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    for &name in FRESH_ONLY_OPTIMIZED_INDEX_NAMES {
        let exists: bool = store
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = ?1)",
                [name],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists, "fresh store omitted optimized index {name}");
    }
    let completed_ledgers: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM import_pending_reason_repairs WHERE completed = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(completed_ledgers, 2);

    let projection_index_exists: bool = store
        .conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema \
             WHERE type = 'index' AND name = 'idx_import_pending_work_selection')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(projection_index_exists);
    let projection_trigger_count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'trigger' \
             AND name LIKE 'trg_%_pending_work_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(projection_trigger_count, 0);
    let count_trigger_count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'trigger' \
             AND name LIKE 'trg_%_pending_count_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count_trigger_count, 8);
    let selection_mode: String = store
        .conn
        .query_row(
            "SELECT selection_mode FROM import_pending_work_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(selection_mode, "direct");

    store
        .conn
        .execute_batch(
            r#"
            INSERT INTO catalog_sessions
              (source_path, provider, source_format, source_root, agent_type,
               file_size_bytes, file_modified_at_ms, cataloged_at_ms,
               pending_reason)
            VALUES
              ('/fresh/catalog.jsonl', 'codex', 'codex_session_jsonl',
               '/fresh', 'primary', 10, 1, 1, 'fresh_new');
            INSERT INTO source_import_files
              (provider, source_format, source_root, source_path,
               file_size_bytes, file_modified_at_ms, observed_at_ms,
               indexed_at_ms, pending_reason)
            VALUES
              ('pi', 'pi_session_jsonl', '/fresh', '/fresh/source.jsonl',
               20, 1, 1, 5, 'recovery_retry');
            "#,
        )
        .unwrap();
    let pending_work_count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM import_pending_work", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(pending_work_count, 0);
    let pending_counts: Vec<(String, String, i64)> = store
        .conn
        .prepare(
            "SELECT inventory_family, work_class, pending_count \
             FROM import_pending_work_counts ORDER BY inventory_family",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        pending_counts,
        vec![
            ("catalog_sessions".into(), "fresh".into(), 1),
            ("source_import_files".into(), "recovery".into(), 1),
        ]
    );

    store
        .conn
        .execute_batch(
            r#"
            UPDATE catalog_sessions
            SET pending_reason = 'legacy', indexed_at_ms = 9
            WHERE source_path = '/fresh/catalog.jsonl';
            UPDATE source_import_files
            SET is_stale = 1
            WHERE provider = 'pi' AND source_root = '/fresh'
              AND source_path = '/fresh/source.jsonl';
            "#,
        )
        .unwrap();
    let remaining: (String, i64) = store
        .conn
        .query_row(
            "SELECT work_class, pending_count FROM import_pending_work_counts",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(remaining, ("recovery".into(), 1));
    let pending_work_count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM import_pending_work", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(pending_work_count, 0);
}

#[test]
fn schema_v46_upgrade_preserves_index_rootpages_and_uses_trigger_only_invariant() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let rootpages_before = {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(CREATE_TABLES_SQL).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch(
            r#"
            CREATE INDEX idx_events_seq ON events(seq);
            INSERT INTO capture_sources
              (id, kind, provider, machine_id, raw_source_path, source_format,
               source_root, external_session_id, started_at_ms, fidelity)
            VALUES
              ('v46-source', 'provider_import', 'codex', 'machine',
               '/legacy/session.jsonl', 'codex_session_jsonl', '/legacy',
               'legacy-session', 1, 'imported');
            INSERT INTO sessions
              (id, capture_source_id, provider, external_session_id, agent_type,
               is_primary, status, fidelity, started_at_ms, created_at_ms, updated_at_ms)
            VALUES
              ('v46-session', 'v46-source', 'codex', 'legacy-session', 'primary',
               1, 'imported', 'imported', 1, 1, 1);
            INSERT INTO events
              (id, seq, session_id, event_type, role, occurred_at_ms,
               capture_source_id, payload_json)
            VALUES
              ('v46-event', 1, 'v46-session', 'message', 'user', 1,
               'v46-source', '{}');
            INSERT INTO catalog_sessions
              (source_path, provider, source_format, source_root, agent_type,
               file_size_bytes, file_modified_at_ms, cataloged_at_ms,
               indexed_status)
            VALUES
              ('/legacy/session.jsonl', 'codex', 'codex_session_jsonl',
               '/legacy', 'primary', 1, 1, 1, 'indexed');
            PRAGMA user_version = 46;
            "#,
        )
        .unwrap();
        let rootpages = conn
            .prepare(
                "SELECT name, rootpage FROM sqlite_schema \
             WHERE type = 'index' AND name LIKE 'idx_%' \
               AND name <> 'idx_events_seq' ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rootpages
    };

    let store = Store::open(&path).unwrap();
    for (name, rootpage_before) in rootpages_before {
        let rootpage_after: i64 = store
            .conn
            .query_row(
                "SELECT rootpage FROM sqlite_schema WHERE type = 'index' AND name = ?1",
                [&name],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            rootpage_after, rootpage_before,
            "upgrade rebuilt v46 index {name}"
        );
    }
    let retired_events_seq_exists: bool = store
        .conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = 'idx_events_seq')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!retired_events_seq_exists);

    for &name in FRESH_ONLY_OPTIMIZED_INDEX_NAMES {
        let exists: bool = store
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = ?1)",
                [name],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!exists, "v46 upgrade created optimized index {name}");
    }
    let projection_state: (i64, i64, i64) = store
        .conn
        .query_row(
            "SELECT \
               (SELECT COUNT(*) FROM import_pending_work), \
               (SELECT COUNT(*) FROM import_pending_work_counts), \
               (SELECT COUNT(*) FROM import_pending_reason_repairs \
                WHERE completed = 0)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(projection_state, (0, 0, 2));
    let selection_mode: String = store
        .conn
        .query_row(
            "SELECT selection_mode FROM import_pending_work_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(selection_mode, "projection");
    let projection_index_exists: bool = store
        .conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema \
             WHERE type = 'index' AND name = 'idx_import_pending_work_selection')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(projection_index_exists);
    let projection_trigger_count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'trigger' \
             AND name LIKE 'trg_%_pending_work_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(projection_trigger_count, 8);
    store
        .conn
        .execute(
            "UPDATE catalog_sessions SET pending_reason = 'legacy' \
             WHERE source_path = '/legacy/session.jsonl'",
            [],
        )
        .unwrap();
    let mirrored: (String, String, i64) = store
        .conn
        .query_row(
            "SELECT pending.source_path, pending.work_class, counts.pending_count \
             FROM import_pending_work AS pending \
             JOIN import_pending_work_counts AS counts USING (\
               inventory_family, provider, source_root, work_class\
             )",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        mirrored,
        ("/legacy/session.jsonl".into(), "recovery".into(), 1)
    );
    let trigger_count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'trigger' \
             AND name IN ('trg_sessions_provider_source_identity_insert', \
                          'trg_sessions_provider_source_identity_update')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(trigger_count, 2);

    store
        .conn
        .execute_batch(
            r#"
            INSERT INTO capture_sources
              (id, kind, provider, machine_id, raw_source_path, source_format,
               source_root, external_session_id, started_at_ms, fidelity)
            VALUES
              ('upgraded-source-a', 'provider_import', 'codex', 'machine',
               '/upgraded/shared.jsonl', 'codex_session_jsonl', '/upgraded',
               'upgraded-session', 1, 'imported'),
              ('upgraded-source-b', 'provider_import', 'codex', 'machine',
               '/upgraded/shared.jsonl', 'codex_session_jsonl', '/upgraded',
               'upgraded-session', 1, 'imported');
            INSERT INTO sessions
              (id, capture_source_id, provider, external_session_id, agent_type,
               is_primary, status, fidelity, started_at_ms, created_at_ms, updated_at_ms)
            VALUES
              ('upgraded-session-a', 'upgraded-source-a', 'codex',
               'upgraded-session', 'primary', 1, 'imported', 'imported', 1, 1, 1);
            "#,
        )
        .unwrap();
    let duplicate = store.conn.execute(
        r#"
        INSERT INTO sessions
          (id, capture_source_id, provider, external_session_id, agent_type,
           is_primary, status, fidelity, started_at_ms, created_at_ms, updated_at_ms)
        VALUES
          ('upgraded-session-b', 'upgraded-source-b', 'codex',
           'upgraded-session', 'primary', 1, 'imported', 'imported', 1, 1, 1)
        "#,
        [],
    );
    assert!(duplicate
        .unwrap_err()
        .to_string()
        .contains("duplicate provider session for capture source identity"));
}

#[test]
fn v57_migration_creates_empty_projection_without_reading_inventory() {
    let temp = tempdir();
    let path = temp.path().join("v57-pending-work.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(CREATE_TABLES_SQL).unwrap();
    conn.execute_batch(INDEXES_SQL).unwrap();
    conn.execute_batch(
        r#"
        DROP TABLE import_pending_work_state;
        DROP TABLE import_pending_work_counts;
        DROP TABLE import_pending_work;

        INSERT INTO catalog_sessions
          (source_path, provider, source_format, source_root, agent_type,
           file_size_bytes, file_modified_at_ms, cataloged_at_ms,
           pending_reason)
        VALUES
          ('/legacy/catalog.jsonl', 'codex', 'codex_session_jsonl',
           '/legacy', 'primary', 10, 1, 1, 'legacy');
        INSERT INTO source_import_files
          (provider, source_format, source_root, source_path,
           file_size_bytes, file_modified_at_ms, observed_at_ms,
           pending_reason)
        VALUES
          ('pi', 'pi_session_jsonl', '/legacy', '/legacy/source.jsonl',
           20, 1, 1, 'legacy');
        INSERT INTO import_pending_reason_repairs
          (inventory_family, cursor_provider, cursor_source_root,
           cursor_source_path, completed)
        VALUES
          ('catalog_sessions', 'codex', '/legacy', '/legacy/catalog.jsonl', 1),
          ('source_import_files', 'pi', '/legacy', '/legacy/source.jsonl', 1);
        PRAGMA user_version = 56;
        "#,
    )
    .unwrap();
    let inventory_rootpages = ["catalog_sessions", "source_import_files"].map(|table| {
        conn.query_row(
            "SELECT rootpage FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    });
    let total_changes_before = conn.total_changes();
    let forbidden_actions = Arc::new(Mutex::new(Vec::new()));
    let callback_forbidden_actions = Arc::clone(&forbidden_actions);
    conn.authorizer(Some(move |context: AuthContext<'_>| {
        let action = match context.action {
            AuthAction::Read { table_name, .. }
                if context.accessor.is_none()
                    && matches!(table_name, "catalog_sessions" | "source_import_files") =>
            {
                Some(format!("read:{table_name}"))
            }
            AuthAction::CreateIndex { table_name, .. }
                if matches!(table_name, "catalog_sessions" | "source_import_files") =>
            {
                Some(format!("index:{table_name}"))
            }
            _ => None,
        };
        if let Some(action) = action {
            callback_forbidden_actions.lock().unwrap().push(action);
            Authorization::Deny
        } else {
            Authorization::Allow
        }
    }));

    crate::schema::migrations::migrate_pending_work_projection_to_v57(&conn).unwrap();
    assert!(forbidden_actions.lock().unwrap().is_empty());
    assert_eq!(conn.total_changes() - total_changes_before, 3);
    let inventory_rootpages_after = ["catalog_sessions", "source_import_files"].map(|table| {
        conn.query_row(
            "SELECT rootpage FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    });
    assert_eq!(inventory_rootpages_after, inventory_rootpages);
    let state: (i64, i64, String, i64) = conn
        .query_row(
            "SELECT \
               (SELECT COUNT(*) FROM import_pending_work), \
               (SELECT COUNT(*) FROM import_pending_work_counts), \
               (SELECT selection_mode FROM import_pending_work_state WHERE singleton = 1), \
               (SELECT COUNT(*) FROM import_pending_reason_repairs \
                WHERE completed = 0 AND cursor_provider IS NULL \
                  AND cursor_source_root IS NULL AND cursor_source_path IS NULL)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(state, (0, 0, "projection".into(), 2));
}

#[test]
fn v57_migration_reuses_a_preexisting_compatible_selection_index() {
    let temp = tempdir();
    let path = temp.path().join("v57-compatible-selection-index.sqlite");
    let store = Store::open(&path).unwrap();
    let rootpage_before: i64 = store
        .conn
        .query_row(
            "SELECT rootpage FROM sqlite_schema \
             WHERE type = 'index' AND name = 'idx_import_pending_work_selection'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    store
        .conn
        .execute_batch("PRAGMA user_version = 56")
        .unwrap();

    crate::schema::migrations::migrate_pending_work_projection_to_v57(&store.conn).unwrap();

    let rootpage_after: i64 = store
        .conn
        .query_row(
            "SELECT rootpage FROM sqlite_schema \
             WHERE type = 'index' AND name = 'idx_import_pending_work_selection'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rootpage_after, rootpage_before);
    assert_eq!(
        store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        57
    );
}

#[test]
fn real_schema_v45_fixture_migrates_import_state_through_v49() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(include_str!("../fixtures/schema_v45.sql"))
            .unwrap();
        conn.execute_batch(
            r#"
            INSERT INTO catalog_sessions
            (source_path, provider, source_format, source_root, external_session_id,
             agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms,
             indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms,
             indexed_status, last_imported_at_ms, last_imported_file_size_bytes,
             last_imported_file_modified_at_ms, last_imported_event_count)
            VALUES
            ('/missing/v45-indexed.jsonl', 'codex', 'codex_session_jsonl', '/missing/v45',
             'v45-indexed', 'primary', 21, 31, 41, 51, 21, 31, 'indexed', 51, 21, 31, 2);

            INSERT INTO source_import_files
            (provider, source_format, source_root, source_path, file_size_bytes,
             file_modified_at_ms, observed_at_ms, indexed_at_ms, indexed_status,
             indexed_error)
            VALUES
            ('claude', 'claude_projects_jsonl_tree', '/missing/v45-claude',
             '/missing/v45-claude/failed.jsonl', 22, 32, 42, 52, 'failed',
             'legacy transient failure');

            PRAGMA user_version = 45;
            "#,
        )
        .unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let before_repair: Option<i64> = store
        .conn
        .query_row(
            "SELECT indexed_import_revision FROM catalog_sessions WHERE source_path = '/missing/v45-indexed.jsonl'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(before_repair, None);
    let mut repair_complete = false;
    for _ in 0..64 {
        if store
            .repair_import_pending_reasons(1, 64 * 1024, std::time::Duration::from_secs(1))
            .unwrap()
            .complete
        {
            repair_complete = true;
            break;
        }
    }
    assert!(repair_complete);
    let indexed: (String, i64, Option<i64>, Option<i64>) = store
        .conn
        .query_row(
            "SELECT indexed_status, import_revision, indexed_import_revision, last_imported_file_size_bytes FROM catalog_sessions WHERE source_path = '/missing/v45-indexed.jsonl'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(indexed, ("indexed".to_owned(), 1, Some(1), Some(21)));
    let pending = store
        .list_pending_source_import_files(CaptureProvider::Claude, "/missing/v45-claude")
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].import_revision, 1);
    let indexed_revision: Option<i64> = store
        .conn
        .query_row(
            "SELECT indexed_import_revision FROM source_import_files WHERE source_path = '/missing/v45-claude/failed.jsonl'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(indexed_revision, None);
    let generations: Vec<(String, String, String, i64, i64)> = store
        .conn
        .prepare(
            "SELECT provider, source_root, inventory_family, current_generation, completed_generation FROM import_inventory_generations ORDER BY inventory_family, provider, source_root",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(generations.is_empty());
}

#[test]
fn schema_v48_preserves_inventory_tables_and_defers_bounded_legacy_repair() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let v46_sql = CREATE_TABLES_SQL
            .replace(
                "CREATE TABLE IF NOT EXISTS import_inventory_generations (\n    provider TEXT NOT NULL,\n    source_root TEXT NOT NULL,\n    inventory_family TEXT NOT NULL CHECK (inventory_family IN ('catalog_sessions', 'source_import_files')),\n    current_generation INTEGER NOT NULL CHECK (current_generation > 0),\n    completed_generation INTEGER NOT NULL DEFAULT 0 CHECK (completed_generation >= 0 AND completed_generation <= current_generation),\n    PRIMARY KEY (provider, source_root, inventory_family)\n);\n\n",
                "",
            )
            .replace(
                "    import_revision INTEGER NOT NULL DEFAULT 1 CHECK (import_revision > 0),\n",
                "",
            )
            .replace(
                "    indexed_import_revision INTEGER CHECK (indexed_import_revision > 0),\n",
                "",
            )
            .replace(
                "CHECK (indexed_status IN ('pending', 'indexed', 'completed_with_rejections', 'rejected', 'failed'))",
                "CHECK (indexed_status IN ('pending', 'indexed', 'failed'))",
            );
        conn.execute_batch(&v46_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch(
            r#"
            INSERT INTO catalog_sessions
            (source_path, provider, source_format, source_root, external_session_id,
             agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms,
             indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms,
             indexed_status, indexed_error)
            VALUES
            ('/missing/indexed.jsonl', 'codex', 'codex_session_jsonl', '/missing',
             'indexed-session', 'primary', 10, 20, 30, 40, 10, 20, 'indexed', NULL),
            ('/missing/pr75-failed.jsonl', 'codex', 'codex_session_jsonl', '/missing',
             'failed-session', 'primary', 11, 21, 31, 41, NULL, NULL, 'failed',
             'full import failed for one or more sessions');

            INSERT INTO source_import_files
            (provider, source_format, source_root, source_path, file_size_bytes,
             file_modified_at_ms, observed_at_ms, indexed_at_ms,
             indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_status,
             indexed_error)
            VALUES
            ('claude', 'claude_projects_jsonl_tree', '/missing/claude',
             '/missing/claude/indexed.jsonl', 12, 22, 32, 42, 12, 22, 'indexed', NULL),
            ('antigravity', 'antigravity_cli_transcript_jsonl_tree', '/missing/agy',
             '/missing/agy/rejected.jsonl', 13, 23, 33, 43, NULL, NULL, 'failed',
             'provider import reported 1 failure(s)');

            PRAGMA user_version = 46;
            "#,
        )
        .unwrap();

        let root_pages_before: Vec<(String, i64)> = conn
            .prepare(
                "SELECT name, rootpage FROM sqlite_schema \
                 WHERE type = 'table' AND name IN ('catalog_sessions', 'source_import_files') \
                 ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        migrate_import_outcomes_to_v48(&conn).unwrap();
        let root_pages_after: Vec<(String, i64)> = conn
            .prepare(
                "SELECT name, rootpage FROM sqlite_schema \
                 WHERE type = 'table' AND name IN ('catalog_sessions', 'source_import_files') \
                 ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(root_pages_after, root_pages_before);
        let deferred_revisions: (Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT \
                   (SELECT indexed_import_revision FROM catalog_sessions \
                    WHERE source_path = '/missing/indexed.jsonl'), \
                   (SELECT indexed_import_revision FROM source_import_files \
                    WHERE source_path = '/missing/claude/indexed.jsonl')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(deferred_revisions, (None, None));
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    let before_repair: (Option<i64>, Option<i64>) = store
        .conn
        .query_row(
            "SELECT \
               (SELECT indexed_import_revision FROM catalog_sessions \
                WHERE source_path = '/missing/indexed.jsonl'), \
               (SELECT indexed_import_revision FROM source_import_files \
                WHERE source_path = '/missing/claude/indexed.jsonl')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(before_repair, (None, None));
    let mut repaired_rows = 0;
    let mut repair_complete = false;
    for _ in 0..64 {
        let progress = store
            .repair_import_pending_reasons(1, 64 * 1024, std::time::Duration::from_secs(1))
            .unwrap();
        assert!(progress.processed_rows <= 1);
        repaired_rows += progress.classified_rows;
        if progress.complete {
            repair_complete = true;
            break;
        }
    }
    assert!(repair_complete);
    assert_eq!(repaired_rows, 4);

    let catalog_rows = store
        .conn
        .prepare(
            "SELECT source_path, indexed_status, import_revision, indexed_import_revision FROM catalog_sessions ORDER BY source_path",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        catalog_rows,
        vec![
            (
                "/missing/indexed.jsonl".to_owned(),
                "indexed".to_owned(),
                1,
                Some(1),
            ),
            (
                "/missing/pr75-failed.jsonl".to_owned(),
                "failed".to_owned(),
                1,
                None,
            ),
        ]
    );
    let failed_catalog = store
        .list_pending_catalog_sessions(CaptureProvider::Codex, "/missing")
        .unwrap();
    assert!(failed_catalog
        .iter()
        .any(|row| row.source_path == "/missing/pr75-failed.jsonl"));

    let file_rows = store
        .conn
        .prepare(
            "SELECT source_path, indexed_status, import_revision, indexed_import_revision FROM source_import_files ORDER BY source_path",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        file_rows
            .iter()
            .find(|row| row.0.ends_with("claude/indexed.jsonl"))
            .unwrap()
            .3,
        Some(1)
    );
    assert_eq!(
        file_rows
            .iter()
            .find(|row| row.0.ends_with("agy/rejected.jsonl"))
            .unwrap()
            .3,
        None
    );
    assert_eq!(
        store
            .list_pending_source_import_files(CaptureProvider::Antigravity, "/missing/agy")
            .unwrap()
            .len(),
        1
    );
    let generation_count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM import_inventory_generations WHERE current_generation = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(generation_count, 0);
}

#[test]
fn migrated_schema_rejects_writers_without_the_connection_capability() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    let stale_connection = Connection::open(&path).unwrap();

    let stale_write = stale_connection.execute(
        r#"
        INSERT INTO catalog_sessions (
          source_path, provider, source_format, source_root, agent_type,
          file_size_bytes, file_modified_at_ms, cataloged_at_ms
        ) VALUES (
          '/stale/writer.jsonl', 'codex', 'codex_session_jsonl', '/stale',
          'primary', 1, 1, 1
        )
        "#,
        [],
    );
    let error = stale_write.unwrap_err().to_string();
    assert!(
        error.contains("ctx_schema_writer_version"),
        "unexpected stale-writer error: {error}"
    );

    store
        .conn
        .execute(
            r#"
            INSERT INTO catalog_sessions (
              source_path, provider, source_format, source_root, agent_type,
              file_size_bytes, file_modified_at_ms, cataloged_at_ms
            ) VALUES (
              '/current/writer.jsonl', 'codex', 'codex_session_jsonl', '/current',
              'primary', 1, 1, 1
            )
            "#,
            [],
        )
        .unwrap();
}

#[test]
fn real_schema_v49_fixture_adds_provider_file_contract_tables() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(include_str!("../fixtures/schema_v49.sql"))
            .unwrap();
        let legacy_views: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'view' AND name IN ('ctx_sessions', 'ctx_events', 'ctx_files_touched', 'ctx_sources')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_views, 4);
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let columns = store
        .conn
        .prepare("PRAGMA table_info(provider_file_checkpoints)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        columns,
        vec![
            "provider",
            "source_format",
            "source_root",
            "source_path",
            "import_revision",
            "checkpoint_version",
            "stable_file_identity",
            "committed_byte_offset",
            "committed_complete_line_count",
            "head_sha256",
            "boundary_sha256",
            "resume_state",
            "updated_at_ms",
        ]
    );
    let rows: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM provider_file_checkpoints",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
    let semantic_revision: i64 = store
        .conn
        .query_row(
            "SELECT current_revision FROM semantic_replacement_revision WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(semantic_revision, 0);
    for present in [
        "provider_file_checkpoints",
        "provider_file_publications",
        "semantic_replacement_revision",
    ] {
        let exists: bool = store
            .conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                params![present],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists);
    }

    let fresh_path = temp.path().join("fresh-v50.sqlite");
    let fresh = Store::open(&fresh_path).unwrap();
    assert_schema_object_parity(&store.conn, &fresh.conn);
}

#[test]
fn real_schema_v49_fixture_stages_pending_reason_repairs() {
    let temp = tempdir();
    let path = temp.path().join("pending-reasons.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(include_str!("../fixtures/schema_v49.sql"))
            .unwrap();
        conn.execute_batch(include_str!("../fixtures/pending_reason_v49_rows.sql"))
            .unwrap();
    }

    let store = Store::open(&path).unwrap();
    let repairs = store
        .conn
        .prepare(
            "SELECT inventory_family, cursor_provider, cursor_source_root, \
                    cursor_source_path, completed \
             FROM import_pending_reason_repairs ORDER BY inventory_family",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        repairs,
        vec![
            ("catalog_sessions".into(), None, None, None, false),
            ("source_import_files".into(), None, None, None, false),
        ]
    );

    let pending_reason_count: usize = store
        .conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM catalog_sessions WHERE pending_reason IS NOT NULL) + \
                    (SELECT COUNT(*) FROM source_import_files WHERE pending_reason IS NOT NULL)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending_reason_count, 0);
}

#[test]
fn v52_migration_preserves_rows_and_repair_progress_when_retried() {
    let temp = tempdir();
    let path = temp.path().join("v52-failure-reasons.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(CREATE_TABLES_SQL).unwrap();
    for (index, prior_reason) in [
        (1, Some("fresh_append")),
        (2, Some("recovery_retry")),
        (3, Some("fresh_changed")),
        (4, Some("recovery_replacement")),
        (5, None),
    ] {
        conn.execute(
            r#"
            INSERT INTO source_import_files (
                provider, source_format, source_root, source_path,
                file_size_bytes, file_modified_at_ms, observed_at_ms,
                indexed_status, pending_reason
            ) VALUES ('pi', 'pi_session_jsonl', ?1, ?2, 1, 1, 1, 'failed', ?3)
            "#,
            params![
                format!("/fixture/failure-{index}"),
                format!("/fixture/failure-{index}/session.jsonl"),
                prior_reason,
            ],
        )
        .unwrap();
    }

    migrate_fresh_scheduling_to_v52(&conn).unwrap();
    let reasons = conn
        .prepare("SELECT pending_reason FROM source_import_files ORDER BY source_root")
        .unwrap()
        .query_map([], |row| row.get::<_, Option<String>>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        reasons,
        vec![
            Some("fresh_append".into()),
            Some("recovery_retry".into()),
            Some("fresh_changed".into()),
            Some("recovery_replacement".into()),
            None,
        ]
    );

    conn.execute(
        r#"
        UPDATE import_pending_reason_repairs
        SET cursor_provider = 'pi', cursor_source_root = '/fixture/failure-5',
            cursor_source_path = '/fixture/failure-5/session.jsonl', completed = 1
        WHERE inventory_family = 'source_import_files'
        "#,
        [],
    )
    .unwrap();
    migrate_fresh_scheduling_to_v52(&conn).unwrap();

    let repair = conn
        .query_row(
            r#"
            SELECT cursor_provider, cursor_source_root, cursor_source_path, completed
            FROM import_pending_reason_repairs
            WHERE inventory_family = 'source_import_files'
            "#,
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, bool>(3)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        repair,
        (
            "pi".into(),
            "/fixture/failure-5".into(),
            "/fixture/failure-5/session.jsonl".into(),
            true,
        )
    );
}

#[test]
fn v52_legacy_publications_reconstruct_without_sidecars_and_match_fresh_schema() {
    let temp = tempdir();
    let path = temp.path().join("v52-publication-staging.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL
            .replace(
                "    tracks_prior_material INTEGER NOT NULL DEFAULT 0 CHECK (tracks_prior_material IN (0, 1)),\n",
                "",
            )
            .replace(
                "    staging_initialized INTEGER NOT NULL DEFAULT 0 CHECK (staging_initialized IN (0, 1)),\n",
                "",
            )
            .replace(
                ",\n    pending_reason TEXT CHECK (pending_reason IS NULL OR pending_reason IN ('fresh_new', 'fresh_changed', 'fresh_append', 'recovery_retry', 'recovery_replacement', 'parser_revision', 'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan'))",
                "",
            );
        assert!(!legacy_sql.contains("staging_initialized"));
        assert!(!legacy_sql.contains("tracks_prior_material"));
        conn.execute_batch(&legacy_sql).unwrap();
        assert!(!table_has_column(&conn, "catalog_sessions", "pending_reason").unwrap());
        assert!(!table_has_column(&conn, "source_import_files", "pending_reason").unwrap());
        conn.execute_batch("PRAGMA user_version = 49;").unwrap();
        migrate_provider_publication_to_v51(&conn).unwrap();
        conn.execute_batch(
            r#"
            INSERT INTO provider_file_publications (
              replacement_id, owner_id, publication_kind, staging_id, provider,
              inventory_family, inventory_source_format, inventory_source_root,
              source_path, material_source_format, material_source_root,
              inventory_generation, file_size_bytes, file_modified_at_ms,
              import_revision, mutation_started, preparation_complete, preparation_cursor,
              cleanup_phase, cleanup_source_cursor, cleanup_entity_cursor,
              removed_events, started_at_ms, updated_at_ms
            ) VALUES
            (
              'legacy-mutated', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
              'replacement', 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'codex',
              'catalog_sessions', 'codex_session_jsonl', '/legacy/inventory',
              '/legacy/mutated.jsonl', 'codex_session_jsonl', '/legacy/material',
              1, 10, 20, 1, 1, 1, 'prior-source', 4, 'prior-source', 'event-cursor',
              7, 30, 30
            ),
            (
              'legacy-unmutated', 'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
              'replacement', 'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd', 'codex',
              'catalog_sessions', 'codex_session_jsonl', '/legacy/inventory',
              '/legacy/unmutated.jsonl', 'codex_session_jsonl', '/legacy/material',
              1, 10, 20, 1, 0, 1, 'prior-source', 0, NULL, NULL,
              0, 31, 31
            ),
            (
              'legacy-mutated-incremental', 'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
              'incremental', 'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff', 'codex',
              'catalog_sessions', 'codex_session_jsonl', '/legacy/inventory',
              '/legacy/incremental.jsonl', 'codex_session_jsonl', '/legacy/material',
              1, 10, 20, 1, 1, 1, NULL, 0, NULL, NULL,
              0, 32, 32
            );
            "#,
        )
        .unwrap();
    }

    let upgraded = Store::open(&path).unwrap();
    let mut statement = upgraded
        .conn
        .prepare(
            "SELECT replacement_id, mutation_started, tracks_prior_material, staging_initialized, \
                    preparation_complete, preparation_cursor, cleanup_phase, \
                    cleanup_source_cursor, cleanup_entity_cursor, removed_events \
             FROM provider_file_publications ORDER BY replacement_id",
        )
        .unwrap();
    let publications = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, bool>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, i64>(9)?,
            ))
        })
        .unwrap();
    let publications = publications.collect::<rusqlite::Result<Vec<_>>>().unwrap();
    assert_eq!(
        publications,
        vec![
            (
                "legacy-mutated".into(),
                true,
                true,
                false,
                false,
                None,
                0,
                None,
                None,
                0,
            ),
            (
                "legacy-mutated-incremental".into(),
                true,
                true,
                false,
                true,
                None,
                0,
                None,
                None,
                0,
            ),
            (
                "legacy-unmutated".into(),
                false,
                false,
                false,
                false,
                None,
                0,
                None,
                None,
                0,
            ),
        ]
    );
    for table in [
        "provider_file_publication_seen",
        "provider_file_publication_prior_sources",
        "provider_file_publication_batch",
    ] {
        let count: i64 = upgraded
            .conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    let default_value: String = upgraded
        .conn
        .query_row(
            "SELECT dflt_value FROM pragma_table_info('provider_file_publications') \
             WHERE name = 'staging_initialized'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(default_value, "0");

    let fresh = Store::open(temp.path().join("fresh-v52.sqlite")).unwrap();
    assert_schema_object_parity(&upgraded.conn, &fresh.conn);
}

#[test]
fn v52_migration_defers_material_classification_to_bounded_repair() {
    let temp = tempdir();
    let path = temp.path().join("v52-material-owners.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(CREATE_TABLES_SQL).unwrap();
    conn.execute_batch(
        r#"
        INSERT INTO catalog_sessions (
          source_path, provider, source_format, source_root, external_session_id,
          agent_type, file_size_bytes, file_modified_at_ms, import_revision,
          cataloged_at_ms, indexed_at_ms, indexed_file_size_bytes,
          indexed_file_modified_at_ms, indexed_status, indexed_import_revision
        ) VALUES
          ('/fixture/catalog/correct.jsonl', 'codex', 'codex_session_jsonl',
           '/fixture/catalog', 'correct', 'primary', 1, 1, 1, 1, 2, 1, 1, 'indexed', 1),
          ('/fixture/catalog/unowned.jsonl', 'codex', 'codex_session_jsonl',
           '/fixture/catalog', 'unowned', 'primary', 1, 1, 1, 1, 2, 1, 1, 'indexed', 1),
          ('/fixture/catalog/wrong-format.jsonl', 'codex', 'codex_session_jsonl',
           '/fixture/catalog', 'wrong-format', 'primary', 1, 1, 1, 1, 2, 1, 1, 'indexed', 1),
          ('/fixture/catalog/wrong-root.jsonl', 'codex', 'codex_session_jsonl',
           '/fixture/catalog', 'wrong-root', 'primary', 1, 1, 1, 1, 2, 1, 1, 'indexed', 1);

        INSERT INTO source_import_files (
          provider, source_format, source_root, source_path, file_size_bytes,
          file_modified_at_ms, import_revision, observed_at_ms, indexed_at_ms,
          indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_status,
          indexed_import_revision, metadata_json
        ) VALUES
          ('pi', 'pi_session_jsonl', '/fixture/source', '/fixture/source/correct.jsonl',
           1, 1, 1, 1, 2, 1, 1, 'indexed', 1, '{"inventory_unit":"logical_import_unit"}'),
          ('mistral_vibe', 'mistral_vibe_session_jsonl_tree', '/fixture/source',
           '/fixture/source/mistral/messages.jsonl',
           1, 1, 1, 1, 2, 1, 1, 'indexed', 1, '{"inventory_unit":"logical_import_unit"}'),
          ('pi', 'pi_session_jsonl', '/fixture/source', '/fixture/source/wrong-format.jsonl',
           1, 1, 1, 1, 2, 1, 1, 'indexed', 1, '{"inventory_unit":"logical_import_unit"}'),
          ('pi', 'pi_session_jsonl', '/fixture/source', '/fixture/source/wrong-root.jsonl',
           1, 1, 1, 1, 2, 1, 1, 'indexed', 1, '{"inventory_unit":"logical_import_unit"}');

        INSERT INTO capture_sources (
          id, kind, provider, machine_id, raw_source_path, source_format,
          source_root, external_session_id, started_at_ms, fidelity
        ) VALUES
          ('catalog-correct', 'provider_import', 'codex', 'fixture',
           '/fixture/catalog/correct.jsonl', 'codex_session_jsonl',
           '/fixture/catalog', 'correct', 1, 'imported'),
          ('catalog-wrong-format', 'provider_import', 'codex', 'fixture',
           '/fixture/catalog/wrong-format.jsonl', 'codex_session_jsonl_tree',
           '/fixture/catalog', 'wrong-format', 1, 'imported'),
          ('catalog-wrong-root', 'provider_import', 'codex', 'fixture',
           '/fixture/catalog/wrong-root.jsonl', 'codex_session_jsonl',
           '/fixture/catalog/other', 'wrong-root', 1, 'imported'),
          ('source-correct', 'provider_import', 'pi', 'fixture',
           '/fixture/source/correct.jsonl', 'pi_session_jsonl',
           '/fixture/source', NULL, 1, 'imported'),
          ('source-mistral', 'provider_import', 'mistral_vibe', 'fixture',
           '/fixture/source/mistral/messages.jsonl', 'mistral_vibe_session_jsonl',
           '/fixture/source/mistral/messages.jsonl', NULL, 1, 'imported'),
          ('source-wrong-format', 'provider_import', 'pi', 'fixture',
           '/fixture/source/wrong-format.jsonl', 'pi_session_json',
           '/fixture/source', NULL, 1, 'imported'),
          ('source-wrong-root', 'provider_import', 'pi', 'fixture',
           '/fixture/source/wrong-root.jsonl', 'pi_session_jsonl',
           '/fixture/source/other', NULL, 1, 'imported');

        INSERT INTO sessions (
          id, capture_source_id, provider, external_session_id, agent_type,
          status, fidelity, started_at_ms, created_at_ms, updated_at_ms
        ) VALUES
          ('session-correct', 'catalog-correct', 'codex', 'correct',
           'primary', 'imported', 'imported', 1, 1, 1),
          ('session-unowned', NULL, 'codex', 'unowned',
           'primary', 'imported', 'imported', 1, 1, 1),
          ('session-wrong-format', 'catalog-wrong-format', 'codex', 'wrong-format',
           'primary', 'imported', 'imported', 1, 1, 1),
          ('session-wrong-root', 'catalog-wrong-root', 'codex', 'wrong-root',
           'primary', 'imported', 'imported', 1, 1, 1);
        "#,
    )
    .unwrap();

    migrate_fresh_scheduling_to_v52(&conn).unwrap();

    let catalog_reasons = conn
        .prepare("SELECT source_path, pending_reason FROM catalog_sessions ORDER BY source_path")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        catalog_reasons,
        vec![
            ("/fixture/catalog/correct.jsonl".into(), None),
            ("/fixture/catalog/unowned.jsonl".into(), None),
            ("/fixture/catalog/wrong-format.jsonl".into(), None),
            ("/fixture/catalog/wrong-root.jsonl".into(), None),
        ]
    );
    let source_reasons = conn
        .prepare("SELECT source_path, pending_reason FROM source_import_files ORDER BY source_path")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        source_reasons,
        vec![
            ("/fixture/source/correct.jsonl".into(), None),
            ("/fixture/source/mistral/messages.jsonl".into(), None),
            ("/fixture/source/wrong-format.jsonl".into(), None),
            ("/fixture/source/wrong-root.jsonl".into(), None),
        ]
    );
    let incomplete_repairs: usize = conn
        .query_row(
            "SELECT COUNT(*) FROM import_pending_reason_repairs WHERE completed = 0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(incomplete_repairs, 2);
}

#[test]
fn v52_migration_does_not_rewrite_inventory_rows_or_churn_indexes() {
    let temp = tempdir();
    let path = temp.path().join("v52-no-rebuild.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(include_str!("../fixtures/schema_v49.sql"))
        .unwrap();
    migrate_provider_publication_to_v51(&conn).unwrap();
    let rootpages_before = ["catalog_sessions", "source_import_files"].map(|table| {
        conn.query_row(
            "SELECT rootpage FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    });
    let total_changes_before = conn.total_changes();

    let observed = Arc::new(Mutex::new(Vec::new()));
    let callback_observed = Arc::clone(&observed);
    conn.authorizer(Some(move |context: AuthContext<'_>| {
        let (description, forbidden) = match context.action {
            AuthAction::AlterTable { table_name, .. }
                if matches!(table_name, "catalog_sessions" | "source_import_files") =>
            {
                (format!("alter:{table_name}"), false)
            }
            AuthAction::Update { table_name, .. }
                if matches!(table_name, "catalog_sessions" | "source_import_files") =>
            {
                (format!("forbidden-update:{table_name}"), true)
            }
            AuthAction::CreateIndex { table_name, .. }
                if matches!(
                    table_name,
                    "capture_sources" | "catalog_sessions" | "source_import_files"
                ) =>
            {
                (format!("forbidden-index:{table_name}"), true)
            }
            AuthAction::CreateTable { table_name }
            | AuthAction::DropTable { table_name }
            | AuthAction::Insert { table_name }
                if matches!(
                    table_name,
                    "catalog_sessions"
                        | "catalog_sessions_new"
                        | "source_import_files"
                        | "source_import_files_new"
                ) =>
            {
                (format!("forbidden:{table_name}"), true)
            }
            _ => return Authorization::Allow,
        };
        callback_observed.lock().unwrap().push(description);
        if forbidden {
            Authorization::Deny
        } else {
            Authorization::Allow
        }
    }));

    migrate_fresh_scheduling_to_v52(&conn).unwrap();
    let observed = observed.lock().unwrap();
    assert!(observed
        .iter()
        .any(|action| action == "alter:catalog_sessions"));
    assert!(observed
        .iter()
        .any(|action| action == "alter:source_import_files"));
    assert!(!observed
        .iter()
        .any(|action| action.starts_with("forbidden")));
    drop(observed);
    assert_eq!(conn.total_changes() - total_changes_before, 2);

    let rootpages_after = ["catalog_sessions", "source_import_files"].map(|table| {
        conn.query_row(
            "SELECT rootpage FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    });
    assert_eq!(rootpages_after, rootpages_before);
}

#[test]
fn v53_migration_adds_bounded_completion_without_row_or_index_churn() {
    let temp = tempdir();
    let path = temp.path().join("v53-publication-completion.sqlite");
    let conn = Connection::open(&path).unwrap();
    let legacy_sql = CREATE_TABLES_SQL
        .replace(
            "    completion_payload_json TEXT CHECK (\n        completion_payload_json IS NULL OR\n        length(CAST(completion_payload_json AS BLOB)) BETWEEN 1 AND 262144\n    ),\n",
            "",
        )
        .replace(
            "    inventory_observation_invalidated INTEGER NOT NULL DEFAULT 0\n        CHECK (inventory_observation_invalidated IN (0, 1)),\n",
            "",
        )
        .replace(
            "    retirement_started INTEGER NOT NULL DEFAULT 0 CHECK (retirement_started IN (0, 1)),\n",
            "",
        );
    assert!(!legacy_sql.contains("completion_payload_json"));
    assert!(!legacy_sql.contains("inventory_observation_invalidated"));
    assert!(!legacy_sql.contains("retirement_started"));
    conn.execute_batch(&legacy_sql).unwrap();
    conn.execute_batch(INDEXES_SQL).unwrap();
    conn.execute_batch("PRAGMA user_version = 52;").unwrap();
    conn.execute_batch(
        r#"
        INSERT INTO provider_file_publications (
          replacement_id, owner_id, publication_kind, staging_id, provider,
          inventory_family, inventory_source_format, inventory_source_root,
          source_path, material_source_format, material_source_root,
          inventory_generation, file_size_bytes, file_modified_at_ms,
          import_revision, preparation_complete, started_at_ms, updated_at_ms
        ) VALUES (
          'v51-publication',
          'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
          'replacement',
          'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
          'claude', 'source_import_files', 'claude_projects_jsonl_tree',
          '/history/claude/projects', '/history/claude/projects/a.jsonl',
          'claude_projects_jsonl', '/history/claude/projects',
          1, 20, 100, 7, 1, 105, 105
        );
        "#,
    )
    .unwrap();
    let schema_objects = [
        "provider_file_publications",
        "idx_provider_file_publications_owner",
        "idx_provider_file_publications_fence",
    ];
    let rootpages_before = schema_objects.map(|name| {
        conn.query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name = ?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    });
    let total_changes_before = conn.total_changes();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let callback_observed = Arc::clone(&observed);
    conn.authorizer(Some(move |context: AuthContext<'_>| {
        let (description, forbidden) = match context.action {
            AuthAction::AlterTable { table_name, .. }
                if table_name == "provider_file_publications" =>
            {
                (format!("alter:{table_name}"), false)
            }
            AuthAction::CreateIndex { table_name, .. }
                if table_name == "provider_file_publications" =>
            {
                (format!("forbidden-index:{table_name}"), true)
            }
            AuthAction::CreateTable { table_name } | AuthAction::DropTable { table_name }
                if matches!(
                    table_name,
                    "provider_file_publications" | "provider_file_publications_new"
                ) =>
            {
                (format!("forbidden-table:{table_name}"), true)
            }
            AuthAction::Insert { table_name }
            | AuthAction::Update { table_name, .. }
            | AuthAction::Delete { table_name }
                if table_name == "provider_file_publications" =>
            {
                (format!("forbidden-row-write:{table_name}"), true)
            }
            _ => return Authorization::Allow,
        };
        callback_observed.lock().unwrap().push(description);
        if forbidden {
            Authorization::Deny
        } else {
            Authorization::Allow
        }
    }));

    migrate_publication_completion_to_v53(&conn).unwrap();
    assert_eq!(conn.total_changes(), total_changes_before);
    assert!(table_has_column(
        &conn,
        "provider_file_publications",
        "completion_payload_json"
    )
    .unwrap());
    let row: (String, Option<String>) = conn
        .query_row(
            "SELECT replacement_id, completion_payload_json \
             FROM provider_file_publications",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, ("v51-publication".into(), None));
    let rootpages_after = schema_objects.map(|name| {
        conn.query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name = ?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    });
    assert_eq!(rootpages_after, rootpages_before);
    let observed = observed.lock().unwrap();
    assert!(observed
        .iter()
        .any(|action| action == "alter:provider_file_publications"));
    assert!(!observed
        .iter()
        .any(|action| action.starts_with("forbidden")));
    drop(observed);
    drop(conn);

    let conn = Connection::open(&path).unwrap();
    let maximum_json = format!(
        "\"{}\"",
        "x".repeat(PROVIDER_FILE_PUBLICATION_COMPLETION_MAX_BYTES - 2)
    );
    assert_eq!(
        maximum_json.len(),
        PROVIDER_FILE_PUBLICATION_COMPLETION_MAX_BYTES
    );
    conn.execute(
        "UPDATE provider_file_publications SET completion_payload_json = ?1",
        [&maximum_json],
    )
    .unwrap();
    let oversized_json = format!(
        "\"{}\"",
        "x".repeat(PROVIDER_FILE_PUBLICATION_COMPLETION_MAX_BYTES - 1)
    );
    assert!(conn
        .execute(
            "UPDATE provider_file_publications SET completion_payload_json = ?1",
            [&oversized_json],
        )
        .is_err());
    migrate_publication_completion_to_v53(&conn).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT completion_payload_json FROM provider_file_publications",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        maximum_json
    );

    drop(conn);
    let upgraded = Store::open(&path).unwrap();
    let fresh = Store::open(temp.path().join("fresh-v52.sqlite")).unwrap();
    let publication_schema = |conn: &Connection| {
        schema_object_signature(conn)
            .into_iter()
            .filter(|(_, name, _)| {
                matches!(
                    name.as_str(),
                    "provider_file_publications"
                        | "idx_provider_file_publications_owner"
                        | "idx_provider_file_publications_fence"
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        publication_schema(&upgraded.conn),
        publication_schema(&fresh.conn)
    );
}

#[test]
fn v54_migration_adds_durable_retirement_state_without_row_or_index_churn() {
    let temp = tempdir();
    let path = temp.path().join("v54-publication-retirement.sqlite");
    let conn = Connection::open(&path).unwrap();
    let legacy_sql = CREATE_TABLES_SQL
        .replace(
            "    inventory_observation_invalidated INTEGER NOT NULL DEFAULT 0\n        CHECK (inventory_observation_invalidated IN (0, 1)),\n",
            "",
        )
        .replace(
            "    retirement_started INTEGER NOT NULL DEFAULT 0 CHECK (retirement_started IN (0, 1)),\n",
            "",
        );
    assert!(!legacy_sql.contains("retirement_started"));
    assert!(!legacy_sql.contains("inventory_observation_invalidated"));
    conn.execute_batch(&legacy_sql).unwrap();
    conn.execute_batch(INDEXES_SQL).unwrap();
    conn.execute_batch("PRAGMA user_version = 53;").unwrap();
    conn.execute_batch(
        r#"
        INSERT INTO provider_file_publications (
          replacement_id, owner_id, publication_kind, staging_id, provider,
          inventory_family, inventory_source_format, inventory_source_root,
          source_path, material_source_format, material_source_root,
          inventory_generation, file_size_bytes, file_modified_at_ms,
          import_revision, preparation_complete, started_at_ms, updated_at_ms
        ) VALUES (
          'v52-publication',
          'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
          'replacement',
          'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
          'claude', 'source_import_files', 'claude_projects_jsonl_tree',
          '/history/claude/projects', '/history/claude/projects/a.jsonl',
          'claude_projects_jsonl', '/history/claude/projects',
          1, 20, 100, 7, 1, 105, 105
        );
        "#,
    )
    .unwrap();
    let rootpage_before: i64 = conn
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name = 'provider_file_publications'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let total_changes_before = conn.total_changes();

    migrate_publication_retirement_to_v54(&conn).unwrap();
    assert_eq!(conn.total_changes(), total_changes_before);
    assert!(table_has_column(&conn, "provider_file_publications", "retirement_started").unwrap());
    assert!(table_has_column(
        &conn,
        "provider_file_publications",
        "inventory_observation_invalidated",
    )
    .unwrap());
    assert!(!conn
        .query_row(
            "SELECT retirement_started FROM provider_file_publications",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap());
    assert!(!conn
        .query_row(
            "SELECT inventory_observation_invalidated FROM provider_file_publications",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap());
    assert_eq!(
        conn.query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name = 'provider_file_publications'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        rootpage_before
    );
    migrate_publication_retirement_to_v54(&conn).unwrap();
    drop(conn);

    let upgraded = Store::open(&path).unwrap();
    let fresh = Store::open(temp.path().join("fresh-v53.sqlite")).unwrap();
    assert_eq!(
        schema_object_signature(&upgraded.conn)
            .into_iter()
            .find(|(_, name, _)| name == "provider_file_publications"),
        schema_object_signature(&fresh.conn)
            .into_iter()
            .find(|(_, name, _)| name == "provider_file_publications")
    );
}

#[test]
fn v54_migration_repairs_mutated_publications_that_lost_prior_material_scope() {
    let conn = Connection::open_in_memory().unwrap();
    let legacy_sql = CREATE_TABLES_SQL
        .replace(
            "    inventory_observation_invalidated INTEGER NOT NULL DEFAULT 0\n        CHECK (inventory_observation_invalidated IN (0, 1)),\n",
            "",
        )
        .replace(
            "    retirement_started INTEGER NOT NULL DEFAULT 0 CHECK (retirement_started IN (0, 1)),\n",
            "",
        );
    conn.execute_batch(&legacy_sql).unwrap();
    conn.execute_batch(INDEXES_SQL).unwrap();
    conn.execute_batch("PRAGMA user_version = 53;").unwrap();
    conn.execute_batch(
        r#"
        INSERT INTO provider_file_publications (
          replacement_id, owner_id, publication_kind, staging_id, provider,
          inventory_family, inventory_source_format, inventory_source_root,
          source_path, material_source_format, material_source_root,
          inventory_generation, file_size_bytes, file_modified_at_ms,
          import_revision, mutation_started, tracks_prior_material,
          preparation_complete, started_at_ms, updated_at_ms
        ) VALUES (
          'v52-mutated-incremental',
          'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
          'incremental',
          'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
          'codex', 'source_import_files', 'codex_sessions_root',
          '/history/codex/sessions', '/history/codex/sessions/a.jsonl',
          'codex_session_jsonl', '/history/codex/sessions',
          1, 20, 100, 7, 1, 0, 1, 105, 105
        );
        "#,
    )
    .unwrap();

    migrate_publication_retirement_to_v54(&conn).unwrap();

    assert!(conn
        .query_row(
            "SELECT tracks_prior_material FROM provider_file_publications",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap());
}

fn schema_object_signature(conn: &Connection) -> Vec<(String, String, String)> {
    conn.prepare(
        r#"
        SELECT type, name, sql
        FROM sqlite_master
        WHERE type IN ('table', 'index', 'view')
          AND name NOT LIKE 'sqlite_%'
          AND sql IS NOT NULL
        ORDER BY type, name
        "#,
    )
    .unwrap()
    .query_map([], |row| {
        let sql: String = row.get(2)?;
        Ok((
            row.get(0)?,
            row.get(1)?,
            sql.replace('"', "")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        ))
    })
    .unwrap()
    .collect::<rusqlite::Result<Vec<_>>>()
    .unwrap()
}

fn assert_schema_object_parity(upgraded: &Connection, fresh: &Connection) {
    let omit_fresh_only_indexes = |signature: Vec<(String, String, String)>| {
        signature
            .into_iter()
            .filter(|(_, name, _)| !FRESH_ONLY_OPTIMIZED_INDEX_NAMES.contains(&name.as_str()))
            .collect::<Vec<_>>()
    };
    let upgraded = omit_fresh_only_indexes(schema_object_signature(upgraded));
    let fresh = omit_fresh_only_indexes(schema_object_signature(fresh));
    let mismatch = upgraded
        .iter()
        .zip(&fresh)
        .find(|(upgraded, fresh)| upgraded != fresh);
    assert!(
        mismatch.is_none() && upgraded.len() == fresh.len(),
        "schema mismatch: upgraded objects={}, fresh objects={}, first mismatch={mismatch:?}",
        upgraded.len(),
        fresh.len(),
    );
}

fn assert_provider_migration_accepts(
    legacy_version: i64,
    provider: &str,
    source_format: &str,
    source_root: &str,
    source_path: &str,
) {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        let legacy_sql = CREATE_TABLES_SQL.replace(&format!(", '{provider}'"), "");
        conn.execute_batch(&legacy_sql).unwrap();
        conn.execute_batch(INDEXES_SQL).unwrap();
        conn.execute_batch(&format!("PRAGMA user_version = {legacy_version};"))
            .unwrap();
    }

    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (id, kind, provider, machine_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', ?2, 'test-machine', 0, 'imported')
            "#,
            params![new_id().to_string(), provider],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO catalog_sessions
            (source_path, provider, source_format, source_root, agent_type, file_size_bytes, file_modified_at_ms, cataloged_at_ms)
            VALUES (?1, ?2, ?3, ?4, 'primary', 1, 0, 0)
            "#,
            params![source_path, provider, source_format, source_root],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO source_import_files
            (provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms)
            VALUES (?1, ?2, ?3, ?4, 1, 0, 0)
            "#,
            params![provider, source_format, source_root, source_path],
        )
        .unwrap();
}
