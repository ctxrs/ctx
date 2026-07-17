const INVENTORY_CHECKPOINT_ROOT: &str = "/home/user/.codex/sessions";
const INVENTORY_DATABASE_IDENTITY: [u8; 32] = [88; 32];
const NO_PUBLICATION_STATE_MARKER: &str =
    "b558de1f76ca5db8c121394c9b0c61802f8de569b199244b17c5693487f18c99";

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
        store_schema_version: crate::current_history_store_schema_version(),
        scratch_identity,
        scratch_lock_identity,
        scratch_database_identity: &INVENTORY_DATABASE_IDENTITY,
        publication_state_marker: NO_PUBLICATION_STATE_MARKER,
        publication_owner: None,
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
    let selection_complete = discovery_complete && planned_path_count == 0;
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
        selection_complete,
        selection_commitment: selection_complete.then(|| {
            let format_version = crate::IMPORT_INVENTORY_SELECTION_FORMAT_VERSION;
            let algorithm_version = crate::IMPORT_INVENTORY_SELECTION_ALGORITHM_VERSION;
            crate::ImportInventoryFrozenSelectionCommitment {
                format_version,
                algorithm_version,
                total_count: 0,
                final_keyset: None,
                final_prefix: crate::import_inventory_selection_initial_prefix(
                    format_version,
                    algorithm_version,
                )
                .unwrap(),
            }
        }),
        replay_count,
        next_retry_at_ms: None,
        last_error: None,
    }
}

fn first_effect_membership(
    inventory_family: crate::ProviderFileInventoryFamily,
    provider: CaptureProvider,
    source_format: &str,
    source_root: &str,
    capture_journal_identity: &[u8; 32],
    native_path: crate::ImportInventoryNativePathIdentity<'_>,
    accounted_bytes: u64,
    effect: crate::ImportInventoryCanonicalEffect<'_>,
) -> (
    crate::ImportInventoryEffectMembership,
    crate::ImportInventoryFrozenSelectionCommitment,
) {
    let format_version = crate::IMPORT_INVENTORY_SELECTION_FORMAT_VERSION;
    let algorithm_version = crate::IMPORT_INVENTORY_SELECTION_ALGORITHM_VERSION;
    let prior_prefix =
        crate::import_inventory_selection_initial_prefix(format_version, algorithm_version)
            .unwrap();
    let canonical = crate::canonical_import_inventory_selection_step(
        crate::ImportInventorySelectionCanonicalizationRequest {
            format_version,
            algorithm_version,
            ordinal: 0,
            capture_journal_identity,
            native_path,
            inventory_family,
            provider,
            source_format,
            source_root,
            prior_keyset: None,
            resulting_keyset: capture_journal_identity,
            prior_prefix: &prior_prefix,
            accounted_bytes,
            effect,
        },
    )
    .unwrap();
    let commitment = crate::ImportInventoryFrozenSelectionCommitment {
        format_version,
        algorithm_version,
        total_count: 1,
        final_keyset: Some(*capture_journal_identity),
        final_prefix: canonical.resulting_prefix,
    };
    (
        crate::ImportInventoryEffectMembership {
            commitment_identity: crate::import_inventory_selection_commitment_identity(commitment)
                .unwrap(),
            ordinal: 0,
            prior_keyset: None,
            resulting_keyset: *capture_journal_identity,
            prior_prefix,
            resulting_prefix: canonical.resulting_prefix,
        },
        commitment,
    )
}

fn install_effective_inventory_publication(store: &Store, generation: u64) {
    store
        .conn
        .execute(
            "INSERT INTO provider_file_publications (\
               replacement_id, owner_id, publication_kind, staging_id, provider, \
               inventory_family, inventory_source_format, inventory_source_root, source_path, \
               material_source_format, material_source_root, inventory_generation, \
               file_size_bytes, file_modified_at_ms, import_revision, mutation_started, \
               started_at_ms, updated_at_ms\
             ) VALUES (\
               'checkpoint-publication', ?1, 'replacement', ?2, 'codex', \
               'catalog_sessions', 'codex_session_jsonl', ?3, ?4, \
               'codex_session_jsonl', ?3, ?5, 42, 100, 1, 1, 1, 1\
             )",
            params![
                "a".repeat(64),
                "b".repeat(64),
                INVENTORY_CHECKPOINT_ROOT,
                "/home/user/.codex/sessions/2026/session.jsonl",
                i64::try_from(generation).unwrap(),
            ],
        )
        .unwrap();
}

fn remove_effective_inventory_publication(store: &Store) {
    store
        .conn
        .execute(
            "DELETE FROM provider_file_publications \
             WHERE replacement_id = 'checkpoint-publication'",
            [],
        )
        .unwrap();
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
    let journal_identity = [8; 32];
    let native_path = crate::ImportInventoryNativePathIdentity {
        platform_tag: "unix",
        encoding_tag: "bytes",
        opaque_hash: &[9; 32],
    };
    let (membership, commitment) = first_effect_membership(
        crate::ProviderFileInventoryFamily::Catalog,
        CaptureProvider::Codex,
        "codex_session_jsonl",
        INVENTORY_CHECKPOINT_ROOT,
        &journal_identity,
        native_path,
        42,
        crate::ImportInventoryCanonicalEffect::CatalogUpsert(&session),
    );
    let request = crate::ImportInventoryPathEffectRequest {
        scratch: inventory_scratch(
            &scratch_identity,
            &discovered_integrity,
            &scratch_lock_identity,
            Some(&lease),
        ),
        capture_journal_identity: &journal_identity,
        native_path,
        membership,
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
        selection_complete: true,
        selection_commitment: Some(commitment),
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
                membership: crate::ImportInventoryEffectMembership {
                    resulting_keyset: [14; 32],
                    ..membership
                },
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
    install_effective_inventory_publication(&store, generation);
    assert!(matches!(
        store
            .apply_import_inventory_path_effect(&lease, request, 5)
            .unwrap_err(),
        StoreError::ImportInventoryCheckpointPublicationTransition { .. }
    ));
    let rows_after_transition: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM catalog_sessions WHERE source_path = ?1",
            [&session.source_path],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows_after_transition, 0);
    remove_effective_inventory_publication(&store);
    assert_eq!(
        store
            .apply_import_inventory_path_effect(&lease, request, 6)
            .unwrap(),
        crate::ImportInventoryPathEffectOutcome::Applied(crate::ImportInventoryEffectCounters {
            affected_rows: 1,
            affected_bytes: 42,
        })
    );
    assert_eq!(
        store
            .apply_import_inventory_path_effect(&lease, request, 7)
            .unwrap(),
        crate::ImportInventoryPathEffectOutcome::AlreadyApplied(
            crate::ImportInventoryEffectCounters {
                affected_rows: 1,
                affected_bytes: 42,
            }
        )
    );
    let conflicting_journal = [11; 32];
    let (mut conflicting_membership, _) = first_effect_membership(
        crate::ProviderFileInventoryFamily::Catalog,
        CaptureProvider::Codex,
        "codex_session_jsonl",
        INVENTORY_CHECKPOINT_ROOT,
        &conflicting_journal,
        native_path,
        42,
        crate::ImportInventoryCanonicalEffect::CatalogUpsert(&session),
    );
    conflicting_membership.commitment_identity = membership.commitment_identity;
    let conflicting = crate::ImportInventoryPathEffectRequest {
        capture_journal_identity: &conflicting_journal,
        membership: conflicting_membership,
        ..request
    };
    assert!(matches!(
        store
            .apply_import_inventory_path_effect(&lease, conflicting, 8)
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
        selection_complete: true,
        selection_commitment: Some(commitment),
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
        .record_import_inventory_capture_checkpoint(&lease, complete_capture, 9)
        .unwrap();
    let reconciliation = store
        .reconcile_import_inventory_store_rows_page(
            &lease,
            complete_capture.scratch,
            crate::ImportInventoryStoreReconciliationBudget {
                max_rows: 2,
                max_bytes: 4096,
            },
            10,
        )
        .unwrap();
    assert!(reconciliation.complete);
    assert_eq!(reconciliation.visited_rows, 1);
    assert_eq!(reconciliation.stale_rows, 0);
    install_effective_inventory_publication(&store, generation);
    assert!(matches!(
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
                11,
            )
            .unwrap_err(),
        StoreError::ImportInventoryCheckpointPublicationTransition { .. }
    ));
    remove_effective_inventory_publication(&store);
    let cleanup_proof = store
        .finalize_import_inventory_checkpoint(
            &lease,
            trust,
            crate::ImportInventoryCheckpointCompletionProof {
                capture: complete_capture,
                applied_path_count: 1,
                applied_row_count: 1,
                applied_bytes: 42,
            },
            12,
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
    assert_eq!(status.application_ordinal, 1);
    assert_eq!(status.application_keyset, Some(journal_identity.to_vec()));
    assert_eq!(status.application_prefix, commitment.final_prefix);
    assert_eq!(status.selection_commitment, Some(commitment));
    assert!(status.store_reconciliation_complete);
    assert_eq!(status.store_reconciliation_visited_rows, 1);
    assert_eq!(status.store_reconciliation_stale_rows, 0);
    assert_eq!(
        status.selection_commitment_identity,
        Some(crate::import_inventory_selection_commitment_identity(commitment).unwrap())
    );
    assert!(store
        .recoverable_import_inventory_checkpoint(
            crate::ProviderFileInventoryFamily::Catalog,
            CaptureProvider::Codex,
            INVENTORY_CHECKPOINT_ROOT,
        )
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .import_inventory_checkpoint_cleanup_proof(
                &[40; 32],
                crate::ProviderFileInventoryFamily::Catalog,
                CaptureProvider::Codex,
                INVENTORY_CHECKPOINT_ROOT,
            )
            .unwrap(),
        Some(cleanup_proof.clone())
    );
    let cleanup = store
        .advance_import_inventory_checkpoint_cleanup(
            &cleanup_proof,
            crate::ImportInventoryCleanupAdvance {
                expected_cleanup_keyset: None,
                cleanup_keyset: Some(b"capture-clean"),
                visited_rows_delta: 0,
                cleaned_rows_delta: 0,
                cleaned_bytes_delta: 0,
                disposition: crate::ImportInventoryCleanupDisposition::Complete,
            },
            13,
        )
        .unwrap();
    assert_eq!(
        cleanup.disposition,
        crate::ImportInventoryCleanupDisposition::Complete
    );
    assert_eq!(cleanup.attempt_count, 1);
}

#[test]
fn inventory_checkpoint_store_reconciliation_is_bounded_and_resumes_after_takeover() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    let prior_generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, INVENTORY_CHECKPOINT_ROOT)
        .unwrap();
    let selected = catalog_session(
        "/home/user/.codex/sessions/2026/selected.jsonl",
        "selected",
        1_700_000_000_000,
    );
    let missing_a = catalog_session(
        "/home/user/.codex/sessions/2026/missing-a.jsonl",
        "missing-a",
        1_700_000_000_001,
    );
    let missing_b = catalog_session(
        "/home/user/.codex/sessions/2026/missing-b.jsonl",
        "missing-b",
        1_700_000_000_002,
    );
    store
        .upsert_catalog_sessions(
            prior_generation,
            &[selected.clone(), missing_a.clone(), missing_b.clone()],
        )
        .unwrap();
    assert!(store
        .complete_catalog_inventory_generation(
            CaptureProvider::Codex,
            INVENTORY_CHECKPOINT_ROOT,
            prior_generation,
        )
        .unwrap());
    let generation = store
        .allocate_catalog_inventory_generation(CaptureProvider::Codex, INVENTORY_CHECKPOINT_ROOT)
        .unwrap();
    let scratch_identity = [91; 32];
    let scratch_integrity = [92; 32];
    let scratch_lock_identity = [93; 32];
    let trust = inventory_checkpoint_trust(
        generation,
        &[94; 32],
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
            "first-owner",
            0,
            100,
        )
        .unwrap();
    let first_lease = first.lease;
    let adopted = inventory_scratch(
        &scratch_identity,
        &scratch_integrity,
        &scratch_lock_identity,
        Some(&first_lease),
    );
    store
        .confirm_import_inventory_checkpoint_scratch_adoption(
            &first_lease,
            inventory_capture(adopted, None, false, false, true, 0, 0, 0, 0, 0),
            1,
        )
        .unwrap();
    let journal_identity = [95; 32];
    let native_path = crate::ImportInventoryNativePathIdentity {
        platform_tag: "unix",
        encoding_tag: "bytes",
        opaque_hash: &[96; 32],
    };
    let (membership, commitment) = first_effect_membership(
        crate::ProviderFileInventoryFamily::Catalog,
        CaptureProvider::Codex,
        "codex_session_jsonl",
        INVENTORY_CHECKPOINT_ROOT,
        &journal_identity,
        native_path,
        32,
        crate::ImportInventoryCanonicalEffect::CatalogUpsert(&selected),
    );
    let ready = crate::ImportInventoryCaptureCheckpoint {
        selection_keyset: Some(b"selection-eof"),
        selection_complete: true,
        selection_commitment: Some(commitment),
        ..inventory_capture(adopted, None, true, false, true, 1, 1, 1, 1, 0)
    };
    store
        .record_import_inventory_capture_checkpoint(&first_lease, ready, 2)
        .unwrap();
    store
        .apply_import_inventory_path_effect(
            &first_lease,
            crate::ImportInventoryPathEffectRequest {
                scratch: adopted,
                capture_journal_identity: &journal_identity,
                native_path,
                membership,
                accounted_bytes: 32,
                effect: crate::ImportInventoryCanonicalEffect::CatalogUpsert(&selected),
            },
            3,
        )
        .unwrap();
    let complete_capture = crate::ImportInventoryCaptureCheckpoint {
        effects_complete: true,
        ..ready
    };
    store
        .record_import_inventory_capture_checkpoint(&first_lease, complete_capture, 4)
        .unwrap();
    let first_page = store
        .reconcile_import_inventory_store_rows_page(
            &first_lease,
            adopted,
            crate::ImportInventoryStoreReconciliationBudget {
                max_rows: 1,
                max_bytes: 4096,
            },
            5,
        )
        .unwrap();
    assert!(!first_page.complete);
    assert_eq!(first_page.visited_rows, 1);
    assert!(first_page.visited_bytes <= 4096);
    drop(store);

    let reopened = Store::open(&path).unwrap();
    let takeover = reopened
        .acquire_import_inventory_checkpoint(trust, complete_capture, "takeover-owner", 101, 200)
        .unwrap();
    assert!(takeover.requires_scratch_adoption);
    let lease = takeover.lease;
    let adopted = inventory_scratch(
        &scratch_identity,
        &scratch_integrity,
        &scratch_lock_identity,
        Some(&lease),
    );
    let complete_capture = crate::ImportInventoryCaptureCheckpoint {
        scratch: adopted,
        ..complete_capture
    };
    reopened
        .confirm_import_inventory_checkpoint_scratch_adoption(&lease, complete_capture, 102)
        .unwrap();
    let mut prior_visited = first_page.visited_rows;
    let completed = loop {
        let page = reopened
            .reconcile_import_inventory_store_rows_page(
                &lease,
                adopted,
                crate::ImportInventoryStoreReconciliationBudget {
                    max_rows: 1,
                    max_bytes: 4096,
                },
                103,
            )
            .unwrap();
        assert!(page.visited_rows - prior_visited <= 1);
        assert!(page.visited_bytes <= 3 * 4096);
        prior_visited = page.visited_rows;
        if page.complete {
            break page;
        }
    };
    assert_eq!(completed.visited_rows, 3);
    assert_eq!(completed.stale_rows, 2);
    let stale: i64 = reopened
        .conn
        .query_row(
            "SELECT COUNT(*) FROM catalog_sessions \
             WHERE provider = 'codex' AND source_root = ?1 AND is_stale = 1",
            [INVENTORY_CHECKPOINT_ROOT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stale, 2);
    reopened
        .finalize_import_inventory_checkpoint(
            &lease,
            trust,
            crate::ImportInventoryCheckpointCompletionProof {
                capture: complete_capture,
                applied_path_count: 1,
                applied_row_count: 1,
                applied_bytes: 32,
            },
            104,
        )
        .unwrap();
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
        store_schema_version: crate::current_history_store_schema_version(),
        scratch_identity: &scratch_identity,
        scratch_lock_identity: &scratch_lock_identity,
        scratch_database_identity: &INVENTORY_DATABASE_IDENTITY,
        publication_state_marker: NO_PUBLICATION_STATE_MARKER,
        publication_owner: None,
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
    let file = source_import_file(
        CaptureProvider::Codex,
        "codex_session_jsonl",
        source_root,
        "/tmp/source-import/session.jsonl",
        1_700_000_000_000,
    );
    let journal_identity = [59; 32];
    let native_path = crate::ImportInventoryNativePathIdentity {
        platform_tag: "unix",
        encoding_tag: "bytes",
        opaque_hash: &[60; 32],
    };
    let (membership, commitment) = first_effect_membership(
        crate::ProviderFileInventoryFamily::SourceImport,
        CaptureProvider::Codex,
        "codex_session_jsonl",
        source_root,
        &journal_identity,
        native_path,
        42,
        crate::ImportInventoryCanonicalEffect::SourceImportUpsert(&file),
    );
    let ready = crate::ImportInventoryCaptureCheckpoint {
        selection_complete: true,
        selection_commitment: Some(commitment),
        ..inventory_capture(adopted.scratch, None, true, false, true, 1, 1, 1, 1, 0)
    };
    store
        .record_import_inventory_capture_checkpoint(&lease, ready, 2)
        .unwrap();
    let mut wrong_format = file.clone();
    wrong_format.source_format = "other_format".to_owned();
    assert!(matches!(
        store
            .apply_import_inventory_path_effect(
                &lease,
                crate::ImportInventoryPathEffectRequest {
                    scratch: ready.scratch,
                    capture_journal_identity: &journal_identity,
                    native_path,
                    membership,
                    accounted_bytes: 42,
                    effect: crate::ImportInventoryCanonicalEffect::SourceImportUpsert(
                        &wrong_format
                    ),
                },
                3,
            )
            .unwrap_err(),
        StoreError::ImportInventoryCheckpointTrustMismatch {
            field: "canonical effect scope"
        }
    ));
    assert!(matches!(
        store
            .apply_import_inventory_path_effect(
                &lease,
                crate::ImportInventoryPathEffectRequest {
                    scratch: ready.scratch,
                    capture_journal_identity: &journal_identity,
                    native_path: crate::ImportInventoryNativePathIdentity {
                        platform_tag: "unix",
                        encoding_tag: "bytes",
                        opaque_hash: &[62; 32],
                    },
                    membership,
                    accounted_bytes: 42,
                    effect: crate::ImportInventoryCanonicalEffect::SourceImportUpsert(&file),
                },
                3,
            )
            .unwrap_err(),
        StoreError::ImportInventoryCheckpointIdempotenceConflict
    ));
    let outcome = store
        .apply_import_inventory_path_effect(
            &lease,
            crate::ImportInventoryPathEffectRequest {
                scratch: ready.scratch,
                capture_journal_identity: &journal_identity,
                native_path,
                membership,
                accounted_bytes: 42,
                effect: crate::ImportInventoryCanonicalEffect::SourceImportUpsert(&file),
            },
            4,
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
    let cleanup_proof = store
        .abandon_import_inventory_checkpoint(&lease, adopted_scratch, "test cleanup", 2)
        .unwrap();

    let oversized = store
        .advance_import_inventory_checkpoint_cleanup(
            &cleanup_proof,
            crate::ImportInventoryCleanupAdvance {
                expected_cleanup_keyset: None,
                cleanup_keyset: Some(b"oversized-page"),
                visited_rows_delta: crate::IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_ROWS as u64 + 1,
                cleaned_rows_delta: crate::IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_ROWS as u64 + 1,
                cleaned_bytes_delta: 1,
                disposition: crate::ImportInventoryCleanupDisposition::Pending,
            },
            3,
        )
        .unwrap_err();
    assert!(matches!(
        oversized,
        StoreError::ImportInventoryCheckpointPageTooManyRows { .. }
    ));

    let first_page = store
        .advance_import_inventory_checkpoint_cleanup(
            &cleanup_proof,
            crate::ImportInventoryCleanupAdvance {
                expected_cleanup_keyset: None,
                cleanup_keyset: Some(b"page-1"),
                visited_rows_delta: 3,
                cleaned_rows_delta: 2,
                cleaned_bytes_delta: 64,
                disposition: crate::ImportInventoryCleanupDisposition::Pending,
            },
            4,
        )
        .unwrap();
    assert_eq!(first_page.visited_rows, 3);
    assert_eq!(first_page.cleaned_rows, 2);
    assert_eq!(first_page.attempt_count, 1);
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
    assert_eq!(status.owner_state, "inactive");
    assert_eq!(status.owner_epoch, lease.owner_epoch);
    assert_eq!(status.cleanup_keyset.as_deref(), Some(&b"page-1"[..]));
    assert_eq!(status.cleanup_visited_row_count, 3);
    assert_eq!(status.cleanup_row_count, 2);
    assert_eq!(status.cleanup_attempt_count, 1);

    let stale = store
        .advance_import_inventory_checkpoint_cleanup(
            &cleanup_proof,
            crate::ImportInventoryCleanupAdvance {
                expected_cleanup_keyset: None,
                cleanup_keyset: Some(b"stale-page"),
                visited_rows_delta: 1,
                cleaned_rows_delta: 1,
                cleaned_bytes_delta: 1,
                disposition: crate::ImportInventoryCleanupDisposition::Pending,
            },
            5,
        )
        .unwrap_err();
    assert!(matches!(
        stale,
        StoreError::ImportInventoryCheckpointStaleAuthority
    ));

    let complete = store
        .advance_import_inventory_checkpoint_cleanup(
            &cleanup_proof,
            crate::ImportInventoryCleanupAdvance {
                expected_cleanup_keyset: Some(b"page-1"),
                cleanup_keyset: Some(b"page-2"),
                visited_rows_delta: 1,
                cleaned_rows_delta: 1,
                cleaned_bytes_delta: 16,
                disposition: crate::ImportInventoryCleanupDisposition::Complete,
            },
            6,
        )
        .unwrap();
    assert_eq!(complete.visited_rows, 4);
    assert_eq!(complete.cleaned_rows, 3);
    assert_eq!(complete.attempt_count, 2);
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
    assert_eq!(status.cleanup_status, "complete");
    assert_eq!(status.cleanup_row_count, 3);
    assert_eq!(status.cleanup_bytes, 80);
    assert_eq!(status.cleanup_attempt_count, 2);
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
