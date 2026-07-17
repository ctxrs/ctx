const PI_PUBLICATION_ROOT: &str = "/history/pi/sessions";
const PI_PUBLICATION_PATH_A: &str = "/history/pi/sessions/a.jsonl";
const PI_PUBLICATION_PATH_B: &str = "/history/pi/sessions/b.jsonl";
const PI_PUBLICATION_FORMAT: &str = "pi_session_jsonl";

fn pi_publication_file(source_path: &str, size: u64, modified_at_ms: i64) -> SourceImportFile {
    SourceImportFile {
        provider: CaptureProvider::Pi,
        source_format: PI_PUBLICATION_FORMAT.to_owned(),
        source_root: PI_PUBLICATION_ROOT.to_owned(),
        source_path: source_path.to_owned(),
        file_size_bytes: size,
        file_modified_at_ms: modified_at_ms,
        import_revision: 1,
        observed_at_ms: modified_at_ms,
        metadata: json!({"inventory_unit": "pi_session_file"}),
    }
}

fn pi_publication_checkpoint(
    file: &SourceImportFile,
    size: u64,
    lines: u64,
    identity: &str,
    updated_at_ms: i64,
) -> ProviderFileCheckpoint {
    ProviderFileCheckpoint {
        provider: file.provider,
        source_format: file.source_format.clone(),
        source_root: file.source_root.clone(),
        source_path: file.source_path.clone(),
        import_revision: file.import_revision,
        checkpoint_version: 1,
        stable_file_identity: identity.to_owned(),
        committed_byte_offset: size,
        committed_complete_line_count: lines,
        head_sha256: "a".repeat(64),
        boundary_sha256: "b".repeat(64),
        resume_state: None,
        updated_at_ms,
    }
}

fn persist_pi_publication_inventory(
    store: &Store,
    file_a: &SourceImportFile,
    file_b: &SourceImportFile,
) -> u64 {
    let generation = store
        .allocate_source_import_inventory_generation(CaptureProvider::Pi, PI_PUBLICATION_ROOT)
        .unwrap();
    store
        .upsert_source_import_files(generation, &[file_a.clone(), file_b.clone()])
        .unwrap();
    generation
}

fn insert_pi_publication_source(
    store: &Store,
    source_id: Uuid,
    source_path: &str,
    external_session_id: &str,
) {
    let mut source = capture_source_fixture(source_id, source_path, external_session_id);
    source.descriptor.provider = CaptureProvider::Pi;
    source.descriptor.source_format = Some(PI_PUBLICATION_FORMAT.to_owned());
    source.descriptor.source_root = Some(source_path.to_owned());
    store.upsert_capture_source(&source).unwrap();
}

fn assert_pi_publication_owner_a(store: &Store, scope: &ProviderFilePublicationScope) {
    let owner: (String, String, String) = store
        .conn
        .query_row(
            "SELECT material_source_format, material_source_root, source_path \
             FROM provider_file_publications WHERE replacement_id = ?1",
            params![scope.scope_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        owner,
        (
            PI_PUBLICATION_FORMAT.to_owned(),
            PI_PUBLICATION_PATH_A.to_owned(),
            PI_PUBLICATION_PATH_A.to_owned(),
        )
    );
}

#[test]
fn pi_file_owner_isolates_incremental_and_recovered_replacement_lifecycles() {
    let temp = tempdir().unwrap();
    let database_path = temp.path().join("work.sqlite");
    let store = Store::open(&database_path).unwrap();
    let file_a = pi_publication_file(PI_PUBLICATION_PATH_A, 20, 100);
    let file_b = pi_publication_file(PI_PUBLICATION_PATH_B, 20, 100);
    let first_generation = persist_pi_publication_inventory(&store, &file_a, &file_b);
    let initial_checkpoint =
        pi_publication_checkpoint(&file_a, 20, 2, "unix:2049:pi-a-initial", 105);
    store
        .upsert_provider_file_checkpoint(
            source_outcome(&file_a, first_generation, 105),
            &initial_checkpoint,
        )
        .unwrap();

    let source_a = Uuid::from_u128(410_001);
    let source_b = Uuid::from_u128(410_002);
    let initial_event_a = Uuid::from_u128(410_003);
    let incremental_event_a = Uuid::from_u128(410_004);
    let replacement_event_a = Uuid::from_u128(410_005);
    let event_b = Uuid::from_u128(410_006);
    let rejected_event_b = Uuid::from_u128(410_007);
    insert_pi_publication_source(&store, source_a, PI_PUBLICATION_PATH_A, "pi-a");
    insert_pi_publication_source(&store, source_b, PI_PUBLICATION_PATH_B, "pi-b");
    store
        .upsert_event(&event_fixture(
            initial_event_a,
            1,
            source_a,
            "pi-a-initial".to_owned(),
            "A initial material",
        ))
        .unwrap();
    let sibling_event = event_fixture(
        event_b,
        2,
        source_b,
        "pi-b-stable".to_owned(),
        "B stable material",
    );
    store.upsert_event(&sibling_event).unwrap();
    let sibling_source_before = store.get_capture_source(source_b).unwrap();
    let sibling_before = store.get_event(event_b).unwrap();

    let appended_a = pi_publication_file(PI_PUBLICATION_PATH_A, 30, 200);
    let append_generation = persist_pi_publication_inventory(&store, &appended_a, &file_b);
    let append_outcome = source_outcome(&appended_a, append_generation, 210);
    let append_scope = store
        .begin_provider_file_publication(
            CaptureProvider::Pi,
            append_outcome.observation,
            PI_PUBLICATION_FORMAT,
            ProviderFilePublicationKind::Incremental,
            205,
        )
        .unwrap();
    assert_pi_publication_owner_a(&store, &append_scope);
    store
        .with_provider_file_publication_writes(&append_scope, |store| {
            store.upsert_event(&event_fixture(
                incremental_event_a,
                3,
                source_a,
                "pi-a-incremental".to_owned(),
                "A incremental material",
            ))
        })
        .unwrap();
    let append_checkpoint =
        pi_publication_checkpoint(&appended_a, 30, 3, "unix:2049:pi-a-initial", 210);
    store
        .finalize_provider_file_publication(
            append_scope,
            append_outcome,
            ProviderFilePublicationCommit::Append(&append_checkpoint),
        )
        .unwrap();
    assert!(store.get_event(initial_event_a).is_ok());
    assert!(store.get_event(incremental_event_a).is_ok());
    assert_eq!(store.get_event(event_b).unwrap(), sibling_before);

    let rewritten_a = pi_publication_file(PI_PUBLICATION_PATH_A, 15, 300);
    let replacement_generation = persist_pi_publication_inventory(&store, &rewritten_a, &file_b);
    let replacement_outcome = source_outcome(&rewritten_a, replacement_generation, 310);
    let replacement_scope = store
        .begin_provider_file_publication(
            CaptureProvider::Pi,
            replacement_outcome.observation,
            PI_PUBLICATION_FORMAT,
            ProviderFilePublicationKind::Replacement,
            305,
        )
        .unwrap();
    assert_pi_publication_owner_a(&store, &replacement_scope);
    assert!(replacement_scope.tracks_prior_material());
    prepare_all(&store, &replacement_scope, 1);
    assert!(store.get_event(initial_event_a).is_err());
    assert!(store.get_event(incremental_event_a).is_err());
    assert_eq!(
        store.get_capture_source(source_b).unwrap(),
        sibling_source_before
    );
    assert_eq!(store.get_event(event_b).unwrap(), sibling_before);

    let rejected_sibling = event_fixture(
        rejected_event_b,
        4,
        source_b,
        "pi-b-rejected".to_owned(),
        "B must remain outside A publication",
    );
    assert!(matches!(
        store
            .with_provider_file_publication_writes(&replacement_scope, |store| {
                store.upsert_event(&rejected_sibling)
            })
            .unwrap_err(),
        StoreError::ProviderFilePublicationOwnerMismatch { .. }
    ));
    assert!(!row_exists(&store, "events", rejected_event_b));
    store
        .with_provider_file_publication_writes(&replacement_scope, |store| {
            store.upsert_event(&event_fixture(
                replacement_event_a,
                5,
                source_a,
                "pi-a-replacement".to_owned(),
                "A replacement material",
            ))
        })
        .unwrap();
    store
        .stage_provider_file_publication_completion(
            &replacement_scope,
            &ProviderFilePublicationCompletion {
                version: 1,
                payload: json!({"fixture": "pi-a-replacement"}),
            },
        )
        .unwrap();
    assert!(store.get_event(replacement_event_a).is_err());
    assert_eq!(store.get_event(event_b).unwrap(), sibling_before);
    store
        .abandon_provider_file_publication(replacement_scope)
        .unwrap();
    assert!(store.has_pending_provider_file_publications().unwrap());
    assert!(store.get_event(initial_event_a).is_err());
    assert!(store.get_event(incremental_event_a).is_err());
    assert!(store.get_event(replacement_event_a).is_err());
    assert_eq!(store.get_event(event_b).unwrap(), sibling_before);
    drop(store);

    let store = Store::open(&database_path).unwrap();
    let recovery_outcome = source_outcome(&rewritten_a, replacement_generation, 320);
    let recovered_scope = store
        .begin_provider_file_publication(
            CaptureProvider::Pi,
            recovery_outcome.observation,
            PI_PUBLICATION_FORMAT,
            ProviderFilePublicationKind::Incremental,
            315,
        )
        .unwrap();
    assert_eq!(
        recovered_scope.kind(),
        ProviderFilePublicationKind::Replacement
    );
    assert!(recovered_scope.tracks_prior_material());
    assert_pi_publication_owner_a(&store, &recovered_scope);
    assert_eq!(
        store
            .provider_file_publication_phase(&recovered_scope)
            .unwrap(),
        ProviderFilePublicationPhase::Reconciling
    );
    assert!(store.get_event(initial_event_a).is_err());
    assert!(store.get_event(incremental_event_a).is_err());
    assert!(store.get_event(replacement_event_a).is_err());
    assert_eq!(store.get_event(event_b).unwrap(), sibling_before);

    reconcile_all(&store, &recovered_scope, 1);
    let replacement_checkpoint =
        pi_publication_checkpoint(&rewritten_a, 15, 2, "unix:2049:pi-a-rewritten", 320);
    store
        .finalize_provider_file_publication(
            recovered_scope,
            recovery_outcome,
            ProviderFilePublicationCommit::Replacement(Some(&replacement_checkpoint)),
        )
        .unwrap();

    assert!(!store.has_pending_provider_file_publications().unwrap());
    assert!(!row_exists(&store, "events", initial_event_a));
    assert!(!row_exists(&store, "events", incremental_event_a));
    assert!(row_exists(&store, "events", replacement_event_a));
    assert_eq!(
        store.get_capture_source(source_b).unwrap(),
        sibling_source_before
    );
    assert_eq!(store.get_event(event_b).unwrap(), sibling_before);
    assert_eq!(
        store
            .provider_file_checkpoint(replacement_checkpoint.key())
            .unwrap(),
        Some(replacement_checkpoint)
    );
}
