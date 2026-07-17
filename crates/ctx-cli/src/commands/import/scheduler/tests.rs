#[cfg(test)]
fn admit_catalog_work(
    slice: &mut ImportSlice,
    work: Vec<CatalogImportWork>,
) -> Vec<CatalogImportWork> {
    admit_work(slice, work, |work| work.estimated_bytes)
}

#[cfg(test)]
fn admit_work<T>(
    slice: &mut ImportSlice,
    work: Vec<T>,
    estimated_bytes: impl Fn(&T) -> u64,
) -> Vec<T> {
    let mut admitted = Vec::new();
    for unit in work {
        let bytes = estimated_bytes(&unit);
        let exceeds_target =
            slice.units > 0 && slice.bytes.saturating_add(bytes) > IMPORT_SLICE_TARGET_BYTES;
        if slice.units >= IMPORT_SLICE_MAX_UNITS || exceeds_target {
            break;
        }
        slice.units += 1;
        slice.bytes = slice.bytes.saturating_add(bytes);
        admitted.push(unit);
        if slice.units >= IMPORT_SLICE_MAX_UNITS || slice.bytes >= IMPORT_SLICE_TARGET_BYTES {
            break;
        }
    }
    admitted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::import::{
        import_totals_json, import_work_progress_done, import_work_progress_message, ImportTotals,
    };
    use crate::provider_sources::explicit_path_source;
    use ctx_history_core::{AgentType, CaptureProvider};
    use ctx_history_store::{
        CatalogIndexedStatus, CatalogSession, ImportPendingReason, SourceImportFile,
        SourceImportFileIndexUpdate, StoreError,
    };
    use serde_json::json;

    fn complete_source_inventory(store: &Store, root: &str, generation: u64) {
        assert!(store
            .complete_source_import_inventory_generation(CaptureProvider::Pi, root, generation,)
            .unwrap());
    }

    fn configured_test_writer(path: &std::path::Path) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(path).unwrap();
        let schema_version = conn
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap();
        conn.create_scalar_function(
            "ctx_schema_writer_version",
            0,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8
                | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
            move |_| Ok(schema_version),
        )
        .unwrap();
        conn
    }

    #[test]
    fn append_observation_metadata_only_allows_change_token_updates() {
        let previous = json!({
            "change_token_v1": {"size": 64},
            "dependencies": [],
            "session_id": "session-1",
        });
        let current = json!({
            "change_token_v1": {"size": 128},
            "dependencies": [],
            "session_id": "session-1",
        });
        assert!(append_observation_metadata_is_compatible(
            &previous, &current
        ));

        let changed_dependencies = json!({
            "change_token_v1": {"size": 128},
            "dependencies": ["metadata.json"],
            "session_id": "session-1",
        });
        assert!(!append_observation_metadata_is_compatible(
            &previous,
            &changed_dependencies,
        ));

        let missing_dependencies = json!({
            "change_token_v1": {"size": 128},
            "session_id": "session-1",
        });
        assert!(!append_observation_metadata_is_compatible(
            &previous,
            &missing_dependencies,
        ));

        let changed_identity = json!({
            "change_token_v1": {"size": 128},
            "dependencies": [],
            "session_id": "session-2",
        });
        assert!(!append_observation_metadata_is_compatible(
            &previous,
            &changed_identity,
        ));
    }

    fn catalog_work(path: &str, bytes: u64) -> CatalogImportWork {
        CatalogImportWork {
            session: CatalogSession {
                provider: CaptureProvider::Codex,
                source_format: "codex_session_jsonl_tree".to_owned(),
                source_root: "/sessions".to_owned(),
                source_path: path.to_owned(),
                external_session_id: Some(path.to_owned()),
                parent_external_session_id: None,
                agent_type: AgentType::Primary,
                role_hint: None,
                external_agent_id: None,
                cwd: None,
                session_started_at_ms: None,
                file_size_bytes: bytes,
                file_modified_at_ms: 1,
                import_revision: 1,
                cataloged_at_ms: 1,
                metadata: json!({}),
            },
            reason: ImportPendingReason::FreshNew,
            estimated_bytes: bytes,
            last_attempt_at_ms: None,
            has_active_publication: false,
        }
    }

    fn recovery_source(
        store: &Store,
        root: &str,
        attempted_at_ms: Option<i64>,
    ) -> (PlannedImportSource, SourceImportFile, u64) {
        let source = explicit_path_source(CaptureProvider::Pi, root.into());
        let file = SourceImportFile {
            provider: CaptureProvider::Pi,
            source_format: source.source_format.to_owned(),
            source_root: root.to_owned(),
            source_path: format!("{root}/session.jsonl"),
            file_size_bytes: 64,
            file_modified_at_ms: 100,
            import_revision: 1,
            observed_at_ms: 100,
            metadata: json!({}),
        };
        let generation = store
            .allocate_source_import_inventory_generation(CaptureProvider::Pi, root)
            .unwrap();
        store
            .upsert_source_import_files(generation, std::slice::from_ref(&file))
            .unwrap();
        match attempted_at_ms {
            Some(indexed_at_ms) => {
                store
                    .record_source_import_file_result(
                        CaptureProvider::Pi,
                        SourceImportFileIndexUpdate {
                            source_root: root,
                            source_path: &file.source_path,
                            file_size_bytes: file.file_size_bytes,
                            file_modified_at_ms: file.file_modified_at_ms,
                            import_revision: file.import_revision,
                            inventory_generation: generation,
                            metadata: &file.metadata,
                            indexed_at_ms,
                        },
                        CatalogIndexedStatus::Failed,
                        Some("deterministic failure"),
                    )
                    .unwrap();
            }
            None => {
                store
                    .record_source_import_file_result(
                        CaptureProvider::Pi,
                        SourceImportFileIndexUpdate {
                            source_root: root,
                            source_path: &file.source_path,
                            file_size_bytes: file.file_size_bytes,
                            file_modified_at_ms: file.file_modified_at_ms,
                            import_revision: file.import_revision,
                            inventory_generation: generation,
                            metadata: &file.metadata,
                            indexed_at_ms: 100,
                        },
                        CatalogIndexedStatus::Indexed,
                        None,
                    )
                    .unwrap();
            }
        }
        complete_source_inventory(store, root, generation);
        if attempted_at_ms.is_none() {
            assert_eq!(
                store
                    .schedule_source_import_explicit_rescan(CaptureProvider::Pi, root, generation,)
                    .unwrap(),
                1
            );
        }
        (
            PlannedImportSource {
                source,
                stats: SourceStats::default(),
                preinventory: SourcePreinventory::SourceImportFiles {
                    files: vec![file.clone()],
                    inventory_generation: generation,
                },
            },
            file,
            generation,
        )
    }

    #[test]
    fn slice_admits_one_oversized_unit() {
        let mut slice = ImportSlice::empty();
        let admitted = admit_catalog_work(
            &mut slice,
            vec![
                catalog_work("oversized", IMPORT_SLICE_TARGET_BYTES + 1),
                catalog_work("later", 1),
            ],
        );
        assert_eq!(admitted.len(), 1);
        assert_eq!(slice.units, 1);
        assert_eq!(slice.bytes, IMPORT_SLICE_TARGET_BYTES + 1);
    }

    #[test]
    fn slice_caps_units_and_bytes() {
        let mut slice = ImportSlice::empty();
        let admitted = admit_catalog_work(
            &mut slice,
            (0..100)
                .map(|index| catalog_work(&format!("unit-{index:03}"), 1))
                .collect(),
        );
        assert_eq!(admitted.len(), IMPORT_SLICE_MAX_UNITS);

        let mut slice = ImportSlice::empty();
        let admitted = admit_catalog_work(
            &mut slice,
            vec![
                catalog_work("first", IMPORT_SLICE_TARGET_BYTES - 1),
                catalog_work("second", 2),
            ],
        );
        assert_eq!(admitted.len(), 1);
    }

    #[test]
    fn locked_revalidation_preserves_missing_material_recovery_work() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("state.json");
        std::fs::write(&source_path, b"{}").unwrap();
        let source = explicit_path_source(CaptureProvider::CodeBuddy, source_path);
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let (stats, file) = observe_source_root(&source).unwrap();
        let first =
            persist_new_source_import_observation(&store, &source, std::slice::from_ref(&file))
                .unwrap();
        store
            .record_source_import_file_result(
                source.provider,
                SourceImportFileIndexUpdate {
                    source_root: &file.source_root,
                    source_path: &file.source_path,
                    file_size_bytes: file.file_size_bytes,
                    file_modified_at_ms: file.file_modified_at_ms,
                    import_revision: file.import_revision,
                    inventory_generation: first.inventory_generation,
                    metadata: &file.metadata,
                    indexed_at_ms: 1,
                },
                CatalogIndexedStatus::Indexed,
                None,
            )
            .unwrap();
        let current =
            persist_new_source_import_observation(&store, &source, std::slice::from_ref(&file))
                .unwrap();
        let plan = ImportPlan::build(
            &store,
            vec![PlannedImportSource {
                source,
                stats,
                preinventory: SourcePreinventory::SourceRoot {
                    file,
                    inventory_generation: current.inventory_generation,
                },
            }],
        )
        .unwrap();
        assert_eq!(plan.recovery_units, 1);
        let mut execution_state = ImportExecutionState::for_plan(&plan);

        let executable = plan
            .select_slice_for_execution_with_pre_lock_hook(
                &store,
                ImportWorkClass::Recovery,
                plan.recovery_units,
                &mut execution_state,
                || {},
            )
            .unwrap()
            .unwrap();
        assert_eq!(executable.slice.units, 1);
        let SelectedImportWork::SourceFiles(work) = &executable.slice.sources[0].work else {
            panic!("source-root recovery must select source-file work");
        };
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].reason, ImportPendingReason::MissingMaterial);
        store
            .finish_event_search_bulk_mode(&executable.bulk_guard)
            .unwrap();
    }

    #[test]
    fn execution_policies_bound_only_the_intended_phases() {
        assert_eq!(ImportExecutionPolicy::Drain.fresh_slice_limit(), None);
        assert_eq!(ImportExecutionPolicy::Drain.recovery_slice_limit(), None);
        assert_eq!(
            ImportExecutionPolicy::Interactive.fresh_slice_limit(),
            Some(1)
        );
        assert_eq!(
            ImportExecutionPolicy::Interactive.recovery_slice_limit(),
            Some(1)
        );
        assert_eq!(ImportExecutionPolicy::Daemon.fresh_slice_limit(), Some(1));
        assert_eq!(
            ImportExecutionPolicy::Daemon.recovery_slice_limit(),
            Some(1)
        );
        assert_eq!(
            ImportExecutionPolicy::Drain.disk_io_limits(),
            (64 * MEBIBYTE, 8 * MEBIBYTE)
        );
        assert_eq!(
            ImportExecutionPolicy::Interactive.disk_io_limits(),
            (32 * MEBIBYTE, 4 * MEBIBYTE)
        );
        assert_eq!(
            ImportExecutionPolicy::Daemon.disk_io_limits(),
            (8 * MEBIBYTE, MEBIBYTE)
        );
    }

    #[test]
    fn progress_and_json_distinguish_fresh_from_recovery() {
        assert_eq!(
            import_work_progress_message(ImportWorkClass::Fresh, CaptureProvider::Pi),
            ("indexing", "indexing new/changed pi history".to_owned())
        );
        assert_eq!(
            import_work_progress_message(ImportWorkClass::Recovery, CaptureProvider::Pi),
            ("repairing", "repairing prior pi history".to_owned())
        );
        let source = explicit_path_source(CaptureProvider::Pi, "/fixture/pi".into());
        assert_eq!(
            import_work_progress_done(ImportWorkClass::Fresh, &source),
            ("indexing", "Indexed new/changed Pi history.".to_owned())
        );
        assert_eq!(
            import_work_progress_done(ImportWorkClass::Recovery, &source),
            ("repairing", "Repaired prior Pi history.".to_owned())
        );

        let totals = ImportTotals {
            fresh_units_processed: 3,
            recovery_units_processed: 2,
            fresh_units_pending: 1,
            recovery_units_pending: 4,
            ..ImportTotals::default()
        };
        let snapshot = import_totals_json(&totals);
        assert_eq!(snapshot["fresh_units_processed"], 3);
        assert_eq!(snapshot["recovery_units_processed"], 2);
        assert_eq!(snapshot["fresh_units_pending"], 1);
        assert_eq!(snapshot["fresh_units_pending_exact"], true);
        assert_eq!(snapshot["recovery_units_pending"], 4);
        assert_eq!(snapshot["recovery_units_pending_exact"], true);
    }

    #[test]
    fn large_backlog_reporting_is_a_bounded_lower_bound() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let root = "/fixture/large-backlog";
        let source = explicit_path_source(CaptureProvider::Pi, root.into());
        let files = (0..10_000)
            .map(|index| SourceImportFile {
                provider: CaptureProvider::Pi,
                source_format: source.source_format.to_owned(),
                source_root: root.to_owned(),
                source_path: format!("{root}/{index:05}.jsonl"),
                file_size_bytes: 1,
                file_modified_at_ms: 1,
                import_revision: 1,
                observed_at_ms: 1,
                metadata: json!({}),
            })
            .collect::<Vec<_>>();
        let generation = store
            .allocate_source_import_inventory_generation(CaptureProvider::Pi, root)
            .unwrap();
        store
            .upsert_source_import_files(generation, &files)
            .unwrap();
        complete_source_inventory(&store, root, generation);
        let plan = ImportPlan::build(
            &store,
            vec![PlannedImportSource {
                source,
                stats: SourceStats::default(),
                preinventory: SourcePreinventory::SourceImportFiles {
                    files,
                    inventory_generation: generation,
                },
            }],
        )
        .unwrap();

        assert_eq!(plan.fresh_units, usize::MAX);
        assert_eq!(
            plan.pending_counts(&store).unwrap(),
            (IMPORT_PENDING_REPORT_LIMIT, 0)
        );
        let snapshot = import_totals_json(&ImportTotals {
            fresh_units_pending: IMPORT_PENDING_REPORT_LIMIT,
            ..ImportTotals::default()
        });
        assert_eq!(snapshot["fresh_units_pending"], IMPORT_PENDING_REPORT_LIMIT);
        assert_eq!(snapshot["fresh_units_pending_exact"], false);
    }

    #[test]
    fn fresh_work_is_selected_before_a_global_failed_and_revision_backlog() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let backlog_root = "/fixture/backlog";
        let mut backlog = (0..100)
            .map(|index| SourceImportFile {
                provider: CaptureProvider::Pi,
                source_format: "pi_session_jsonl".to_owned(),
                source_root: backlog_root.to_owned(),
                source_path: format!("{backlog_root}/{index:03}.jsonl"),
                file_size_bytes: 128,
                file_modified_at_ms: 1000 + index,
                import_revision: 1,
                observed_at_ms: 2000,
                metadata: json!({}),
            })
            .collect::<Vec<_>>();
        let generation = store
            .allocate_source_import_inventory_generation(CaptureProvider::Pi, backlog_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, &backlog)
            .unwrap();
        for file in &backlog[..50] {
            store
                .record_source_import_file_result(
                    CaptureProvider::Pi,
                    SourceImportFileIndexUpdate {
                        source_root: backlog_root,
                        source_path: &file.source_path,
                        file_size_bytes: file.file_size_bytes,
                        file_modified_at_ms: file.file_modified_at_ms,
                        import_revision: file.import_revision,
                        inventory_generation: generation,
                        metadata: &file.metadata,
                        indexed_at_ms: 3000,
                    },
                    CatalogIndexedStatus::Failed,
                    Some("retry"),
                )
                .unwrap();
        }
        for file in &backlog[50..] {
            store
                .record_source_import_file_result(
                    CaptureProvider::Pi,
                    SourceImportFileIndexUpdate {
                        source_root: backlog_root,
                        source_path: &file.source_path,
                        file_size_bytes: file.file_size_bytes,
                        file_modified_at_ms: file.file_modified_at_ms,
                        import_revision: file.import_revision,
                        inventory_generation: generation,
                        metadata: &file.metadata,
                        indexed_at_ms: 3000,
                    },
                    CatalogIndexedStatus::Indexed,
                    None,
                )
                .unwrap();
        }
        complete_source_inventory(&store, backlog_root, generation);
        for file in &mut backlog[50..] {
            file.import_revision = 2;
        }
        let generation = store
            .allocate_source_import_inventory_generation(CaptureProvider::Pi, backlog_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, &backlog)
            .unwrap();
        complete_source_inventory(&store, backlog_root, generation);

        let fresh_root = "/fixture/fresh";
        let fresh = SourceImportFile {
            provider: CaptureProvider::Pi,
            source_format: "pi_session_jsonl".to_owned(),
            source_root: fresh_root.to_owned(),
            source_path: format!("{fresh_root}/new.jsonl"),
            file_size_bytes: 64,
            file_modified_at_ms: 5000,
            import_revision: 1,
            observed_at_ms: 5000,
            metadata: json!({}),
        };
        let fresh_generation = store
            .allocate_source_import_inventory_generation(CaptureProvider::Pi, fresh_root)
            .unwrap();
        store
            .upsert_source_import_files(fresh_generation, std::slice::from_ref(&fresh))
            .unwrap();
        complete_source_inventory(&store, fresh_root, fresh_generation);

        let plan = ImportPlan::build(
            &store,
            vec![
                PlannedImportSource {
                    source: explicit_path_source(CaptureProvider::Pi, backlog_root.into()),
                    stats: SourceStats::default(),
                    preinventory: SourcePreinventory::SourceImportFiles {
                        files: backlog,
                        inventory_generation: generation,
                    },
                },
                PlannedImportSource {
                    source: explicit_path_source(CaptureProvider::Pi, fresh_root.into()),
                    stats: SourceStats::default(),
                    preinventory: SourcePreinventory::SourceImportFiles {
                        files: vec![fresh],
                        inventory_generation: fresh_generation,
                    },
                },
            ],
        )
        .unwrap();
        assert_eq!(plan.fresh_units, 1);
        assert_eq!(plan.recovery_units, 100);

        let fresh_slice = plan
            .select_slice(&store, ImportWorkClass::Fresh, plan.fresh_units)
            .unwrap();
        assert_eq!(fresh_slice.units, 1);
        assert_eq!(fresh_slice.sources[0].source_index, 1);
        let recovery_slice = plan
            .select_slice(&store, ImportWorkClass::Recovery, plan.recovery_units)
            .unwrap();
        assert_eq!(recovery_slice.units, IMPORT_SLICE_MAX_UNITS);
        assert_eq!(recovery_slice.sources[0].source_index, 0);
    }

    #[test]
    fn global_recovery_prefers_unattempted_work_from_a_later_source() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("work.sqlite");
        let store = Store::open(&db_path).unwrap();
        let (first, _, _) = recovery_source(&store, "/fixture/first", Some(100));
        let (later, _, _) = recovery_source(&store, "/fixture/later", Some(200));
        configured_test_writer(&db_path)
            .execute(
                "UPDATE source_import_files SET indexed_at_ms = NULL WHERE source_root = ?1",
                ["/fixture/later"],
            )
            .unwrap();
        let plan = ImportPlan::build(&store, vec![first, later]).unwrap();

        let slice = plan
            .select_slice(&store, ImportWorkClass::Recovery, 1)
            .unwrap();
        assert_eq!(slice.units, 1);
        assert_eq!(slice.sources[0].source_index, 1);
    }

    #[test]
    fn failed_recovery_source_rotates_behind_the_older_other_source() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let (first, first_file, first_generation) =
            recovery_source(&store, "/fixture/first", Some(100));
        let (later, _, _) = recovery_source(&store, "/fixture/later", Some(200));
        let plan = ImportPlan::build(&store, vec![first, later]).unwrap();

        let first_slice = plan
            .select_slice(&store, ImportWorkClass::Recovery, 1)
            .unwrap();
        assert_eq!(first_slice.sources[0].source_index, 0);
        store
            .record_source_import_file_result(
                CaptureProvider::Pi,
                SourceImportFileIndexUpdate {
                    source_root: &first_file.source_root,
                    source_path: &first_file.source_path,
                    file_size_bytes: first_file.file_size_bytes,
                    file_modified_at_ms: first_file.file_modified_at_ms,
                    import_revision: first_file.import_revision,
                    inventory_generation: first_generation,
                    metadata: &first_file.metadata,
                    indexed_at_ms: 300,
                },
                CatalogIndexedStatus::Failed,
                Some("still failing"),
            )
            .unwrap();

        let second_slice = plan
            .select_slice(&store, ImportWorkClass::Recovery, 1)
            .unwrap();
        assert_eq!(second_slice.sources[0].source_index, 1);
    }

    #[test]
    fn one_execution_does_not_select_the_same_pending_unit_twice() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let (source, _, _) = recovery_source(&store, "/fixture/deferred", None);
        let plan = ImportPlan::build(&store, vec![source]).unwrap();
        let mut state = ImportExecutionState::for_plan(&plan);

        let first = plan
            .select_slice_with_state(&store, ImportWorkClass::Recovery, 1, &state, None)
            .unwrap();
        assert_eq!(first.units, 1);
        first.sources[0].persist_attempt_started(&store).unwrap();
        state.record_source_attempt(&first.sources[0].work);
        let second = plan
            .select_slice_with_state(&store, ImportWorkClass::Recovery, 1, &state, None)
            .unwrap();
        assert!(second.is_empty());
    }

    #[test]
    fn post_import_cache_replaces_file_metadata_with_the_new_observation() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let (source, file, _) = recovery_source(&store, "/fixture/reobserved", None);
        let plan = ImportPlan::build(&store, vec![source]).unwrap();
        let mut state = ImportExecutionState::for_plan(&plan);
        let selected = plan
            .select_slice_with_state(&store, ImportWorkClass::Recovery, 1, &state, None)
            .unwrap();
        let selected_work = &selected.sources[0].work;

        let mut reobserved = file;
        reobserved.file_size_bytes = 128;
        reobserved.file_modified_at_ms = 200;
        state.record_source_outcome(
            0,
            selected_work,
            Some(SourcePreinventory::SourceImportFiles {
                files: vec![reobserved.clone()],
                inventory_generation: 99,
            }),
        );

        let SourcePreinventory::SourceImportFiles {
            files,
            inventory_generation,
        } = plan.selected_preinventory(&state, 0)
        else {
            panic!("manifest source must cache its post-import observation");
        };
        assert_eq!(inventory_generation, 99);
        assert_eq!(files, vec![reobserved]);
    }

    #[test]
    fn rebased_execution_state_preserves_only_unaffected_source_observations() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let (dirty_source, _, _) = recovery_source(&store, "/fixture/dirty", None);
        let (stable_source, _, stable_generation) =
            recovery_source(&store, "/fixture/stable", None);
        let old_plan = ImportPlan {
            sources: vec![dirty_source.clone(), stable_source.clone()],
            fresh_units: 0,
            recovery_units: 0,
        };
        let mut state = ImportExecutionState::for_plan(&old_plan);
        state.observed_preinventories = vec![
            Some(dirty_source.preinventory.clone()),
            Some(stable_source.preinventory.clone()),
        ];
        state.attempted_work.insert("attempted-work".to_owned());

        let new_plan = ImportPlan {
            sources: vec![stable_source, dirty_source],
            fresh_units: 0,
            recovery_units: 0,
        };
        let rebased = state.rebase_for_plan(
            &old_plan,
            &new_plan,
            &BTreeSet::from([PathBuf::from("/fixture/dirty")]),
        );

        assert_eq!(
            rebased.observed_preinventories[0]
                .as_ref()
                .and_then(SourcePreinventory::inventory_generation),
            Some(stable_generation)
        );
        assert!(rebased.observed_preinventories[1].is_none());
        assert!(rebased.attempted_work.contains("attempted-work"));
    }

    #[test]
    fn selected_but_still_pending_work_is_not_completed_progress() {
        let mut result = ImportExecutionResult::default();
        result.add_slice(1, 0, 1, false);
        assert_eq!(result.selected_units, 1);
        assert_eq!(result.completed_units, 0);
        assert_eq!(result.deferred_units, 1);
        assert!(!result.made_durable_progress());
    }

    #[test]
    fn maintenance_pending_stops_drain_progress() {
        let mut result = ImportExecutionResult::default();
        result.add_slice(10, 10, 0, true);
        result.stop_admission();
        assert_eq!(result.completed_units, 10);
        assert!(!result.made_durable_progress());
    }

    #[test]
    fn projection_repair_gates_plan_counts_and_selection() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("work.sqlite");
        let store = Store::open(&path).unwrap();
        let (source, _, _) = recovery_source(&store, "/fixture/projection-gate", Some(10));
        drop(store);
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "UPDATE import_pending_work_state SET selection_mode = 'projection'; \
                 UPDATE import_pending_reason_repairs \
                 SET cursor_provider = NULL, cursor_source_root = NULL, \
                     cursor_source_path = NULL, completed = 0;",
        )
        .unwrap();
        drop(conn);
        let store = Store::open(&path).unwrap();

        let plan = ImportPlan::build(&store, vec![source]).unwrap();
        assert_eq!(plan.fresh_units, 0);
        assert_eq!(plan.recovery_units, 0);
        let count_error = plan.pending_counts(&store).unwrap_err();
        assert!(matches!(
            count_error.downcast_ref::<StoreError>(),
            Some(StoreError::ImportPendingWorkProjectionIncomplete)
        ));
        let selection_error = plan
            .select_slice(&store, ImportWorkClass::Recovery, 1)
            .unwrap_err();
        assert!(matches!(
            selection_error.downcast_ref::<StoreError>(),
            Some(StoreError::ImportPendingWorkProjectionIncomplete)
        ));
    }

    #[test]
    fn only_fresh_new_work_uses_the_atomic_group_path() {
        let mut candidate = catalog_work("session.jsonl", 1);
        candidate.session.source_format = CODEX_SESSION_SOURCE_FORMAT.to_owned();
        assert!(SelectedImportWork::Catalog(vec![candidate.clone()]).is_fresh_new_group());
        for reason in [
            ImportPendingReason::FreshChanged,
            ImportPendingReason::FreshAppend,
            ImportPendingReason::ParserRevision,
            ImportPendingReason::RecoveryReplacement,
        ] {
            candidate.reason = reason;
            assert!(!SelectedImportWork::Catalog(vec![candidate.clone()]).is_fresh_new_group());
        }
    }

    #[test]
    fn fresh_new_selection_is_one_efficient_group_in_every_mode() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let mut sources = Vec::new();
        for root in ["/fixture/fresh-window-a", "/fixture/fresh-window-b"] {
            let source = explicit_path_source(CaptureProvider::Pi, root.into());
            let files = (0..65)
                .map(|index| SourceImportFile {
                    provider: CaptureProvider::Pi,
                    source_format: source.source_format.to_owned(),
                    source_root: root.to_owned(),
                    source_path: format!("{root}/{index:03}.jsonl"),
                    file_size_bytes: 1,
                    file_modified_at_ms: 1,
                    import_revision: 1,
                    observed_at_ms: 1,
                    metadata: json!({}),
                })
                .collect::<Vec<_>>();
            let generation = store
                .allocate_source_import_inventory_generation(CaptureProvider::Pi, root)
                .unwrap();
            store
                .upsert_source_import_files(generation, &files)
                .unwrap();
            complete_source_inventory(&store, root, generation);
            sources.push(PlannedImportSource {
                source,
                stats: SourceStats::default(),
                preinventory: SourcePreinventory::SourceImportFiles {
                    files,
                    inventory_generation: generation,
                },
            });
        }
        let plan = ImportPlan::build(&store, sources).unwrap();

        for max_units in [1, IMPORT_SLICE_MAX_UNITS] {
            let mut state = ImportExecutionState::for_plan(&plan);
            for _ in 0..2 {
                let slice = plan
                    .select_slice_with_state(
                        &store,
                        ImportWorkClass::Fresh,
                        max_units,
                        &state,
                        None,
                    )
                    .unwrap();
                assert_eq!(slice.units, 65);
                assert_eq!(slice.sources.len(), 1);
                assert!(slice.sources[0].work.is_fresh_new_group());
                state.record_source_attempt(&slice.sources[0].work);
            }
            let exhausted = plan
                .select_slice_with_state(&store, ImportWorkClass::Fresh, max_units, &state, None)
                .unwrap();
            assert!(exhausted.is_empty());
        }
    }
}
