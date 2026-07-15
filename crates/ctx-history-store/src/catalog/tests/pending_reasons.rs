fn insert_matching_checkpoint(store: &Store, file: &SourceImportFile) {
    store
        .conn
        .execute(
            r#"
            INSERT INTO provider_file_checkpoints (
                provider, source_format, source_root, source_path, import_revision,
                checkpoint_version, stable_file_identity, committed_byte_offset,
                committed_complete_line_count, head_sha256, boundary_sha256, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, 1, 'test-file', ?6, 0, ?7, ?8, ?9)
            "#,
            params![
                file.provider.as_str(),
                &file.source_format,
                &file.source_root,
                &file.source_path,
                i64::from(file.import_revision),
                file.file_size_bytes,
                "a".repeat(64),
                "b".repeat(64),
                file.observed_at_ms,
            ],
        )
        .unwrap();
}

#[test]
fn fresh_schema_stages_both_pending_reason_repairs() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("catalog.sqlite")).unwrap();
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
            ("catalog_sessions".into(), None, None, None, true),
            ("source_import_files".into(), None, None, None, true),
        ]
    );
}

#[test]
fn pending_reason_repair_is_bounded_resumable_idempotent_and_conservative() {
    let temp = tempdir();
    let path = temp.path().join("catalog.sqlite");
    let store = Store::open(&path).unwrap();
    store
        .conn
        .execute_batch(
            r#"
            INSERT INTO catalog_sessions (
              source_path, provider, source_format, source_root, external_session_id,
              agent_type, file_size_bytes, file_modified_at_ms, import_revision,
              cataloged_at_ms, indexed_at_ms, indexed_file_size_bytes,
              indexed_file_modified_at_ms, indexed_status, indexed_import_revision,
              pending_reason
            ) VALUES
              ('/repair/catalog/01-clean.jsonl', 'codex', 'codex_session_jsonl',
               '/repair/catalog', 'clean', 'primary', 10, 10, 1, 10,
               20, 10, 10, 'indexed', 1, NULL),
              ('/repair/catalog/02-pending.jsonl', 'codex', 'codex_session_jsonl',
               '/repair/catalog', 'pending', 'primary', 10, 10, 1, 10,
               NULL, NULL, NULL, 'pending', NULL, NULL),
              ('/repair/catalog/03-rejected.jsonl', 'codex', 'codex_session_jsonl',
               '/repair/catalog', 'rejected', 'primary', 10, 10, 1, 10,
               20, NULL, NULL, 'rejected', NULL, NULL),
              ('/repair/catalog/04-runtime.jsonl', 'codex', 'codex_session_jsonl',
               '/repair/catalog', 'runtime', 'primary', 10, 10, 1, 10,
               NULL, NULL, NULL, 'pending', NULL, 'fresh_changed'),
              ('/repair/catalog/05-missing.jsonl', 'codex', 'codex_session_jsonl',
               '/repair/catalog', 'missing', 'primary', 10, 10, 1, 10,
               20, 10, 10, 'indexed', 1, NULL);

            INSERT INTO source_import_files (
              provider, source_format, source_root, source_path, file_size_bytes,
              file_modified_at_ms, import_revision, observed_at_ms, is_stale,
              indexed_at_ms, indexed_file_size_bytes, indexed_file_modified_at_ms,
              indexed_status, indexed_import_revision, pending_reason
            ) VALUES
              ('pi', 'pi_session_jsonl', '/repair/source',
               '/repair/source/01-clean.jsonl', 10, 10, 1, 10, 0,
               20, 10, 10, 'indexed', 1, NULL),
              ('pi', 'pi_session_jsonl', '/repair/source',
               '/repair/source/02-failed.jsonl', 10, 10, 1, 10, 0,
               20, 10, 10, 'failed', 1, NULL),
              ('pi', 'pi_session_jsonl', '/repair/source',
               '/repair/source/03-stale.jsonl', 10, 10, 1, 10, 1,
               NULL, NULL, NULL, 'pending', NULL, NULL),
              ('pi', 'pi_session_jsonl', '/repair/source',
               '/repair/source/04-runtime.jsonl', 10, 10, 1, 10, 0,
               NULL, NULL, NULL, 'pending', NULL, 'missing_material'),
              ('pi', 'pi_session_jsonl', '/repair/source',
               '/repair/source/05-missing.jsonl', 10, 10, 1, 10, 0,
               20, 10, 10, 'indexed', 1, NULL);

            INSERT INTO capture_sources (
              id, kind, provider, machine_id, raw_source_path, source_format,
              source_root, external_session_id, started_at_ms, fidelity
            ) VALUES
              ('repair-catalog-clean', 'provider_import', 'codex', 'fixture',
               '/repair/catalog/01-clean.jsonl', 'codex_session_jsonl',
               '/repair/catalog', 'clean', 1, 'imported'),
              ('repair-source-clean', 'provider_import', 'pi', 'fixture',
               '/repair/source/01-clean.jsonl', 'pi_session_jsonl',
               '/repair/source', NULL, 1, 'imported');

            INSERT INTO sessions (
              id, capture_source_id, provider, external_session_id, agent_type,
              status, fidelity, started_at_ms, created_at_ms, updated_at_ms
            ) VALUES (
              'repair-session-clean', 'repair-catalog-clean', 'codex', 'clean',
              'primary', 'imported', 'imported', 1, 1, 1
            );
            "#,
        )
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE import_pending_reason_repairs \
             SET cursor_provider = NULL, cursor_source_root = NULL, \
                 cursor_source_path = NULL, completed = 0",
            [],
        )
        .unwrap();

    let first = store.repair_import_pending_reasons(2).unwrap();
    assert_eq!(first.processed_rows, 2);
    assert_eq!(first.classified_rows, 1);
    assert!(!first.complete);
    let first_cursor: String = store
        .conn
        .query_row(
            "SELECT cursor_source_path FROM import_pending_reason_repairs \
             WHERE inventory_family = 'catalog_sessions'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(first_cursor, "/repair/catalog/02-pending.jsonl");
    drop(store);

    let store = Store::open(&path).unwrap();
    let mut processed_rows = first.processed_rows;
    let mut classified_rows = first.classified_rows;
    let mut completed = false;
    for _ in 0..5 {
        let progress = store.repair_import_pending_reasons(2).unwrap();
        assert!(progress.processed_rows <= 2);
        processed_rows += progress.processed_rows;
        classified_rows += progress.classified_rows;
        if progress.complete {
            completed = true;
            break;
        }
    }
    assert!(completed);
    assert_eq!(processed_rows, 10);
    assert_eq!(classified_rows, 4);

    let catalog_reasons = store
        .conn
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
            ("/repair/catalog/01-clean.jsonl".into(), None),
            (
                "/repair/catalog/02-pending.jsonl".into(),
                Some("legacy".into())
            ),
            ("/repair/catalog/03-rejected.jsonl".into(), None),
            (
                "/repair/catalog/04-runtime.jsonl".into(),
                Some("fresh_changed".into())
            ),
            (
                "/repair/catalog/05-missing.jsonl".into(),
                Some("legacy".into())
            ),
        ]
    );
    let source_reasons = store
        .conn
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
            ("/repair/source/01-clean.jsonl".into(), None),
            (
                "/repair/source/02-failed.jsonl".into(),
                Some("legacy".into())
            ),
            ("/repair/source/03-stale.jsonl".into(), None),
            (
                "/repair/source/04-runtime.jsonl".into(),
                Some("missing_material".into())
            ),
            (
                "/repair/source/05-missing.jsonl".into(),
                Some("legacy".into())
            ),
        ]
    );

    let idempotent = store.repair_import_pending_reasons(2).unwrap();
    assert_eq!(idempotent.processed_rows, 0);
    assert_eq!(idempotent.classified_rows, 0);
    assert_eq!(idempotent.completed_families, 2);
    assert!(idempotent.complete);
}

#[test]
fn pending_work_orders_never_attempted_then_oldest_attempt_across_reopen() {
    let temp = tempdir();
    let path = temp.path().join("catalog.sqlite");
    let store = Store::open(&path).unwrap();
    for (family, reason) in [
        ("fresh", "fresh_changed"),
        ("recovery", "recovery_replacement"),
    ] {
        for (attempt, name) in [
            (None, "never-a"),
            (None, "never-b"),
            (Some(100), "oldest"),
            (Some(200), "newest"),
        ] {
            store
                .conn
                .execute(
                    r#"
                    INSERT INTO catalog_sessions (
                      source_path, provider, source_format, source_root, agent_type,
                      file_size_bytes, file_modified_at_ms, cataloged_at_ms,
                      indexed_at_ms, indexed_status, pending_reason
                    ) VALUES (?1, 'pi', 'pi_session_jsonl', ?2, 'primary',
                              1, 1, 1, ?3, 'pending', ?4)
                    "#,
                    params![
                        format!("/order/catalog/{family}/{name}.jsonl"),
                        format!("/order/catalog/{family}"),
                        attempt,
                        reason,
                    ],
                )
                .unwrap();
            store
                .conn
                .execute(
                    r#"
                    INSERT INTO source_import_files (
                      provider, source_format, source_root, source_path,
                      file_size_bytes, file_modified_at_ms, observed_at_ms,
                      indexed_at_ms, indexed_status, pending_reason
                    ) VALUES ('pi', 'pi_session_jsonl', ?1, ?2,
                              1, 1, 1, ?3, 'pending', ?4)
                    "#,
                    params![
                        format!("/order/source/{family}"),
                        format!("/order/source/{family}/{name}.jsonl"),
                        attempt,
                        reason,
                    ],
                )
                .unwrap();
        }
    }
    drop(store);

    let store = Store::open(&path).unwrap();
    for (family, class) in [
        ("fresh", ImportWorkClass::Fresh),
        ("recovery", ImportWorkClass::Recovery),
    ] {
        let catalog = store
            .list_catalog_import_work(
                CaptureProvider::Pi,
                &format!("/order/catalog/{family}"),
                class,
                4,
            )
            .unwrap();
        assert_eq!(
            catalog
                .iter()
                .map(|work| {
                    (
                        work.session
                            .source_path
                            .rsplit('/')
                            .next()
                            .unwrap()
                            .to_owned(),
                        work.last_attempt_at_ms,
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                ("never-a.jsonl".into(), None),
                ("never-b.jsonl".into(), None),
                ("oldest.jsonl".into(), Some(100)),
                ("newest.jsonl".into(), Some(200)),
            ]
        );

        let source = store
            .list_source_import_file_work(
                CaptureProvider::Pi,
                &format!("/order/source/{family}"),
                class,
                4,
            )
            .unwrap();
        assert_eq!(
            source
                .iter()
                .map(|work| {
                    (
                        work.file.source_path.rsplit('/').next().unwrap().to_owned(),
                        work.last_attempt_at_ms,
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                ("never-a.jsonl".into(), None),
                ("never-b.jsonl".into(), None),
                ("oldest.jsonl".into(), Some(100)),
                ("newest.jsonl".into(), Some(200)),
            ]
        );
    }
}

#[test]
fn source_import_work_is_selectable_by_freshness_class_within_one_source() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("catalog.sqlite")).unwrap();
    let root = "/fixture/mixed";
    let failed = source_import_file(
        CaptureProvider::Pi,
        "pi_session_jsonl",
        root,
        "/fixture/mixed/failed.jsonl",
        1000,
    );
    let fresh = source_import_file(
        CaptureProvider::Pi,
        "pi_session_jsonl",
        root,
        "/fixture/mixed/fresh.jsonl",
        1001,
    );
    upsert_source_inventory(&store, &[failed.clone(), fresh.clone()]);
    let generation = current_source_generation(&store, CaptureProvider::Pi, root);
    store
        .record_source_import_file_result(
            CaptureProvider::Pi,
            SourceImportFileIndexUpdate {
                source_root: root,
                source_path: &failed.source_path,
                file_size_bytes: failed.file_size_bytes,
                file_modified_at_ms: failed.file_modified_at_ms,
                import_revision: failed.import_revision,
                inventory_generation: generation,
                metadata: &failed.metadata,
                indexed_at_ms: 2000,
            },
            CatalogIndexedStatus::Failed,
            Some("retry me"),
        )
        .unwrap();

    let fresh_work = store
        .list_source_import_file_work(CaptureProvider::Pi, root, ImportWorkClass::Fresh, 10)
        .unwrap();
    assert_eq!(fresh_work.len(), 1);
    assert_eq!(fresh_work[0].file.source_path, fresh.source_path);
    assert_eq!(fresh_work[0].reason, ImportPendingReason::FreshNew);

    let recovery_work = store
        .list_source_import_file_work(CaptureProvider::Pi, root, ImportWorkClass::Recovery, 10)
        .unwrap();
    assert_eq!(recovery_work.len(), 1);
    assert_eq!(recovery_work[0].file.source_path, failed.source_path);
    assert_eq!(
        recovery_work[0].reason,
        ImportPendingReason::RecoveryReplacement
    );
    assert_eq!(recovery_work[0].reason.class(), ImportWorkClass::Recovery);
    assert!(recovery_work[0].reason.requires_replacement());

    upsert_source_inventory(&store, &[failed, fresh]);
    let reclassified = store
        .list_source_import_file_work(CaptureProvider::Pi, root, ImportWorkClass::Recovery, 10)
        .unwrap();
    assert_eq!(
        reclassified[0].reason,
        ImportPendingReason::RecoveryReplacement
    );
}

#[test]
fn explicit_source_rescan_survives_same_fingerprint_reobservation() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("catalog.sqlite")).unwrap();
    let root = "/fixture/explicit-rescan";
    let file = source_import_file(
        CaptureProvider::Pi,
        "pi_session_jsonl",
        root,
        "/fixture/explicit-rescan/session.jsonl",
        1000,
    );
    upsert_source_inventory(&store, std::slice::from_ref(&file));
    store
        .mark_source_import_file_indexed(
            file.provider,
            SourceImportFileIndexUpdate {
                source_root: root,
                source_path: &file.source_path,
                file_size_bytes: file.file_size_bytes,
                file_modified_at_ms: file.file_modified_at_ms,
                import_revision: file.import_revision,
                inventory_generation: current_source_generation(&store, file.provider, root),
                metadata: &file.metadata,
                indexed_at_ms: 1001,
            },
        )
        .unwrap();
    let mut material_source = imported_source(new_id(), root, "explicit-rescan-session");
    material_source.descriptor.provider = file.provider;
    material_source.descriptor.raw_source_path = Some(file.source_path.clone());
    material_source.descriptor.source_format = Some(file.source_format.clone());
    store.upsert_capture_source(&material_source).unwrap();

    store
        .schedule_source_import_explicit_rescan(
            file.provider,
            root,
            current_source_generation(&store, file.provider, root),
        )
        .unwrap();
    upsert_source_inventory(&store, std::slice::from_ref(&file));

    let recovery = store
        .list_source_import_file_work(file.provider, root, ImportWorkClass::Recovery, 10)
        .unwrap();
    assert_eq!(recovery.len(), 1);
    assert_eq!(recovery[0].reason, ImportPendingReason::ExplicitRescan);
}

#[test]
fn source_import_append_failure_retries_incrementally() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("catalog.sqlite")).unwrap();
    let root = "/fixture/append";
    let mut file = source_import_file(
        CaptureProvider::Pi,
        "pi_session_jsonl",
        root,
        "/fixture/append/session.jsonl",
        1000,
    );
    upsert_source_inventory(&store, std::slice::from_ref(&file));
    store
        .mark_source_import_file_indexed(
            file.provider,
            SourceImportFileIndexUpdate {
                source_root: root,
                source_path: &file.source_path,
                file_size_bytes: file.file_size_bytes,
                file_modified_at_ms: file.file_modified_at_ms,
                import_revision: file.import_revision,
                inventory_generation: current_source_generation(&store, file.provider, root),
                metadata: &file.metadata,
                indexed_at_ms: 1001,
            },
        )
        .unwrap();
    let mut material_source = imported_source(new_id(), root, "append-session");
    material_source.descriptor.provider = file.provider;
    material_source.descriptor.raw_source_path = Some(file.source_path.clone());
    material_source.descriptor.source_format = Some(file.source_format.clone());
    store.upsert_capture_source(&material_source).unwrap();
    insert_matching_checkpoint(&store, &file);

    file.file_size_bytes = 64;
    file.file_modified_at_ms += 1;
    file.observed_at_ms += 1;
    upsert_source_inventory(&store, std::slice::from_ref(&file));
    let fresh = store
        .list_source_import_file_work(file.provider, root, ImportWorkClass::Fresh, 10)
        .unwrap();
    assert_eq!(fresh[0].reason, ImportPendingReason::FreshAppend);

    store
        .record_source_import_file_result(
            file.provider,
            SourceImportFileIndexUpdate {
                source_root: root,
                source_path: &file.source_path,
                file_size_bytes: file.file_size_bytes,
                file_modified_at_ms: file.file_modified_at_ms,
                import_revision: file.import_revision,
                inventory_generation: current_source_generation(&store, file.provider, root),
                metadata: &file.metadata,
                indexed_at_ms: 1002,
            },
            CatalogIndexedStatus::Failed,
            Some("retry append"),
        )
        .unwrap();
    let recovery = store
        .list_source_import_file_work(file.provider, root, ImportWorkClass::Recovery, 10)
        .unwrap();
    assert_eq!(recovery[0].reason, ImportPendingReason::RecoveryRetry);
    assert_eq!(recovery[0].reason.class(), ImportWorkClass::Recovery);
    assert!(!recovery[0].reason.requires_replacement());

    upsert_source_inventory(&store, std::slice::from_ref(&file));
    let reclassified = store
        .list_source_import_file_work(file.provider, root, ImportWorkClass::Recovery, 10)
        .unwrap();
    assert_eq!(reclassified[0].reason, ImportPendingReason::RecoveryRetry);

    file.file_size_bytes = 80;
    file.file_modified_at_ms += 1;
    file.observed_at_ms += 1;
    upsert_source_inventory(&store, std::slice::from_ref(&file));
    let after_growth = store
        .list_source_import_file_work(file.provider, root, ImportWorkClass::Recovery, 10)
        .unwrap();
    assert_eq!(after_growth[0].reason, ImportPendingReason::RecoveryRetry);
    assert!(!after_growth[0].reason.requires_replacement());
}

#[test]
fn source_growth_requires_exact_material_and_checkpoint_for_fresh_append() {
    for missing in ["material", "checkpoint"] {
        let temp = tempdir();
        let store = Store::open(temp.path().join(format!("missing-{missing}.sqlite"))).unwrap();
        let root = "/fixture/append-guard";
        let mut file = source_import_file(
            CaptureProvider::Pi,
            "pi_session_jsonl",
            root,
            "/fixture/append-guard/session.jsonl",
            1000,
        );
        upsert_source_inventory(&store, std::slice::from_ref(&file));
        store
            .mark_source_import_file_indexed(
                file.provider,
                SourceImportFileIndexUpdate {
                    source_root: root,
                    source_path: &file.source_path,
                    file_size_bytes: file.file_size_bytes,
                    file_modified_at_ms: file.file_modified_at_ms,
                    import_revision: file.import_revision,
                    inventory_generation: current_source_generation(&store, file.provider, root),
                    metadata: &file.metadata,
                    indexed_at_ms: 1001,
                },
            )
            .unwrap();
        if missing != "material" {
            upsert_source_material(&store, &file);
        }
        if missing != "checkpoint" {
            insert_matching_checkpoint(&store, &file);
        }

        file.file_size_bytes += 10;
        file.file_modified_at_ms += 1;
        file.observed_at_ms += 1;
        upsert_source_inventory(&store, std::slice::from_ref(&file));

        let fresh = store
            .list_source_import_file_work(file.provider, root, ImportWorkClass::Fresh, 10)
            .unwrap();
        assert_eq!(
            fresh[0].reason,
            ImportPendingReason::FreshChanged,
            "missing {missing}"
        );
        assert!(fresh[0].reason.requires_replacement());
    }
}

#[test]
fn catalog_changed_truncated_and_revision_failures_require_replacement() {
    for scenario in ["changed", "truncated", "revision"] {
        let temp = tempdir();
        let store = Store::open(temp.path().join(format!("{scenario}.sqlite"))).unwrap();
        let root = "/fixture/catalog-replacement";
        let source_path = format!("{root}/{scenario}.jsonl");
        let mut session = catalog_session_for_root(root, &source_path, scenario, 1000);
        upsert_catalog_inventory(&store, std::slice::from_ref(&session));
        store
            .mark_catalog_source_indexed(
                session.provider,
                CatalogSourceIndexUpdate {
                    source_root: root,
                    source_path: &source_path,
                    file_size_bytes: session.file_size_bytes,
                    file_modified_at_ms: session.file_modified_at_ms,
                    import_revision: session.import_revision,
                    inventory_generation: current_catalog_generation(
                        &store,
                        session.provider,
                        root,
                    ),
                    file_sha256: None,
                    event_count: Some(1),
                    indexed_at_ms: 1001,
                },
            )
            .unwrap();

        let expected_initial_reason = match scenario {
            "changed" => {
                session.file_modified_at_ms += 1;
                ImportPendingReason::FreshChanged
            }
            "truncated" => {
                session.file_size_bytes /= 2;
                session.file_modified_at_ms += 1;
                ImportPendingReason::FreshChanged
            }
            "revision" => {
                session.import_revision += 1;
                ImportPendingReason::ParserRevision
            }
            _ => unreachable!(),
        };
        session.cataloged_at_ms += 1;
        upsert_catalog_inventory(&store, std::slice::from_ref(&session));
        let initial_class = expected_initial_reason.class();
        let pending = store
            .list_catalog_import_work(session.provider, root, initial_class, 10)
            .unwrap();
        assert_eq!(pending[0].reason, expected_initial_reason, "{scenario}");

        store
            .record_catalog_source_import_result(
                session.provider,
                CatalogSourceIndexUpdate {
                    source_root: root,
                    source_path: &source_path,
                    file_size_bytes: session.file_size_bytes,
                    file_modified_at_ms: session.file_modified_at_ms,
                    import_revision: session.import_revision,
                    inventory_generation: current_catalog_generation(
                        &store,
                        session.provider,
                        root,
                    ),
                    file_sha256: None,
                    event_count: None,
                    indexed_at_ms: 1002,
                },
                CatalogIndexedStatus::Failed,
                Some("retry replacement"),
            )
            .unwrap();
        let recovery = store
            .list_catalog_import_work(session.provider, root, ImportWorkClass::Recovery, 10)
            .unwrap();
        assert_eq!(
            recovery[0].reason,
            ImportPendingReason::RecoveryReplacement,
            "{scenario}"
        );
        assert_eq!(recovery[0].reason.class(), ImportWorkClass::Recovery);
        assert!(recovery[0].reason.requires_replacement());

        upsert_catalog_inventory(&store, std::slice::from_ref(&session));
        let reclassified = store
            .list_catalog_import_work(session.provider, root, ImportWorkClass::Recovery, 10)
            .unwrap();
        assert_eq!(
            reclassified[0].reason,
            ImportPendingReason::RecoveryReplacement,
            "{scenario}"
        );

        session.file_size_bytes += 10;
        session.file_modified_at_ms += 1;
        session.cataloged_at_ms += 1;
        upsert_catalog_inventory(&store, std::slice::from_ref(&session));
        let after_growth = store
            .list_catalog_import_work(session.provider, root, ImportWorkClass::Recovery, 10)
            .unwrap();
        assert_eq!(
            after_growth[0].reason,
            ImportPendingReason::RecoveryReplacement,
            "{scenario} growth"
        );
    }
}

#[test]
fn source_growth_preserves_every_replacement_required_pending_reason() {
    for reason in [
        ImportPendingReason::FreshNew,
        ImportPendingReason::FreshChanged,
        ImportPendingReason::RecoveryReplacement,
        ImportPendingReason::ParserRevision,
        ImportPendingReason::MissingMaterial,
        ImportPendingReason::AbandonedPublication,
        ImportPendingReason::Legacy,
        ImportPendingReason::ExplicitRescan,
    ] {
        let temp = tempdir();
        let store = Store::open(temp.path().join(format!("{}.sqlite", reason.as_str()))).unwrap();
        let root = "/fixture/preserve-replacement";
        let mut file = source_import_file(
            CaptureProvider::Pi,
            "pi_session_jsonl",
            root,
            "/fixture/preserve-replacement/session.jsonl",
            1000,
        );
        upsert_source_inventory(&store, std::slice::from_ref(&file));
        store
            .conn
            .execute(
                "UPDATE source_import_files SET pending_reason = ?1 WHERE provider = ?2 AND source_root = ?3 AND source_path = ?4",
                params![reason.as_str(), file.provider.as_str(), root, &file.source_path],
            )
            .unwrap();

        file.file_size_bytes += 10;
        file.file_modified_at_ms += 1;
        file.observed_at_ms += 1;
        upsert_source_inventory(&store, std::slice::from_ref(&file));

        let work = store
            .list_source_import_file_work(file.provider, root, reason.class(), 10)
            .unwrap();
        assert_eq!(work[0].reason, reason, "{}", reason.as_str());
        assert!(work[0].reason.requires_replacement());
    }
}

#[test]
fn pending_work_queries_use_partial_reason_indexes() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("catalog.sqlite")).unwrap();
    store
        .conn
        .execute_batch(
            r#"
            WITH RECURSIVE rows(value) AS (
              SELECT 1 UNION ALL SELECT value + 1 FROM rows WHERE value < 4096
            )
            INSERT INTO catalog_sessions (
              source_path, provider, source_format, source_root, agent_type,
              file_size_bytes, file_modified_at_ms, cataloged_at_ms,
              indexed_status, pending_reason
            )
            SELECT printf('/fixture/catalog-%04d.jsonl', value), 'pi', 'pi_session_jsonl',
                   '/fixture', 'primary', 1, 1, 1, 'indexed',
                   CASE
                     WHEN value > 4092 THEN 'fresh_changed'
                     WHEN value > 4088 THEN 'recovery_replacement'
                   END
            FROM rows;

            WITH RECURSIVE rows(value) AS (
              SELECT 1 UNION ALL SELECT value + 1 FROM rows WHERE value < 4096
            )
            INSERT INTO source_import_files (
              provider, source_format, source_root, source_path,
              file_size_bytes, file_modified_at_ms, observed_at_ms,
              indexed_status, pending_reason
            )
            SELECT 'pi', 'pi_session_jsonl', '/fixture',
                   printf('/fixture/source-%04d.jsonl', value),
                   1, 1, 1, 'indexed',
                   CASE
                     WHEN value > 4092 THEN 'fresh_changed'
                     WHEN value > 4088 THEN 'recovery_replacement'
                   END
            FROM rows;

            ANALYZE;
            "#,
        )
        .unwrap();
    for (table, fresh_index, recovery_index) in [
        (
            "catalog_sessions",
            "idx_catalog_sessions_pending_fresh_attempt",
            "idx_catalog_sessions_pending_recovery_attempt",
        ),
        (
            "source_import_files",
            "idx_source_import_files_pending_fresh_attempt",
            "idx_source_import_files_pending_recovery_attempt",
        ),
    ] {
        for (reasons, order, index) in [
            (
                "'fresh_new', 'fresh_changed', 'fresh_append'",
                "indexed_at_ms, source_path",
                fresh_index,
            ),
            (
                "'recovery_retry', 'recovery_replacement', 'parser_revision', \
                 'missing_material', 'abandoned_publication', 'legacy', 'explicit_rescan'",
                "indexed_at_ms, source_path",
                recovery_index,
            ),
        ] {
            let sql = format!(
                "EXPLAIN QUERY PLAN SELECT source_path FROM {table} \
                 WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0 \
                   AND pending_reason IN ({reasons}) \
                 ORDER BY {order} LIMIT 64"
            );
            let mut stmt = store.conn.prepare(&sql).unwrap();
            let details = stmt
                .query_map(params![CaptureProvider::Pi.as_str(), "/fixture"], |row| {
                    row.get::<_, String>(3)
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert!(
                details.iter().any(|detail| detail.contains(index)),
                "query plan for {table} did not use {index}: {details:?}"
            );
            assert!(
                details
                    .iter()
                    .all(|detail| !detail.contains("USE TEMP B-TREE")),
                "query plan for {table} used a temp sort: {details:?}"
            );
        }
    }
}
