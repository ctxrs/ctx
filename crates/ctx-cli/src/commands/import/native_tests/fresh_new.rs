#[test]
fn fresh_new_outcome_maps_completion_deferral_and_admission() {
    let source = explicit_path_source(CaptureProvider::Pi, "/fixture/pi".into());
    let file = SourceImportFile {
        provider: CaptureProvider::Pi,
        source_format: source.source_format.to_owned(),
        source_root: "/fixture/pi".to_owned(),
        source_path: "/fixture/pi/session.jsonl".to_owned(),
        file_size_bytes: 42,
        file_modified_at_ms: 1,
        import_revision: 1,
        observed_at_ms: 1,
        metadata: json!({"change_token_v1": "token"}),
    };
    let work = SelectedImportWork::SourceFiles(vec![SourceImportFileWork {
        file: file.clone(),
        reason: ImportPendingReason::FreshNew,
        estimated_bytes: 42,
        last_attempt_at_ms: None,
        has_active_publication: false,
    }]);
    let preinventory = SourcePreinventory::SourceImportFiles {
        files: vec![file],
        inventory_generation: 7,
    };
    let (outcome, error) = provider_batch_outcome_from_fresh_new(
        &work,
        &preinventory,
        FreshNewImportOutcome {
            committed_paths: vec!["/fixture/pi/session.jsonl".to_owned()],
            maintenance_pending: true,
            ..FreshNewImportOutcome::default()
        },
    );

    assert!(error.is_none());
    assert_eq!(outcome.completed_units, 1);
    assert_eq!(outcome.completed_bytes, 42);
    assert_eq!(outcome.deferred_units, 0);
    assert!(outcome.durable_progress);
    assert!(outcome.stop_admission);
    assert_eq!(outcome.post_import_inventory_generation, Some(7));
}

#[test]
fn fresh_new_durable_only_path_is_deferred_without_reselection_loop() {
    let source = explicit_path_source(CaptureProvider::Pi, "/fixture/pi".into());
    let file = SourceImportFile {
        provider: CaptureProvider::Pi,
        source_format: source.source_format.to_owned(),
        source_root: "/fixture/pi".to_owned(),
        source_path: "/fixture/pi/session.jsonl".to_owned(),
        file_size_bytes: 42,
        file_modified_at_ms: 1,
        import_revision: 1,
        observed_at_ms: 1,
        metadata: json!({"change_token_v1": "token"}),
    };
    let work = SelectedImportWork::SourceFiles(vec![SourceImportFileWork {
        file: file.clone(),
        reason: ImportPendingReason::FreshNew,
        estimated_bytes: 42,
        last_attempt_at_ms: None,
        has_active_publication: false,
    }]);
    let preinventory = SourcePreinventory::SourceImportFiles {
        files: vec![file],
        inventory_generation: 7,
    };
    let (outcome, error) = provider_batch_outcome_from_fresh_new(
        &work,
        &preinventory,
        FreshNewImportOutcome {
            durable_only_paths: vec!["/fixture/pi/session.jsonl".to_owned()],
            ..FreshNewImportOutcome::default()
        },
    );

    assert!(error.is_none());
    assert_eq!(outcome.completed_units, 0);
    assert_eq!(outcome.deferred_units, 1);
    assert!(outcome.durable_progress);
    assert!(!outcome.stop_admission);
}
