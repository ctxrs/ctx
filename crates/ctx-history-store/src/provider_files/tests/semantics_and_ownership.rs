#[test]
fn equal_count_replacement_advances_semantic_replacement_revision() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let source = Uuid::from_u128(95);
    let old_event = Uuid::from_u128(96);
    let new_event = Uuid::from_u128(97);
    insert_capture_source(&store, source, PATH_A, "equal-count");
    let mut old = event_fixture(
        old_event,
        96,
        source,
        "unused-old".to_owned(),
        "old content",
    );
    old.dedupe_key = None;
    store.upsert_event(&old).unwrap();
    store
        .refresh_event_embedding_document_count_cache()
        .unwrap();
    assert_eq!(table_row_count(&store, "events"), 1);
    assert_eq!(
        store.cached_event_embedding_document_count().unwrap(),
        Some(1)
    );
    assert_eq!(store.semantic_replacement_revision().unwrap(), 0);

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
    prepare_all(&store, &scope, 8);
    assert_eq!(store.cached_event_embedding_document_count().unwrap(), None);
    store
        .refresh_event_embedding_document_count_cache()
        .unwrap();
    assert_eq!(store.cached_event_embedding_document_count().unwrap(), None);
    let mut new = event_fixture(
        new_event,
        97,
        source,
        "unused-new".to_owned(),
        "new content",
    );
    new.dedupe_key = None;
    store
        .with_provider_file_publication_writes(&scope, |store| store.upsert_event(&new))
        .unwrap();
    reconcile_all(&store, &scope, 1);
    store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(Some(&checkpoint(
                20,
                4,
                "unix:2049:equal",
                120,
            ))),
        )
        .unwrap();

    assert_eq!(table_row_count(&store, "events"), 1);
    assert!(!row_exists(&store, "events", old_event));
    assert!(row_exists(&store, "events", new_event));
    assert_eq!(store.cached_event_embedding_document_count().unwrap(), None);
    assert_eq!(store.semantic_replacement_revision().unwrap(), 1);
}

#[test]
fn semantic_cache_refresh_and_publication_marker_are_one_serialized_snapshot() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    let file = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    drop(store);

    let (counted_tx, counted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let refresh_path = path.clone();
    let refresh = thread::spawn(move || {
        let store = Store::open(refresh_path).unwrap();
        crate::search::projections::refresh_semantic_searchable_item_stats_with_hook(
            &store.conn,
            || {
                counted_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            },
        )
        .unwrap();
    });
    counted_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let (attempted_tx, attempted_rx) = mpsc::channel();
    let (published_tx, published_rx) = mpsc::channel();
    let publication_path = path.clone();
    let publish = thread::spawn(move || {
        attempted_tx.send(()).unwrap();
        let store = Store::open(publication_path).unwrap();
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
        published_tx.send(()).unwrap();
        std::mem::drop(scope);
    });
    attempted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(published_rx
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    release_tx.send(()).unwrap();
    refresh.join().unwrap();
    published_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    publish.join().unwrap();

    let observer = Store::open(path).unwrap();
    assert!(observer.has_pending_provider_file_publications().unwrap());
    assert_eq!(
        observer.cached_event_embedding_document_count().unwrap(),
        None
    );
}

#[test]
fn replacement_owned_event_conflict_overwrites_and_cross_source_conflict_rejects() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let owner_source = Uuid::from_u128(300);
    let sibling_source = Uuid::from_u128(301);
    insert_capture_source(&store, owner_source, PATH_A, "owner-conflict");
    insert_capture_source(
        &store,
        sibling_source,
        "/history/claude/projects/b.jsonl",
        "sibling-conflict",
    );
    let owner_id = Uuid::from_u128(302);
    let sibling_id = Uuid::from_u128(303);
    let owner_old_key =
        Store::provider_event_dedupe_key(CaptureProvider::Claude, "owner-conflict", 1, "old-hash");
    let sibling_old_key = Store::provider_event_dedupe_key(
        CaptureProvider::Claude,
        "sibling-conflict",
        2,
        "sibling-old-hash",
    );
    store
        .upsert_event(&event_fixture(
            owner_id,
            1,
            owner_source,
            owner_old_key,
            "old payload",
        ))
        .unwrap();
    store
        .upsert_event(&event_fixture(
            sibling_id,
            2,
            sibling_source,
            sibling_old_key,
            "sibling payload",
        ))
        .unwrap();

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
    prepare_all(&store, &scope, 8);
    let owner_new_key =
        Store::provider_event_dedupe_key(CaptureProvider::Claude, "owner-conflict", 1, "new-hash");
    let replacement_id = store
        .with_provider_file_publication_writes(&scope, |store| {
            store.upsert_event(&event_fixture(
                Uuid::from_u128(304),
                99,
                owner_source,
                owner_new_key.clone(),
                "new payload",
            ))
        })
        .unwrap();
    assert_eq!(replacement_id, owner_id);
    let replaced: (i64, String, String) = store
        .conn
        .query_row(
            "SELECT seq, dedupe_key, payload_json FROM events WHERE id = ?1",
            params![owner_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(replaced.0, 99);
    assert_eq!(replaced.1, owner_new_key);
    assert!(replaced.2.contains("new payload"));

    let owner_conflict_from_sibling = Store::provider_event_dedupe_key(
        CaptureProvider::Claude,
        "owner-conflict",
        1,
        "incoming-sibling-hash",
    );
    let error = store
        .with_provider_file_publication_writes(&scope, |store| {
            store.upsert_event(&event_fixture(
                Uuid::from_u128(306),
                100,
                sibling_source,
                owner_conflict_from_sibling,
                "incoming owner mismatch",
            ))
        })
        .unwrap_err();
    assert!(matches!(error, StoreError::ProviderEventConflict { .. }));

    let sibling_new_key = Store::provider_event_dedupe_key(
        CaptureProvider::Claude,
        "sibling-conflict",
        2,
        "sibling-new-hash",
    );
    let error = store
        .with_provider_file_publication_writes(&scope, |store| {
            store.upsert_event(&event_fixture(
                Uuid::from_u128(305),
                100,
                owner_source,
                sibling_new_key,
                "must reject",
            ))
        })
        .unwrap_err();
    assert!(matches!(error, StoreError::ProviderEventConflict { .. }));

    reconcile_all(&store, &scope, 2);
    store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(Some(&checkpoint(
                20,
                4,
                "unix:2049:conflict",
                120,
            ))),
        )
        .unwrap();
    assert!(row_exists(&store, "events", owner_id));
    assert!(row_exists(&store, "events", sibling_id));
}

#[test]
fn scoped_natural_key_and_unowned_record_writes_cannot_contaminate_publication() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let owner_source = Uuid::from_u128(45_000);
    let sibling_source = Uuid::from_u128(45_001);
    insert_capture_source(&store, owner_source, PATH_A, "write-owner");
    insert_capture_source(
        &store,
        sibling_source,
        "/history/claude/projects/sibling.jsonl",
        "write-sibling",
    );
    insert_raw_event(
        &store,
        Uuid::from_u128(45_002),
        1,
        owner_source,
        "owner material",
    );
    let sibling_artifact = Uuid::from_u128(45_003);
    store
        .conn
        .execute(
            "INSERT INTO artifacts (id, kind, blob_hash, blob_path, byte_size, created_at_ms, updated_at_ms, source_id) VALUES (?1, 'markdown', ?2, 'sibling', 1, 1, 1, ?3)",
            params![sibling_artifact.to_string(), "2".repeat(64), sibling_source.to_string()],
        )
        .unwrap();
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
    prepare_all(&store, &scope, 8);
    let time = DateTime::parse_from_rfc3339("2026-07-14T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let incoming = ctx_history_core::Artifact {
        id: Uuid::from_u128(45_004),
        kind: ctx_history_core::ArtifactKind::Markdown,
        blob_hash: "2".repeat(64),
        blob_path: "owner".to_owned(),
        byte_size: 2,
        media_type: None,
        preview_text: None,
        timestamps: ctx_history_core::EntityTimestamps {
            created_at: time,
            updated_at: time,
        },
        source_id: Some(owner_source),
        sync: SyncMetadata {
            visibility: Visibility::LocalOnly,
            fidelity: Fidelity::Imported,
            sync_state: SyncState::LocalOnly,
            sync_version: 0,
            deleted_at: None,
            metadata: json!({}),
        },
    };
    assert!(matches!(
        store
            .with_provider_file_publication_writes(&scope, |store| {
                store.upsert_artifact(&incoming)
            })
            .unwrap_err(),
        StoreError::ProviderFilePublicationOwnerMismatch { .. }
    ));
    let artifact_state: (String, String) = store
        .conn
        .query_row(
            "SELECT blob_path, source_id FROM artifacts WHERE id = ?1",
            params![sibling_artifact.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        artifact_state,
        ("sibling".to_owned(), sibling_source.to_string())
    );

    let record = ctx_history_core::HistoryRecord::new(
        "unowned write",
        "must fail closed",
        Vec::new(),
        "note",
        None,
    );
    assert!(matches!(
        store
            .with_provider_file_publication_writes(&scope, |store| store.upsert_record(&record))
            .unwrap_err(),
        StoreError::ProviderFilePublicationOwnerMismatch { .. }
    ));
    let busy = store.upsert_record(&record).unwrap_err();
    assert!(matches!(
        busy,
        StoreError::ProviderFileReplacementBusy { .. }
    ));
    let rendered = busy.to_string();
    assert!(!rendered.contains(ROOT));
    assert!(!rendered.contains(PATH_A));
    assert!(!row_exists(&store, "history_records", record.id));
    store.abandon_provider_file_publication(scope).unwrap();
}

#[test]
fn child_owned_session_and_explicit_edge_allow_sibling_parent_references() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let (scope, child_source, _, parent_session, _, _) =
        begin_parent_child_ownership_publication(&store);
    let child_session = Uuid::from_u128(80_010);
    let mut child = session_fixture(child_session, child_source, "publication-child");
    child.parent_session_id = Some(parent_session);
    child.root_session_id = Some(parent_session);
    child.agent_type = AgentType::Subagent;
    child.is_primary = false;
    store
        .with_provider_file_publication_writes(&scope, |store| store.upsert_session(&child))
        .unwrap();

    let edge_id = Uuid::from_u128(80_011);
    let edge = session_edge_fixture(edge_id, child_session, parent_session, Some(child_source));
    store
        .with_provider_file_publication_writes(&scope, |store| store.upsert_session_edge(&edge))
        .unwrap();
    let inherited_edge_id = Uuid::from_u128(80_012);
    let inherited_edge =
        session_edge_fixture(inherited_edge_id, child_session, parent_session, None);
    store
        .with_provider_file_publication_writes(&scope, |store| {
            store.upsert_session_edge(&inherited_edge)
        })
        .unwrap();

    let stored_session: (String, String, String) = store
        .conn
        .query_row(
            "SELECT capture_source_id, parent_session_id, root_session_id FROM sessions WHERE id = ?1",
            params![child_session.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        stored_session,
        (
            child_source.to_string(),
            parent_session.to_string(),
            parent_session.to_string(),
        )
    );
    let stored_edge_source: String = store
        .conn
        .query_row(
            "SELECT source_id FROM session_edges WHERE id = ?1",
            params![edge_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_edge_source, child_source.to_string());
    let inherited_edge_source: Option<String> = store
        .conn
        .query_row(
            "SELECT source_id FROM session_edges WHERE id = ?1",
            params![inherited_edge_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(inherited_edge_source, None);
    store.abandon_provider_file_publication(scope).unwrap();
}

#[test]
fn session_and_edge_owners_reject_sibling_overwrites_and_missing_sources() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let (scope, child_source, sibling_source, parent_session, sibling_peer, sibling_edge) =
        begin_parent_child_ownership_publication(&store);

    let mut hijacked_parent = store.get_session(parent_session).unwrap();
    hijacked_parent.capture_source_id = Some(child_source);
    hijacked_parent.role_hint = Some("must not overwrite sibling owner".to_owned());
    assert!(matches!(
        store
            .with_provider_file_publication_writes(&scope, |store| {
                store.upsert_session(&hijacked_parent)
            })
            .unwrap_err(),
        StoreError::ProviderFilePublicationOwnerMismatch { .. }
    ));
    let stored_parent_source: String = store
        .conn
        .query_row(
            "SELECT capture_source_id FROM sessions WHERE id = ?1",
            params![parent_session.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_parent_source, sibling_source.to_string());

    let hijacked_edge = session_edge_fixture(
        sibling_edge,
        parent_session,
        sibling_peer,
        Some(child_source),
    );
    assert!(matches!(
        store
            .with_provider_file_publication_writes(&scope, |store| {
                store.upsert_session_edge(&hijacked_edge)
            })
            .unwrap_err(),
        StoreError::ProviderFilePublicationOwnerMismatch { .. }
    ));
    let stored_edge_source: String = store
        .conn
        .query_row(
            "SELECT source_id FROM session_edges WHERE id = ?1",
            params![sibling_edge.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_edge_source, sibling_source.to_string());

    let missing_session_id = Uuid::from_u128(80_020);
    let mut missing_session =
        session_fixture(missing_session_id, child_source, "missing-session-owner");
    missing_session.capture_source_id = None;
    missing_session.parent_session_id = Some(parent_session);
    missing_session.root_session_id = Some(parent_session);
    assert!(matches!(
        store
            .with_provider_file_publication_writes(&scope, |store| {
                store.upsert_session(&missing_session)
            })
            .unwrap_err(),
        StoreError::ProviderFilePublicationOwnerMismatch { .. }
    ));
    assert!(!row_exists(&store, "sessions", missing_session_id));

    let missing_edge_id = Uuid::from_u128(80_021);
    let missing_edge = session_edge_fixture(
        missing_edge_id,
        Uuid::from_u128(80_022),
        Uuid::from_u128(80_023),
        None,
    );
    assert!(matches!(
        store
            .with_provider_file_publication_writes(&scope, |store| {
                store.upsert_session_edge(&missing_edge)
            })
            .unwrap_err(),
        StoreError::ProviderFilePublicationOwnerMismatch { .. }
    ));
    assert!(!row_exists(&store, "session_edges", missing_edge_id));
    store.abandon_provider_file_publication(scope).unwrap();
}

#[test]
fn legacy_root_effective_session_owner_is_hidden_and_seen_empty_session_survives() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    let file = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let source = Uuid::from_u128(320);
    let session = Uuid::from_u128(321);
    let event = Uuid::from_u128(322);
    insert_capture_source(&store, source, PATH_A, "legacy-root");
    store
        .conn
        .execute(
            "UPDATE capture_sources SET source_root = raw_source_path WHERE id = ?1",
            params![source.to_string()],
        )
        .unwrap();
    insert_raw_session(&store, session, source, "legacy-root");
    store
        .conn
        .execute(
            r#"
            INSERT INTO events
                (id, seq, session_id, event_type, role, occurred_at_ms, payload_json)
            VALUES (?1, 1, ?2, 'message', 'user', 1, '{"text":"session owned"}')
            "#,
            params![event.to_string(), session.to_string()],
        )
        .unwrap();
    store.rebuild_search_projection().unwrap();

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
    assert!(scope.tracks_prior_material);
    let observer = Store::open(&path).unwrap();
    assert!(observer.list_sessions().unwrap().is_empty());
    assert!(observer.list_events().unwrap().is_empty());
    assert!(observer
        .search_event_hits("session owned", 10)
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .provider_file_publication_session(&scope, session)
            .unwrap()
            .id,
        session
    );
    assert_eq!(
        store
            .provider_file_publication_event(&scope, event)
            .unwrap()
            .id,
        event
    );
    assert_eq!(
        store
            .provider_file_publication_events_for_session(&scope, session)
            .unwrap()
            .len(),
        1
    );
    store
        .track_provider_file_publication_session(session)
        .unwrap();
    reconcile_all(&store, &scope, 1);
    store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(Some(&checkpoint(
                20,
                4,
                "unix:2049:legacy-root",
                120,
            ))),
        )
        .unwrap();

    assert!(!row_exists(&store, "events", event));
    assert_eq!(session_deleted_at(&store, session), None);
    assert!(store.get_session(session).is_ok());
}

#[test]
fn replacement_identity_reads_are_idempotent_and_scoped_to_the_active_file() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    let file_a = source_file(20, 100);
    let mut file_b = source_file(20, 100);
    file_b.source_path = PATH_B.to_owned();
    let generation = store
        .allocate_source_import_inventory_generation(file_a.provider, &file_a.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, &[file_a.clone(), file_b.clone()])
        .unwrap();

    let source_a = Uuid::from_u128(329_100);
    let source_b = Uuid::from_u128(329_101);
    let session_a = Uuid::from_u128(329_102);
    let session_b = Uuid::from_u128(329_103);
    let event_a = Uuid::from_u128(329_104);
    let event_b = Uuid::from_u128(329_105);
    insert_capture_source(&store, source_a, PATH_A, "shared-external-session");
    insert_capture_source(&store, source_b, PATH_B, "shared-external-session");
    let session_a_row = session_fixture(session_a, source_a, "shared-external-session");
    let session_b_row = session_fixture(session_b, source_b, "shared-external-session");
    store.upsert_session(&session_a_row).unwrap();
    store.upsert_session(&session_b_row).unwrap();
    let mut event_a_row = event_fixture(
        event_a,
        1,
        source_a,
        "owner-event-dedupe".to_owned(),
        "owner event",
    );
    event_a_row.session_id = Some(session_a);
    let mut event_b_row = event_fixture(
        event_b,
        2,
        source_b,
        "sibling-event-dedupe".to_owned(),
        "sibling event",
    );
    event_b_row.session_id = Some(session_b);
    store.upsert_event(&event_a_row).unwrap();
    store.upsert_event(&event_b_row).unwrap();

    let sibling_store = Store::open(&path).unwrap();
    let sibling_scope = sibling_store
        .begin_provider_file_publication(
            file_b.provider,
            source_outcome(&file_b, generation, 110).observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            105,
        )
        .unwrap();
    let outcome_a = source_outcome(&file_a, generation, 110);
    let scope = store
        .begin_provider_file_publication(
            file_a.provider,
            outcome_a.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            106,
        )
        .unwrap();
    prepare_all(&store, &scope, 1);

    assert!(store.get_session(session_a).is_err());
    assert!(store.get_session(session_b).is_err());
    assert!(store.get_event(event_a).is_err());
    assert!(store.get_event(event_b).is_err());
    let active_owner =
        material_source_matches_replacement_owner_predicate("active_source", "active_publication");
    let sibling_matches_active_owner: bool = store
        .conn
        .query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM capture_sources AS active_source \
                 JOIN provider_file_publications AS active_publication ON ({active_owner}) \
                 WHERE active_publication.replacement_id = ?1 AND active_source.id = ?2)"
            ),
            params![scope.scope_id.to_string(), source_b.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!sibling_matches_active_owner);
    store
        .with_provider_file_publication_writes(&scope, |store| {
            assert_eq!(store.get_capture_source(source_a)?.id, source_a);
            assert_eq!(store.get_session(session_a)?.id, session_a);
            assert!(store.get_session(session_b).is_err());
            assert_eq!(store.get_event(event_a)?.id, event_a);
            assert!(store.get_event(event_b).is_err());
            assert_eq!(
                store
                    .session_by_capture_source_and_external_session(
                        source_a,
                        CaptureProvider::Claude,
                        "shared-external-session",
                    )?
                    .unwrap()
                    .id,
                session_a
            );
            assert!(store
                .session_by_capture_source_and_external_session(
                    source_b,
                    CaptureProvider::Claude,
                    "shared-external-session",
                )?
                .is_none());
            assert_eq!(store.event_id_by_dedupe_key("owner-event-dedupe")?, event_a);
            assert!(store
                .event_id_by_dedupe_key("sibling-event-dedupe")
                .is_err());
            assert_eq!(store.events_for_session_limited(session_a, 10)?.len(), 1);
            assert!(store.events_for_session_limited(session_b, 10)?.is_empty());

            store.upsert_session(&session_a_row)?;
            assert_eq!(store.upsert_event(&event_a_row)?, event_a);
            Ok(())
        })
        .unwrap();

    let owner_sessions: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE capture_source_id = ?1",
            params![source_a.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let owner_events: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE capture_source_id = ?1",
            params![source_a.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((owner_sessions, owner_events), (1, 1));
    assert_eq!(staged_seen_count(&store), 2);

    reconcile_all(&store, &scope, 1);
    store
        .finalize_provider_file_publication(
            scope,
            outcome_a,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap();
    assert!(store.get_session(session_a).is_ok());
    assert!(store.get_session(session_b).is_err());
    assert!(matches!(
        sibling_store
            .abort_provider_file_publication(sibling_scope)
            .unwrap(),
        std::ops::ControlFlow::Continue(None)
    ));
    assert!(store.get_session(session_b).is_ok());
}

#[test]
fn replacement_removes_prior_file_material_but_preserves_shared_session() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let original = source_file(100, 100);
    let first_generation = store
        .allocate_source_import_inventory_generation(original.provider, &original.source_root)
        .unwrap();
    store
        .upsert_source_import_files(first_generation, std::slice::from_ref(&original))
        .unwrap();
    let old_checkpoint = checkpoint(100, 10, "unix:2049:old", 110);
    store
        .upsert_provider_file_checkpoint(
            source_outcome(&original, first_generation, 110),
            &old_checkpoint,
        )
        .unwrap();

    let source_a = Uuid::from_u128(1);
    let source_b = Uuid::from_u128(2);
    let shared_session = Uuid::from_u128(3);
    let removed_session = Uuid::from_u128(4);
    let old_run = Uuid::from_u128(5);
    let new_run = Uuid::from_u128(6);
    let other_run = Uuid::from_u128(7);
    let old_event = Uuid::from_u128(8);
    let new_event = Uuid::from_u128(9);
    let other_event = Uuid::from_u128(10);
    let old_file = Uuid::from_u128(11);
    let new_file = Uuid::from_u128(12);
    let old_edge = Uuid::from_u128(13);
    let new_edge = Uuid::from_u128(14);
    insert_reconciliation_fixture(
        &store,
        source_a,
        source_b,
        shared_session,
        removed_session,
        old_run,
        new_run,
        other_run,
        old_event,
        new_event,
        other_event,
        old_file,
        new_file,
        old_edge,
        new_edge,
    );

    let rewritten = source_file(50, 200);
    let second_generation = store
        .allocate_source_import_inventory_generation(rewritten.provider, &rewritten.source_root)
        .unwrap();
    store
        .upsert_source_import_files(second_generation, std::slice::from_ref(&rewritten))
        .unwrap();
    let replacement_checkpoint = checkpoint(50, 6, "unix:2049:new", 210);
    let outcome = source_outcome(&rewritten, second_generation, 210);
    let scope = store
        .begin_provider_file_publication(
            rewritten.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Replacement,
            200,
        )
        .unwrap();
    store
        .track_provider_file_publication_event(new_event)
        .unwrap();
    store.track_provider_file_publication_run(new_run).unwrap();
    store
        .track_provider_file_publication_file_touched(new_file)
        .unwrap();
    store
        .track_provider_file_publication_session_edge(new_edge)
        .unwrap();
    store
        .track_provider_file_publication_session(shared_session)
        .unwrap();
    reconcile_all(&store, &scope, 2);

    let counts = store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(Some(&replacement_checkpoint)),
        )
        .unwrap();

    assert_eq!(
        counts.reconciliation,
        ProviderFileReconciliationCounts {
            artifacts: 0,
            summaries: 0,
            history_record_links: 0,
            history_records: 0,
            history_record_tags: 0,
            record_edges: 0,
            audit_log_entries: 0,
            vcs_workspaces: 0,
            vcs_changes: 0,
            events: 1,
            runs: 1,
            files_touched: 1,
            session_edges: 1,
            sessions_tombstoned: 1,
        }
    );
    for (table, removed, survivors) in [
        ("events", old_event, vec![new_event, other_event]),
        ("runs", old_run, vec![new_run, other_run]),
        ("files_touched", old_file, vec![new_file]),
        ("session_edges", old_edge, vec![new_edge]),
    ] {
        assert!(!row_exists(&store, table, removed));
        for survivor in survivors {
            assert!(row_exists(&store, table, survivor));
        }
    }
    assert_eq!(session_deleted_at(&store, shared_session), None);
    assert_eq!(session_deleted_at(&store, removed_session), Some(200));
    assert!(!projection_row_exists(&store, old_event));
    assert!(projection_row_exists(&store, new_event));
    assert_eq!(
        store
            .provider_file_checkpoint(replacement_checkpoint.key())
            .unwrap()
            .unwrap(),
        replacement_checkpoint
    );
    assert_eq!(store.semantic_replacement_revision().unwrap(), 1);
}

#[test]
fn replacement_preserves_sessions_with_each_kind_of_sibling_owned_material() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();

    let file_session = Uuid::from_u128(200);
    let edge_session = Uuid::from_u128(201);
    let summary_session = Uuid::from_u128(202);
    let orphan_session = Uuid::from_u128(203);
    let edge_peer = Uuid::from_u128(204);
    let link_session = Uuid::from_u128(205);
    let parent_session = Uuid::from_u128(206);
    let owned_summary_session = Uuid::from_u128(207);
    let owner_sources = [
        (Uuid::from_u128(210), file_session, "file-only"),
        (Uuid::from_u128(211), edge_session, "edge-only"),
        (Uuid::from_u128(212), summary_session, "summary-only"),
        (Uuid::from_u128(213), orphan_session, "orphan"),
        (Uuid::from_u128(214), link_session, "link-only"),
        (Uuid::from_u128(215), parent_session, "parent-only"),
        (
            Uuid::from_u128(216),
            owned_summary_session,
            "owned-summary-only",
        ),
    ];
    for (source, session, external) in owner_sources {
        insert_capture_source(&store, source, PATH_A, external);
        insert_raw_session(&store, session, source, external);
    }

    let sibling_file_source = Uuid::from_u128(220);
    let sibling_edge_source = Uuid::from_u128(221);
    let sibling_summary_source = Uuid::from_u128(222);
    let sibling_link_source = Uuid::from_u128(223);
    let sibling_child_source = Uuid::from_u128(224);
    insert_capture_source(
        &store,
        sibling_file_source,
        "/history/claude/projects/b.jsonl",
        "file-only",
    );
    insert_capture_source(
        &store,
        sibling_edge_source,
        "/history/claude/projects/b.jsonl",
        "edge-only",
    );
    insert_capture_source(
        &store,
        sibling_summary_source,
        "/history/claude/projects/b.jsonl",
        "summary-only",
    );
    insert_capture_source(
        &store,
        sibling_link_source,
        "/history/claude/projects/b.jsonl",
        "link-only",
    );
    insert_capture_source(
        &store,
        sibling_child_source,
        "/history/claude/projects/b.jsonl",
        "child",
    );
    insert_raw_session(&store, edge_peer, sibling_edge_source, "edge-peer");
    let child_session = Uuid::from_u128(225);
    insert_raw_session(&store, child_session, sibling_child_source, "child");
    store
        .conn
        .execute(
            "UPDATE sessions SET parent_session_id = ?1, root_session_id = ?1 WHERE id = ?2",
            params![parent_session.to_string(), child_session.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO files_touched
                (id, path, created_at_ms, updated_at_ms, source_id)
            VALUES (?1, 'sibling.rs', 1, 1, ?2)
            "#,
            params![
                Uuid::from_u128(230).to_string(),
                sibling_file_source.to_string()
            ],
        )
        .unwrap();
    let history_record = Uuid::from_u128(233);
    store
        .conn
        .execute(
            "INSERT INTO history_records (id, title) VALUES (?1, 'sibling record')",
            params![history_record.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO history_record_links
                (id, history_record_id, target_type, target_id, link_type, source_id,
                 created_at_ms, updated_at_ms)
            VALUES (?1, ?2, 'session', ?3, 'references', ?4, 1, 1)
            "#,
            params![
                Uuid::from_u128(234).to_string(),
                history_record.to_string(),
                link_session.to_string(),
                sibling_link_source.to_string(),
            ],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO summaries
                (id, session_id, kind, text, created_at_ms, updated_at_ms, source_id)
            VALUES (?1, ?2, 'imported_provider_summary', 'stale owner summary', 1, 1, ?3)
            "#,
            params![
                Uuid::from_u128(235).to_string(),
                owned_summary_session.to_string(),
                Uuid::from_u128(216).to_string(),
            ],
        )
        .unwrap();
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
                Uuid::from_u128(231).to_string(),
                edge_session.to_string(),
                edge_peer.to_string(),
                sibling_edge_source.to_string(),
            ],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO summaries
                (id, session_id, kind, text, created_at_ms, updated_at_ms, source_id)
            VALUES (?1, ?2, 'imported_provider_summary', 'sibling summary', 1, 1, ?3)
            "#,
            params![
                Uuid::from_u128(232).to_string(),
                summary_session.to_string(),
                sibling_summary_source.to_string(),
            ],
        )
        .unwrap();

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
    reconcile_all(&store, &scope, 3);
    let counts = store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(Some(&checkpoint(
                20,
                4,
                "unix:2049:sibling",
                120,
            ))),
        )
        .unwrap();

    assert_eq!(counts.reconciliation.sessions_tombstoned, 2);
    assert_eq!(session_deleted_at(&store, file_session), None);
    assert_eq!(session_deleted_at(&store, edge_session), None);
    assert_eq!(session_deleted_at(&store, summary_session), None);
    assert_eq!(session_deleted_at(&store, link_session), None);
    assert_eq!(session_deleted_at(&store, parent_session), None);
    assert_eq!(session_deleted_at(&store, orphan_session), Some(100));
    assert_eq!(session_deleted_at(&store, owned_summary_session), Some(100));
}
