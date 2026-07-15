fn imported_session(external_session_id: &str) -> Session {
    Session {
        id: new_id(),
        history_record_id: None,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: None,
        provider: CaptureProvider::Codex,
        external_session_id: Some(external_session_id.into()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".into()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: fixed_time(),
        ended_at: None,
        timestamps: timestamps(),
        sync: sync_metadata(),
    }
}

fn source_scoped_imported_session(external_session_id: &str, source_id: Uuid) -> Session {
    Session {
        capture_source_id: Some(source_id),
        ..imported_session(external_session_id)
    }
}

fn imported_source(source_id: Uuid, source_root: &str, external_session_id: &str) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Codex,
            machine_id: "test-machine".into(),
            process_id: None,
            cwd: Some("/repo".into()),
            raw_source_path: Some(format!("{source_root}/session.jsonl")),
            source_format: Some("codex_session_jsonl".into()),
            source_root: Some(source_root.into()),
            source_identity: None,
            external_session_id: Some(external_session_id.into()),
        },
        started_at: fixed_time(),
        ended_at: None,
        sync: sync_metadata(),
    }
}

fn upsert_catalog_material(
    store: &Store,
    source_root: &str,
    source_path: &str,
    external_session_id: &str,
) {
    let source_id = new_id();
    let mut source = imported_source(source_id, source_root, external_session_id);
    source.descriptor.raw_source_path = Some(source_path.to_owned());
    store.upsert_capture_source(&source).unwrap();
    store
        .upsert_session(&source_scoped_imported_session(
            external_session_id,
            source_id,
        ))
        .unwrap();
}

fn upsert_source_material(store: &Store, file: &SourceImportFile) {
    let mut source = imported_source(new_id(), &file.source_root, "source-material");
    source.descriptor.provider = file.provider;
    source.descriptor.raw_source_path = Some(file.source_path.clone());
    source.descriptor.source_format =
        Some(expected_material_source_format(file.provider, &file.source_format).to_owned());
    store.upsert_capture_source(&source).unwrap();
}

fn session_event(session_id: Uuid, index: u64) -> Event {
    Event {
        id: new_id(),
        seq: index,
        history_record_id: None,
        session_id: Some(session_id),
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::Assistant),
        occurred_at: fixed_time() + chrono::Duration::seconds(index as i64),
        capture_source_id: None,
        payload: serde_json::json!({"index": index}),
        payload_blob_id: None,
        dedupe_key: None,
        sync: sync_metadata(),
    }
}

fn artifact_record(id: Uuid, byte_size: u64) -> Artifact {
    Artifact {
        id,
        kind: ArtifactKind::Markdown,
        blob_hash: format!("{:064x}", 1),
        blob_path: format!("{OBJECTS_DIR}/00/test-artifact"),
        byte_size,
        media_type: Some("text/markdown".to_owned()),
        preview_text: Some("artifact preview".to_owned()),
        timestamps: timestamps(),
        source_id: None,
        sync: sync_metadata(),
    }
}

fn assert_sql_conversion_error<T: std::fmt::Debug>(result: Result<T>) {
    assert!(
        matches!(result, Err(StoreError::Sql(_))),
        "expected sqlite conversion error, got {result:?}"
    );
}

#[test]
fn catalog_session_upsert_skips_unchanged_rows() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    let session = catalog_session(
        "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
        "codex-session-1",
        cataloged_at_ms,
    );
    let inventory_generation = store
        .allocate_catalog_inventory_generation(session.provider, &session.source_root)
        .unwrap();
    store
        .upsert_catalog_sessions(inventory_generation, std::slice::from_ref(&session))
        .unwrap();
    let after_insert: i64 = store
        .conn
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();

    let mut recataloged = session.clone();
    recataloged.cataloged_at_ms += 1_000;
    store
        .upsert_catalog_sessions(inventory_generation, std::slice::from_ref(&recataloged))
        .unwrap();
    let after_noop: i64 = store
        .conn
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    assert_eq!(after_noop, after_insert);

    let mut changed = recataloged;
    changed.file_size_bytes += 1;
    changed.cataloged_at_ms += 1_000;
    store
        .upsert_catalog_sessions(inventory_generation, std::slice::from_ref(&changed))
        .unwrap();
    let after_changed: i64 = store
        .conn
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    assert!(after_changed > after_noop);
}

#[test]
fn events_for_session_window_returns_bounded_neighbors() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let session = imported_session("window-session");
    store.upsert_session(&session).unwrap();
    let events = (0..10)
        .map(|index| {
            let event = session_event(session.id, index);
            store.upsert_event(&event).unwrap();
            event
        })
        .collect::<Vec<_>>();

    let middle = store
        .events_for_session_window(&events[5], 2, 3)
        .unwrap()
        .into_iter()
        .map(|event| event.seq)
        .collect::<Vec<_>>();
    assert_eq!(middle, vec![3, 4, 5, 6, 7, 8]);

    let first = store
        .events_for_session_window(&events[0], 50, 1)
        .unwrap()
        .into_iter()
        .map(|event| event.seq)
        .collect::<Vec<_>>();
    assert_eq!(first, vec![0, 1]);

    let last = store
        .events_for_session_window(&events[9], 1, 50)
        .unwrap()
        .into_iter()
        .map(|event| event.seq)
        .collect::<Vec<_>>();
    assert_eq!(last, vec![8, 9]);
}

#[test]
fn sessions_by_external_session_limited_caps_ambiguity_scan() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    for index in 0..5 {
        let mut session = imported_session("shared-provider-session");
        session.started_at = fixed_time() + chrono::Duration::seconds(index);
        store.upsert_session(&session).unwrap();
    }

    let matches = store
        .sessions_by_external_session_limited(CaptureProvider::Codex, "shared-provider-session", 2)
        .unwrap();

    assert_eq!(matches.len(), 2);
    assert_eq!(
        matches[0].external_session_id.as_deref(),
        Some("shared-provider-session")
    );
}

#[test]
fn search_index_optimize_is_safe_on_initialized_store() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    store.optimize_search_index().unwrap();
}

#[test]
fn catalog_sessions_count_indexed_and_stale_rows() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    upsert_catalog_inventory(
        &store,
        &[catalog_session(
            "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
            "codex-session-1",
            cataloged_at_ms,
        )],
    );

    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.total, 1);
    assert_eq!(counts.indexed, 0);
    assert_eq!(counts.stale, 0);
    assert_eq!(counts.pending, 1);
    assert_eq!(counts.failed, 0);
    assert_eq!(
        store
            .catalog_source_stale_session_count(
                CaptureProvider::Codex,
                "/home/user/.codex/sessions"
            )
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .list_pending_catalog_sessions(CaptureProvider::Codex, "/home/user/.codex/sessions")
            .unwrap()
            .len(),
        1
    );

    upsert_catalog_material(
        &store,
        "/home/user/.codex/sessions",
        "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
        "codex-session-1",
    );
    store
        .mark_catalog_source_indexed(
            CaptureProvider::Codex,
            CatalogSourceIndexUpdate {
                source_root: "/home/user/.codex/sessions",
                source_path: "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
                file_size_bytes: 42,
                file_modified_at_ms: cataloged_at_ms,
                import_revision: 1,
                inventory_generation: current_catalog_generation(
                    &store,
                    CaptureProvider::Codex,
                    "/home/user/.codex/sessions",
                ),
                file_sha256: None,
                event_count: Some(3),
                indexed_at_ms: cataloged_at_ms + 10,
            },
        )
        .unwrap();
    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.indexed, 1);
    assert_eq!(counts.pending, 0);

    store
        .mark_catalog_source_stale(
            CaptureProvider::Codex,
            "/home/user/.codex/sessions",
            cataloged_at_ms + 1,
        )
        .unwrap();
    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.total, 0);
    assert_eq!(counts.indexed, 0);
    assert_eq!(counts.stale, 1);
    assert_eq!(counts.pending, 0);
    assert_eq!(
        store
            .catalog_source_stale_session_count(
                CaptureProvider::Codex,
                "/home/user/.codex/sessions"
            )
            .unwrap(),
        1
    );
}

#[test]
fn catalog_import_planning_requires_current_index_state_and_matching_session() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    upsert_catalog_inventory(
        &store,
        &[catalog_session(
            "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
            "codex-session-1",
            cataloged_at_ms,
        )],
    );
    store
        .mark_catalog_source_indexed(
            CaptureProvider::Codex,
            CatalogSourceIndexUpdate {
                source_root: "/home/user/.codex/sessions",
                source_path: "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
                file_size_bytes: 42,
                file_modified_at_ms: cataloged_at_ms,
                import_revision: 1,
                inventory_generation: current_catalog_generation(
                    &store,
                    CaptureProvider::Codex,
                    "/home/user/.codex/sessions",
                ),
                file_sha256: None,
                event_count: Some(3),
                indexed_at_ms: cataloged_at_ms + 10,
            },
        )
        .unwrap();

    let pending = store
        .list_pending_catalog_sessions(CaptureProvider::Codex, "/home/user/.codex/sessions")
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(store.catalog_session_counts().unwrap().indexed, 0);

    upsert_catalog_material(
        &store,
        "/home/user/.codex/sessions",
        "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
        "codex-session-1",
    );
    let pending = store
        .list_pending_catalog_sessions(CaptureProvider::Codex, "/home/user/.codex/sessions")
        .unwrap();
    assert!(pending.is_empty());
    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.indexed, 1);
    assert_eq!(counts.pending, 0);
}

#[test]
fn catalog_import_planning_scopes_matching_sessions_by_source_root() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    let first_root = "/home/user/.codex/first/sessions";
    let second_root = "/home/user/.codex/second/sessions";
    let first_path = "/home/user/.codex/first/sessions/rollout.jsonl";
    let second_path = "/home/user/.codex/second/sessions/rollout.jsonl";
    let external_session_id = "shared-provider-session";
    upsert_catalog_inventory(
        &store,
        &[
            catalog_session_for_root(first_root, first_path, external_session_id, cataloged_at_ms),
            catalog_session_for_root(
                second_root,
                second_path,
                external_session_id,
                cataloged_at_ms,
            ),
        ],
    );
    for (source_root, source_path) in [(first_root, first_path), (second_root, second_path)] {
        store
            .mark_catalog_source_indexed(
                CaptureProvider::Codex,
                CatalogSourceIndexUpdate {
                    source_root,
                    source_path,
                    file_size_bytes: 42,
                    file_modified_at_ms: cataloged_at_ms,
                    import_revision: 1,
                    inventory_generation: current_catalog_generation(
                        &store,
                        CaptureProvider::Codex,
                        source_root,
                    ),
                    file_sha256: None,
                    event_count: Some(3),
                    indexed_at_ms: cataloged_at_ms + 10,
                },
            )
            .unwrap();
    }

    upsert_catalog_material(&store, first_root, first_path, external_session_id);

    assert!(store
        .list_pending_catalog_sessions(CaptureProvider::Codex, first_root)
        .unwrap()
        .is_empty());
    let second_pending = store
        .list_pending_catalog_sessions(CaptureProvider::Codex, second_root)
        .unwrap();
    assert_eq!(second_pending.len(), 1);
    assert_eq!(second_pending[0].source_path, second_path);
    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.indexed, 1);
    assert_eq!(counts.pending, 1);
}

#[test]
fn catalog_material_requires_the_exact_owned_codex_tuple() {
    for scenario in ["unowned", "wrong_format", "wrong_root", "wrong_external"] {
        let temp = tempdir();
        let store = Store::open(temp.path().join(format!("{scenario}.sqlite"))).unwrap();
        let root = "/fixture/codex/sessions";
        let path = "/fixture/codex/sessions/session.jsonl";
        let external_session_id = "exact-owner";
        let observed_at_ms = timestamp_ms(fixed_time());
        let mut catalog = catalog_session_for_root(root, path, external_session_id, observed_at_ms);
        upsert_catalog_inventory(&store, std::slice::from_ref(&catalog));
        store
            .mark_catalog_source_indexed(
                catalog.provider,
                CatalogSourceIndexUpdate {
                    source_root: root,
                    source_path: path,
                    file_size_bytes: catalog.file_size_bytes,
                    file_modified_at_ms: catalog.file_modified_at_ms,
                    import_revision: catalog.import_revision,
                    inventory_generation: current_catalog_generation(
                        &store,
                        catalog.provider,
                        root,
                    ),
                    file_sha256: None,
                    event_count: Some(1),
                    indexed_at_ms: observed_at_ms + 1,
                },
            )
            .unwrap();

        if scenario == "unowned" {
            store
                .upsert_session(&imported_session(external_session_id))
                .unwrap();
        } else {
            let source_id = new_id();
            let mut source = imported_source(source_id, root, external_session_id);
            source.descriptor.raw_source_path = Some(path.to_owned());
            match scenario {
                "wrong_format" => {
                    source.descriptor.source_format = Some("codex_session_jsonl_tree".into())
                }
                "wrong_root" => source.descriptor.source_root = Some("/fixture/other".into()),
                "wrong_external" => {
                    source.descriptor.external_session_id = Some("other-session".into())
                }
                _ => unreachable!(),
            }
            store.upsert_capture_source(&source).unwrap();
            store
                .upsert_session(&source_scoped_imported_session(
                    external_session_id,
                    source_id,
                ))
                .unwrap();
        }

        catalog.cataloged_at_ms += 1;
        upsert_catalog_inventory(&store, std::slice::from_ref(&catalog));
        let recovery = store
            .list_catalog_import_work(catalog.provider, root, ImportWorkClass::Recovery, 10)
            .unwrap();
        assert_eq!(
            recovery[0].reason,
            ImportPendingReason::MissingMaterial,
            "{scenario}"
        );
        let counts = store.catalog_session_counts().unwrap();
        assert_eq!(counts.indexed, 0, "{scenario}");
        assert_eq!(counts.pending, 1, "{scenario}");
    }
}

#[test]
fn catalog_import_mark_failed_records_error_and_remains_pending() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    upsert_catalog_inventory(
        &store,
        &[catalog_session(
            "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
            "codex-session-1",
            cataloged_at_ms,
        )],
    );

    let changed = store
        .record_catalog_source_import_result(
            CaptureProvider::Codex,
            CatalogSourceIndexUpdate {
                source_root: "/home/user/.codex/sessions",
                source_path: "/home/user/.codex/sessions/2026/06/24/rollout.jsonl",
                file_size_bytes: 42,
                file_modified_at_ms: cataloged_at_ms,
                import_revision: 1,
                inventory_generation: current_catalog_generation(
                    &store,
                    CaptureProvider::Codex,
                    "/home/user/.codex/sessions",
                ),
                file_sha256: None,
                event_count: None,
                indexed_at_ms: cataloged_at_ms + 10,
            },
            CatalogIndexedStatus::Failed,
            Some("bad json"),
        )
        .unwrap();
    assert_eq!(changed, 1);

    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.pending, 1);
    let (status, error, indexed_at_ms): (String, Option<String>, Option<i64>) = store
        .conn
        .query_row(
            "SELECT indexed_status, indexed_error, indexed_at_ms FROM catalog_sessions WHERE source_path = ?1",
            ["/home/user/.codex/sessions/2026/06/24/rollout.jsonl"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, CatalogIndexedStatus::Failed.as_str());
    assert_eq!(error.as_deref(), Some("bad json"));
    assert_eq!(indexed_at_ms, Some(cataloged_at_ms + 10));
}

#[test]
fn catalog_upsert_clears_completion_metadata_but_preserves_append_checkpoint() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let cataloged_at_ms = timestamp_ms(fixed_time());
    let source_path = "/home/user/.codex/sessions/2026/06/24/rollout.jsonl";
    upsert_catalog_inventory(
        &store,
        &[catalog_session(
            source_path,
            "codex-session-1",
            cataloged_at_ms,
        )],
    );
    upsert_catalog_material(
        &store,
        "/home/user/.codex/sessions",
        source_path,
        "codex-session-1",
    );
    store
        .mark_catalog_source_indexed(
            CaptureProvider::Codex,
            CatalogSourceIndexUpdate {
                source_root: "/home/user/.codex/sessions",
                source_path,
                file_size_bytes: 42,
                file_modified_at_ms: cataloged_at_ms,
                import_revision: 1,
                inventory_generation: current_catalog_generation(
                    &store,
                    CaptureProvider::Codex,
                    "/home/user/.codex/sessions",
                ),
                file_sha256: None,
                event_count: Some(3),
                indexed_at_ms: cataloged_at_ms + 10,
            },
        )
        .unwrap();

    upsert_catalog_inventory(
        &store,
        &[catalog_session(
            source_path,
            "codex-session-1",
            cataloged_at_ms,
        )],
    );
    assert_eq!(store.catalog_session_counts().unwrap().indexed, 1);

    let mut changed = catalog_session(source_path, "codex-session-1", cataloged_at_ms + 1);
    changed.file_size_bytes = 43;
    upsert_catalog_inventory(&store, &[changed]);

    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.indexed, 0);
    assert_eq!(counts.pending, 1);
    let (
        status,
        indexed_at_ms,
        indexed_size,
        indexed_mtime,
        indexed_event_count,
        checkpoint_at_ms,
        checkpoint_size,
        checkpoint_mtime,
        checkpoint_event_count,
    ): CatalogSessionCheckpointRow = store
        .conn
        .query_row(
            "SELECT indexed_status, indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_event_count, last_imported_at_ms, last_imported_file_size_bytes, last_imported_file_modified_at_ms, last_imported_event_count FROM catalog_sessions WHERE source_path = ?1",
            [source_path],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(status, CatalogIndexedStatus::Pending.as_str());
    assert_eq!(indexed_at_ms, None);
    assert_eq!(indexed_size, None);
    assert_eq!(indexed_mtime, None);
    assert_eq!(indexed_event_count, None);
    assert_eq!(checkpoint_at_ms, Some(cataloged_at_ms + 10));
    assert_eq!(checkpoint_size, Some(42));
    assert_eq!(checkpoint_mtime, Some(cataloged_at_ms));
    assert_eq!(checkpoint_event_count, Some(3));

    let checkpoint = store
        .catalog_source_index_state(
            CaptureProvider::Codex,
            "/home/user/.codex/sessions",
            source_path,
        )
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint.last_imported_file_size_bytes, Some(42));
    assert_eq!(
        checkpoint.last_imported_file_modified_at_ms,
        Some(cataloged_at_ms)
    );
    assert_eq!(checkpoint.last_imported_file_sha256, None);
    assert_eq!(checkpoint.last_imported_event_count, Some(3));
    assert_eq!(checkpoint.last_imported_at_ms, Some(cataloged_at_ms + 10));
}

#[test]
fn completed_with_rejections_self_rooted_material_resumes_from_safe_tail() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let source_path = "/home/user/.codex/sessions/2026/06/24/mixed-tail.jsonl";
    let initial = catalog_session(source_path, "mixed-tail", observed_at_ms);
    upsert_catalog_inventory(&store, &[initial.clone()]);
    upsert_catalog_material(
        &store,
        "/home/user/.codex/sessions",
        source_path,
        "mixed-tail",
    );
    store
        .conn
        .execute(
            "UPDATE capture_sources SET source_root = raw_source_path \
             WHERE provider = 'codex' AND external_session_id = 'mixed-tail'",
            [],
        )
        .unwrap();
    assert!(store.catalog_session_material_exists(&initial).unwrap());
    store
        .mark_catalog_source_indexed(
            CaptureProvider::Codex,
            CatalogSourceIndexUpdate {
                source_root: "/home/user/.codex/sessions",
                source_path,
                file_size_bytes: 42,
                file_modified_at_ms: observed_at_ms,
                import_revision: 1,
                inventory_generation: current_catalog_generation(
                    &store,
                    CaptureProvider::Codex,
                    "/home/user/.codex/sessions",
                ),
                file_sha256: Some("safe-prefix"),
                event_count: Some(3),
                indexed_at_ms: observed_at_ms + 10,
            },
        )
        .unwrap();

    let mut appended = catalog_session(source_path, "mixed-tail", observed_at_ms + 1);
    appended.file_size_bytes = 64;
    upsert_catalog_inventory(&store, &[appended]);
    assert_eq!(
        store
            .record_catalog_source_import_result(
                CaptureProvider::Codex,
                CatalogSourceIndexUpdate {
                    source_root: "/home/user/.codex/sessions",
                    source_path,
                    file_size_bytes: 64,
                    file_modified_at_ms: observed_at_ms + 1,
                    import_revision: 1,
                    inventory_generation: current_catalog_generation(
                        &store,
                        CaptureProvider::Codex,
                        "/home/user/.codex/sessions",
                    ),
                    file_sha256: None,
                    event_count: Some(4),
                    indexed_at_ms: observed_at_ms + 20,
                },
                CatalogIndexedStatus::CompletedWithRejections,
                Some("line 4: malformed record"),
            )
            .unwrap(),
        1
    );

    assert!(store
        .list_pending_catalog_sessions(CaptureProvider::Codex, "/home/user/.codex/sessions")
        .unwrap()
        .is_empty());
    let state = store
        .catalog_source_index_state(
            CaptureProvider::Codex,
            "/home/user/.codex/sessions",
            source_path,
        )
        .unwrap()
        .unwrap();
    assert_eq!(state.last_imported_file_size_bytes, Some(42));
    assert_eq!(
        state.last_imported_file_sha256.as_deref(),
        Some("safe-prefix")
    );
    assert_eq!(state.last_imported_event_count, Some(3));
    let observation: (String, i64, i64, i64) = store
        .conn
        .query_row(
            "SELECT indexed_status, indexed_file_size_bytes, indexed_file_modified_at_ms, indexed_import_revision FROM catalog_sessions WHERE source_path = ?1",
            [source_path],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        observation,
        (
            "completed_with_rejections".to_owned(),
            64,
            observed_at_ms + 1,
            1,
        )
    );

    let mut appended_again = catalog_session(source_path, "mixed-tail", observed_at_ms + 2);
    appended_again.file_size_bytes = 80;
    upsert_catalog_inventory(&store, &[appended_again]);
    let carried = store
        .catalog_source_index_state(
            CaptureProvider::Codex,
            "/home/user/.codex/sessions",
            source_path,
        )
        .unwrap()
        .unwrap();
    assert_eq!(carried.last_imported_file_size_bytes, Some(42));
    assert_eq!(
        carried.last_imported_file_sha256.as_deref(),
        Some("safe-prefix")
    );
    assert_eq!(carried.last_imported_event_count, Some(3));
    assert_eq!(
        store
            .list_pending_catalog_sessions(CaptureProvider::Codex, "/home/user/.codex/sessions")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn stale_revision_results_cannot_complete_newer_inventory() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let source_root = "/home/user/.codex/sessions";
    let source_path = "/home/user/.codex/sessions/revised.jsonl";
    let mut session = catalog_session(source_path, "revised", observed_at_ms);
    let stale_catalog_generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, source_root)
        .unwrap();
    store
        .upsert_catalog_sessions(stale_catalog_generation, &[session.clone()])
        .unwrap();
    session.import_revision = 2;
    session.cataloged_at_ms += 1;
    let current_catalog_generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, source_root)
        .unwrap();
    store
        .upsert_catalog_sessions(current_catalog_generation, &[session.clone()])
        .unwrap();

    let stale_catalog = store
        .record_catalog_source_import_result(
            CaptureProvider::Codex,
            CatalogSourceIndexUpdate {
                source_root,
                source_path,
                file_size_bytes: session.file_size_bytes,
                file_modified_at_ms: session.file_modified_at_ms,
                import_revision: 1,
                inventory_generation: stale_catalog_generation,
                file_sha256: None,
                event_count: Some(1),
                indexed_at_ms: observed_at_ms + 2,
            },
            CatalogIndexedStatus::Indexed,
            None,
        )
        .unwrap();
    assert_eq!(stale_catalog, 0);
    assert_eq!(
        store
            .list_pending_catalog_sessions(CaptureProvider::Codex, source_root)
            .unwrap(),
        vec![session]
    );

    let file_root = "/home/user/.claude/projects";
    let mut file = source_import_file(
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
        file_root,
        "/home/user/.claude/projects/revised.jsonl",
        observed_at_ms,
    );
    let stale_file_generation = store
        .allocate_source_import_inventory_generation(CaptureProvider::Claude, file_root)
        .unwrap();
    store
        .upsert_source_import_files(stale_file_generation, std::slice::from_ref(&file))
        .unwrap();
    file.import_revision = 2;
    file.observed_at_ms += 1;
    let current_file_generation = store
        .allocate_source_import_inventory_generation(CaptureProvider::Claude, file_root)
        .unwrap();
    store
        .upsert_source_import_files(current_file_generation, std::slice::from_ref(&file))
        .unwrap();

    let stale_file = store
        .record_source_import_file_result(
            CaptureProvider::Claude,
            SourceImportFileIndexUpdate {
                source_root: file_root,
                source_path: &file.source_path,
                file_size_bytes: file.file_size_bytes,
                file_modified_at_ms: file.file_modified_at_ms,
                import_revision: 1,
                inventory_generation: stale_file_generation,
                metadata: &file.metadata,
                indexed_at_ms: observed_at_ms + 2,
            },
            CatalogIndexedStatus::Rejected,
            Some("stale parser result"),
        )
        .unwrap();
    assert_eq!(stale_file, 0);
    assert_eq!(
        store
            .list_pending_source_import_files(CaptureProvider::Claude, file_root)
            .unwrap(),
        vec![file]
    );
}

#[test]
fn catalog_append_discards_checkpoint_past_prior_observation() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let source_path = "/home/user/.codex/sessions/invalid-checkpoint.jsonl";
    upsert_catalog_inventory(
        &store,
        &[catalog_session(
            source_path,
            "invalid-checkpoint",
            observed_at_ms,
        )],
    );
    store
        .mark_catalog_source_indexed(
            CaptureProvider::Codex,
            CatalogSourceIndexUpdate {
                source_root: "/home/user/.codex/sessions",
                source_path,
                file_size_bytes: 42,
                file_modified_at_ms: observed_at_ms,
                import_revision: 1,
                inventory_generation: current_catalog_generation(
                    &store,
                    CaptureProvider::Codex,
                    "/home/user/.codex/sessions",
                ),
                file_sha256: Some("invalid-prefix"),
                event_count: Some(3),
                indexed_at_ms: observed_at_ms + 1,
            },
        )
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE catalog_sessions SET last_imported_file_size_bytes = 43 WHERE source_path = ?1",
            [source_path],
        )
        .unwrap();

    let mut appended = catalog_session(source_path, "invalid-checkpoint", observed_at_ms + 2);
    appended.file_size_bytes = 64;
    upsert_catalog_inventory(&store, &[appended]);

    let state = store
        .catalog_source_index_state(
            CaptureProvider::Codex,
            "/home/user/.codex/sessions",
            source_path,
        )
        .unwrap()
        .unwrap();
    assert_eq!(state.last_imported_file_size_bytes, None);
    assert_eq!(state.last_imported_file_sha256, None);
}

#[test]
fn terminal_rejected_catalog_observation_needs_no_materialized_session() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let source_path = "/home/user/.codex/sessions/2026/06/24/all-invalid.jsonl";
    upsert_catalog_inventory(
        &store,
        &[catalog_session(source_path, "all-invalid", observed_at_ms)],
    );
    store
        .record_catalog_source_import_result(
            CaptureProvider::Codex,
            CatalogSourceIndexUpdate {
                source_root: "/home/user/.codex/sessions",
                source_path,
                file_size_bytes: 42,
                file_modified_at_ms: observed_at_ms,
                import_revision: 1,
                inventory_generation: current_catalog_generation(
                    &store,
                    CaptureProvider::Codex,
                    "/home/user/.codex/sessions",
                ),
                file_sha256: None,
                event_count: Some(0),
                indexed_at_ms: observed_at_ms + 10,
            },
            CatalogIndexedStatus::Rejected,
            Some("all content records were rejected"),
        )
        .unwrap();

    assert!(store
        .list_pending_catalog_sessions(CaptureProvider::Codex, "/home/user/.codex/sessions")
        .unwrap()
        .is_empty());
    let counts = store.catalog_session_counts().unwrap();
    assert_eq!(counts.rejected, 1);
    assert_eq!(counts.pending, 0);
    assert_eq!(counts.indexed, 0);
}

#[test]
fn source_outcomes_converge_and_revision_invalidation_is_provider_scoped() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let claude_root = "/home/user/.claude/projects";
    let antigravity_root = "/home/user/.gemini/antigravity/brain";
    let claude = source_import_file(
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
        claude_root,
        "/home/user/.claude/projects/mixed.jsonl",
        observed_at_ms,
    );
    let antigravity = source_import_file(
        CaptureProvider::Antigravity,
        "antigravity_cli_transcript_jsonl_tree",
        antigravity_root,
        "/home/user/.gemini/antigravity/brain/rejected.jsonl",
        observed_at_ms,
    );
    upsert_source_inventory(&store, &[claude.clone(), antigravity.clone()]);
    for (file, status) in [
        (&claude, CatalogIndexedStatus::CompletedWithRejections),
        (&antigravity, CatalogIndexedStatus::Rejected),
    ] {
        store
            .record_source_import_file_result(
                file.provider,
                SourceImportFileIndexUpdate {
                    source_root: &file.source_root,
                    source_path: &file.source_path,
                    file_size_bytes: file.file_size_bytes,
                    file_modified_at_ms: file.file_modified_at_ms,
                    import_revision: file.import_revision,
                    inventory_generation: current_source_generation(
                        &store,
                        file.provider,
                        &file.source_root,
                    ),
                    metadata: &file.metadata,
                    indexed_at_ms: observed_at_ms + 10,
                },
                status,
                Some("deterministic rejection"),
            )
            .unwrap();
    }
    upsert_source_material(&store, &claude);

    assert!(store
        .list_pending_source_import_files(CaptureProvider::Claude, claude_root)
        .unwrap()
        .is_empty());
    assert!(store
        .list_pending_source_import_files(CaptureProvider::Antigravity, antigravity_root)
        .unwrap()
        .is_empty());

    let mut revised_claude = claude.clone();
    revised_claude.import_revision = 2;
    revised_claude.observed_at_ms += 20;
    upsert_source_inventory(&store, &[revised_claude.clone()]);
    assert_eq!(
        store
            .list_pending_source_import_files(CaptureProvider::Claude, claude_root)
            .unwrap(),
        vec![revised_claude]
    );
    assert!(store
        .list_pending_source_import_files(CaptureProvider::Antigravity, antigravity_root)
        .unwrap()
        .is_empty());

    let mut corrected_antigravity = antigravity;
    corrected_antigravity.file_size_bytes += 1;
    corrected_antigravity.file_modified_at_ms += 1;
    corrected_antigravity.observed_at_ms += 20;
    upsert_source_inventory(&store, &[corrected_antigravity.clone()]);
    assert_eq!(
        store
            .list_pending_source_import_files(CaptureProvider::Antigravity, antigravity_root)
            .unwrap(),
        vec![corrected_antigravity]
    );
}
