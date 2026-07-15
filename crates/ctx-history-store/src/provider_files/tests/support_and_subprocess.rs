#[allow(clippy::too_many_arguments)]
fn insert_reconciliation_fixture(
    store: &Store,
    source_a: Uuid,
    source_b: Uuid,
    shared_session: Uuid,
    removed_session: Uuid,
    old_run: Uuid,
    new_run: Uuid,
    other_run: Uuid,
    old_event: Uuid,
    new_event: Uuid,
    other_event: Uuid,
    old_file: Uuid,
    new_file: Uuid,
    old_edge: Uuid,
    new_edge: Uuid,
) {
    let path_b = "/history/claude/projects/b.jsonl";
    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
                (id, kind, provider, machine_id, raw_source_path, source_format, source_root,
                 external_session_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', 'claude', 'machine', ?2, ?3, ?4, ?5, 1, 'imported')
            "#,
            params![
                source_a.to_string(),
                PATH_A,
                MATERIAL_FORMAT,
                ROOT,
                "session-a"
            ],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
                (id, kind, provider, machine_id, raw_source_path, source_format, source_root,
                 external_session_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', 'claude', 'machine', ?2, ?3, ?4, ?5, 1, 'imported')
            "#,
            params![
                source_b.to_string(),
                path_b,
                MATERIAL_FORMAT,
                ROOT,
                "session-b"
            ],
        )
        .unwrap();
    for (session, source, external) in [
        (shared_session, source_a, "shared"),
        (removed_session, source_a, "removed"),
    ] {
        store
            .conn
            .execute(
                r#"
                INSERT INTO sessions
                    (id, capture_source_id, provider, external_session_id, agent_type, is_primary,
                     status, fidelity, started_at_ms, created_at_ms, updated_at_ms)
                VALUES (?1, ?2, 'claude', ?3, 'primary', 1, 'imported', 'imported', 1, 1, 1)
                "#,
                params![session.to_string(), source.to_string(), external],
            )
            .unwrap();
    }
    for (run, session, source) in [
        (old_run, removed_session, source_a),
        (new_run, shared_session, source_a),
        (other_run, shared_session, source_b),
    ] {
        store
            .conn
            .execute(
                r#"
                INSERT INTO runs
                    (id, session_id, run_type, status, started_at_ms, created_at_ms, updated_at_ms,
                     source_id)
                VALUES (?1, ?2, 'agent_turn', 'succeeded', 1, 1, 1, ?3)
                "#,
                params![run.to_string(), session.to_string(), source.to_string()],
            )
            .unwrap();
    }
    for (seq, event, session, run, source, text) in [
        (1, old_event, removed_session, old_run, source_a, "old"),
        (2, new_event, shared_session, new_run, source_a, "new"),
        (3, other_event, shared_session, other_run, source_b, "other"),
    ] {
        store
            .conn
            .execute(
                r#"
                INSERT INTO events
                    (id, seq, session_id, run_id, event_type, role, occurred_at_ms,
                     capture_source_id, payload_json)
                VALUES (?1, ?2, ?3, ?4, 'message', 'user', 1, ?5, ?6)
                "#,
                params![
                    event.to_string(),
                    seq,
                    session.to_string(),
                    run.to_string(),
                    source.to_string(),
                    json!({"text": text}).to_string(),
                ],
            )
            .unwrap();
    }
    for (file, event, run) in [
        (old_file, old_event, old_run),
        (new_file, new_event, new_run),
    ] {
        store
            .conn
            .execute(
                r#"
                INSERT INTO files_touched
                    (id, run_id, event_id, path, created_at_ms, updated_at_ms, source_id)
                VALUES (?1, ?2, ?3, 'src/lib.rs', 1, 1, ?4)
                "#,
                params![
                    file.to_string(),
                    run.to_string(),
                    event.to_string(),
                    source_a.to_string(),
                ],
            )
            .unwrap();
    }
    for (edge, from, to) in [
        (old_edge, removed_session, shared_session),
        (new_edge, shared_session, shared_session),
    ] {
        store
            .conn
            .execute(
                r#"
                INSERT INTO session_edges
                    (id, from_session_id, to_session_id, edge_type, created_at_ms, updated_at_ms,
                     source_id)
                VALUES (?1, ?2, ?3, 'imported_related', 1, 1, ?4)
                "#,
                params![
                    edge.to_string(),
                    from.to_string(),
                    to.to_string(),
                    source_a.to_string(),
                ],
            )
            .unwrap();
    }
    if table_exists(&store.conn, "event_search").unwrap() {
        for event in [old_event, new_event, other_event] {
            store
                .conn
                .execute(
                    "INSERT INTO event_search (event_id, preview_text) VALUES (?1, 'text')",
                    params![event.to_string()],
                )
                .unwrap();
        }
    }
}

fn row_exists(store: &Store, table: &str, id: Uuid) -> bool {
    store
        .conn
        .query_row(
            &format!("SELECT 1 FROM {table} WHERE id = ?1"),
            params![id.to_string()],
            |_| Ok(()),
        )
        .optional()
        .unwrap()
        .is_some()
}

fn session_deleted_at(store: &Store, id: Uuid) -> Option<i64> {
    store
        .conn
        .query_row(
            "SELECT deleted_at_ms FROM sessions WHERE id = ?1",
            params![id.to_string()],
            |row| row.get(0),
        )
        .unwrap()
}

fn projection_row_exists(store: &Store, event_id: Uuid) -> bool {
    if !table_exists(&store.conn, "event_search").unwrap() {
        return false;
    }
    store
        .conn
        .query_row(
            "SELECT 1 FROM event_search WHERE event_id = ?1",
            params![event_id.to_string()],
            |_| Ok(()),
        )
        .optional()
        .unwrap()
        .is_some()
}

fn insert_capture_source(
    store: &Store,
    source_id: Uuid,
    source_path: &str,
    external_session_id: &str,
) {
    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
                (id, kind, provider, machine_id, raw_source_path, source_format, source_root,
                 external_session_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', 'claude', 'machine', ?2, ?3, ?4, ?5, 1, 'imported')
            "#,
            params![
                source_id.to_string(),
                source_path,
                MATERIAL_FORMAT,
                ROOT,
                external_session_id,
            ],
        )
        .unwrap();
}

fn insert_raw_event(store: &Store, event_id: Uuid, seq: i64, source_id: Uuid, text: &str) {
    store
        .conn
        .execute(
            r#"
            INSERT INTO events
                (id, seq, event_type, role, occurred_at_ms, capture_source_id, payload_json)
            VALUES (?1, ?2, 'message', 'user', 1, ?3, ?4)
            "#,
            params![
                event_id.to_string(),
                seq,
                source_id.to_string(),
                json!({"text": text}).to_string(),
            ],
        )
        .unwrap();
}

fn capture_source_fixture(id: Uuid, source_path: &str, external_session_id: &str) -> CaptureSource {
    CaptureSource {
        id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Claude,
            machine_id: "machine".to_owned(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(source_path.to_owned()),
            source_format: Some(MATERIAL_FORMAT.to_owned()),
            source_root: Some(ROOT.to_owned()),
            source_identity: None,
            external_session_id: Some(external_session_id.to_owned()),
        },
        started_at: DateTime::parse_from_rfc3339("2026-07-14T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        ended_at: None,
        sync: SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Imported,
            sync_state: SyncState::LocalOnly,
            sync_version: 0,
            deleted_at: None,
            metadata: json!({}),
        },
    }
}

fn session_fixture(id: Uuid, source_id: Uuid, external_session_id: &str) -> Session {
    let now = DateTime::parse_from_rfc3339("2026-07-14T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    Session {
        id,
        history_record_id: None,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Claude,
        external_session_id: Some(external_session_id.to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: None,
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: now,
        ended_at: None,
        timestamps: EntityTimestamps {
            created_at: now,
            updated_at: now,
        },
        sync: SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Imported,
            sync_state: SyncState::LocalOnly,
            sync_version: 0,
            deleted_at: None,
            metadata: json!({}),
        },
    }
}

fn session_edge_fixture(
    id: Uuid,
    from_session_id: Uuid,
    to_session_id: Uuid,
    source_id: Option<Uuid>,
) -> SessionEdge {
    let now = DateTime::parse_from_rfc3339("2026-07-14T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    SessionEdge {
        id,
        from_session_id,
        to_session_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: Confidence::Explicit,
        source_id,
        timestamps: EntityTimestamps {
            created_at: now,
            updated_at: now,
        },
        sync: SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Imported,
            sync_state: SyncState::LocalOnly,
            sync_version: 0,
            deleted_at: None,
            metadata: json!({}),
        },
    }
}

fn begin_parent_child_ownership_publication(
    store: &Store,
) -> (ProviderFilePublicationScope, Uuid, Uuid, Uuid, Uuid, Uuid) {
    let file = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();

    let child_source = Uuid::from_u128(80_000);
    let sibling_source = Uuid::from_u128(80_001);
    let parent_session = Uuid::from_u128(80_002);
    let sibling_peer = Uuid::from_u128(80_003);
    let sibling_edge = Uuid::from_u128(80_004);
    insert_capture_source(store, child_source, PATH_A, "publication-child");
    insert_capture_source(
        store,
        sibling_source,
        "/history/claude/projects/parent.jsonl",
        "publication-parent",
    );
    insert_raw_session(store, parent_session, sibling_source, "publication-parent");
    insert_raw_session(
        store,
        sibling_peer,
        sibling_source,
        "publication-sibling-peer",
    );
    store
        .upsert_session_edge(&session_edge_fixture(
            sibling_edge,
            parent_session,
            sibling_peer,
            Some(sibling_source),
        ))
        .unwrap();

    let outcome = source_outcome(&file, generation, 120);
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Incremental,
            110,
        )
        .unwrap();
    (
        scope,
        child_source,
        sibling_source,
        parent_session,
        sibling_peer,
        sibling_edge,
    )
}

fn event_fixture(id: Uuid, seq: u64, source_id: Uuid, dedupe_key: String, text: &str) -> Event {
    Event {
        id,
        seq,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::User),
        occurred_at: DateTime::parse_from_rfc3339("2026-07-14T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        capture_source_id: Some(source_id),
        payload: json!({"text": text}),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Imported,
            sync_state: SyncState::LocalOnly,
            sync_version: 0,
            deleted_at: None,
            metadata: json!({}),
        },
    }
}

fn insert_raw_session(store: &Store, session_id: Uuid, source_id: Uuid, external_session_id: &str) {
    store
        .conn
        .execute(
            r#"
            INSERT INTO sessions
                (id, capture_source_id, provider, external_session_id, agent_type, is_primary,
                 status, fidelity, started_at_ms, created_at_ms, updated_at_ms)
            VALUES (?1, ?2, 'claude', ?3, 'primary', 1, 'imported', 'imported', 1, 1, 1)
            "#,
            params![
                session_id.to_string(),
                source_id.to_string(),
                external_session_id,
            ],
        )
        .unwrap();
}

fn staged_seen_count(store: &Store) -> i64 {
    let replacement_id = store
        .provider_file_publication
        .borrow()
        .as_ref()
        .unwrap()
        .scope_id
        .to_string();
    store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM provider_file_publication_seen WHERE replacement_id = ?1",
            params![replacement_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn staged_prior_source_count(store: &Store) -> i64 {
    let replacement_id = store
        .provider_file_publication
        .borrow()
        .as_ref()
        .unwrap()
        .scope_id
        .to_string();
    store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM provider_file_publication_prior_sources \
             WHERE replacement_id = ?1",
            params![replacement_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn main_table_exists(store: &Store, table: &str) -> bool {
    store
        .conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            params![table],
            |row| row.get(0),
        )
        .unwrap()
}

fn table_row_count(store: &Store, table: &str) -> i64 {
    store
        .conn
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn reconcile_all(store: &Store, scope: &ProviderFilePublicationScope, max_rows: usize) {
    prepare_all(store, scope, max_rows);
    loop {
        let progress = store
            .reconcile_provider_file_publication_slice(scope, max_rows)
            .unwrap();
        assert!(progress.rows_scanned <= max_rows);
        if progress.complete {
            break;
        }
    }
}

fn prepare_all(store: &Store, scope: &ProviderFilePublicationScope, max_rows: usize) {
    loop {
        let progress = store
            .prepare_provider_file_publication_slice(scope, max_rows)
            .unwrap();
        assert!(progress.source_ids_staged <= max_rows);
        if progress.complete {
            break;
        }
    }
}

fn spawn_provider_file_helper(
    action: &str,
    store_path: &std::path::Path,
    ready_path: Option<&std::path::Path>,
    release_path: Option<&std::path::Path>,
    publication: Option<(u64, Uuid)>,
) -> std::process::Child {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .arg("--ignored")
        .arg("--exact")
        .arg("provider_files::tests::provider_file_subprocess_helper")
        .arg("--test-threads=1")
        .env("CTX_PROVIDER_FILE_HELPER_ACTION", action)
        .env("CTX_PROVIDER_FILE_HELPER_STORE", store_path)
        .stdout(Stdio::null());
    if let Some(path) = ready_path {
        command.env("CTX_PROVIDER_FILE_HELPER_READY", path);
    }
    if let Some(path) = release_path {
        command.env("CTX_PROVIDER_FILE_HELPER_RELEASE", path);
    }
    if let Some((generation, event_id)) = publication {
        command
            .env(
                "CTX_PROVIDER_FILE_HELPER_GENERATION",
                generation.to_string(),
            )
            .env("CTX_PROVIDER_FILE_HELPER_EVENT", event_id.to_string());
    }
    command.spawn().unwrap()
}

fn spawn_provider_file_vcs_writer(
    store_path: &std::path::Path,
    change: &ctx_history_core::VcsChange,
) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .arg("--ignored")
        .arg("--exact")
        .arg("provider_files::tests::provider_file_subprocess_helper")
        .arg("--test-threads=1")
        .env(
            "CTX_PROVIDER_FILE_HELPER_ACTION",
            "upsert-vcs-change-expect-busy",
        )
        .env("CTX_PROVIDER_FILE_HELPER_STORE", store_path)
        .env(
            "CTX_PROVIDER_FILE_HELPER_VCS_CHANGE",
            serde_json::to_string(change).unwrap(),
        )
        .stdout(Stdio::null())
        .spawn()
        .unwrap()
}

fn wait_for_path(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for helper signal"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn helper_owner_lock(store_path: &std::path::Path) -> std::io::Result<File> {
    let identity = crate::store_identity::CanonicalStoreIdentity::open_target(store_path, false)
        .map_err(std::io::Error::other)?;
    let root = identity.private_root();
    create_or_validate_private_lock_dir(&root)?;
    let name = provider_file_owner_lock_name(
        identity.digest(),
        CaptureProvider::Claude,
        MATERIAL_FORMAT,
        ROOT,
        PATH_A,
    );
    let path = root.join(format!("{name}.lock"));
    let lock = open_private_owner_lock_file(&path)?;
    lock.try_lock_exclusive()?;
    validate_open_private_owner_lock_file(&lock, &path)?;
    Ok(lock)
}

#[test]
#[ignore = "subprocess protocol helper"]
fn provider_file_subprocess_helper() {
    let action = std::env::var("CTX_PROVIDER_FILE_HELPER_ACTION").unwrap();
    let store_path =
        std::path::PathBuf::from(std::env::var_os("CTX_PROVIDER_FILE_HELPER_STORE").unwrap());
    match action.as_str() {
        "hold-lock" => {
            let _lock = helper_owner_lock(&store_path).unwrap();
            let ready = std::path::PathBuf::from(
                std::env::var_os("CTX_PROVIDER_FILE_HELPER_READY").unwrap(),
            );
            let release = std::path::PathBuf::from(
                std::env::var_os("CTX_PROVIDER_FILE_HELPER_RELEASE").unwrap(),
            );
            std::fs::write(ready, b"ready").unwrap();
            wait_for_path(&release);
        }
        "try-lock" => match helper_owner_lock(&store_path) {
            Ok(_lock) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::process::exit(23);
            }
            Err(error) => panic!("unexpected lock error: {error}"),
        },
        "create-identity" => {
            let identity =
                crate::store_identity::CanonicalStoreIdentity::open_target(&store_path, true)
                    .unwrap();
            let output = std::path::PathBuf::from(
                std::env::var_os("CTX_PROVIDER_FILE_HELPER_READY").unwrap(),
            );
            std::fs::write(output, identity.digest()).unwrap();
        }
        "upsert-vcs-change-expect-busy" => {
            let store = Store::open(&store_path).unwrap();
            let change = serde_json::from_str::<ctx_history_core::VcsChange>(
                &std::env::var("CTX_PROVIDER_FILE_HELPER_VCS_CHANGE").unwrap(),
            )
            .unwrap();
            assert!(matches!(
                store.upsert_vcs_change(&change).unwrap_err(),
                StoreError::ProviderFileReplacementBusy { .. }
            ));
        }
        "non-utf8-private-root" => {
            let store = Store::open(&store_path).unwrap();
            let generation = std::env::var("CTX_PROVIDER_FILE_HELPER_GENERATION")
                .unwrap()
                .parse::<u64>()
                .unwrap();
            let file = source_file(20, 100);
            let scope = store
                .begin_provider_file_publication(
                    file.provider,
                    source_outcome(&file, generation, 120).observation,
                    MATERIAL_FORMAT,
                    ProviderFilePublicationKind::Replacement,
                    110,
                )
                .unwrap();
            assert!(matches!(
                store.abort_provider_file_publication(scope).unwrap(),
                std::ops::ControlFlow::Continue(None)
            ));
            assert!(!store.has_pending_provider_file_publications().unwrap());
            assert_eq!(store.list_events().unwrap().len(), 1);
        }
        "partial-crash" => {
            let store = Store::open(&store_path).unwrap();
            let generation = std::env::var("CTX_PROVIDER_FILE_HELPER_GENERATION")
                .unwrap()
                .parse::<u64>()
                .unwrap();
            let event_id =
                Uuid::parse_str(&std::env::var("CTX_PROVIDER_FILE_HELPER_EVENT").unwrap()).unwrap();
            let file = source_file(20, 100);
            let outcome = source_outcome(&file, generation, 120);
            let scope = store
                .begin_provider_file_publication(
                    file.provider,
                    outcome.observation,
                    MATERIAL_FORMAT,
                    ProviderFilePublicationKind::Replacement,
                    110,
                )
                .unwrap();
            prepare_all(&store, &scope, 1);
            for _ in 0..100 {
                store
                    .reconcile_provider_file_publication_slice(&scope, 1)
                    .unwrap();
                if !row_exists(&store, "events", event_id) {
                    let ready = std::path::PathBuf::from(
                        std::env::var_os("CTX_PROVIDER_FILE_HELPER_READY").unwrap(),
                    );
                    std::fs::write(ready, b"partially-cleaned").unwrap();
                    std::process::exit(29);
                }
            }
            panic!("helper never reached a destructive event slice");
        }
        "retirement-finalize-crash" => {
            let store = Store::open(&store_path).unwrap();
            let scope = store
                .begin_provider_file_publication_retirement(
                    CaptureProvider::Claude,
                    MATERIAL_FORMAT,
                    ROOT,
                    PATH_A,
                    160,
                )
                .unwrap()
                .unwrap();
            prepare_all(&store, &scope, 1);
            reconcile_all(&store, &scope, 1);
            store.inject_provider_file_fault(ProviderFileFaultPoint::RetirementFinalizeProcessExit);
            let _ = store.retire_provider_file_publication(scope);
            panic!("retirement finalization fault did not terminate the process");
        }
        other => panic!("unknown helper action {other}"),
    }
}
