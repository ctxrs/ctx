#[cfg(test)]
mod freshness_tests {
    use super::*;
    use crate::commands::import::{run_import_internal, ImportRunOptions};
    use crate::provider_args::NativeProviderArg;
    use crate::provider_sources::explicit_path_source;
    use crate::ImportArgs;
    use ctx_history_core::{canonical_provider_material_source_format, utc_now, CaptureProvider};
    use ctx_history_store::{
        CatalogIndexedStatus, ProviderFileInventoryObservation, ProviderFilePublicationKind,
        SourceImportFileIndexUpdate,
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

    fn seed_failed_pi_backlog(data_root: &Path, count: usize) -> SourceInfo {
        fs::create_dir_all(data_root).unwrap();
        let source = write_pi_source(&data_root.join("pi-backlog"), count, "recovery");
        let store = Store::open(database_path(data_root.to_path_buf())).unwrap();
        let inventory = inventory_import_sources(&store, vec![source.clone()], false).unwrap();
        let (files, inventory_generation) = match &inventory.sources[0].preinventory {
            crate::commands::import::SourcePreinventory::SourceImportFiles {
                files,
                inventory_generation,
            } => (files, *inventory_generation),
            other => panic!("unexpected Pi inventory: {other:?}"),
        };
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

    fn leave_unmutated_pi_publication(store: &Store, source: &SourceInfo) {
        let inventory = inventory_import_sources(store, vec![source.clone()], false).unwrap();
        let inventory_generation = match &inventory.sources[0].preinventory {
            crate::commands::import::SourcePreinventory::SourceImportFiles {
                inventory_generation,
                ..
            } => *inventory_generation,
            other => panic!("unexpected Pi inventory: {other:?}"),
        };
        let source_root = source.path.to_str().unwrap();
        store
            .schedule_source_import_explicit_rescan(
                source.provider,
                source_root,
                inventory_generation,
            )
            .unwrap();
        let file = store
            .list_pending_source_import_files(source.provider, source_root)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let scope = store
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
            .unwrap();
        drop(scope);
    }

    fn stage_pi_recovery_publication(data_root: &Path, source: &SourceInfo) {
        let baseline = refresh(
            data_root,
            vec![source.clone()],
            ImportExecutionPolicy::Drain,
        );
        assert_eq!(baseline.fresh_units_pending, 0, "{baseline:?}");

        let store = Store::open(database_path(data_root.to_path_buf())).unwrap();
        let inventory = inventory_import_sources(&store, vec![source.clone()], false).unwrap();
        let inventory_generation = match &inventory.sources[0].preinventory {
            crate::commands::import::SourcePreinventory::SourceImportFiles {
                inventory_generation,
                ..
            } => *inventory_generation,
            other => panic!("unexpected Pi inventory: {other:?}"),
        };
        store
            .schedule_source_import_explicit_rescan(
                source.provider,
                source.path.to_str().unwrap(),
                inventory_generation,
            )
            .unwrap();
        drop(store);

        let mut runtime = SearchRefreshRuntime::default();
        let staged = refresh_with_runtime(data_root, &mut runtime, vec![source.clone()]);
        assert!(staged.recovery_units_pending > 0, "{staged:?}");
        let store = Store::open(database_path(data_root.to_path_buf())).unwrap();
        assert!(store
            .effective_provider_file_publication_has_staged_completion()
            .unwrap());
    }

    #[test]
    fn daemon_cached_refresh_watcher_generation_tracks_source_changes() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source = write_pi_source(&data_root.join("pi-watch"), 1, "watch");
        let mut runtime = SearchRefreshRuntime::default();

        let initial = runtime.watcher_generation(std::slice::from_ref(&source));
        assert_eq!(initial, Some(0));
        runtime.force_source_change_for_test();
        let changed = runtime.watcher_generation(std::slice::from_ref(&source));
        assert_eq!(changed, Some(1));
    }

    #[test]
    fn daemon_cached_refresh_watcher_errors_invalidate_cached_work() {
        let generation = AtomicU64::new(0);

        note_search_refresh_source_event(
            &generation,
            Err(notify::Error::generic("deterministic watcher failure")),
        );

        assert_eq!(generation.load(Ordering::Acquire), 1);
    }

    #[test]
    fn daemon_cached_refresh_handles_deferred_recovery_across_passes() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source = seed_failed_pi_backlog(&data_root, 1);
        let mut runtime = SearchRefreshRuntime::default();
        let mut completed = 0usize;
        let mut pending = usize::MAX;

        for _ in 0..8 {
            let totals = refresh_with_runtime(&data_root, &mut runtime, vec![source.clone()]);
            assert!(runtime.cached_work.is_some());
            completed = completed.saturating_add(totals.recovery_units_processed);
            pending = totals.recovery_units_pending;
            if pending == 0 {
                break;
            }
        }

        assert_eq!(completed, 1);
        assert_eq!(pending, 0);
        let store = Store::open(database_path(data_root.clone())).unwrap();
        assert!(!store.has_pending_provider_file_publications().unwrap());

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
        for _ in 0..32 {
            refresh_with_runtime(&data_root, &mut runtime, vec![cached.clone()]);
            if !Store::open(database_path(data_root.clone()))
                .unwrap()
                .has_pending_provider_file_publications()
                .unwrap()
            {
                break;
            }
        }
        refresh_with_runtime(&data_root, &mut runtime, vec![cached.clone()]);
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
        for _ in 0..8 {
            let totals = refresh_with_runtime(&data_root, &mut runtime, vec![source.clone()]);
            pending = totals
                .fresh_units_pending
                .saturating_add(totals.recovery_units_pending);
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
    fn rewritten_growing_source_invalidates_staged_snapshot_and_converges() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source_root = data_root.join("pi-rewritten-growth");
        let source = write_pi_source(&source_root, 1, "stale-snapshot");
        let mut runtime = SearchRefreshRuntime::default();

        let first = refresh_with_runtime(&data_root, &mut runtime, vec![source.clone()]);
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
        let mut runtime = SearchRefreshRuntime::default();

        let first = refresh_with_runtime(&data_root, &mut runtime, vec![source.clone()]);
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
        let mut lock_store = Store::open(&db_path).unwrap();
        let inventory = inventory_import_sources(&lock_store, vec![source.clone()], false).unwrap();
        let waiting_plan = ImportPlan::build(&lock_store, inventory.sources.clone()).unwrap();
        let winning_plan = ImportPlan::build(&lock_store, inventory.sources).unwrap();
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
            (result, totals, first_refresh_failure, failed_sources)
        });
        waiting_rx.recv().unwrap();

        let winning_slice = winning_plan
            .select_slice(
                &lock_store,
                ImportWorkClass::Fresh,
                winning_plan.fresh_units,
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
        assert_eq!(result.completed_units, 1);
        assert_eq!(result.deferred_units, 0);
        assert!(result.made_durable_progress());
        assert_eq!(totals.fresh_units_processed, 1);
        assert!(first_failure.is_none());
        assert!(failed_sources.is_empty());

        let completed = refresh(&data_root, vec![source], ImportExecutionPolicy::Drain);
        assert_eq!(
            completed
                .fresh_units_processed
                .saturating_add(completed.recovery_units_processed),
            0,
            "{completed:?}"
        );
        assert_eq!(completed.fresh_units_pending, 0, "{completed:?}");
        assert_eq!(completed.recovery_units_pending, 0, "{completed:?}");
        assert!(!lock_store.has_pending_provider_file_publications().unwrap());
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
        let inventory = inventory_import_sources(&store, vec![source], false).unwrap();
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
        assert_eq!(failed_sources, BTreeSet::from([0]));
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
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let source = write_pi_source(&data_root.join("pi-fresh"), 130, "bounded-background");

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
            data_root,
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
        assert_eq!(report.totals.recovery_units_processed, 3);
        assert_eq!(report.totals.recovery_units_pending, 0);
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
}
