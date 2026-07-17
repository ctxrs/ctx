const INVENTORY_CHECKPOINT_ROOT: &str = "/home/user/.codex/sessions";
const INVENTORY_DATABASE_IDENTITY: [u8; 32] = [88; 32];

fn inventory_checkpoint_trust<'a>(
    generation: u64,
    source_fingerprint: &'a [u8],
    scratch_identity: &'a [u8],
    scratch_lock_identity: &'a [u8],
) -> crate::ImportInventoryCheckpointTrust<'a> {
    crate::ImportInventoryCheckpointTrust {
        run_id: &[40; 32],
        inventory_family: crate::ProviderFileInventoryFamily::Catalog,
        provider: CaptureProvider::Codex,
        source_format: "codex_session_jsonl",
        source_root: INVENTORY_CHECKPOINT_ROOT,
        source_identity: &[1; 32],
        source_fingerprint,
        root_path: crate::ImportInventoryNativePathIdentity {
            platform_tag: "unix",
            encoding_tag: "bytes",
            opaque_hash: &[2; 32],
        },
        inventory_generation: generation,
        checkpoint_format_version: crate::IMPORT_INVENTORY_CHECKPOINT_FORMAT_VERSION,
        producer_build_id: &[41; 32],
        store_schema_version: crate::SCHEMA_VERSION as u32,
        scratch_identity,
        scratch_lock_identity,
        scratch_database_identity: &INVENTORY_DATABASE_IDENTITY,
    }
}

fn inventory_scratch<'a>(
    identity: &'a [u8],
    integrity: &'a [u8],
    lock_identity: &'a [u8],
    lease: Option<&'a crate::ImportInventoryCheckpointLease>,
) -> crate::ImportInventoryScratchState<'a> {
    crate::ImportInventoryScratchState::Trusted {
        identity,
        integrity,
        lock_identity,
        database_identity: &INVENTORY_DATABASE_IDENTITY,
        owner: lease.map(|lease| crate::ImportInventoryScratchOwner {
            owner_epoch: lease.owner_epoch,
            owner_token: &lease.owner_token,
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn inventory_capture<'a>(
    scratch: crate::ImportInventoryScratchState<'a>,
    active_directory: Option<crate::ImportInventoryActiveDirectoryProof<'a>>,
    discovery_complete: bool,
    effects_complete: bool,
    directory_queue_empty: bool,
    directory_count: u64,
    completed_directory_count: u64,
    planned_path_count: u64,
    discovered_path_count: u64,
    replay_count: u64,
) -> crate::ImportInventoryCaptureCheckpoint<'a> {
    crate::ImportInventoryCaptureCheckpoint {
        scratch,
        active_directory,
        discovery_complete,
        effects_complete,
        directory_queue_empty,
        directory_count,
        completed_directory_count,
        discovered_path_count,
        planned_path_count,
        selection_keyset: None,
        selection_eof: discovery_complete,
        selection_complete: discovery_complete,
        replay_count,
        next_retry_at_ms: None,
        last_error: None,
    }
}

#[test]
fn inventory_checkpoint_applies_concrete_payload_idempotently_and_publishes() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, INVENTORY_CHECKPOINT_ROOT)
        .unwrap();
    let scratch_identity = [3; 32];
    let scratch_integrity = [4; 32];
    let scratch_lock_identity = [5; 32];
    let trust = inventory_checkpoint_trust(
        generation,
        &[6; 32],
        &scratch_identity,
        &scratch_lock_identity,
    );
    let acquisition = store
        .start_import_inventory_checkpoint(
            trust,
            inventory_capture(
                inventory_scratch(
                    &scratch_identity,
                    &scratch_integrity,
                    &scratch_lock_identity,
                    None,
                ),
                None,
                false,
                false,
                true,
                0,
                0,
                0,
                0,
                0,
            ),
            "owner-a",
            0,
            100,
        )
        .unwrap();
    let lease = acquisition.lease;
    assert!(acquisition.requires_scratch_adoption);
    let recovery = store
        .recoverable_import_inventory_checkpoint(
            crate::ProviderFileInventoryFamily::Catalog,
            CaptureProvider::Codex,
            INVENTORY_CHECKPOINT_ROOT,
        )
        .unwrap()
        .unwrap();
    assert_eq!(recovery.run_id.as_slice(), &[40; 32]);
    assert_eq!(recovery.scratch_identity.as_slice(), &scratch_identity);
    assert_eq!(recovery.scratch_integrity.as_slice(), &scratch_integrity);
    assert_eq!(
        recovery.scratch_database_identity.as_slice(),
        INVENTORY_DATABASE_IDENTITY
    );
    assert_eq!(recovery.trust().inventory_generation, generation);

    let competing_run_id = [75; 32];
    let competing_trust = crate::ImportInventoryCheckpointTrust {
        run_id: &competing_run_id,
        ..trust
    };
    let split_brain = store
        .start_import_inventory_checkpoint(
            competing_trust,
            inventory_capture(
                inventory_scratch(
                    &scratch_identity,
                    &scratch_integrity,
                    &scratch_lock_identity,
                    None,
                ),
                None,
                false,
                false,
                true,
                0,
                0,
                0,
                0,
                0,
            ),
            "competing-owner",
            0,
            100,
        )
        .unwrap_err();
    assert!(matches!(
        split_brain,
        StoreError::ImportInventoryCheckpointInvariant(
            "source generation already has a durable checkpoint"
        )
    ));
    let pending_error = store
        .ensure_import_inventory_checkpoint_authority(
            &lease,
            inventory_scratch(
                &scratch_identity,
                &scratch_integrity,
                &scratch_lock_identity,
                Some(&lease),
            ),
            1,
        )
        .unwrap_err();
    assert!(matches!(
        pending_error,
        StoreError::ImportInventoryCheckpointStaleAuthority
    ));

    store
        .confirm_import_inventory_checkpoint_scratch_adoption(
            &lease,
            inventory_capture(
                inventory_scratch(
                    &scratch_identity,
                    &scratch_integrity,
                    &scratch_lock_identity,
                    Some(&lease),
                ),
                None,
                false,
                false,
                true,
                0,
                0,
                0,
                0,
                0,
            ),
            1,
        )
        .unwrap();
    let discovered_integrity = [7; 32];
    let session = catalog_session(
        "/home/user/.codex/sessions/2026/session.jsonl",
        "session-1",
        1_700_000_000_000,
    );
    let request = crate::ImportInventoryPathEffectRequest {
        scratch: inventory_scratch(
            &scratch_identity,
            &discovered_integrity,
            &scratch_lock_identity,
            Some(&lease),
        ),
        capture_journal_identity: &[8; 32],
        native_path: crate::ImportInventoryNativePathIdentity {
            platform_tag: "unix",
            encoding_tag: "bytes",
            opaque_hash: &[9; 32],
        },
        effect_fingerprint: &[10; 32],
        application_keyset: b"journal-1",
        accounted_bytes: 42,
        effect: crate::ImportInventoryCanonicalEffect::CatalogUpsert(&session),
    };
    assert!(matches!(
        store
            .apply_import_inventory_path_effect(&lease, request, 2)
            .unwrap_err(),
        StoreError::ImportInventoryCheckpointIncomplete(
            "capture journal membership is not complete"
        )
    ));
    let ready_capture = crate::ImportInventoryCaptureCheckpoint {
        selection_keyset: Some(b"selection-eof"),
        ..inventory_capture(
            inventory_scratch(
                &scratch_identity,
                &discovered_integrity,
                &scratch_lock_identity,
                Some(&lease),
            ),
            None,
            true,
            false,
            true,
            1,
            1,
            1,
            1,
            0,
        )
    };
    store
        .record_import_inventory_capture_checkpoint(&lease, ready_capture, 3)
        .unwrap();
    let mut oversized_session = session.clone();
    oversized_session.metadata = serde_json::json!({
        "oversized": "x".repeat(crate::IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES + 1)
    });
    let oversized = store
        .apply_import_inventory_path_effect(
            &lease,
            crate::ImportInventoryPathEffectRequest {
                native_path: crate::ImportInventoryNativePathIdentity {
                    platform_tag: "unix",
                    encoding_tag: "bytes",
                    opaque_hash: &[13; 32],
                },
                capture_journal_identity: &[14; 32],
                effect_fingerprint: &[15; 32],
                effect: crate::ImportInventoryCanonicalEffect::CatalogUpsert(&oversized_session),
                ..request
            },
            4,
        )
        .unwrap_err();
    assert!(matches!(
        oversized,
        StoreError::ImportInventoryCheckpointPageTooLarge { .. }
    ));
    assert_eq!(
        store
            .apply_import_inventory_path_effect(&lease, request, 5)
            .unwrap(),
        crate::ImportInventoryPathEffectOutcome::Applied(crate::ImportInventoryEffectCounters {
            affected_rows: 1,
            affected_bytes: 42,
        })
    );
    assert_eq!(
        store
            .apply_import_inventory_path_effect(&lease, request, 6)
            .unwrap(),
        crate::ImportInventoryPathEffectOutcome::AlreadyApplied
    );
    let conflicting = crate::ImportInventoryPathEffectRequest {
        capture_journal_identity: &[11; 32],
        ..request
    };
    assert!(matches!(
        store
            .apply_import_inventory_path_effect(&lease, conflicting, 7)
            .unwrap_err(),
        StoreError::ImportInventoryCheckpointIdempotenceConflict
    ));
    let rows: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM catalog_sessions WHERE source_path = ?1",
            [&session.source_path],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1);

    let complete_integrity = [12; 32];
    let complete_capture = crate::ImportInventoryCaptureCheckpoint {
        selection_keyset: Some(b"selection-eof"),
        ..inventory_capture(
            inventory_scratch(
                &scratch_identity,
                &complete_integrity,
                &scratch_lock_identity,
                Some(&lease),
            ),
            None,
            true,
            true,
            true,
            1,
            1,
            1,
            1,
            0,
        )
    };
    store
        .finalize_import_inventory_checkpoint(
            &lease,
            trust,
            crate::ImportInventoryCheckpointCompletionProof {
                capture: complete_capture,
                applied_path_count: 1,
                applied_row_count: 1,
                applied_bytes: 42,
            },
            8,
        )
        .unwrap();
    let status = store
        .import_inventory_checkpoint_status(
            &[40; 32],
            crate::ProviderFileInventoryFamily::Catalog,
            CaptureProvider::Codex,
            INVENTORY_CHECKPOINT_ROOT,
        )
        .unwrap()
        .unwrap();
    assert_eq!(status.status, "completed");
    assert_eq!(status.owner_state, "inactive");
    assert!(status.selection_eof);
    assert!(status.selection_complete);
    assert_eq!(
        status.selection_keyset.as_deref(),
        Some(&b"selection-eof"[..])
    );
    assert_eq!(
        status.scratch_database_identity.as_slice(),
        INVENTORY_DATABASE_IDENTITY
    );
    assert_eq!(status.applied_path_count, 1);
    assert!(store
        .recoverable_import_inventory_checkpoint(
            crate::ProviderFileInventoryFamily::Catalog,
            CaptureProvider::Codex,
            INVENTORY_CHECKPOINT_ROOT,
        )
        .unwrap()
        .is_none());
}

#[test]
fn inventory_checkpoint_applies_source_import_payload_inside_the_fence() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let source_root = "/tmp/source-import";
    let generation = store
        .allocate_source_import_inventory_generation(CaptureProvider::Codex, source_root)
        .unwrap();
    let scratch_identity = [51; 32];
    let scratch_integrity = [52; 32];
    let scratch_lock_identity = [53; 32];
    let trust = crate::ImportInventoryCheckpointTrust {
        run_id: &[54; 32],
        inventory_family: crate::ProviderFileInventoryFamily::SourceImport,
        provider: CaptureProvider::Codex,
        source_format: "codex_session_jsonl",
        source_root,
        source_identity: &[55; 32],
        source_fingerprint: &[56; 32],
        root_path: crate::ImportInventoryNativePathIdentity {
            platform_tag: "unix",
            encoding_tag: "bytes",
            opaque_hash: &[57; 32],
        },
        inventory_generation: generation,
        checkpoint_format_version: crate::IMPORT_INVENTORY_CHECKPOINT_FORMAT_VERSION,
        producer_build_id: &[58; 32],
        store_schema_version: crate::SCHEMA_VERSION as u32,
        scratch_identity: &scratch_identity,
        scratch_lock_identity: &scratch_lock_identity,
        scratch_database_identity: &INVENTORY_DATABASE_IDENTITY,
    };
    let acquisition = store
        .start_import_inventory_checkpoint(
            trust,
            inventory_capture(
                inventory_scratch(
                    &scratch_identity,
                    &scratch_integrity,
                    &scratch_lock_identity,
                    None,
                ),
                None,
                false,
                false,
                true,
                0,
                0,
                0,
                0,
                0,
            ),
            "source-owner",
            0,
            100,
        )
        .unwrap();
    let lease = acquisition.lease;
    let adopted = inventory_capture(
        inventory_scratch(
            &scratch_identity,
            &scratch_integrity,
            &scratch_lock_identity,
            Some(&lease),
        ),
        None,
        false,
        false,
        true,
        0,
        0,
        0,
        0,
        0,
    );
    store
        .confirm_import_inventory_checkpoint_scratch_adoption(&lease, adopted, 1)
        .unwrap();
    let ready = inventory_capture(adopted.scratch, None, true, false, true, 1, 1, 1, 1, 0);
    store
        .record_import_inventory_capture_checkpoint(&lease, ready, 2)
        .unwrap();
    let file = source_import_file(
        CaptureProvider::Codex,
        "codex_session_jsonl",
        source_root,
        "/tmp/source-import/session.jsonl",
        1_700_000_000_000,
    );
    let outcome = store
        .apply_import_inventory_path_effect(
            &lease,
            crate::ImportInventoryPathEffectRequest {
                scratch: ready.scratch,
                capture_journal_identity: &[59; 32],
                native_path: crate::ImportInventoryNativePathIdentity {
                    platform_tag: "unix",
                    encoding_tag: "bytes",
                    opaque_hash: &[60; 32],
                },
                effect_fingerprint: &[61; 32],
                application_keyset: b"source-journal-1",
                accounted_bytes: 42,
                effect: crate::ImportInventoryCanonicalEffect::SourceImportUpsert(&file),
            },
            3,
        )
        .unwrap();
    assert!(matches!(
        outcome,
        crate::ImportInventoryPathEffectOutcome::Applied(_)
    ));
    let rows: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM source_import_files \
             WHERE provider = 'codex' AND source_root = ?1 AND source_path = ?2",
            params![source_root, &file.source_path],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn inventory_checkpoint_cleanup_uses_abandoned_scratch_cas_without_a_second_owner() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, INVENTORY_CHECKPOINT_ROOT)
        .unwrap();
    let scratch_identity = [71; 32];
    let scratch_integrity = [72; 32];
    let scratch_lock_identity = [73; 32];
    let trust = inventory_checkpoint_trust(
        generation,
        &[74; 32],
        &scratch_identity,
        &scratch_lock_identity,
    );
    let acquisition = store
        .start_import_inventory_checkpoint(
            trust,
            inventory_capture(
                inventory_scratch(
                    &scratch_identity,
                    &scratch_integrity,
                    &scratch_lock_identity,
                    None,
                ),
                None,
                false,
                false,
                true,
                0,
                0,
                0,
                0,
                0,
            ),
            "cleanup-owner",
            0,
            100,
        )
        .unwrap();
    let lease = acquisition.lease;
    let adopted_scratch = inventory_scratch(
        &scratch_identity,
        &scratch_integrity,
        &scratch_lock_identity,
        Some(&lease),
    );
    store
        .confirm_import_inventory_checkpoint_scratch_adoption(
            &lease,
            inventory_capture(adopted_scratch, None, false, false, true, 0, 0, 0, 0, 0),
            1,
        )
        .unwrap();
    store
        .abandon_import_inventory_checkpoint(&lease, adopted_scratch, "test cleanup", 2)
        .unwrap();

    let oversized = store
        .advance_import_inventory_checkpoint_cleanup(
            trust,
            crate::ImportInventoryCleanupAdvance {
                scratch_identity: &scratch_identity,
                scratch_integrity: &scratch_integrity,
                scratch_lock_identity: &scratch_lock_identity,
                scratch_database_identity: &INVENTORY_DATABASE_IDENTITY,
                expected_cleanup_keyset: None,
                cleanup_keyset: b"oversized-page",
                cleaned_rows_delta: crate::IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_ROWS as u64 + 1,
                cleaned_bytes_delta: 1,
                complete: false,
            },
            3,
        )
        .unwrap_err();
    assert!(matches!(
        oversized,
        StoreError::ImportInventoryCheckpointPageTooManyRows { .. }
    ));

    store
        .advance_import_inventory_checkpoint_cleanup(
            trust,
            crate::ImportInventoryCleanupAdvance {
                scratch_identity: &scratch_identity,
                scratch_integrity: &scratch_integrity,
                scratch_lock_identity: &scratch_lock_identity,
                scratch_database_identity: &INVENTORY_DATABASE_IDENTITY,
                expected_cleanup_keyset: None,
                cleanup_keyset: b"page-1",
                cleaned_rows_delta: 2,
                cleaned_bytes_delta: 64,
                complete: false,
            },
            4,
        )
        .unwrap();
    let status = store
        .import_inventory_checkpoint_status(
            &[40; 32],
            crate::ProviderFileInventoryFamily::Catalog,
            CaptureProvider::Codex,
            INVENTORY_CHECKPOINT_ROOT,
        )
        .unwrap()
        .unwrap();
    assert_eq!(status.status, "cleaning");
    assert_eq!(status.owner_state, "inactive");
    assert_eq!(status.owner_epoch, lease.owner_epoch);
    assert_eq!(status.cleanup_keyset.as_deref(), Some(&b"page-1"[..]));
    assert_eq!(status.cleanup_row_count, 2);

    let stale = store
        .advance_import_inventory_checkpoint_cleanup(
            trust,
            crate::ImportInventoryCleanupAdvance {
                scratch_identity: &scratch_identity,
                scratch_integrity: &scratch_integrity,
                scratch_lock_identity: &scratch_lock_identity,
                scratch_database_identity: &INVENTORY_DATABASE_IDENTITY,
                expected_cleanup_keyset: None,
                cleanup_keyset: b"stale-page",
                cleaned_rows_delta: 1,
                cleaned_bytes_delta: 1,
                complete: false,
            },
            5,
        )
        .unwrap_err();
    assert!(matches!(
        stale,
        StoreError::ImportInventoryCheckpointStaleAuthority
    ));

    store
        .advance_import_inventory_checkpoint_cleanup(
            trust,
            crate::ImportInventoryCleanupAdvance {
                scratch_identity: &scratch_identity,
                scratch_integrity: &scratch_integrity,
                scratch_lock_identity: &scratch_lock_identity,
                scratch_database_identity: &INVENTORY_DATABASE_IDENTITY,
                expected_cleanup_keyset: Some(b"page-1"),
                cleanup_keyset: b"page-2",
                cleaned_rows_delta: 1,
                cleaned_bytes_delta: 16,
                complete: true,
            },
            6,
        )
        .unwrap();
    let status = store
        .import_inventory_checkpoint_status(
            &[40; 32],
            crate::ProviderFileInventoryFamily::Catalog,
            CaptureProvider::Codex,
            INVENTORY_CHECKPOINT_ROOT,
        )
        .unwrap()
        .unwrap();
    assert_eq!(status.status, "cleaned");
    assert_eq!(status.cleanup_status, "complete");
    assert_eq!(status.cleanup_row_count, 3);
    assert_eq!(status.cleanup_bytes, 80);
}

#[test]
fn inventory_checkpoint_takeover_recovers_both_scratch_adoption_crash_windows() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, INVENTORY_CHECKPOINT_ROOT)
        .unwrap();
    let scratch_identity = [21; 32];
    let scratch_integrity = [22; 32];
    let scratch_lock_identity = [23; 32];
    let trust = inventory_checkpoint_trust(
        generation,
        &[24; 32],
        &scratch_identity,
        &scratch_lock_identity,
    );
    let first = store
        .start_import_inventory_checkpoint(
            trust,
            inventory_capture(
                inventory_scratch(
                    &scratch_identity,
                    &scratch_integrity,
                    &scratch_lock_identity,
                    None,
                ),
                None,
                false,
                false,
                true,
                0,
                0,
                0,
                0,
                0,
            ),
            "owner-a",
            0,
            10,
        )
        .unwrap()
        .lease;
    let adopted_first = inventory_capture(
        inventory_scratch(
            &scratch_identity,
            &scratch_integrity,
            &scratch_lock_identity,
            Some(&first),
        ),
        None,
        false,
        false,
        true,
        0,
        0,
        0,
        0,
        0,
    );
    store
        .confirm_import_inventory_checkpoint_scratch_adoption(&first, adopted_first, 1)
        .unwrap();

    let second = store
        .acquire_import_inventory_checkpoint(trust, adopted_first, "owner-b", 11, 20)
        .unwrap()
        .lease;
    assert!(matches!(
        store
            .ensure_import_inventory_checkpoint_authority(&first, adopted_first.scratch, 12)
            .unwrap_err(),
        StoreError::ImportInventoryCheckpointStaleAuthority
    ));
    let third = store
        .acquire_import_inventory_checkpoint(trust, adopted_first, "owner-c", 21, 30)
        .unwrap()
        .lease;
    assert!(third.owner_epoch > second.owner_epoch);

    let adopted_third = inventory_capture(
        inventory_scratch(
            &scratch_identity,
            &[25; 32],
            &scratch_lock_identity,
            Some(&third),
        ),
        Some(crate::ImportInventoryActiveDirectoryProof {
            path: crate::ImportInventoryNativePathIdentity {
                platform_tag: "unix",
                encoding_tag: "bytes",
                opaque_hash: &[26; 32],
            },
            directory_identity: &[27; 32],
            directory_fingerprint: &[28; 32],
            scratch_identity: &scratch_identity,
            scratch_integrity: &[25; 32],
            scratch_lock_identity: &scratch_lock_identity,
            scratch_database_identity: &INVENTORY_DATABASE_IDENTITY,
            attempt_count: 1,
            replay_count: 0,
            observed_entries: 5,
            next_retry_at_ms: None,
        }),
        false,
        false,
        true,
        1,
        0,
        0,
        1,
        0,
    );
    let fourth = store
        .acquire_import_inventory_checkpoint(trust, adopted_third, "owner-d", 31, 40)
        .unwrap()
        .lease;
    assert!(fourth.owner_epoch > third.owner_epoch);
    let adopted_fourth = inventory_capture(
        inventory_scratch(
            &scratch_identity,
            &[29; 32],
            &scratch_lock_identity,
            Some(&fourth),
        ),
        Some(crate::ImportInventoryActiveDirectoryProof {
            path: crate::ImportInventoryNativePathIdentity {
                platform_tag: "unix",
                encoding_tag: "bytes",
                opaque_hash: &[26; 32],
            },
            directory_identity: &[27; 32],
            directory_fingerprint: &[28; 32],
            scratch_identity: &scratch_identity,
            scratch_integrity: &[29; 32],
            scratch_lock_identity: &scratch_lock_identity,
            scratch_database_identity: &INVENTORY_DATABASE_IDENTITY,
            attempt_count: 2,
            replay_count: 1,
            observed_entries: 7,
            next_retry_at_ms: Some(35),
        }),
        false,
        false,
        true,
        1,
        0,
        0,
        2,
        1,
    );
    store
        .confirm_import_inventory_checkpoint_scratch_adoption(&fourth, adopted_fourth, 32)
        .unwrap();
    let status = store
        .import_inventory_checkpoint_status(
            &[40; 32],
            crate::ProviderFileInventoryFamily::Catalog,
            CaptureProvider::Codex,
            INVENTORY_CHECKPOINT_ROOT,
        )
        .unwrap()
        .unwrap();
    assert_eq!(status.owner_state, "active");
    assert!(status.active_directory.is_some());
    assert_eq!(status.attempt_count, 2);
    assert_eq!(status.replay_count, 1);
    assert_eq!(status.discovered_path_count, 2);
    assert_eq!(
        status
            .active_directory
            .as_ref()
            .and_then(|active| active.next_retry_at_ms),
        Some(35)
    );
    let cleared_without_completion = store
        .record_import_inventory_capture_checkpoint(
            &fourth,
            inventory_capture(
                inventory_scratch(
                    &scratch_identity,
                    &[29; 32],
                    &scratch_lock_identity,
                    Some(&fourth),
                ),
                None,
                false,
                false,
                true,
                1,
                0,
                0,
                2,
                1,
            ),
            33,
        )
        .unwrap_err();
    assert!(matches!(
        cleared_without_completion,
        StoreError::ImportInventoryCheckpointTrustMismatch {
            field: "active directory changed or cleared without completion"
        }
    ));

    let missing = store
        .acquire_import_inventory_checkpoint(
            trust,
            crate::ImportInventoryCaptureCheckpoint {
                scratch: crate::ImportInventoryScratchState::Missing,
                ..adopted_fourth
            },
            "owner-e",
            41,
            50,
        )
        .unwrap_err();
    assert!(matches!(
        missing,
        StoreError::ImportInventoryCheckpointScratchMissing
    ));
    let status = store
        .import_inventory_checkpoint_status(
            &[40; 32],
            crate::ProviderFileInventoryFamily::Catalog,
            CaptureProvider::Codex,
            INVENTORY_CHECKPOINT_ROOT,
        )
        .unwrap()
        .unwrap();
    assert_eq!(status.status, "abandoned");
    assert_eq!(status.cleanup_status, "blocked");
}

#[test]
fn inventory_checkpoint_recovery_abandons_corrupt_tampered_and_replaced_sources() {
    for (case, scratch_state, replacement_fingerprint) in [
        ("corrupt", crate::ImportInventoryScratchState::Corrupt, None),
        (
            "tampered",
            crate::ImportInventoryScratchState::Tampered,
            None,
        ),
        (
            "root-replaced",
            crate::ImportInventoryScratchState::Trusted {
                identity: &[31; 32],
                integrity: &[32; 32],
                lock_identity: &[33; 32],
                database_identity: &INVENTORY_DATABASE_IDENTITY,
                owner: None,
            },
            Some(&[99; 32][..]),
        ),
    ] {
        let temp = tempdir();
        let store = Store::open(temp.path().join(format!("{case}.sqlite"))).unwrap();
        let generation = store
            .allocate_catalog_inventory_generation(
                CaptureProvider::Codex,
                INVENTORY_CHECKPOINT_ROOT,
            )
            .unwrap();
        let scratch_identity = [31; 32];
        let scratch_integrity = [32; 32];
        let scratch_lock_identity = [33; 32];
        let fingerprint = [34; 32];
        let trust = inventory_checkpoint_trust(
            generation,
            &fingerprint,
            &scratch_identity,
            &scratch_lock_identity,
        );
        let first = store
            .start_import_inventory_checkpoint(
                trust,
                inventory_capture(
                    inventory_scratch(
                        &scratch_identity,
                        &scratch_integrity,
                        &scratch_lock_identity,
                        None,
                    ),
                    None,
                    false,
                    false,
                    true,
                    0,
                    0,
                    0,
                    0,
                    0,
                ),
                "owner-a",
                0,
                10,
            )
            .unwrap()
            .lease;
        store
            .confirm_import_inventory_checkpoint_scratch_adoption(
                &first,
                inventory_capture(
                    inventory_scratch(
                        &scratch_identity,
                        &scratch_integrity,
                        &scratch_lock_identity,
                        Some(&first),
                    ),
                    None,
                    false,
                    false,
                    true,
                    0,
                    0,
                    0,
                    0,
                    0,
                ),
                1,
            )
            .unwrap();
        let recovery_trust = inventory_checkpoint_trust(
            generation,
            replacement_fingerprint.unwrap_or(&fingerprint),
            &scratch_identity,
            &scratch_lock_identity,
        );
        let observed_scratch = match scratch_state {
            crate::ImportInventoryScratchState::Trusted { .. } => inventory_scratch(
                &scratch_identity,
                &scratch_integrity,
                &scratch_lock_identity,
                Some(&first),
            ),
            other => other,
        };
        let error = store
            .acquire_import_inventory_checkpoint(
                recovery_trust,
                inventory_capture(observed_scratch, None, false, false, true, 0, 0, 0, 0, 0),
                "owner-b",
                11,
                20,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            StoreError::ImportInventoryCheckpointScratchCorrupt
                | StoreError::ImportInventoryCheckpointScratchTampered
                | StoreError::ImportInventoryCheckpointTrustMismatch { .. }
        ));
        let status = store
            .import_inventory_checkpoint_status(
                &[40; 32],
                crate::ProviderFileInventoryFamily::Catalog,
                CaptureProvider::Codex,
                INVENTORY_CHECKPOINT_ROOT,
            )
            .unwrap()
            .unwrap();
        assert_eq!(status.status, "abandoned", "{case}");
    }
}
