#[cfg(test)]
mod freshness_tests {
    use super::*;
    use crate::commands::import::{
        inject_import_maintenance_progress_steps, inject_inventory_failure_once,
        repair_import_maintenance, run_import_internal, ImportFailureType, ImportInventory,
        ImportInventoryFailures, ImportMaintenancePendingReason, ImportMaintenanceStep,
        ImportRunOptions, InventoryFailurePoint, NativeSourceReports,
    };
    use crate::provider_args::NativeProviderArg;
    use crate::provider_sources::explicit_path_source;
    use crate::ImportArgs;
    use ctx_history_core::{canonical_provider_material_source_format, utc_now, CaptureProvider};
    use ctx_history_store::{
        CatalogIndexedStatus, ProviderFileInventoryObservation, ProviderFilePublicationKind,
        ProviderFilePublicationScope, SourceImportFile, SourceImportFileIndexUpdate,
    };

    fn write_pi_source(root: &Path, count: usize, label: &str) -> SourceInfo {
        fs::create_dir_all(root).unwrap();
        for index in 0..count {
            fs::write(
                root.join(format!("{index:03}.jsonl")),
                format!(
                    "{}\n{}\n",
                    json!({
                        "type": "session",
                        "id": format!("{label}-{index}"),
                        "timestamp": "2026-07-14T12:00:00Z"
                    }),
                    json!({
                        "type": "message",
                        "id": format!("{label}-message-{index}"),
                        "timestamp": "2026-07-14T12:00:01Z",
                        "message": {"role": "user", "content": format!("{label} {index}")}
                    })
                ),
            )
            .unwrap();
        }
        explicit_path_source(CaptureProvider::Pi, root.to_path_buf())
    }

    fn write_codex_source(root: &Path, label: &str) -> (SourceInfo, PathBuf) {
        fs::create_dir_all(root).unwrap();
        let path = root.join(format!("{label}.jsonl"));
        fs::write(
            &path,
            format!(
                "{}\n{}\n",
                json!({
                    "timestamp": "2026-07-14T12:00:00Z",
                    "type": "session_meta",
                    "payload": {
                        "id": label,
                        "timestamp": "2026-07-14T12:00:00Z",
                        "cwd": "/repo",
                        "originator": "codex-cli",
                        "source": "cli"
                    }
                }),
                json!({
                    "timestamp": "2026-07-14T12:00:01Z",
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "user",
                        "content": [{"type": "input_text", "text": label}]
                    }
                })
            ),
        )
        .unwrap();
        (
            explicit_path_source(CaptureProvider::Codex, root.to_path_buf()),
            path,
        )
    }

    fn write_many_codex_sources(root: &Path, count: usize, label: &str) -> SourceInfo {
        let mut source = None;
        for index in 0..count {
            let (candidate, _) = write_codex_source(root, &format!("{label}-{index:03}"));
            source = Some(candidate);
        }
        source.expect("Codex transition fixture must contain at least one session")
    }

    const PUBLICATION_TRANSITION_POINTS: [(&str, usize, bool); 8] = [
        ("before synthetic owner page", 0, true),
        ("after synthetic owner page", 1, true),
        ("mid Codex discover", 2, false),
        ("mid Codex validate", 3, false),
        ("mid Codex process", 4, false),
        ("mid Codex stale", 5, false),
        ("mid Codex resume", 6, false),
        ("before Codex completion", 7, false),
    ];

    fn drain_inventory_after_injected_failure(
        store: &Store,
        source: SourceInfo,
        point: InventoryFailurePoint,
    ) -> Vec<ImportInventory> {
        let mut cursor = ImportInventoryCursor::new(store, vec![source], false, false).unwrap();
        let mut pages = Vec::new();
        let mut failures = 0usize;
        inject_inventory_failure_once(point);
        loop {
            match cursor.advance(store) {
                Ok(ImportInventoryCursorStep::Pending(_)) => {}
                Ok(ImportInventoryCursorStep::SourceComplete(page)) => pages.push(page),
                Ok(ImportInventoryCursorStep::Complete) => break,
                Ok(ImportInventoryCursorStep::PublicationTransition(transition)) => {
                    panic!("unexpected publication transition: {transition:?}")
                }
                Err(error) => {
                    failures += 1;
                    assert!(error
                        .to_string()
                        .contains("injected inventory boundary failure"));
                }
            }
        }
        assert_eq!(failures, 1);
        pages
    }

    fn finalized_manifest_inventory(
        store: &Store,
        source: &SourceInfo,
    ) -> (Vec<SourceImportFile>, u64) {
        let inventory = inventory_import_sources(store, vec![source.clone()], false).unwrap();
        assert!(inventory.failures.is_empty());
        let planned = inventory
            .sources
            .iter()
            .find(|planned| planned.source.path == source.path)
            .expect("bounded inventory must return the requested source");
        let inventory_generation = match &planned.preinventory {
            crate::commands::import::SourcePreinventory::SourceImportFiles {
                inventory_generation,
                ..
            } => *inventory_generation,
            other => panic!("unexpected manifested inventory: {other:?}"),
        };
        let source_root = source.path.to_str().unwrap();
        assert!(store
            .source_import_inventory_generation_is_complete(
                source.provider,
                source_root,
                inventory_generation,
            )
            .unwrap());
        let files = store
            .list_pending_source_import_files(source.provider, source_root)
            .unwrap();
        (files, inventory_generation)
    }

    #[test]
    fn inventory_page_failures_do_not_advance_root_or_manifest_state() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();

        let root_path = temp.path().join("codebuddy-root");
        fs::create_dir_all(&root_path).unwrap();
        for index in 0..33 {
            fs::write(root_path.join(format!("source-{index:02}.json")), b"{}\n").unwrap();
        }
        let root_source = explicit_path_source(CaptureProvider::CodeBuddy, root_path);
        let root_pages = drain_inventory_after_injected_failure(
            &store,
            root_source,
            InventoryFailurePoint::RootAfterObservation,
        );
        assert_eq!(root_pages.len(), 1);
        assert_eq!(root_pages[0].totals.source_files, 33);
        assert!(root_pages[0].failures.is_empty());

        let manifest_source = write_pi_source(&temp.path().join("pi-manifest"), 70, "retry");
        let manifest_root = manifest_source.path.to_str().unwrap().to_owned();
        let manifest_pages = drain_inventory_after_injected_failure(
            &store,
            manifest_source.clone(),
            InventoryFailurePoint::ManifestAfterObservation,
        );
        assert_eq!(manifest_pages.len(), 1);
        assert_eq!(manifest_pages[0].totals.source_files, 70);
        assert!(manifest_pages[0].failures.is_empty());
        assert_eq!(
            store
                .list_pending_source_import_files(manifest_source.provider, &manifest_root)
                .unwrap()
                .len(),
            70
        );
    }

    fn seed_failed_pi_backlog(data_root: &Path, count: usize) -> SourceInfo {
        fs::create_dir_all(data_root).unwrap();
        let source = write_pi_source(&data_root.join("pi-backlog"), count, "recovery");
        let store = Store::open(database_path(data_root.to_path_buf())).unwrap();
        let (files, inventory_generation) = finalized_manifest_inventory(&store, &source);
        assert_eq!(files.len(), count);
        for file in &files {
            assert_eq!(
                store
                    .record_source_import_file_result(
                        file.provider,
                        SourceImportFileIndexUpdate {
                            source_root: &file.source_root,
                            source_path: &file.source_path,
                            file_size_bytes: file.file_size_bytes,
                            file_modified_at_ms: file.file_modified_at_ms,
                            import_revision: file.import_revision,
                            inventory_generation,
                            metadata: &file.metadata,
                            indexed_at_ms: utc_now().timestamp_millis(),
                        },
                        CatalogIndexedStatus::Failed,
                        Some("deterministic recovery fixture"),
                    )
                    .unwrap(),
                1
            );
        }
        source
    }

    fn refresh(
        data_root: &Path,
        sources: Vec<SourceInfo>,
        policy: ImportExecutionPolicy,
    ) -> ImportTotals {
        refresh_sources_for_search(
            data_root,
            sources,
            Vec::new(),
            RefreshArg::Background,
            false,
            policy,
        )
        .unwrap()
    }

    fn refresh_with_runtime(
        data_root: &Path,
        runtime: &mut SearchRefreshRuntime,
        sources: Vec<SourceInfo>,
    ) -> ImportTotals {
        refresh_sources_for_search_with_runtime(
            data_root,
            sources,
            Vec::new(),
            RefreshArg::Background,
            false,
            ImportExecutionPolicy::Daemon,
            runtime,
        )
        .unwrap()
    }

    fn drive_daemon_until_staged_publication(
        data_root: &Path,
        runtime: &mut SearchRefreshRuntime,
        source: &SourceInfo,
    ) -> ImportTotals {
        for _ in 0..64 {
            let totals = refresh_with_runtime(data_root, runtime, vec![source.clone()]);
            if Store::open(database_path(data_root.to_path_buf()))
                .unwrap()
                .effective_provider_file_publication_has_staged_completion()
                .unwrap()
            {
                return totals;
            }
        }
        panic!("daemon refresh did not stage a provider publication");
    }

    fn cached_inventory_generation(runtime: &SearchRefreshRuntime, source: &SourceInfo) -> u64 {
        runtime
            .cached_work
            .as_ref()
            .unwrap()
            .plan
            .sources
            .iter()
            .find(|planned| planned.source.path == source.path)
            .and_then(|planned| planned.preinventory.inventory_generation())
            .unwrap()
    }

    fn begin_unmutated_pi_publication(
        store: &Store,
        source: &SourceInfo,
    ) -> ProviderFilePublicationScope {
        let (_, inventory_generation) = finalized_manifest_inventory(store, source);
        let source_root = source.path.to_str().unwrap();
        assert_eq!(
            store
                .schedule_source_import_explicit_rescan(
                    source.provider,
                    source_root,
                    inventory_generation,
                )
                .unwrap(),
            1
        );
        let file = store
            .list_pending_source_import_files(source.provider, source_root)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        store
            .begin_provider_file_publication(
                file.provider,
                ProviderFileInventoryObservation::SourceImport {
                    source_format: &file.source_format,
                    update: SourceImportFileIndexUpdate {
                        source_root: file.source_root.as_str(),
                        source_path: file.source_path.as_str(),
                        file_size_bytes: file.file_size_bytes,
                        file_modified_at_ms: file.file_modified_at_ms,
                        import_revision: file.import_revision,
                        inventory_generation,
                        metadata: &file.metadata,
                        indexed_at_ms: utc_now().timestamp_millis(),
                    },
                },
                canonical_provider_material_source_format(file.provider, &file.source_format)
                    .unwrap(),
                ProviderFilePublicationKind::Replacement,
                utc_now().timestamp_millis(),
            )
            .unwrap()
    }

    fn leave_unmutated_pi_publication(store: &Store, source: &SourceInfo) {
        drop(begin_unmutated_pi_publication(store, source));
    }

    fn restore_indexed_manifest_observation(store: &Store, source: &SourceInfo) {
        let (files, inventory_generation) = finalized_manifest_inventory(store, source);
        for file in files {
            assert_eq!(
                store
                    .record_source_import_file_result(
                        file.provider,
                        SourceImportFileIndexUpdate {
                            source_root: &file.source_root,
                            source_path: &file.source_path,
                            file_size_bytes: file.file_size_bytes,
                            file_modified_at_ms: file.file_modified_at_ms,
                            import_revision: file.import_revision,
                            inventory_generation,
                            metadata: &file.metadata,
                            indexed_at_ms: utc_now().timestamp_millis(),
                        },
                        CatalogIndexedStatus::Indexed,
                        None,
                    )
                    .unwrap(),
                1
            );
        }
    }

    fn arm_inventory_publication_transition(
        store: &Store,
        point: usize,
        synthetic_owner: bool,
        owner_a: &SourceInfo,
        owner_b: SourceInfo,
    ) {
        let owner_a_scope = synthetic_owner.then(|| begin_unmutated_pi_publication(store, owner_a));
        let owner_a = synthetic_owner.then(|| owner_a.clone());
        ImportInventoryCursor::inject_publication_transition_once(point, move |store| {
            if let Some(scope) = owner_a_scope {
                let aborted = store.abort_provider_file_publication(scope)?;
                assert!(matches!(aborted, std::ops::ControlFlow::Continue(_)));
                restore_indexed_manifest_observation(
                    store,
                    owner_a
                        .as_ref()
                        .expect("synthetic owner fixture is missing"),
                );
            }
            leave_unmutated_pi_publication(store, &owner_b);
            Ok(())
        });
    }

    fn stage_pi_recovery_publication(data_root: &Path, source: &SourceInfo) {
        let baseline = refresh(
            data_root,
            vec![source.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(baseline.fresh_units_pending, 0, "{baseline:?}");

        let store = Store::open(database_path(data_root.to_path_buf())).unwrap();
        let (_, inventory_generation) = finalized_manifest_inventory(&store, source);
        assert_eq!(
            store
                .schedule_source_import_explicit_rescan(
                    source.provider,
                    source.path.to_str().unwrap(),
                    inventory_generation,
                )
                .unwrap(),
            1
        );
        drop(store);

        let mut runtime = SearchRefreshRuntime::default();
        let staged = drive_daemon_until_staged_publication(data_root, &mut runtime, source);
        assert!(staged.recovery_units_pending > 0, "{staged:?}");
        let store = Store::open(database_path(data_root.to_path_buf())).unwrap();
        assert!(store
            .effective_provider_file_publication_has_staged_completion()
            .unwrap());
    }

    #[test]
    fn daemon_cached_refresh_watcher_tracks_source_changes() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source = write_pi_source(&data_root.join("pi-watch"), 1, "watch");
        let mut runtime = SearchRefreshRuntime::default();

        let initial = runtime.watcher_changes(std::slice::from_ref(&source));
        assert_eq!(initial, Some(SearchRefreshSourceChanges::default()));
        runtime.force_source_change_for_test(&source.path);
        let changed = runtime
            .watcher_changes(std::slice::from_ref(&source))
            .unwrap();
        assert!(!changed.full_rebuild);
        assert_eq!(
            changed.dirty_paths,
            BTreeSet::from([SearchDirtyPath {
                source_path: source.path.clone(),
                changed_path: source.path,
            }])
        );
    }

    #[test]
    fn daemon_codex_append_reobserves_one_path_without_tree_inventory() {
        use std::io::Write;

        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let (source, session_path) =
            write_codex_source(&data_root.join("codex-watch"), "bounded-dirty");
        let baseline = refresh(
            &data_root,
            vec![source.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(baseline.fresh_units_pending, 0, "{baseline:?}");

        let mut runtime = SearchRefreshRuntime::default();
        let _ = refresh_with_runtime(&data_root, &mut runtime, vec![source.clone()]);
        while runtime.inventory_progress.is_some() {
            let _ = refresh_with_runtime(&data_root, &mut runtime, vec![source.clone()]);
        }
        let generation = cached_inventory_generation(&runtime, &source);
        let operations_before = runtime
            .disk_io_pacer(ImportExecutionPolicy::Daemon)
            .filesystem_operation_count();
        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .open(&session_path)
                .unwrap(),
            "{}",
            json!({
                "timestamp": "2026-07-14T12:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "exact dirty path"}]
                }
            })
        )
        .unwrap();
        runtime.force_source_file_change_for_test(&source.path, &session_path);

        let _ = refresh_with_runtime(&data_root, &mut runtime, vec![source.clone()]);

        assert!(runtime.inventory_progress.is_none());
        assert!(runtime.pending_dirty_paths.is_empty());
        assert_eq!(cached_inventory_generation(&runtime, &source), generation);
        let operations = runtime
            .disk_io_pacer(ImportExecutionPolicy::Daemon)
            .filesystem_operation_count()
            .saturating_sub(operations_before);
        assert!(
            operations < 64,
            "exact dirty path used {operations} operations"
        );

        let (_, created_path) = write_codex_source(&source.path, "bounded-created");
        runtime.force_source_file_change_for_test(&source.path, &created_path);
        let _ = refresh_with_runtime(&data_root, &mut runtime, vec![source.clone()]);
        assert!(runtime.inventory_progress.is_none());
        assert_eq!(cached_inventory_generation(&runtime, &source), generation);

        fs::remove_file(&session_path).unwrap();
        runtime.force_source_file_change_for_test(&source.path, &session_path);
        let _ = refresh_with_runtime(&data_root, &mut runtime, vec![source.clone()]);
        assert!(runtime.inventory_progress.is_none());
        assert_eq!(cached_inventory_generation(&runtime, &source), generation);
        let store = Store::open(database_path(data_root)).unwrap();
        assert!(store
            .list_active_catalog_sessions_for_source(
                CaptureProvider::Codex,
                source.path.to_str().unwrap(),
            )
            .unwrap()
            .iter()
            .all(|session| session.source_path != session_path.to_string_lossy().as_ref()));
    }

    #[test]
    fn daemon_failed_codex_exact_page_escalates_to_scoped_inventory() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let (source, session_path) =
            write_codex_source(&data_root.join("codex-failed-page"), "failed-page");
        let (unaffected_source, _) =
            write_codex_source(&data_root.join("codex-unaffected"), "unaffected");
        let baseline = refresh(
            &data_root,
            vec![source.clone(), unaffected_source.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(baseline.fresh_units_pending, 0, "{baseline:?}");

        let mut runtime = SearchRefreshRuntime::default();
        let _ = refresh_with_runtime(
            &data_root,
            &mut runtime,
            vec![source.clone(), unaffected_source.clone()],
        );
        while runtime.inventory_progress.is_some() {
            let _ = refresh_with_runtime(
                &data_root,
                &mut runtime,
                vec![source.clone(), unaffected_source.clone()],
            );
        }
        let sweep_deadline = runtime.next_inventory_at;
        let generation = cached_inventory_generation(&runtime, &source);
        let unaffected_generation = cached_inventory_generation(&runtime, &unaffected_source);
        fs::write(&session_path, b"{not-json\n").unwrap();
        runtime.force_source_file_change_for_test(&source.path, &session_path);

        let _ = refresh_with_runtime(
            &data_root,
            &mut runtime,
            vec![source.clone(), unaffected_source.clone()],
        );

        assert!(runtime.pending_dirty_paths.is_empty());
        for _ in 0..64 {
            let Some(progress) = runtime.inventory_progress.as_ref() else {
                break;
            };
            assert!(progress.scoped);
            let _ = refresh_with_runtime(
                &data_root,
                &mut runtime,
                vec![source.clone(), unaffected_source.clone()],
            );
        }
        assert!(runtime.inventory_progress.is_none());
        assert_ne!(cached_inventory_generation(&runtime, &source), generation);
        assert_eq!(
            cached_inventory_generation(&runtime, &unaffected_source),
            unaffected_generation
        );
        assert_eq!(runtime.next_inventory_at, sweep_deadline);
    }

    #[test]
    fn daemon_cached_refresh_watcher_errors_invalidate_cached_work() {
        let changes = Mutex::new(SearchRefreshSourceChanges::default());
        let healthy = AtomicBool::new(true);

        note_search_refresh_source_event(
            &changes,
            &healthy,
            &Mutex::new(None),
            &[],
            Err(notify::Error::generic("deterministic watcher failure")),
        );

        assert!(changes.into_inner().unwrap().full_rebuild);
        assert!(!healthy.load(Ordering::Acquire));
    }

    #[test]
    fn daemon_watcher_dirty_path_overflow_escalates_without_retaining_the_corpus() {
        let mut changes = SearchRefreshSourceChanges::default();
        for index in 0..=MAX_PENDING_DIRTY_PATHS {
            record_search_refresh_dirty_path(
                &mut changes,
                SearchDirtyPath {
                    source_path: PathBuf::from("/sessions"),
                    changed_path: PathBuf::from(format!("/sessions/{index}.jsonl")),
                },
            );
        }

        assert!(changes.full_rebuild);
        assert!(changes.dirty_paths.is_empty());
    }

    #[test]
    fn daemon_cached_refresh_watcher_ignores_non_mutating_access() {
        let changes = Mutex::new(SearchRefreshSourceChanges::default());
        let healthy = AtomicBool::new(true);

        note_search_refresh_source_event(
            &changes,
            &healthy,
            &Mutex::new(None),
            &[],
            Ok(notify::Event::new(notify::EventKind::Access(
                notify::event::AccessKind::Read,
            ))),
        );
        note_search_refresh_source_event(
            &changes,
            &healthy,
            &Mutex::new(None),
            &[],
            Ok(notify::Event::new(notify::EventKind::Access(
                notify::event::AccessKind::Open(notify::event::AccessMode::Any),
            ))),
        );
        assert_eq!(
            changes.into_inner().unwrap(),
            SearchRefreshSourceChanges::default()
        );
        assert!(healthy.load(Ordering::Acquire));
    }

    #[test]
    fn daemon_cached_refresh_watcher_scopes_mutations_to_matching_roots() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let changes = Mutex::new(SearchRefreshSourceChanges::default());
        let healthy = AtomicBool::new(true);
        let watches = search_refresh_watch_specs(&[first.clone(), second]);

        note_search_refresh_source_event(
            &changes,
            &healthy,
            &Mutex::new(None),
            &watches,
            Ok(
                notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
                    .add_path(first.join("session.jsonl")),
            ),
        );

        let changes = changes.into_inner().unwrap();
        assert!(!changes.full_rebuild);
        assert_eq!(
            changes.dirty_paths,
            BTreeSet::from([SearchDirtyPath {
                source_path: first.clone(),
                changed_path: first.join("session.jsonl"),
            }])
        );
        assert!(healthy.load(Ordering::Acquire));
    }

    #[test]
    fn daemon_cached_refresh_watcher_maps_sqlite_sidecars_to_the_database_source() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("history.sqlite");
        fs::write(&database, []).unwrap();
        let watches = search_refresh_watch_specs(std::slice::from_ref(&database));
        let changes = Mutex::new(SearchRefreshSourceChanges::default());
        let healthy = AtomicBool::new(true);

        note_search_refresh_source_event(
            &changes,
            &healthy,
            &Mutex::new(None),
            &watches,
            Ok(
                notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
                    .add_path(temp.path().join("history.sqlite-wal")),
            ),
        );

        let changes = changes.into_inner().unwrap();
        assert!(!changes.full_rebuild);
        assert_eq!(
            changes.dirty_paths,
            BTreeSet::from([SearchDirtyPath {
                source_path: database,
                changed_path: temp.path().join("history.sqlite-wal"),
            }])
        );
    }

    #[test]
    fn daemon_cached_refresh_watcher_ignores_unrelated_file_siblings() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("history.sqlite");
        fs::write(&database, []).unwrap();
        let watches = search_refresh_watch_specs(std::slice::from_ref(&database));
        let changes = Mutex::new(SearchRefreshSourceChanges::default());
        let healthy = AtomicBool::new(true);

        note_search_refresh_source_event(
            &changes,
            &healthy,
            &Mutex::new(None),
            &watches,
            Ok(
                notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
                    .add_path(temp.path().join("unrelated.sqlite")),
            ),
        );

        assert_eq!(
            changes.into_inner().unwrap(),
            SearchRefreshSourceChanges::default()
        );
        assert!(healthy.load(Ordering::Acquire));
    }

    #[test]
    fn daemon_cached_refresh_watcher_invalidates_coalesced_parent_events() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("history.sqlite");
        fs::write(&database, []).unwrap();
        let watches = search_refresh_watch_specs(std::slice::from_ref(&database));
        let changes = Mutex::new(SearchRefreshSourceChanges::default());
        let healthy = AtomicBool::new(true);

        note_search_refresh_source_event(
            &changes,
            &healthy,
            &Mutex::new(None),
            &watches,
            Ok(
                notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
                    .add_path(temp.path().to_path_buf()),
            ),
        );

        let changes = changes.into_inner().unwrap();
        assert!(changes.full_rebuild);
        assert!(changes.dirty_paths.is_empty());
        assert!(healthy.load(Ordering::Acquire));
    }

    #[test]
    fn daemon_cached_refresh_watcher_invalidates_out_of_scope_events() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("history.sqlite");
        fs::write(&database, []).unwrap();
        let watches = search_refresh_watch_specs(std::slice::from_ref(&database));
        let changes = Mutex::new(SearchRefreshSourceChanges::default());
        let healthy = AtomicBool::new(true);

        note_search_refresh_source_event(
            &changes,
            &healthy,
            &Mutex::new(None),
            &watches,
            Ok(
                notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
                    .add_path(temp.path().join("other").join("history.sqlite")),
            ),
        );

        let changes = changes.into_inner().unwrap();
        assert!(changes.full_rebuild);
        assert!(changes.dirty_paths.is_empty());
        assert!(healthy.load(Ordering::Acquire));
    }

    #[test]
    fn daemon_cached_refresh_watcher_invalidates_ambiguous_access() {
        use notify::event::{AccessKind, AccessMode};

        for access in [
            AccessKind::Any,
            AccessKind::Other,
            AccessKind::Close(AccessMode::Any),
            AccessKind::Close(AccessMode::Write),
            AccessKind::Close(AccessMode::Other),
        ] {
            assert!(!search_refresh_event_is_non_mutating_access(
                notify::EventKind::Access(access)
            ));
        }
        for access in [
            AccessKind::Read,
            AccessKind::Open(AccessMode::Any),
            AccessKind::Open(AccessMode::Read),
            AccessKind::Open(AccessMode::Write),
            AccessKind::Close(AccessMode::Read),
            AccessKind::Close(AccessMode::Execute),
        ] {
            assert!(search_refresh_event_is_non_mutating_access(
                notify::EventKind::Access(access)
            ));
        }
    }

    #[test]
    fn daemon_watcher_root_identity_changes_when_path_is_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("watched-root");
        fs::create_dir(&root).unwrap();
        let initial = watched_source_path_identities(std::slice::from_ref(&root));

        fs::rename(&root, temp.path().join("old-root")).unwrap();
        fs::create_dir(&root).unwrap();
        let replaced = watched_source_path_identities(std::slice::from_ref(&root));

        assert_ne!(initial, replaced);
    }

    #[test]
    fn daemon_directory_watcher_registration_is_nonrecursive_and_constant_size() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("watched-root");
        fs::create_dir(&root).unwrap();
        for index in 0..128 {
            fs::create_dir(root.join(format!("nested-{index}"))).unwrap();
        }
        let pacer = ctx_history_capture::DiskIoPacer::new(u64::MAX, u64::MAX);
        let _pacing = ctx_history_capture::install_disk_io_pacer(pacer.clone());
        let watches = search_refresh_watch_specs(std::slice::from_ref(&root));
        let registrations = search_refresh_watch_registrations(&watches).unwrap();

        let watcher = SearchRefreshSourceWatcher::new(vec![root]).unwrap();

        assert_eq!(registrations.len(), 1);
        assert!(registrations
            .iter()
            .all(|(_, mode)| *mode == RecursiveMode::NonRecursive));
        assert_eq!(watcher.registered_watch_count, 1);
        assert_eq!(watcher.directory_source_count, 1);
        assert!(pacer.filesystem_operation_count() < 32);
    }

    #[test]
    fn daemon_watcher_registration_fails_to_bounded_reconciliation_above_root_limit() {
        let watches = (0..=MAX_SEARCH_REFRESH_ROOT_WATCHES)
            .map(|index| SearchRefreshWatch {
                source_path: PathBuf::from(format!("source-{index}")),
                match_path: PathBuf::from(format!("source-{index}")),
                watch_path: PathBuf::from(format!("watch-{index}")),
                recursive: true,
            })
            .collect::<Vec<_>>();

        let error = search_refresh_watch_registrations(&watches).unwrap_err();

        assert!(error
            .to_string()
            .contains("bounded watcher registration limit"));
    }

    #[test]
    fn daemon_root_only_watcher_reports_degraded_coverage_and_schedules_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source = write_pi_source(&temp.path().join("history"), 1, "root-only");
        let mut runtime = SearchRefreshRuntime::default();

        for _ in 0..32 {
            let _ = refresh_with_runtime(&data_root, &mut runtime, vec![source.clone()]);
            if runtime.inventory_progress.is_none() && runtime.cached_work.is_some() {
                break;
            }
        }

        let status = runtime.daemon_status_json();
        assert!(runtime.watcher_degraded);
        assert!(runtime.next_inventory_at.is_some());
        assert_eq!(status["watcher"]["state"].as_str(), Some("degraded"));
        assert_eq!(status["watcher"]["coverage"].as_str(), Some("root_only"));
        assert_eq!(status["watcher"]["registered_paths"].as_u64(), Some(1));
        assert!(status["inventory"]["next_fallback_at_ms"].is_number());
    }

    #[test]
    fn daemon_root_only_watcher_nested_change_converges_through_bounded_sweep() {
        use std::io::Write as _;

        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = temp.path().join("history");
        let (_, session_path) = write_codex_source(&source_root.join("nested"), "nested-sweep");
        let source = explicit_path_source(CaptureProvider::Codex, source_root);
        let baseline = refresh(
            &data_root,
            vec![source.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(baseline.fresh_units_pending, 0, "{baseline:?}");
        let mut runtime = SearchRefreshRuntime::default();
        for _ in 0..32 {
            let _ = refresh_with_runtime(&data_root, &mut runtime, vec![source.clone()]);
            if runtime.inventory_progress.is_none() && runtime.cached_work.is_some() {
                break;
            }
        }
        assert!(runtime.watcher_degraded);

        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .open(&session_path)
                .unwrap(),
            "{}",
            json!({
                "timestamp": "2026-07-14T12:00:03Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "nested bounded sweep oracle"}]
                }
            })
        )
        .unwrap();
        runtime.next_inventory_at = Some(Instant::now());
        runtime.next_inventory_at_ms = Some(search_refresh_now_ms());

        let mut found = false;
        for _ in 0..64 {
            let _ = refresh_with_runtime(&data_root, &mut runtime, vec![source.clone()]);
            found = !Store::open(database_path(data_root.clone()))
                .unwrap()
                .search_event_hits("nested bounded sweep oracle", 10)
                .unwrap()
                .is_empty();
            if found {
                break;
            }
        }

        assert!(found);
        assert!(runtime.watcher_degraded);
    }

    #[test]
    fn daemon_watcher_identity_probes_charge_the_filesystem_pacer() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        let pacer = ctx_history_capture::DiskIoPacer::new(u64::MAX, u64::MAX);
        let _pacing = ctx_history_capture::install_disk_io_pacer(pacer.clone());

        let identities = watched_source_path_identities(&[first, second]);

        assert_eq!(identities.len(), 2);
        assert!(pacer.filesystem_operation_count() >= 2);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn daemon_watcher_root_identity_ignores_directory_content_changes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("watched-root");
        fs::create_dir(&root).unwrap();
        let initial = watched_source_path_identities(std::slice::from_ref(&root));

        fs::write(root.join("new-session.jsonl"), b"new session").unwrap();
        let changed = watched_source_path_identities(std::slice::from_ref(&root));

        assert_eq!(initial, changed);
        assert!(initial[0].stable_id.is_some());
    }

    #[test]
    fn daemon_periodic_reinventory_uses_elapsed_time_not_backlog_state() {
        assert!(!daemon_search_refresh_reinventory_due(
            DAEMON_SEARCH_REFRESH_REINVENTORY_INTERVAL - StdDuration::from_secs(1),
            false,
        ));
        assert!(daemon_search_refresh_reinventory_due(
            DAEMON_SEARCH_REFRESH_REINVENTORY_INTERVAL,
            false,
        ));
        assert!(!daemon_search_refresh_reinventory_due(
            DAEMON_SEARCH_REFRESH_REINVENTORY_INTERVAL,
            true,
        ));
    }

    #[test]
    fn daemon_failed_watcher_uses_capped_retry_and_fallback_delays() {
        assert_eq!(
            (1..=7)
                .map(daemon_search_refresh_retry_delay)
                .collect::<Vec<_>>(),
            vec![
                StdDuration::from_secs(30),
                StdDuration::from_secs(60),
                StdDuration::from_secs(2 * 60),
                StdDuration::from_secs(4 * 60),
                StdDuration::from_secs(5 * 60),
                StdDuration::from_secs(5 * 60),
                StdDuration::from_secs(5 * 60),
            ]
        );
    }

    #[test]
    fn daemon_scoped_refresh_preserves_unaffected_plan_and_full_sweep_age() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let dirty = write_pi_source(&data_root.join("pi-dirty"), 1, "dirty");
        let unchanged = write_pi_source(&data_root.join("pi-unchanged"), 1, "unchanged");
        let baseline = refresh(
            &data_root,
            vec![dirty.clone(), unchanged.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(baseline.fresh_units_pending, 0, "{baseline:?}");

        let mut runtime = SearchRefreshRuntime::default();
        let mut cached = refresh_with_runtime(
            &data_root,
            &mut runtime,
            vec![dirty.clone(), unchanged.clone()],
        );
        for _ in 0..64 {
            if runtime.inventory_progress.is_none()
                && runtime.cached_work.is_some()
                && runtime.pending_inventory_reason.is_none()
                && runtime.pending_dirty_paths.is_empty()
                && runtime.next_inventory_at.is_some()
            {
                break;
            }
            cached = refresh_with_runtime(
                &data_root,
                &mut runtime,
                vec![dirty.clone(), unchanged.clone()],
            );
        }
        assert!(runtime.inventory_progress.is_none());
        assert!(runtime.next_inventory_at.is_some());
        assert!(runtime.next_inventory_at_ms.is_some());
        assert_eq!(cached.fresh_units_pending, 0, "{cached:?}");
        let dirty_generation = cached_inventory_generation(&runtime, &dirty);
        let unchanged_generation = cached_inventory_generation(&runtime, &unchanged);
        let sweep_deadline = runtime.next_inventory_at;
        let sweep_deadline_ms = runtime.next_inventory_at_ms;
        let last_full_inventory = runtime.cached_work.as_ref().unwrap().last_reinventory_at;
        assert!(runtime.watcher_degraded);
        assert!(runtime.daemon_status_json()["inventory"]["next_fallback_at_ms"].is_number());
        runtime
            .cached_work
            .as_mut()
            .unwrap()
            .passes_since_reinventory = 17;

        write_pi_source(&dirty.path, 2, "dirtychanged");
        runtime.force_source_change_for_test(&dirty.path);
        let mut refreshed = refresh_with_runtime(
            &data_root,
            &mut runtime,
            vec![dirty.clone(), unchanged.clone()],
        );
        if let Some(progress) = runtime.inventory_progress.as_ref() {
            assert!(progress.scoped);
        }
        while runtime.inventory_progress.is_some() {
            assert!(runtime
                .inventory_progress
                .as_ref()
                .is_some_and(|progress| progress.scoped));
            refreshed = refresh_with_runtime(
                &data_root,
                &mut runtime,
                vec![dirty.clone(), unchanged.clone()],
            );
        }
        assert_ne!(
            cached_inventory_generation(&runtime, &dirty),
            dirty_generation
        );
        assert_eq!(
            cached_inventory_generation(&runtime, &unchanged),
            unchanged_generation
        );
        assert_eq!(runtime.next_inventory_at, sweep_deadline);
        assert_eq!(runtime.next_inventory_at_ms, sweep_deadline_ms);
        assert!(runtime.next_inventory_at.is_some());
        assert!(runtime.daemon_status_json()["inventory"]["next_fallback_at_ms"].is_number());
        assert_eq!(
            runtime.cached_work.as_ref().unwrap().last_reinventory_at,
            last_full_inventory
        );
        for _ in 0..16 {
            if refreshed.fresh_units_pending == 0 {
                break;
            }
            refreshed = refresh_with_runtime(
                &data_root,
                &mut runtime,
                vec![dirty.clone(), unchanged.clone()],
            );
        }
        assert_eq!(refreshed.fresh_units_pending, 0, "{refreshed:?}");
        let store = Store::open(database_path(data_root.clone())).unwrap();
        assert!(!store
            .search_event_hits("dirtychanged 1", 10)
            .unwrap()
            .is_empty());
        assert_eq!(
            cached_inventory_generation(&runtime, &unchanged),
            unchanged_generation
        );
    }

    #[test]
    fn daemon_codex_change_during_full_inventory_does_not_schedule_another_full_pass() {
        use std::io::Write;

        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let (changed, session_path) =
            write_codex_source(&data_root.join("codex-first"), "changed-during-inventory");
        let (second, _) = write_codex_source(&data_root.join("codex-second"), "second");
        let sources = vec![changed.clone(), second];
        let baseline = refresh(&data_root, sources.clone(), ImportExecutionPolicy::Drain);
        assert_eq!(baseline.fresh_units_pending, 0, "{baseline:?}");

        let mut runtime = SearchRefreshRuntime::default();
        let _ = refresh_with_runtime(&data_root, &mut runtime, sources.clone());
        assert!(runtime.inventory_progress.is_some());

        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .open(&session_path)
                .unwrap(),
            "{}",
            json!({
                "timestamp": "2026-07-14T12:00:03Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "changed mid inventory"}]
                }
            })
        )
        .unwrap();
        runtime.force_source_file_change_for_test(&changed.path, &session_path);
        while runtime.inventory_progress.is_some() {
            let _ = refresh_with_runtime(&data_root, &mut runtime, sources.clone());
            assert!(!runtime.pending_full_inventory);
        }
        assert_eq!(
            runtime.pending_dirty_paths,
            BTreeSet::from([SearchDirtyPath {
                source_path: changed.path.clone(),
                changed_path: session_path.clone(),
            }])
        );
        assert!(runtime.pending_inventory_reason.is_none());
        let published_generation = cached_inventory_generation(&runtime, &changed);

        let _ = refresh_with_runtime(&data_root, &mut runtime, sources.clone());
        assert!(runtime.pending_dirty_paths.is_empty());
        assert!(runtime.inventory_progress.is_none());
        assert!(!runtime.pending_full_inventory);
        assert_eq!(
            cached_inventory_generation(&runtime, &changed),
            published_generation
        );
    }

    #[test]
    fn daemon_elapsed_safety_sweep_runs_while_recovery_is_pending() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source = seed_failed_pi_backlog(&data_root, 65);
        let mut runtime = SearchRefreshRuntime::default();
        let mut pending_without_publication = false;
        for _ in 0..32 {
            let totals = refresh_with_runtime(&data_root, &mut runtime, vec![source.clone()]);
            let publication_pending = Store::open(database_path(data_root.clone()))
                .unwrap()
                .has_pending_provider_file_publications()
                .unwrap();
            if totals.recovery_units_pending > 0 && !publication_pending {
                pending_without_publication = true;
                break;
            }
        }
        assert!(pending_without_publication);
        // A real next pass would immediately start the remaining recovery unit.
        // Align only the cached owner so this call isolates the elapsed sweep.
        runtime.cached_work.as_mut().unwrap().publication_owner = None;
        let first_generation = cached_inventory_generation(&runtime, &source);
        runtime.cached_work.as_mut().unwrap().last_reinventory_at =
            Instant::now() - DAEMON_SEARCH_REFRESH_REINVENTORY_INTERVAL;

        let _ = refresh_with_runtime(&data_root, &mut runtime, vec![source.clone()]);
        for _ in 0..64 {
            if runtime.inventory_progress.is_none() {
                break;
            }
            let _ = refresh_with_runtime(&data_root, &mut runtime, vec![source.clone()]);
        }

        assert!(runtime.inventory_progress.is_none());
        assert_ne!(
            cached_inventory_generation(&runtime, &source),
            first_generation
        );
    }

    #[test]
    fn daemon_cached_refresh_handles_deferred_recovery_across_passes() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source = seed_failed_pi_backlog(&data_root, 1);
        let mut runtime = SearchRefreshRuntime::default();
        let mut pending = usize::MAX;

        for _ in 0..64 {
            let totals = refresh_with_runtime(&data_root, &mut runtime, vec![source.clone()]);
            pending = totals.recovery_units_pending;
            let publication_pending = Store::open(database_path(data_root.clone()))
                .unwrap()
                .has_pending_provider_file_publications()
                .unwrap();
            if runtime.inventory_progress.is_none() && pending == 0 && !publication_pending {
                break;
            }
        }

        assert_eq!(pending, 0);
        assert!(runtime.inventory_progress.is_none());
        let store = Store::open(database_path(data_root.clone())).unwrap();
        assert!(!store.has_pending_provider_file_publications().unwrap());
        assert!(serde_json::to_string(&store.export_archive().unwrap())
            .unwrap()
            .contains("recovery 0"));

        let no_op = refresh_with_runtime(&data_root, &mut runtime, vec![source]);
        assert_eq!(no_op.fresh_units_processed, 0);
        assert_eq!(no_op.recovery_units_processed, 0);
        assert!(runtime.cached_work.is_some());
    }

    #[test]
    fn daemon_cached_refresh_rebuilds_for_a_new_global_publication_owner() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let owner = write_pi_source(&data_root.join("pi-explicit-owner"), 1, "owner");
        let cached = write_pi_source(&data_root.join("pi-cached"), 1, "cached");
        let imported = refresh(
            &data_root,
            vec![owner.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(imported.fresh_units_processed, 1);

        let mut runtime = SearchRefreshRuntime::default();
        for _ in 0..64 {
            refresh_with_runtime(&data_root, &mut runtime, vec![cached.clone()]);
            let publication_pending = Store::open(database_path(data_root.clone()))
                .unwrap()
                .has_pending_provider_file_publications()
                .unwrap();
            if runtime.inventory_progress.is_none()
                && runtime.cached_work.is_some()
                && runtime.pending_dirty_paths.is_empty()
                && !publication_pending
            {
                break;
            }
        }
        assert!(runtime
            .cached_work
            .as_ref()
            .is_some_and(|work| work.publication_owner.is_none()));

        let store = Store::open(database_path(data_root.clone())).unwrap();
        leave_unmutated_pi_publication(&store, &owner);
        assert!(store.has_pending_provider_file_publications().unwrap());

        let mut completed = 0usize;
        for _ in 0..32 {
            let totals = refresh_with_runtime(&data_root, &mut runtime, vec![cached.clone()]);
            completed = completed.saturating_add(totals.recovery_units_processed);
            if !Store::open(database_path(data_root.clone()))
                .unwrap()
                .has_pending_provider_file_publications()
                .unwrap()
            {
                break;
            }
        }

        assert_eq!(completed, 1);
        assert!(!Store::open(database_path(data_root))
            .unwrap()
            .has_pending_provider_file_publications()
            .unwrap());
    }

    #[test]
    fn direct_inventory_restarts_at_every_publication_transition_boundary() {
        for (point_name, point, synthetic_owner) in PUBLICATION_TRANSITION_POINTS {
            let temp = tempfile::tempdir().unwrap();
            let data_root = temp.path().join(format!("direct-{point}"));
            let owner_a = write_pi_source(&data_root.join("owner-a"), 1, "owner-a");
            let owner_b = write_pi_source(&data_root.join("owner-b"), 1, "owner-b");
            let preceding = write_pi_source(&data_root.join("preceding"), 1, "preceding");
            let codex = write_many_codex_sources(&data_root.join("codex"), 65, "codex");
            let baseline = refresh(
                &data_root,
                vec![owner_a.clone(), owner_b.clone()],
                ImportExecutionPolicy::Drain,
            );
            assert_eq!(
                baseline.fresh_units_pending, 0,
                "{point_name}: {baseline:?}"
            );

            let store = Store::open(database_path(data_root.clone())).unwrap();
            arm_inventory_publication_transition(
                &store,
                point,
                synthetic_owner,
                &owner_a,
                owner_b.clone(),
            );
            let inventory =
                inventory_import_sources(&store, vec![preceding.clone(), codex.clone()], true)
                    .unwrap();

            assert!(
                !ImportInventoryCursor::publication_transition_injection_pending(),
                "{point_name}: transition boundary was not reached"
            );
            assert!(inventory.failures.is_empty(), "{point_name}: {inventory:?}");
            let planned_paths = inventory
                .sources
                .iter()
                .map(|planned| planned.source.path.clone())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                planned_paths,
                BTreeSet::from([
                    owner_b.path.clone(),
                    preceding.path.clone(),
                    codex.path.clone()
                ]),
                "{point_name}: stale or duplicated inventory page"
            );
            assert_eq!(inventory.sources.len(), planned_paths.len(), "{point_name}");
            assert_eq!(inventory.catalog_sources.len(), 1, "{point_name}");
            assert_eq!(
                store
                    .list_active_catalog_sessions_for_source(
                        CaptureProvider::Codex,
                        codex.path.to_str().unwrap(),
                    )
                    .unwrap()
                    .len(),
                65,
                "{point_name}: partial Codex catalog visibility"
            );
            let (_, publication_owner) = store
                .effective_provider_file_publication_inventory_snapshot()
                .unwrap();
            assert_eq!(
                publication_owner.unwrap().source_root,
                owner_b.path.to_str().unwrap(),
                "{point_name}: inventory did not converge on owner B"
            );
            drop(store);

            let drained = refresh(
                &data_root,
                vec![preceding.clone(), codex.clone()],
                ImportExecutionPolicy::Drain,
            );
            assert_eq!(drained.fresh_units_pending, 0, "{point_name}: {drained:?}");
            let store = Store::open(database_path(data_root)).unwrap();
            assert!(
                !store.has_pending_provider_file_publications().unwrap(),
                "{point_name}: direct drain did not converge"
            );
            assert_eq!(
                store
                    .list_active_catalog_sessions_for_source(
                        CaptureProvider::Codex,
                        codex.path.to_str().unwrap(),
                    )
                    .unwrap()
                    .len(),
                65,
                "{point_name}: direct drain exposed a partial Codex tree"
            );
        }
    }

    #[test]
    fn daemon_inventory_restarts_at_every_publication_transition_boundary() {
        for (point_name, point, synthetic_owner) in PUBLICATION_TRANSITION_POINTS {
            let temp = tempfile::tempdir().unwrap();
            let data_root = temp.path().join(format!("daemon-{point}"));
            let owner_a = write_pi_source(&data_root.join("owner-a"), 1, "owner-a");
            let owner_b = write_pi_source(&data_root.join("owner-b"), 1, "owner-b");
            let preceding = write_pi_source(&data_root.join("preceding"), 1, "preceding");
            let codex = write_many_codex_sources(&data_root.join("codex"), 65, "codex");
            let baseline = refresh(
                &data_root,
                vec![owner_a.clone(), owner_b.clone()],
                ImportExecutionPolicy::Drain,
            );
            assert_eq!(
                baseline.fresh_units_pending, 0,
                "{point_name}: {baseline:?}"
            );

            let store = Store::open(database_path(data_root.clone())).unwrap();
            arm_inventory_publication_transition(
                &store,
                point,
                synthetic_owner,
                &owner_a,
                owner_b.clone(),
            );
            drop(store);
            let sources = vec![preceding.clone(), codex.clone()];
            let mut runtime = SearchRefreshRuntime::default();
            for _ in 0..256 {
                let _ = refresh_with_runtime(&data_root, &mut runtime, sources.clone());
                if !ImportInventoryCursor::publication_transition_injection_pending() {
                    break;
                }
            }
            assert!(
                !ImportInventoryCursor::publication_transition_injection_pending(),
                "{point_name}: daemon never reached transition boundary"
            );
            assert!(
                runtime.inventory_progress.is_none(),
                "{point_name}: daemon retained the stale cursor"
            );
            assert!(
                runtime.cached_work.is_none(),
                "{point_name}: daemon exposed a stale accumulated page"
            );

            let mut converged = false;
            for _ in 0..512 {
                let totals = refresh_with_runtime(&data_root, &mut runtime, sources.clone());
                let store = Store::open(database_path(data_root.clone())).unwrap();
                let planned_paths = runtime
                    .cached_work
                    .as_ref()
                    .map(|work| {
                        work.plan
                            .sources
                            .iter()
                            .map(|planned| planned.source.path.clone())
                            .collect::<BTreeSet<_>>()
                    })
                    .unwrap_or_default();
                if runtime.inventory_progress.is_none()
                    && runtime
                        .cached_work
                        .as_ref()
                        .is_some_and(|work| work.publication_owner.is_none())
                    && planned_paths == BTreeSet::from([preceding.path.clone(), codex.path.clone()])
                    && totals.fresh_units_pending == 0
                    && totals.recovery_units_pending == 0
                    && !store.has_pending_provider_file_publications().unwrap()
                {
                    converged = true;
                    break;
                }
            }
            assert!(converged, "{point_name}: daemon did not converge");

            let work = runtime.cached_work.as_ref().unwrap();
            let planned_paths = work
                .plan
                .sources
                .iter()
                .map(|planned| planned.source.path.clone())
                .collect::<BTreeSet<_>>();
            assert_eq!(work.plan.sources.len(), planned_paths.len(), "{point_name}");
            assert!(!planned_paths.contains(&owner_a.path), "{point_name}");
            assert!(!planned_paths.contains(&owner_b.path), "{point_name}");
            let store = Store::open(database_path(data_root)).unwrap();
            assert_eq!(
                store
                    .list_active_catalog_sessions_for_source(
                        CaptureProvider::Codex,
                        codex.path.to_str().unwrap(),
                    )
                    .unwrap()
                    .len(),
                65,
                "{point_name}: daemon exposed a partial Codex tree"
            );
        }
    }

    #[test]
    fn daemon_owner_transition_restarts_inventory_without_merging_a_stale_page() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let owner = write_pi_source(&data_root.join("pi-transition-owner"), 1, "owner");
        let first = write_pi_source(&data_root.join("pi-transition-first"), 1, "first");
        let second = write_pi_source(&data_root.join("pi-transition-second"), 1, "second");
        let baseline = refresh(
            &data_root,
            vec![owner.clone(), first.clone(), second.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(baseline.fresh_units_pending, 0, "{baseline:?}");

        let sources = vec![first.clone(), second.clone()];
        let mut runtime = SearchRefreshRuntime::default();
        for _ in 0..64 {
            let _ = refresh_with_runtime(&data_root, &mut runtime, sources.clone());
            if runtime.inventory_progress.as_ref().is_some_and(|progress| {
                progress.next_source_index == 1 && progress.total_sources == 2
            }) {
                break;
            }
        }
        let stale_marker = runtime
            .inventory_progress
            .as_ref()
            .unwrap()
            .cursor
            .publication_state_marker()
            .to_owned();
        assert!(runtime.cached_work.as_ref().is_some_and(|work| {
            work.plan
                .sources
                .iter()
                .any(|planned| planned.source.path == first.path)
        }));

        let store = Store::open(database_path(data_root.clone())).unwrap();
        leave_unmutated_pi_publication(&store, &owner);
        let transitioned_marker = store
            .effective_provider_file_publication_inventory_snapshot()
            .unwrap()
            .0;
        assert_ne!(stale_marker, transitioned_marker);
        drop(store);

        let _ = refresh_with_runtime(&data_root, &mut runtime, sources);

        let progress = runtime.inventory_progress.as_ref().unwrap();
        assert_eq!(progress.reason, SearchInventoryReason::PublicationChanged);
        assert_eq!(progress.next_source_index, 1);
        assert_eq!(
            progress.cursor.publication_state_marker(),
            transitioned_marker
        );
        let planned_paths = runtime
            .cached_work
            .as_ref()
            .unwrap()
            .plan
            .sources
            .iter()
            .map(|planned| planned.source.path.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(planned_paths, BTreeSet::from([owner.path]));
        assert!(!planned_paths.contains(&first.path));
        assert!(!planned_paths.contains(&second.path));
    }

    #[test]
    fn pending_report_includes_publication_created_after_plan_inventory() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let owner = write_pi_source(&data_root.join("pi-late-owner"), 1, "late-owner");
        let imported = refresh(
            &data_root,
            vec![owner.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(imported.fresh_units_processed, 1);

        let store = Store::open(database_path(data_root)).unwrap();
        let stale_plan = ImportPlan::build(&store, Vec::new()).unwrap();
        leave_unmutated_pi_publication(&store, &owner);

        assert_eq!(
            reported_pending_counts(&store, &stale_plan).unwrap(),
            (0, 1)
        );
    }

    #[test]
    fn synthetic_publication_owner_counts_toward_all_source_failure() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let owner = write_pi_source(&data_root.join("pi-failing-owner"), 1, "failing-owner");
        let imported = refresh(
            &data_root,
            vec![owner.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(imported.fresh_units_processed, 1);
        let store = Store::open(database_path(data_root.clone())).unwrap();
        leave_unmutated_pi_publication(&store, &owner);
        drop(store);
        fs::remove_dir_all(&owner.path).unwrap();
        fs::write(&owner.path, b"publication owner root became a file").unwrap();

        let error = refresh_sources_for_search(
            &data_root,
            Vec::new(),
            Vec::new(),
            RefreshArg::Background,
            false,
            ImportExecutionPolicy::Daemon,
        )
        .expect_err("the only synthetic source failed");

        assert!(
            format!("{error:#}").contains("all search refresh sources failed"),
            "{error:#}"
        );
    }

    #[test]
    fn healthy_no_op_source_prevents_all_sources_failed() {
        let totals = ImportTotals {
            failed_sources: 1,
            ..ImportTotals::default()
        };

        assert!(!all_refresh_sources_failed(2, &totals));
        assert!(all_refresh_sources_failed(1, &totals));
    }

    #[test]
    fn partially_imported_source_does_not_count_as_all_sources_failed() {
        let totals = ImportTotals {
            imported_sources: 1,
            failed_sources: 1,
            ..ImportTotals::default()
        };

        assert!(!all_refresh_sources_failed(1, &totals));
    }

    #[test]
    fn failed_background_refresh_requires_known_indexed_content_for_fallback() {
        assert!(has_usable_search_fallback(Some(true)));
        assert!(!has_usable_search_fallback(Some(false)));
        assert!(!has_usable_search_fallback(None));
    }

    #[test]
    fn incomplete_fresh_tail_does_not_starve_recovery_work() {
        use std::io::Write as _;

        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let incomplete_root = data_root.join("pi-incomplete");
        let incomplete = write_pi_source(&incomplete_root, 1, "still-writing");
        let baseline = refresh(
            &data_root,
            vec![incomplete.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(baseline.fresh_units_pending, 0);
        fs::OpenOptions::new()
            .append(true)
            .open(incomplete_root.join("000.jsonl"))
            .unwrap()
            .write_all(br#"{"type":"message","id":"partial""#)
            .unwrap();
        let backlog = seed_failed_pi_backlog(&data_root, 1);

        let mut recovery_completed = 0usize;
        let mut last = ImportTotals::default();
        for _ in 0..8 {
            last = refresh(
                &data_root,
                vec![backlog.clone(), incomplete.clone()],
                ImportExecutionPolicy::Interactive,
            );
            recovery_completed = recovery_completed.saturating_add(last.recovery_units_processed);
            if recovery_completed > 0 {
                break;
            }
        }

        assert_eq!(last.fresh_units_processed, 0, "{last:?}");
        assert_eq!(last.fresh_units_pending, 1, "{last:?}");
        assert_eq!(recovery_completed, 1, "{last:?}");
        assert_eq!(last.recovery_units_pending, 0, "{last:?}");
    }

    #[test]
    fn incomplete_file_does_not_block_a_later_complete_file_in_the_same_source() {
        use std::io::Write as _;

        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = data_root.join("pi-fair-source");
        let source = write_pi_source(&source_root, 1, "partial-first");
        let baseline = refresh(
            &data_root,
            vec![source.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(baseline.fresh_units_pending, 0, "{baseline:?}");
        fs::OpenOptions::new()
            .append(true)
            .open(source_root.join("000.jsonl"))
            .unwrap()
            .write_all(br#"{"type":"message","id":"partial""#)
            .unwrap();
        fs::write(
            source_root.join("001.jsonl"),
            format!(
                "{}\n{}\n",
                json!({
                    "type": "session",
                    "id": "complete-later",
                    "timestamp": "2026-07-14T12:00:00Z"
                }),
                json!({
                    "type": "message",
                    "id": "complete-later-message",
                    "timestamp": "2026-07-14T12:00:01Z",
                    "message": {"role": "user", "content": "complete later content"}
                })
            ),
        )
        .unwrap();

        let totals = refresh(&data_root, vec![source], ImportExecutionPolicy::Drain);

        assert_eq!(totals.fresh_units_processed, 1, "{totals:?}");
        assert_eq!(totals.fresh_units_pending, 1, "{totals:?}");
        let store = Store::open(database_path(data_root)).unwrap();
        assert!(serde_json::to_string(&store.export_archive().unwrap())
            .unwrap()
            .contains("complete later content"));
        assert!(!store.has_pending_provider_file_publications().unwrap());
    }

    #[test]
    fn growing_append_source_finishes_its_snapshot_then_imports_the_tail() {
        use std::io::Write as _;

        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = data_root.join("pi-growing");
        let source = write_pi_source(&source_root, 1, "growing");
        let baseline = refresh(
            &data_root,
            vec![source.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(baseline.fresh_units_pending, 0, "{baseline:?}");
        let store = Store::open(database_path(data_root.clone())).unwrap();
        leave_unmutated_pi_publication(&store, &source);
        fs::OpenOptions::new()
            .append(true)
            .open(source_root.join("000.jsonl"))
            .unwrap()
            .write_all(
                format!(
                    "{}\n",
                    json!({
                        "type": "message",
                        "id": "growing-tail-message",
                        "timestamp": "2026-07-14T12:00:02Z",
                        "message": {"role": "assistant", "content": "growth after inventory"}
                    })
                )
                .as_bytes(),
            )
            .unwrap();
        drop(store);
        let mut runtime = SearchRefreshRuntime::default();

        let mut pending = usize::MAX;
        for _ in 0..64 {
            let totals = refresh_with_runtime(&data_root, &mut runtime, vec![source.clone()]);
            pending = totals
                .fresh_units_pending
                .saturating_add(totals.recovery_units_pending);
            if pending == 0 {
                break;
            }
        }

        assert_eq!(pending, 0);
        let store = Store::open(database_path(data_root)).unwrap();
        assert!(!store.has_pending_provider_file_publications().unwrap());
        assert!(serde_json::to_string(&store.export_archive().unwrap())
            .unwrap()
            .contains("growth after inventory"));
    }

    #[test]
    fn drain_revisits_fresh_tail_created_by_recovery() {
        use std::io::Write as _;

        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = data_root.join("pi-recovery-tail");
        let source = write_pi_source(&source_root, 1, "recovery-tail");
        stage_pi_recovery_publication(&data_root, &source);

        fs::OpenOptions::new()
            .append(true)
            .open(source_root.join("000.jsonl"))
            .unwrap()
            .write_all(
                format!(
                    "{}\n",
                    json!({
                        "type": "message",
                        "id": "recovery-created-fresh-tail",
                        "timestamp": "2026-07-14T12:00:02Z",
                        "message": {"role": "assistant", "content": "fresh tail after recovery"}
                    })
                )
                .as_bytes(),
            )
            .unwrap();

        let totals = refresh(&data_root, vec![source], ImportExecutionPolicy::Drain);
        assert_eq!(totals.fresh_units_pending, 0, "{totals:?}");
        assert_eq!(totals.recovery_units_pending, 0, "{totals:?}");
        let store = Store::open(database_path(data_root)).unwrap();
        assert!(!store.has_pending_provider_file_publications().unwrap());
        assert!(serde_json::to_string(&store.export_archive().unwrap())
            .unwrap()
            .contains("fresh tail after recovery"));
    }

    #[test]
    fn drain_processes_healthy_recovery_tail_before_returning_mixed_inventory_failure() {
        use std::io::Write as _;

        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let failing = write_pi_source(&data_root.join("pi-a-failing"), 1, "failing");
        let recovery_root = data_root.join("pi-z-recovery");
        let recovery = write_pi_source(&recovery_root, 1, "mixed-recovery");
        stage_pi_recovery_publication(&data_root, &recovery);
        fs::OpenOptions::new()
            .append(true)
            .open(recovery_root.join("000.jsonl"))
            .unwrap()
            .write_all(
                format!(
                    "{}\n",
                    json!({
                        "type": "message",
                        "id": "mixed-reinventory-tail",
                        "timestamp": "2026-07-14T12:00:02Z",
                        "message": {"role": "assistant", "content": "healthy mixed tail"}
                    })
                )
                .as_bytes(),
            )
            .unwrap();
        inject_search_reinventory_hook_once(|| {
            inject_inventory_failure_once(InventoryFailurePoint::ManifestAfterObservation);
        });

        let totals = refresh(
            &data_root,
            vec![failing, recovery],
            ImportExecutionPolicy::Drain,
        );

        assert_eq!(totals.imported_sources, 2, "{totals:?}");
        assert_eq!(totals.failed_sources, 1, "{totals:?}");
        assert_eq!(totals.recovery_units_processed, 1, "{totals:?}");
        assert!(totals.fresh_units_pending > 0, "{totals:?}");
        let store = Store::open(database_path(data_root)).unwrap();
        let archive = serde_json::to_string(&store.export_archive().unwrap()).unwrap();
        assert!(archive.contains("healthy mixed tail"), "{archive}");
        assert!(!store.has_pending_provider_file_publications().unwrap());
    }

    #[test]
    fn drain_processes_healthy_recovery_tail_before_returning_blocked_owner() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let healthy = seed_failed_pi_backlog(&data_root, 1);
        let owner = write_pi_source(&data_root.join("pi-z-blocked-owner"), 1, "blocked-owner");
        let baseline = refresh(
            &data_root,
            vec![owner.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(baseline.fresh_units_pending, 0, "{baseline:?}");

        let store = Store::open(database_path(data_root.clone())).unwrap();
        let inventory =
            inventory_import_sources(&store, vec![healthy.clone(), owner.clone()], false).unwrap();
        let owner_plan = inventory
            .sources
            .iter()
            .find(|planned| planned.source.path == owner.path)
            .unwrap();
        assert_eq!(
            store
                .schedule_source_import_explicit_rescan(
                    owner.provider,
                    owner.path.to_str().unwrap(),
                    owner_plan.preinventory.inventory_generation().unwrap(),
                )
                .unwrap(),
            1
        );
        let plan = ImportPlan::build(&store, inventory.sources).unwrap();
        assert_eq!(plan.recovery_units, 2);
        drop(store);

        let mut totals = refresh(&data_root, vec![healthy], ImportExecutionPolicy::Drain);
        assert_eq!(totals.recovery_units_processed, 1, "{totals:?}");

        let mut store = Store::open(database_path(data_root.clone())).unwrap();
        let owner_store = Store::open(database_path(data_root.clone())).unwrap();
        let owner_scope = begin_unmutated_pi_publication(&owner_store, &owner);
        let progress = ProgressReporter::new(ProgressArg::None, false, "search-refresh", 0);
        let mut first_refresh_failure = None;
        let mut imported_sources = BTreeSet::new();
        let mut failed_sources = BTreeSet::new();
        let mut inventory_failures = ImportInventoryFailures::new(Vec::new());
        let mut failed_inventory_pending = (0, 0);
        reset_search_reinventory_count();
        let (final_plan, maintenance_complete, final_checkpoint_allowed) =
            drain_search_refresh_to_fixed_point(
                &mut store,
                &plan,
                &progress,
                false,
                true,
                &mut totals,
                &mut first_refresh_failure,
                &mut imported_sources,
                &mut failed_sources,
                &mut inventory_failures,
                &mut failed_inventory_pending,
                true,
                false,
            )
            .unwrap();

        assert_eq!(totals.recovery_units_processed, 1, "{totals:?}");
        assert!(totals.failed_sources > 0, "{totals:?}");
        assert!(maintenance_complete);
        assert!(!final_checkpoint_allowed);
        assert!(
            final_plan
                .pending_count(&store, ImportWorkClass::Recovery)
                .unwrap()
                > 0
        );
        assert_eq!(search_reinventory_count(), 1, "{totals:?}");
        assert_eq!(
            failed_sources,
            BTreeSet::from([crate::commands::import::ImportSourceIdentity::new(&owner)])
        );
        assert!(store.has_pending_provider_file_publications().unwrap());
        let archive = serde_json::to_string(&store.export_archive().unwrap()).unwrap();
        assert!(archive.contains("recovery 0"), "{archive}");

        let aborted = owner_store
            .abort_provider_file_publication(owner_scope)
            .unwrap();
        assert!(matches!(aborted, std::ops::ControlFlow::Continue(_)));
        assert!(drain_import_maintenance(&store).unwrap().is_complete());
    }

    #[test]
    fn native_reports_keep_source_identity_when_publication_owner_disappears() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let owner = write_pi_source(&data_root.join("pi-owner-disappears"), 1, "owner");
        let second = write_pi_source(&data_root.join("pi-second-source"), 1, "second");
        let baseline = refresh(
            &data_root,
            vec![owner.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(baseline.fresh_units_pending, 0, "{baseline:?}");
        let store = Store::open(database_path(data_root)).unwrap();
        leave_unmutated_pi_publication(&store, &owner);

        let mut reports = NativeSourceReports::default();
        let mut imported = ProviderImportSummary::default();
        imported.imported_sessions = 1;
        imported.imported_events = 1;
        reports.record_outcome(
            &owner,
            &imported,
            SourceStats {
                files: 1,
                bytes: 10,
                change_token: None,
            },
            None,
        );
        reports.record_outcome(
            &second,
            &imported,
            SourceStats {
                files: 1,
                bytes: 20,
                change_token: None,
            },
            None,
        );

        fs::remove_dir_all(&owner.path).unwrap();
        let inventory =
            inventory_import_sources(&store, vec![owner.clone(), second.clone()], false).unwrap();
        assert_eq!(inventory.failures.len(), 1);
        assert_eq!(inventory.failures[0].source.path, owner.path);
        reports.record_inventory_failure(&inventory.failures[0]);
        let replacement_plan = ImportPlan::build(&store, inventory.sources).unwrap();

        let mut totals = ImportTotals::default();
        reports.apply_totals(&mut totals);
        assert_eq!(totals.imported_sources, 2, "{totals:?}");
        assert_eq!(totals.failed_sources, 1, "{totals:?}");
        assert_eq!(totals.imported_sessions, 2, "{totals:?}");
        let mut sources = Vec::new();
        reports.append_json(&replacement_plan, &mut sources);
        assert_eq!(sources.len(), 2);
        let owner_json = sources
            .iter()
            .find(|source| source["path"] == json!(owner.path))
            .unwrap();
        assert_eq!(owner_json["status"], "completed_with_source_failure");
        assert!(sources
            .iter()
            .any(|source| source["path"] == json!(second.path)));
    }

    #[test]
    fn inventory_failures_accumulate_by_identity_disjoint_from_successful_plan() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let successful = write_pi_source(&data_root.join("pi-success"), 1, "success");
        let retained = write_pi_source(&data_root.join("pi-retained"), 1, "retained");
        let added = write_pi_source(&data_root.join("pi-added"), 1, "added");
        let store = Store::open(database_path(data_root)).unwrap();
        let inventory = inventory_import_sources(&store, vec![successful.clone()], false).unwrap();
        let plan = ImportPlan::build(&store, inventory.sources).unwrap();
        let failure = |source: SourceInfo| ImportSourceFailure {
            source,
            stats: SourceStats::default(),
            error: "injected inventory failure".to_owned(),
            failure_type: ImportFailureType::System,
            rejected_summary: None,
        };
        let mut failures =
            ImportInventoryFailures::new(vec![failure(successful), failure(retained.clone())]);

        let _ = failures.reconcile(&plan.sources, vec![failure(added.clone())]);
        let paths = failures
            .values()
            .map(|failure| failure.source.path.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(paths, BTreeSet::from([retained.path, added.path]));
        assert_eq!(failures.pending_counts(&store, &plan).unwrap(), (2, 0));
    }

    #[test]
    fn rewritten_growing_source_invalidates_staged_snapshot_and_converges() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = data_root.join("pi-rewritten-growth");
        let source = write_pi_source(&source_root, 1, "stale-snapshot");
        let baseline = refresh(
            &data_root,
            vec![source.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(baseline.fresh_units_pending, 0, "{baseline:?}");
        use std::io::Write as _;
        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .open(source_root.join("000.jsonl"))
                .unwrap(),
            "{}",
            json!({
                "type": "message",
                "id": "stale-snapshot-staged",
                "timestamp": "2026-07-14T12:00:02Z",
                "message": {"role": "assistant", "content": "staged replacement seed"}
            })
        )
        .unwrap();
        let mut runtime = SearchRefreshRuntime::default();

        let first = drive_daemon_until_staged_publication(&data_root, &mut runtime, &source);
        assert!(
            first.fresh_units_pending + first.recovery_units_pending > 0,
            "{first:?}"
        );
        let store = Store::open(database_path(data_root.clone())).unwrap();
        assert!(store
            .effective_provider_file_publication_has_staged_completion()
            .unwrap());
        drop(store);

        fs::write(
            source_root.join("000.jsonl"),
            format!(
                "{}\n{}\n{}\n",
                json!({
                    "type": "session",
                    "id": "replacement-session-with-longer-identity",
                    "timestamp": "2026-07-14T12:00:00Z"
                }),
                json!({
                    "type": "message",
                    "id": "replacement-user-message",
                    "timestamp": "2026-07-14T12:00:01Z",
                    "message": {"role": "user", "content": "replacement growth oracle"}
                }),
                json!({
                    "type": "message",
                    "id": "replacement-assistant-message",
                    "timestamp": "2026-07-14T12:00:02Z",
                    "message": {"role": "assistant", "content": "replacement tail is fully indexed"}
                })
            ),
        )
        .unwrap();

        let totals = refresh(&data_root, vec![source], ImportExecutionPolicy::Drain);
        assert_eq!(totals.fresh_units_pending, 0, "{totals:?}");
        assert_eq!(totals.recovery_units_pending, 0, "{totals:?}");
        let store = Store::open(database_path(data_root)).unwrap();
        assert!(!store.has_pending_provider_file_publications().unwrap());
        let archive = serde_json::to_string(&store.export_archive().unwrap()).unwrap();
        assert!(archive.contains("replacement growth oracle"), "{archive}");
        assert!(
            archive.contains("replacement tail is fully indexed"),
            "{archive}"
        );
        assert!(!archive.contains("stale-snapshot 0"), "{archive}");
    }

    #[test]
    fn same_size_rewrite_with_preserved_mtime_invalidates_staged_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = data_root.join("pi-rewritten-same-size");
        let source = write_pi_source(&source_root, 1, "stale-equal");
        let baseline = refresh(
            &data_root,
            vec![source.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(baseline.fresh_units_pending, 0, "{baseline:?}");
        use std::io::Write as _;
        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .open(source_root.join("000.jsonl"))
                .unwrap(),
            "{}",
            json!({
                "type": "message",
                "id": "stale-equal-staged",
                "timestamp": "2026-07-14T12:00:02Z",
                "message": {"role": "assistant", "content": "stale-equal staged"}
            })
        )
        .unwrap();
        let mut runtime = SearchRefreshRuntime::default();

        let first = drive_daemon_until_staged_publication(&data_root, &mut runtime, &source);
        assert!(
            first.fresh_units_pending + first.recovery_units_pending > 0,
            "{first:?}"
        );
        let store = Store::open(database_path(data_root.clone())).unwrap();
        assert!(store
            .effective_provider_file_publication_has_staged_completion()
            .unwrap());
        drop(store);

        let path = source_root.join("000.jsonl");
        let original_modified = fs::metadata(&path).unwrap().modified().unwrap();
        let original = fs::read_to_string(&path).unwrap();
        let replacement = original.replace("stale-equal 0", "fresh-equal 0");
        assert_ne!(replacement, original);
        assert_eq!(replacement.len(), original.len());
        fs::write(&path, replacement).unwrap();
        fs::File::open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(original_modified))
            .unwrap();

        let totals = refresh(&data_root, vec![source], ImportExecutionPolicy::Drain);
        assert_eq!(totals.fresh_units_pending, 0, "{totals:?}");
        assert_eq!(totals.recovery_units_pending, 0, "{totals:?}");
        let store = Store::open(database_path(data_root)).unwrap();
        assert!(!store.has_pending_provider_file_publications().unwrap());
        let archive = serde_json::to_string(&store.export_archive().unwrap()).unwrap();
        assert!(archive.contains("fresh-equal 0"), "{archive}");
        assert!(!archive.contains("stale-equal 0"), "{archive}");
    }

    #[test]
    fn search_refresh_resumes_same_generation_publication_that_wins_the_bulk_lock() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        fs::create_dir_all(&data_root).unwrap();
        let db_path = database_path(data_root.clone());
        let source = write_pi_source(&data_root.join("pi-race-resume"), 1, "bulk-resume");
        let baseline = refresh(
            &data_root,
            vec![source.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(baseline.fresh_units_pending, 0, "{baseline:?}");
        let mut lock_store = Store::open(&db_path).unwrap();
        let inventory = inventory_import_sources(&lock_store, vec![source.clone()], false).unwrap();
        let inventory_generation = inventory.sources[0]
            .preinventory
            .inventory_generation()
            .unwrap();
        assert_eq!(
            lock_store
                .schedule_source_import_explicit_rescan(
                    source.provider,
                    source.path.to_str().unwrap(),
                    inventory_generation,
                )
                .unwrap(),
            1
        );
        let waiting_plan = ImportPlan::build(&lock_store, inventory.sources.clone()).unwrap();
        let winning_plan = ImportPlan::build(&lock_store, inventory.sources).unwrap();
        assert_eq!(waiting_plan.recovery_units, 1);

        let guard = lock_store.begin_event_search_bulk_mode().unwrap();
        let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
        let waiting_db_path = db_path.clone();
        let waiter = std::thread::spawn(move || {
            let mut waiting_store = Store::open(waiting_db_path).unwrap();
            let progress = ProgressReporter::new(ProgressArg::None, false, "search-refresh", 0);
            let mut totals = ImportTotals::default();
            let mut first_refresh_failure = None;
            let mut imported_sources = BTreeSet::new();
            let mut failed_sources = BTreeSet::new();
            let mut execution_state =
                crate::commands::import::ImportExecutionState::for_plan(&waiting_plan);
            let result = execute_search_refresh_plan_class_with_pre_lock_hook(
                &mut waiting_store,
                &waiting_plan,
                &mut execution_state,
                ImportWorkClass::Recovery,
                waiting_plan.recovery_units,
                None,
                &progress,
                false,
                false,
                &mut totals,
                &mut first_refresh_failure,
                &mut imported_sources,
                &mut failed_sources,
                false,
                || waiting_tx.send(()).unwrap(),
            );
            (result, totals, first_refresh_failure, failed_sources)
        });
        waiting_rx.recv().unwrap();

        let winning_slice = winning_plan
            .select_slice(
                &lock_store,
                ImportWorkClass::Recovery,
                winning_plan.recovery_units,
            )
            .unwrap();
        let selected = &winning_slice.sources[0];
        let source_plan = &winning_plan.sources[selected.source_index];
        let winner = import_selected_source(
            &mut lock_store,
            &source_plan.source,
            None,
            &selected.preinventory,
            &selected.work,
        )
        .unwrap();
        assert_eq!(winner.completed_units, 0);
        assert_eq!(winner.deferred_units, 1);
        assert!(winner.durable_progress);
        lock_store.finish_event_search_bulk_mode(&guard).unwrap();
        drop(guard);

        let (result, totals, first_failure, failed_sources) = waiter.join().unwrap();
        let result = result.unwrap();
        assert_eq!(result.selected_units, 1);
        assert!(result.made_durable_progress());
        assert_eq!(totals.recovery_units_processed, result.completed_units);
        assert!(first_failure.is_none());
        assert!(failed_sources.is_empty());

        let completed = refresh(&data_root, vec![source], ImportExecutionPolicy::Drain);
        assert_eq!(completed.fresh_units_pending, 0, "{completed:?}");
        assert_eq!(completed.recovery_units_pending, 0, "{completed:?}");
        assert!(!lock_store.has_pending_provider_file_publications().unwrap());
        assert!(lock_store
            .effective_provider_file_publication_inventory_owner()
            .unwrap()
            .is_none());
        assert_eq!(
            lock_store
                .search_event_hits("bulk-resume 0", 10)
                .unwrap()
                .len(),
            1
        );
        let archive = serde_json::to_string(&lock_store.export_archive().unwrap()).unwrap();
        assert!(archive.contains("bulk-resume 0"), "{archive}");
    }

    #[test]
    fn search_refresh_drops_superseded_completion_that_wins_the_bulk_lock() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        fs::create_dir_all(&data_root).unwrap();
        let db_path = database_path(data_root.clone());
        let source = write_pi_source(&data_root.join("pi-race"), 1, "bulk-winner");
        let mut lock_store = Store::open(&db_path).unwrap();
        let inventory = inventory_import_sources(&lock_store, vec![source.clone()], false).unwrap();
        let waiting_plan = ImportPlan::build(&lock_store, inventory.sources).unwrap();
        assert_eq!(waiting_plan.fresh_units, 1);

        let guard = lock_store.begin_event_search_bulk_mode().unwrap();
        let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
        let waiting_db_path = db_path.clone();
        let waiter = std::thread::spawn(move || {
            let mut waiting_store = Store::open(waiting_db_path).unwrap();
            let progress = ProgressReporter::new(ProgressArg::None, false, "search-refresh", 0);
            let mut totals = ImportTotals::default();
            let mut first_refresh_failure = None;
            let mut imported_sources = BTreeSet::new();
            let mut failed_sources = BTreeSet::new();
            let mut execution_state =
                crate::commands::import::ImportExecutionState::for_plan(&waiting_plan);
            let result = execute_search_refresh_plan_class_with_pre_lock_hook(
                &mut waiting_store,
                &waiting_plan,
                &mut execution_state,
                ImportWorkClass::Fresh,
                waiting_plan.fresh_units,
                None,
                &progress,
                false,
                false,
                &mut totals,
                &mut first_refresh_failure,
                &mut imported_sources,
                &mut failed_sources,
                false,
                || waiting_tx.send(()).unwrap(),
            );
            (
                result,
                totals,
                first_refresh_failure,
                imported_sources,
                failed_sources,
            )
        });
        waiting_rx.recv().unwrap();

        let completion_inventory =
            inventory_import_sources(&lock_store, vec![source], false).unwrap();
        let completion_plan = ImportPlan::build(&lock_store, completion_inventory.sources).unwrap();
        let completion_slice = completion_plan
            .select_slice(
                &lock_store,
                ImportWorkClass::Fresh,
                completion_plan.fresh_units,
            )
            .unwrap();
        let selected = &completion_slice.sources[0];
        let source_plan = &completion_plan.sources[selected.source_index];
        let mut completion_finished = false;
        for _ in 0..64 {
            let completion = import_selected_source(
                &mut lock_store,
                &source_plan.source,
                None,
                &selected.preinventory,
                &selected.work,
            )
            .unwrap();
            if completion.outcome.completed_units == 1 {
                completion_finished = true;
                break;
            }
            assert_eq!(completion.outcome.deferred_units, 1);
            assert!(completion.outcome.made_durable_progress());
        }
        assert!(completion_finished, "winning import did not converge");
        lock_store.finish_event_search_bulk_mode(&guard).unwrap();
        drop(guard);

        let (result, totals, first_failure, imported_sources, failed_sources) =
            waiter.join().unwrap();
        result.unwrap();
        assert_eq!(totals.fresh_units_processed, 0);
        assert_eq!(totals.failed_sources, 0);
        assert!(first_failure.is_none());
        assert!(imported_sources.is_empty());
        assert!(failed_sources.is_empty());
    }

    #[test]
    fn background_refresh_isolates_source_removed_before_locked_revalidation() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        fs::create_dir_all(&data_root).unwrap();
        let source = write_pi_source(&data_root.join("pi-removed"), 1, "removed");
        let source_root = source.path.clone();
        let mut store = Store::open(database_path(data_root)).unwrap();
        let inventory = inventory_import_sources(&store, vec![source.clone()], false).unwrap();
        let plan = ImportPlan::build(&store, inventory.sources).unwrap();
        assert_eq!(plan.fresh_units, 1);

        let progress = ProgressReporter::new(ProgressArg::None, false, "search-refresh", 0);
        let mut totals = ImportTotals::default();
        let mut first_refresh_failure = None;
        let mut imported_sources = BTreeSet::new();
        let mut failed_sources = BTreeSet::new();
        let mut execution_state = crate::commands::import::ImportExecutionState::for_plan(&plan);
        let result = execute_search_refresh_plan_class_with_pre_lock_hook(
            &mut store,
            &plan,
            &mut execution_state,
            ImportWorkClass::Fresh,
            plan.fresh_units,
            None,
            &progress,
            false,
            true,
            &mut totals,
            &mut first_refresh_failure,
            &mut imported_sources,
            &mut failed_sources,
            false,
            || fs::remove_dir_all(&source_root).unwrap(),
        )
        .unwrap();

        assert_eq!(result.selected_units, 1);
        assert_eq!(result.completed_units, 0);
        assert_eq!(totals.failed_sources, 1);
        assert!(first_refresh_failure.is_some());
        assert!(imported_sources.is_empty());
        assert_eq!(
            failed_sources,
            BTreeSet::from([crate::commands::import::ImportSourceIdentity::new(&source)])
        );
        let guard = store.begin_event_search_bulk_mode().unwrap();
        store.finish_event_search_bulk_mode(&guard).unwrap();
    }

    #[test]
    fn repeated_foreground_refreshes_prioritize_fresh_work_and_converge() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let backlog = seed_failed_pi_backlog(&data_root, 3);
        let fresh = write_pi_source(&data_root.join("pi-fresh"), 1, "fresh-first");

        let first = refresh(
            &data_root,
            vec![backlog.clone(), fresh.clone()],
            ImportExecutionPolicy::Interactive,
        );
        assert_eq!(first.recovery_units_processed, 0);
        assert_eq!(first.recovery_units_pending, 3);

        let mut fresh_completed = first.fresh_units_processed;
        let mut recovery_completed = first.recovery_units_processed;
        let mut pending = first
            .fresh_units_pending
            .saturating_add(first.recovery_units_pending);
        for _ in 0..32 {
            if pending == 0 {
                break;
            }
            let outcome = refresh(
                &data_root,
                vec![backlog.clone(), fresh.clone()],
                ImportExecutionPolicy::Interactive,
            );
            fresh_completed = fresh_completed.saturating_add(outcome.fresh_units_processed);
            recovery_completed =
                recovery_completed.saturating_add(outcome.recovery_units_processed);
            pending = outcome
                .fresh_units_pending
                .saturating_add(outcome.recovery_units_pending);
        }
        assert_eq!(fresh_completed.saturating_add(recovery_completed), 4);
        assert_eq!(pending, 0);
    }

    #[test]
    fn interactive_background_refresh_leaves_a_large_fresh_backlog_bounded() {
        use std::io::Write as _;

        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = data_root.join("pi-fresh");
        let source = write_pi_source(&source_root, 130, "bounded-background");
        let baseline = refresh(
            &data_root,
            vec![source.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(baseline.fresh_units_pending, 0, "{baseline:?}");
        for index in 0..130 {
            writeln!(
                fs::OpenOptions::new()
                    .append(true)
                    .open(source_root.join(format!("{index:03}.jsonl")))
                    .unwrap(),
                "{}",
                json!({
                    "type": "message",
                    "id": format!("bounded-background-tail-{index}"),
                    "timestamp": "2026-07-14T12:00:02Z",
                    "message": {"role": "assistant", "content": "bounded changed work"}
                })
            )
            .unwrap();
        }

        let first = refresh(&data_root, vec![source], ImportExecutionPolicy::Interactive);

        assert!(first.fresh_units_pending > 0, "{first:?}");
    }

    #[test]
    fn daemon_bounds_each_pass_and_eventually_converges() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let backlog = seed_failed_pi_backlog(&data_root, 3);

        let first = refresh(
            &data_root,
            vec![backlog.clone()],
            ImportExecutionPolicy::Daemon,
        );
        assert_eq!(first.recovery_units_processed, 0);
        assert_eq!(first.recovery_units_pending, 3);

        let fresh = write_pi_source(&data_root.join("pi-daemon-fresh"), 1, "daemon-fresh");
        let mut fresh_completed = 0usize;
        let mut recovery_completed = first.recovery_units_processed;
        let mut pending = usize::MAX;
        for _ in 0..32 {
            let outcome = refresh(
                &data_root,
                vec![backlog.clone(), fresh.clone()],
                ImportExecutionPolicy::Daemon,
            );
            assert!(
                outcome
                    .fresh_units_processed
                    .saturating_add(outcome.recovery_units_processed)
                    <= 2,
                "one daemon pass must stay within one fresh and one recovery slice"
            );
            fresh_completed = fresh_completed.saturating_add(outcome.fresh_units_processed);
            recovery_completed =
                recovery_completed.saturating_add(outcome.recovery_units_processed);
            pending = outcome
                .fresh_units_pending
                .saturating_add(outcome.recovery_units_pending);
            if pending == 0 {
                break;
            }
        }
        assert_eq!(fresh_completed.saturating_add(recovery_completed), 4);
        assert_eq!(pending, 0);
    }

    #[test]
    fn drain_refresh_serializes_bounded_publications_across_sources() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let first = write_pi_source(&data_root.join("pi-drain-a"), 1, "drain-a");
        let second = write_pi_source(&data_root.join("pi-drain-b"), 1, "drain-b");

        let totals = refresh(
            &data_root,
            vec![first, second],
            ImportExecutionPolicy::Drain,
        );

        assert_eq!(totals.fresh_units_processed, 2);
        assert_eq!(totals.fresh_units_pending, 0);
        let store = Store::open(database_path(data_root)).unwrap();
        assert!(!store.has_pending_provider_file_publications().unwrap());
    }

    #[test]
    fn drain_refresh_converges_multiple_files_within_one_source() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source = write_pi_source(&data_root.join("pi-drain-one-root"), 3, "drain-one-root");

        let totals = refresh(&data_root, vec![source], ImportExecutionPolicy::Drain);

        assert_eq!(totals.fresh_units_processed, 3, "{totals:?}");
        assert_eq!(totals.fresh_units_pending, 0, "{totals:?}");
        assert_eq!(totals.recovery_units_pending, 0, "{totals:?}");
        let store = Store::open(database_path(data_root)).unwrap();
        assert!(!store.has_pending_provider_file_publications().unwrap());
    }

    #[test]
    fn setup_operation_drains_all_recovery_work() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let backlog = seed_failed_pi_backlog(&data_root, 3);
        let args = ImportArgs {
            provider: Some(NativeProviderArg::Pi),
            path: Some(backlog.path),
            history_source: None,
            history_source_manifest: Vec::new(),
            reset_cursor: false,
            format: None,
            all: false,
            resume: false,
            no_daemon: true,
            json: false,
            progress: ProgressArg::None,
        };
        let report = run_import_internal(
            &args,
            data_root.clone(),
            &mut serde_json::Map::new(),
            ImportRunOptions {
                progress: ProgressArg::None,
                json: false,
                print_human: false,
                allow_empty_sources: false,
                include_history_source_plugins: false,
                operation: "setup",
            },
        )
        .unwrap();
        assert!(report.totals.durable_progress, "{report:?}");
        assert_eq!(report.totals.recovery_units_pending, 0, "{report:?}");
        let store = Store::open(database_path(data_root)).unwrap();
        assert!(!store.has_pending_provider_file_publications().unwrap());
        let archive = serde_json::to_string(&store.export_archive().unwrap()).unwrap();
        for index in 0..3 {
            assert!(archive.contains(&format!("recovery {index}")), "{archive}");
        }
    }

    #[test]
    fn setup_revisits_fresh_tail_created_by_recovery() {
        use std::io::Write as _;

        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = data_root.join("pi-setup-recovery-tail");
        let source = write_pi_source(&source_root, 1, "setup-recovery-tail");
        stage_pi_recovery_publication(&data_root, &source);
        fs::OpenOptions::new()
            .append(true)
            .open(source_root.join("000.jsonl"))
            .unwrap()
            .write_all(
                format!(
                    "{}\n",
                    json!({
                        "type": "message",
                        "id": "setup-recovery-created-fresh-tail",
                        "timestamp": "2026-07-14T12:00:02Z",
                        "message": {"role": "assistant", "content": "setup fresh tail after recovery"}
                    })
                )
                .as_bytes(),
            )
            .unwrap();

        let args = ImportArgs {
            provider: Some(NativeProviderArg::Pi),
            path: Some(source.path),
            history_source: None,
            history_source_manifest: Vec::new(),
            reset_cursor: false,
            format: None,
            all: false,
            resume: false,
            no_daemon: true,
            json: false,
            progress: ProgressArg::None,
        };
        let report = run_import_internal(
            &args,
            data_root.clone(),
            &mut serde_json::Map::new(),
            ImportRunOptions {
                progress: ProgressArg::None,
                json: false,
                print_human: false,
                allow_empty_sources: false,
                include_history_source_plugins: false,
                operation: "setup",
            },
        )
        .unwrap();
        assert_eq!(report.totals.fresh_units_pending, 0, "{report:?}");
        assert_eq!(report.totals.recovery_units_pending, 0, "{report:?}");
        let store = Store::open(database_path(data_root)).unwrap();
        assert!(serde_json::to_string(&store.export_archive().unwrap())
            .unwrap()
            .contains("setup fresh tail after recovery"));
    }

    #[test]
    fn pinned_wal_reader_blocks_maintenance_without_admitting_import_work() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source = write_pi_source(&data_root.join("pi-pinned-reader"), 1, "pinned");
        fs::create_dir_all(&data_root).unwrap();
        let db_path = database_path(data_root.clone());
        let store = Store::open(&db_path).unwrap();
        let writer = rusqlite::Connection::open(&db_path).unwrap();
        writer
            .execute_batch(
                "CREATE TABLE maintenance_checkpoint_probe(value INTEGER); \
                 INSERT INTO maintenance_checkpoint_probe VALUES (1);",
            )
            .unwrap();
        let guard = store.begin_event_search_bulk_mode().unwrap();
        let reader = rusqlite::Connection::open(&db_path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let observed = reader
            .query_row(
                "SELECT COUNT(*) FROM maintenance_checkpoint_probe",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!(observed, 1);
        writer
            .execute("INSERT INTO maintenance_checkpoint_probe VALUES (2)", [])
            .unwrap();
        drop(guard);
        drop(writer);

        let reason = (0..8)
            .find_map(|_| match repair_import_maintenance(&store).unwrap() {
                ImportMaintenanceStep::Complete => {
                    panic!("pinned checkpoint unexpectedly completed")
                }
                ImportMaintenanceStep::Progress => None,
                ImportMaintenanceStep::Pending(reason) => Some(reason),
            })
            .expect("bounded maintenance must report the pinned reader");
        assert!(matches!(
            reason,
            ImportMaintenancePendingReason::WalCheckpoint { .. }
        ));
        assert!(reason.diagnostic().contains("WAL reader"));
        assert_eq!(
            store.event_search_bulk_maintenance_outcome().unwrap(),
            ctx_history_store::EventSearchBulkMaintenanceOutcome::Pending
        );
        drop(store);

        let blocked = refresh(
            &data_root,
            vec![source.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(blocked.fresh_units_processed, 0, "{blocked:?}");
        assert_eq!(blocked.recovery_units_processed, 0, "{blocked:?}");
        assert!(blocked.fresh_units_pending > 0, "{blocked:?}");
        assert!(blocked.recovery_units_pending > 0, "{blocked:?}");
        let store = Store::open(&db_path).unwrap();
        assert!(store.export_archive().unwrap().events.is_empty());
        assert_eq!(
            store.event_search_bulk_maintenance_outcome().unwrap(),
            ctx_history_store::EventSearchBulkMaintenanceOutcome::Pending
        );
        drop(store);

        reader.execute_batch("ROLLBACK").unwrap();
        drop(reader);
        let converged = refresh(&data_root, vec![source], ImportExecutionPolicy::Drain);
        assert_eq!(converged.fresh_units_pending, 0, "{converged:?}");
        assert_eq!(converged.recovery_units_pending, 0, "{converged:?}");
        let store = Store::open(db_path).unwrap();
        assert!(serde_json::to_string(&store.export_archive().unwrap())
            .unwrap()
            .contains("pinned"));
        assert!(store
            .event_search_bulk_maintenance_outcome()
            .unwrap()
            .is_complete());
    }

    #[test]
    fn drain_fixed_point_rejects_pending_work_without_progress_or_blocker() {
        let error =
            crate::commands::import::drain_fixed_point_action(true, false, None).unwrap_err();
        assert!(error.chain().any(|cause| matches!(
            cause.downcast_ref::<ctx_history_capture::CaptureError>(),
            Some(ctx_history_capture::CaptureError::SystemInvariant(
                "drain import work remained pending without durable progress"
            ))
        )));
        assert_eq!(
            crate::commands::import::drain_fixed_point_action(
                true,
                false,
                Some(crate::commands::import::DrainFixedPointBlocker::RetryableExternal),
            )
            .unwrap(),
            crate::commands::import::DrainFixedPointAction::RetryableBlocked(
                crate::commands::import::DrainFixedPointBlocker::RetryableExternal
            )
        );
        assert_eq!(
            crate::commands::import::drain_fixed_point_action(false, true, None).unwrap(),
            crate::commands::import::DrainFixedPointAction::Reinventory
        );
        assert_eq!(
            crate::commands::import::drain_fixed_point_action(
                true,
                true,
                Some(crate::commands::import::DrainFixedPointBlocker::RetryableExternal),
            )
            .unwrap(),
            crate::commands::import::DrainFixedPointAction::Reinventory
        );
        assert_eq!(
            crate::commands::import::drain_fixed_point_action(
                true,
                true,
                Some(
                    crate::commands::import::DrainFixedPointBlocker::Maintenance(
                        ImportMaintenancePendingReason::MaintenanceWriter,
                    )
                ),
            )
            .unwrap(),
            crate::commands::import::DrainFixedPointAction::RetryableBlocked(
                crate::commands::import::DrainFixedPointBlocker::Maintenance(
                    ImportMaintenancePendingReason::MaintenanceWriter,
                )
            )
        );
    }

    #[test]
    fn drain_finishes_multi_page_maintenance_without_reinventorying_a_large_tree() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source = write_many_codex_sources(&data_root.join("codex-large"), 65, "maintenance");
        let baseline = refresh(
            &data_root,
            vec![source.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(baseline.fresh_units_pending, 0, "{baseline:?}");
        assert_eq!(baseline.recovery_units_pending, 0, "{baseline:?}");

        reset_search_reinventory_count();
        inject_import_maintenance_progress_steps(3);
        let totals = refresh(&data_root, vec![source], ImportExecutionPolicy::Drain);

        assert!(totals.durable_progress, "{totals:?}");
        assert_eq!(totals.fresh_units_processed, 0, "{totals:?}");
        assert_eq!(totals.recovery_units_processed, 0, "{totals:?}");
        assert_eq!(totals.fresh_units_pending, 0, "{totals:?}");
        assert_eq!(totals.recovery_units_pending, 0, "{totals:?}");
        assert_eq!(
            search_reinventory_count(),
            0,
            "maintenance-only progress restarted inventory of the 65-session tree"
        );
    }
}
