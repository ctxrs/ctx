#[test]
fn archive_export_uses_one_visibility_snapshot_across_publication_commit() {
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
    let source = Uuid::from_u128(76_000);
    let session = Uuid::from_u128(76_001);
    let event = Uuid::from_u128(76_002);
    let file_id = Uuid::from_u128(76_003);
    insert_capture_source(&store, source, PATH_A, "snapshot-owner");
    insert_raw_session(&store, session, source, "snapshot-owner");
    insert_raw_event(&store, event, 76_002, source, "snapshot payload");
    store
        .conn
        .execute(
            "UPDATE events SET session_id = ?1 WHERE id = ?2",
            params![session.to_string(), event.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO files_touched (id, event_id, path, created_at_ms, updated_at_ms, source_id) VALUES (?1, ?2, 'snapshot.rs', 1, 1, ?3)",
            params![file_id.to_string(), event.to_string(), source.to_string()],
        )
        .unwrap();

    let (snapshot_tx, snapshot_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let export_path = path.clone();
    let exporter = thread::spawn(move || {
        let observer = Store::open(export_path).unwrap();
        observer
            .export_archive_with_hook(|| {
                snapshot_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
            .unwrap()
    });
    snapshot_rx.recv_timeout(Duration::from_secs(5)).unwrap();

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
    reconcile_all(&store, &scope, 2);
    store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap();
    release_tx.send(()).unwrap();
    let archive = exporter.join().unwrap();
    assert_eq!(
        archive
            .events
            .iter()
            .map(|event| event.id)
            .collect::<Vec<_>>(),
        vec![event]
    );
    assert_eq!(
        archive
            .files_touched
            .iter()
            .map(|file| file.id)
            .collect::<Vec<_>>(),
        vec![file_id]
    );
    assert!(store.list_events().unwrap().is_empty());
    assert!(store.list_files_touched().unwrap().is_empty());
}

#[test]
fn ancillary_owner_material_is_hidden_then_removed_without_archive_or_cache_leaks() {
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
    let source = Uuid::from_u128(41_000);
    let event = Uuid::from_u128(41_001);
    let artifact = Uuid::from_u128(41_002);
    let workspace = Uuid::from_u128(41_003);
    let change = Uuid::from_u128(41_004);
    let link = Uuid::from_u128(41_005);
    let summary = Uuid::from_u128(41_006);
    let tag = Uuid::from_u128(41_007);
    let edge = Uuid::from_u128(41_008);
    let audit = Uuid::from_u128(41_009);
    insert_capture_source(&store, source, PATH_A, "ancillary-owner");
    insert_raw_event(&store, event, 1, source, "hidden semantic window");

    let mut first_record = ctx_history_core::HistoryRecord::new(
        "hidden owner record",
        "hidden lexical window",
        Vec::new(),
        "note",
        None,
    );
    first_record.id = Uuid::from_u128(41_010);
    let mut second_record = ctx_history_core::HistoryRecord::new(
        "second owner record",
        "edge target",
        Vec::new(),
        "note",
        None,
    );
    second_record.id = Uuid::from_u128(41_011);
    store.insert_record(&first_record).unwrap();
    store.insert_record(&second_record).unwrap();
    store
        .conn
        .execute(
            "UPDATE history_records SET source_id = ?1 WHERE id IN (?2, ?3)",
            params![
                source.to_string(),
                first_record.id.to_string(),
                second_record.id.to_string()
            ],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO artifacts (id, kind, blob_hash, blob_path, byte_size, preview_text, created_at_ms, updated_at_ms, source_id) VALUES (?1, 'markdown', ?2, 'objects/hidden', 1, 'hidden artifact', 1, 1, ?3)",
            params![artifact.to_string(), "1".repeat(64), source.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO vcs_workspaces (id, kind, root_path, repo_fingerprint, created_at_ms, updated_at_ms, source_id) VALUES (?1, 'git', '/hidden', 'hidden-repo', 1, 1, ?2)",
            params![workspace.to_string(), source.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO vcs_changes (id, vcs_workspace_id, kind, change_id, created_at_ms, updated_at_ms) VALUES (?1, ?2, 'git_commit', 'hidden-change', 1, 1)",
            params![change.to_string(), workspace.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO history_record_links (id, history_record_id, target_type, target_id, link_type, source_id, created_at_ms, updated_at_ms) VALUES (?1, ?2, 'artifact', ?3, 'references', ?4, 1, 1)",
            params![
                link.to_string(),
                first_record.id.to_string(),
                artifact.to_string(),
                source.to_string()
            ],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO summaries (id, history_record_id, kind, text, source_id, created_at_ms, updated_at_ms) VALUES (?1, ?2, 'imported_provider_summary', 'hidden summary', ?3, 1, 1)",
            params![summary.to_string(), first_record.id.to_string(), source.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO tags (id, name, created_at_ms, updated_at_ms) VALUES (?1, 'hidden-tag', 1, 1)",
            params![tag.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO history_record_tags (history_record_id, tag_id, source_id, created_at_ms) VALUES (?1, ?2, ?3, 1)",
            params![first_record.id.to_string(), tag.to_string(), source.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO record_edges (id, from_record_id, to_record_id, edge_type, source_id, created_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, 'related', ?4, 1, 1)",
            params![
                edge.to_string(),
                first_record.id.to_string(),
                second_record.id.to_string(),
                source.to_string()
            ],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO audit_log (id, actor_kind, action, occurred_at_ms, source_id) VALUES (?1, 'system', 'hidden-audit', 1, ?2)",
            params![audit.to_string(), source.to_string()],
        )
        .unwrap();
    store.rebuild_search_projection().unwrap();
    store
        .refresh_event_embedding_document_count_cache()
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
    let observer = Store::open(&path).unwrap();
    assert!(observer.list_records(10).unwrap().is_empty());
    assert!(observer
        .search_records("hidden lexical window", 10)
        .unwrap()
        .is_empty());
    assert!(observer.search_event_hits("hidden", 10).unwrap().is_empty());
    assert!(observer.list_artifacts().unwrap().is_empty());
    assert!(observer.list_summaries().unwrap().is_empty());
    assert!(observer.list_history_record_links().unwrap().is_empty());
    assert!(observer.list_vcs_workspaces().unwrap().is_empty());
    assert!(observer.list_vcs_changes().unwrap().is_empty());
    assert_eq!(
        observer.cached_event_embedding_document_count().unwrap(),
        None
    );
    assert_eq!(observer.count_event_embedding_documents_exact().unwrap(), 0);
    assert!(observer
        .event_embedding_documents_by_ids(&[event])
        .unwrap()
        .is_empty());
    let archive = observer.export_archive().unwrap();
    assert!(archive.records.is_empty());
    assert!(archive.events.is_empty());
    assert!(archive.artifact_records.is_empty());
    assert!(archive.vcs_workspaces.is_empty());
    assert!(archive.vcs_changes.is_empty());
    assert!(archive.history_record_links.is_empty());
    assert!(archive.summaries.is_empty());

    reconcile_all(&store, &scope, 2);
    let finalized = store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap();
    assert_eq!(finalized.reconciliation.events, 1);
    assert_eq!(finalized.reconciliation.artifacts, 1);
    assert_eq!(finalized.reconciliation.summaries, 1);
    assert_eq!(finalized.reconciliation.history_record_links, 1);
    assert_eq!(finalized.reconciliation.history_records, 2);
    assert_eq!(finalized.reconciliation.history_record_tags, 1);
    assert_eq!(finalized.reconciliation.record_edges, 1);
    assert_eq!(finalized.reconciliation.audit_log_entries, 1);
    assert_eq!(finalized.reconciliation.vcs_changes, 1);
    assert_eq!(finalized.reconciliation.vcs_workspaces, 1);
    for table in [
        "events",
        "artifacts",
        "summaries",
        "history_record_links",
        "history_records",
        "history_record_tags",
        "record_edges",
        "audit_log",
        "vcs_changes",
        "vcs_workspaces",
    ] {
        assert_eq!(table_row_count(&store, table), 0, "stale rows in {table}");
    }
    assert!(store.search_records("hidden", 10).unwrap().is_empty());
}

#[test]
fn replacement_tracks_source_less_vcs_change_through_its_owned_workspace() {
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
    let source = Uuid::from_u128(41_100);
    let workspace = Uuid::from_u128(41_101);
    let retained = Uuid::from_u128(41_102);
    let omitted = Uuid::from_u128(41_103);
    insert_capture_source(&store, source, PATH_A, "source-less-vcs-owner");
    store
        .conn
        .execute(
            "INSERT INTO vcs_workspaces (id, kind, root_path, repo_fingerprint, created_at_ms, updated_at_ms, source_id) VALUES (?1, 'git', '/owned', 'source-less-owned-repo', 1, 1, ?2)",
            params![workspace.to_string(), source.to_string()],
        )
        .unwrap();
    for (id, change_id) in [(retained, "retained"), (omitted, "omitted")] {
        store
            .conn
            .execute(
                "INSERT INTO vcs_changes (id, vcs_workspace_id, kind, change_id, created_at_ms, updated_at_ms) VALUES (?1, ?2, 'git_commit', ?3, 1, 1)",
                params![id.to_string(), workspace.to_string(), change_id],
            )
            .unwrap();
    }

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
    store
        .track_provider_file_publication_direct_entity("vcs_workspace", "vcs_workspaces", workspace)
        .unwrap();
    store
        .track_provider_file_publication_direct_entity("vcs_change", "vcs_changes", retained)
        .unwrap();
    let observer = Store::open(&path).unwrap();
    assert!(observer.list_vcs_changes().unwrap().is_empty());
    assert!(observer.export_archive().unwrap().vcs_changes.is_empty());

    reconcile_all(&store, &scope, 1);
    let finalized = store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap();
    assert_eq!(finalized.reconciliation.vcs_changes, 1);
    assert_eq!(finalized.reconciliation.vcs_workspaces, 0);
    assert!(row_exists(&store, "vcs_changes", retained));
    assert!(!row_exists(&store, "vcs_changes", omitted));
    assert!(row_exists(&store, "vcs_workspaces", workspace));
    assert_eq!(store.list_vcs_changes().unwrap()[0].id, retained);
}

#[test]
fn cross_process_vcs_hijack_is_rejected_and_scoped_source_less_upsert_is_valid() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let source = Uuid::from_u128(41_200);
    let unrelated_source = Uuid::from_u128(41_203);
    let workspace = Uuid::from_u128(41_201);
    let change_id = Uuid::from_u128(41_202);
    insert_capture_source(&store, source, PATH_A, "public-source-less-vcs");
    insert_capture_source(
        &store,
        unrelated_source,
        "/history/claude/projects/unrelated.jsonl",
        "unrelated-vcs-source",
    );
    store
        .conn
        .execute(
            "INSERT INTO vcs_workspaces (id, kind, root_path, repo_fingerprint, created_at_ms, updated_at_ms, source_id) VALUES (?1, 'git', '/owned-public', 'source-less-public-repo', 1, 1, ?2)",
            params![workspace.to_string(), source.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO vcs_changes (id, vcs_workspace_id, kind, change_id, created_at_ms, updated_at_ms) VALUES (?1, ?2, 'git_commit', 'source-less-public-change', 1, 1)",
            params![change_id.to_string(), workspace.to_string()],
        )
        .unwrap();
    let change = store.list_vcs_changes().unwrap().remove(0);
    assert_eq!(change.source_id, None);

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
    let mut hijack = change.clone();
    hijack.source_id = Some(unrelated_source);
    let mut writer = spawn_provider_file_vcs_writer(&store.path, &hijack);
    assert!(writer.wait().unwrap().success());
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT source_id FROM vcs_changes WHERE id = ?1",
                params![change_id.to_string()],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap(),
        None
    );
    prepare_all(&store, &scope, 1);
    let upserted = store
        .with_provider_file_publication_writes(&scope, |store| store.upsert_vcs_change(&change))
        .unwrap();
    assert_eq!(upserted, change_id);
    assert_eq!(staged_seen_count(&store), 2);
    reconcile_all(&store, &scope, 1);
    store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap();
    assert_eq!(store.list_vcs_changes().unwrap()[0].id, change_id);
}

#[test]
fn omitted_session_releases_transcript_artifact_before_artifact_reconciliation() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    let source = Uuid::from_u128(77_000);
    let session = Uuid::from_u128(77_001);
    let artifact = Uuid::from_u128(77_002);
    insert_capture_source(&store, source, PATH_A, "transcript-owner");
    store
        .conn
        .execute(
            "INSERT INTO artifacts (id, kind, blob_hash, blob_path, byte_size, created_at_ms, updated_at_ms, source_id) VALUES (?1, 'transcript', ?2, 'objects/transcript', 1, 1, 1, ?3)",
            params![artifact.to_string(), "7".repeat(64), source.to_string()],
        )
        .unwrap();
    insert_raw_session(&store, session, source, "transcript-owner");
    store
        .conn
        .execute(
            "UPDATE sessions SET transcript_blob_id = ?1 WHERE id = ?2",
            params![artifact.to_string(), session.to_string()],
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
    reconcile_all(&store, &scope, 1);
    let finalized = store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap();
    assert_eq!(finalized.reconciliation.sessions_tombstoned, 1);
    assert_eq!(finalized.reconciliation.artifacts, 1);
    assert!(!row_exists(&store, "artifacts", artifact));
    let session_state: (Option<String>, Option<i64>) = store
        .conn
        .query_row(
            "SELECT transcript_blob_id, deleted_at_ms FROM sessions WHERE id = ?1",
            params![session.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(session_state, (None, Some(100)));
}

#[test]
fn pending_catalog_owner_is_hidden_from_ctx_sources_view() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let catalog = CatalogSession {
        provider: CaptureProvider::Codex,
        source_format: "codex_session_jsonl".to_owned(),
        source_root: "/history/codex".to_owned(),
        source_path: "/history/codex/session.jsonl".to_owned(),
        external_session_id: Some("view-session".to_owned()),
        parent_external_session_id: None,
        agent_type: AgentType::Primary,
        role_hint: None,
        external_agent_id: None,
        cwd: None,
        session_started_at_ms: Some(1),
        file_size_bytes: 10,
        file_modified_at_ms: 100,
        import_revision: 1,
        cataloged_at_ms: 101,
        metadata: json!({"file_observation_token_v1": "archive-catalog-token"}),
    };
    let generation = store
        .allocate_catalog_inventory_generation(catalog.provider, &catalog.source_root)
        .unwrap();
    store
        .upsert_catalog_sessions(generation, std::slice::from_ref(&catalog))
        .unwrap();
    let update = CatalogSourceIndexUpdate {
        source_root: &catalog.source_root,
        source_path: &catalog.source_path,
        file_size_bytes: catalog.file_size_bytes,
        file_modified_at_ms: catalog.file_modified_at_ms,
        import_revision: catalog.import_revision,
        inventory_generation: generation,
        file_sha256: None,
        event_count: Some(0),
        indexed_at_ms: 110,
    };
    let outcome = ProviderFileImportOutcome {
        provider: catalog.provider,
        observation: ProviderFileInventoryObservation::ObservedCatalog {
            source_format: &catalog.source_format,
            update,
            metadata: &catalog.metadata,
        },
        status: CatalogIndexedStatus::Indexed,
        error: None,
    };
    assert!(store
        .complete_catalog_inventory_generation(catalog.provider, &catalog.source_root, generation,)
        .unwrap());
    assert!(store
        .catalog_inventory_generation_is_complete(
            catalog.provider,
            &catalog.source_root,
            generation,
        )
        .unwrap());
    let scope = store
        .begin_provider_file_publication(
            catalog.provider,
            outcome.observation,
            "codex_session_jsonl",
            ProviderFilePublicationKind::Replacement,
            105,
        )
        .unwrap();
    let hidden: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM ctx_sources", [], |row| row.get(0))
        .unwrap();
    assert_eq!(hidden, 0);
    assert!(store
        .list_catalog_sessions_for_source(catalog.provider, &catalog.source_root)
        .unwrap()
        .is_empty());
    assert!(store
        .list_pending_catalog_sessions(catalog.provider, &catalog.source_root)
        .unwrap()
        .is_empty());
    assert!(store
        .list_active_catalog_sessions_for_source(catalog.provider, &catalog.source_root)
        .unwrap()
        .is_empty());
    assert_eq!(store.catalog_session_count().unwrap(), 0);
    assert_eq!(store.catalog_session_counts().unwrap().total, 0);
    assert!(!store
        .catalog_inventory_generation_is_complete(
            catalog.provider,
            &catalog.source_root,
            generation,
        )
        .unwrap());
    reconcile_all(&store, &scope, 10);
    store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap();
    let published: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM ctx_sources", [], |row| row.get(0))
        .unwrap();
    assert_eq!(published, 1);
    assert_eq!(store.catalog_session_count().unwrap(), 1);
    assert_eq!(store.catalog_session_counts().unwrap().total, 1);
    assert!(store
        .catalog_inventory_generation_is_complete(
            catalog.provider,
            &catalog.source_root,
            generation,
        )
        .unwrap());
}

#[test]
fn pending_source_import_owner_is_hidden_from_pending_list_and_counts() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let file = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&file))
        .unwrap();
    assert_eq!(store.source_import_file_counts().unwrap().total, 1);
    assert_eq!(
        store
            .list_pending_source_import_files(file.provider, &file.source_root)
            .unwrap()
            .len(),
        1
    );

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
    assert_eq!(store.source_import_file_counts().unwrap().total, 0);
    assert!(store
        .list_pending_source_import_files(file.provider, &file.source_root)
        .unwrap()
        .is_empty());
    store
        .finalize_provider_file_publication(
            scope,
            outcome,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap();
    assert_eq!(store.source_import_file_counts().unwrap().total, 1);
}
