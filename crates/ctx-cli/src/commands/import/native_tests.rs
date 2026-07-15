use super::*;
use crate::provider_args::NativeProviderArg;
use crate::provider_sources::explicit_path_source;
use ctx_history_core::{
    new_id, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Event, EventRole, EventType,
    Fidelity, SyncMetadata, SyncState, Visibility,
};
use ctx_history_store::{SourceImportFile, SourceImportFileIndexUpdate};
use serde_json::json;
use std::io::Write;
use std::sync::Arc;

fn tempdir() -> tempfile::TempDir {
    let temp_root = fs::canonicalize(std::env::temp_dir())
        .expect("system temporary directory should be canonicalizable");
    tempfile::Builder::new()
        .prefix("ctx-native-import-")
        .tempdir_in(temp_root)
        .unwrap()
}

include!("native_tests/selection_and_append.rs");
fn create_orphaned_pi_publication(
    data_root: &Path,
    prior_event_count: usize,
    retain_prior_events: bool,
) -> PathBuf {
    fs::create_dir_all(data_root).unwrap();
    let source_root = data_root.join("pi-sessions");
    let source_path = source_root.join("session.jsonl");
    fs::create_dir_all(&source_root).unwrap();
    fs::write(
        &source_path,
        format!(
            "{}{}",
            jsonl(json!({
                "type": "session", "id": "pi-retirement", "timestamp": "2026-07-14T12:00:00Z"
            })),
            jsonl(json!({
                "type": "message", "id": "pi-retirement-user", "timestamp": "2026-07-14T12:00:01Z",
                "message": {"role": "user", "content": "retirement fixture"}
            }))
        ),
    )
    .unwrap();
    let source = explicit_path_source(CaptureProvider::Pi, source_root.clone());
    let db_path = database_path(data_root.to_path_buf());
    let mut store = Store::open(&db_path).unwrap();
    run_single_fresh_unit(&mut store, source.clone());

    let template = store
        .export_archive()
        .unwrap()
        .events
        .into_iter()
        .next()
        .expect("Pi fixture must import an event");
    let mut prior_events = vec![template.clone()];
    for index in 0..prior_event_count {
        let mut event = template.clone();
        event.id = new_id();
        event.seq = 10_000 + index as u64;
        event.dedupe_key = None;
        event.payload = json!({"text": format!("prior retirement event {index}")});
        store.upsert_event(&event).unwrap();
        prior_events.push(event);
    }

    let inventory = inventory_import_sources(&store, vec![source], false).unwrap();
    let (file, generation) = match &inventory.sources[0].preinventory {
        SourcePreinventory::SourceImportFiles {
            files,
            inventory_generation,
        } => (files[0].clone(), *inventory_generation),
        other => panic!("unexpected Pi inventory: {other:?}"),
    };
    let material_source_format =
        provider_canonical_material_source_format(file.provider, &file.source_format).unwrap();
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            ProviderFileInventoryObservation::SourceImport {
                source_format: &file.source_format,
                update: SourceImportFileIndexUpdate {
                    source_root: &file.source_root,
                    source_path: &file.source_path,
                    file_size_bytes: file.file_size_bytes,
                    file_modified_at_ms: file.file_modified_at_ms,
                    import_revision: file.import_revision,
                    inventory_generation: generation,
                    metadata: &file.metadata,
                    indexed_at_ms: utc_now().timestamp_millis(),
                },
            },
            material_source_format,
            ProviderFilePublicationKind::Replacement,
            utc_now().timestamp_millis(),
        )
        .unwrap();
    loop {
        let progress = store
            .prepare_provider_file_publication_slice(&scope, 64)
            .unwrap();
        if progress.complete {
            break;
        }
    }
    if retain_prior_events {
        store
            .with_provider_file_publication_writes(&scope, |store| {
                for event in &prior_events {
                    store.upsert_event(event)?;
                }
                Ok(())
            })
            .unwrap();
    }
    let mut crash_event = template;
    crash_event.id = new_id();
    crash_event.seq = 20_000;
    crash_event.dedupe_key = None;
    crash_event.payload = json!({"text": "crash after mutation"});
    store
        .with_provider_file_publication_writes(&scope, |store| store.upsert_event(&crash_event))
        .unwrap();
    drop(scope);
    drop(store);

    fs::remove_file(&source_path).unwrap();
    let observer = Store::open(&db_path).unwrap();
    let tombstone_generation = observer
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    observer
        .mark_source_import_missing_paths_stale(
            file.provider,
            &file.source_root,
            &[],
            utc_now().timestamp_millis(),
            tombstone_generation,
        )
        .unwrap();
    assert_eq!(
        observer
            .provider_file_publication_retirement_work_count()
            .unwrap(),
        1
    );
    assert!(observer.export_archive().unwrap().events.is_empty());
    source_root
}

fn create_orphaned_pi_incremental_publication(data_root: &Path, owner_source_count: usize) {
    assert!(owner_source_count > 0);
    fs::create_dir_all(data_root).unwrap();
    let source_root = data_root.join("pi-incremental-sessions");
    let source_path = source_root.join("session.jsonl");
    fs::create_dir_all(&source_root).unwrap();
    fs::write(
        &source_path,
        format!(
            "{}{}",
            jsonl(json!({
                "type": "session", "id": "pi-preparation", "timestamp": "2026-07-14T12:00:00Z"
            })),
            jsonl(json!({
                "type": "message", "id": "pi-preparation-user", "timestamp": "2026-07-14T12:00:01Z",
                "message": {"role": "user", "content": "preparation fixture"}
            }))
        ),
    )
    .unwrap();
    let source = explicit_path_source(CaptureProvider::Pi, source_root.clone());
    let db_path = database_path(data_root.to_path_buf());
    let mut store = Store::open(&db_path).unwrap();
    run_single_fresh_unit(&mut store, source.clone());

    let archive = store.export_archive().unwrap();
    let template_source = archive
        .capture_sources
        .first()
        .expect("Pi fixture must import a capture source")
        .clone();
    for index in 1..owner_source_count {
        let mut extra_source = template_source.clone();
        extra_source.id = new_id();
        extra_source.descriptor.external_session_id = Some(format!("pi-preparation-{index}"));
        store.upsert_capture_source(&extra_source).unwrap();
    }
    let template_event = archive
        .events
        .first()
        .expect("Pi fixture must import an event")
        .clone();

    let inventory = inventory_import_sources(&store, vec![source], false).unwrap();
    let (file, generation) = match &inventory.sources[0].preinventory {
        SourcePreinventory::SourceImportFiles {
            files,
            inventory_generation,
        } => (files[0].clone(), *inventory_generation),
        other => panic!("unexpected Pi inventory: {other:?}"),
    };
    let material_source_format =
        provider_canonical_material_source_format(file.provider, &file.source_format).unwrap();
    let scope = store
        .begin_provider_file_publication(
            file.provider,
            ProviderFileInventoryObservation::SourceImport {
                source_format: &file.source_format,
                update: SourceImportFileIndexUpdate {
                    source_root: &file.source_root,
                    source_path: &file.source_path,
                    file_size_bytes: file.file_size_bytes,
                    file_modified_at_ms: file.file_modified_at_ms,
                    import_revision: file.import_revision,
                    inventory_generation: generation,
                    metadata: &file.metadata,
                    indexed_at_ms: utc_now().timestamp_millis(),
                },
            },
            material_source_format,
            ProviderFilePublicationKind::Incremental,
            utc_now().timestamp_millis(),
        )
        .unwrap();
    let mut mutation = template_event;
    mutation.id = new_id();
    mutation.seq = 30_000;
    mutation.dedupe_key = None;
    mutation.payload = json!({"text": "mutation before bounded preparation"});
    store
        .with_provider_file_publication_writes(&scope, |store| store.upsert_event(&mutation))
        .unwrap();
    assert!(matches!(
        store.abort_provider_file_publication(scope).unwrap(),
        std::ops::ControlFlow::Break(None)
    ));

    fs::remove_file(&source_path).unwrap();
    let tombstone_generation = store
        .allocate_source_import_inventory_generation(file.provider, &file.source_root)
        .unwrap();
    store
        .mark_source_import_missing_paths_stale(
            file.provider,
            &file.source_root,
            &[],
            utc_now().timestamp_millis(),
            tombstone_generation,
        )
        .unwrap();
    assert_eq!(
        store
            .provider_file_publication_retirement_work_count()
            .unwrap(),
        1
    );
}

#[test]
fn index_status_keeps_publication_retirement_in_lexical_pending_work() {
    let temp = tempdir();
    create_orphaned_pi_publication(temp.path(), 0, false);

    let status = crate::commands::index::index_status_snapshot(temp.path()).unwrap();
    assert_eq!(
        status["lexical"]["pending_provider_publication_retirements"],
        1
    );
    assert!(matches!(
        status["lexical"]["status"].as_str(),
        Some("pending" | "partial")
    ));
}

#[test]
fn setup_drains_orphaned_mutated_publication_retirement() {
    let temp = tempdir();
    let data_root = temp.path().join("data");
    let source_root = create_orphaned_pi_publication(&data_root, 2, false);
    let args = ImportArgs {
        provider: Some(NativeProviderArg::Pi),
        path: Some(source_root),
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
            allow_empty_sources: true,
            include_history_source_plugins: false,
            operation: "setup",
        },
    )
    .unwrap();
    assert_eq!(report.totals.recovery_units_processed, 1);
    assert_eq!(report.totals.recovery_units_pending, 0);
    let store = Store::open(database_path(data_root)).unwrap();
    assert!(!store.has_pending_provider_file_publications().unwrap());
    assert_eq!(store.export_archive().unwrap().events.len(), 1);
}

#[test]
fn daemon_cycles_restart_and_finish_bounded_publication_retirement() {
    let temp = tempdir();
    let data_root = temp.path().join("data");
    create_orphaned_pi_publication(&data_root, 130, false);

    let first = crate::commands::search::refresh_sources_for_search(
        &data_root,
        Vec::new(),
        Vec::new(),
        crate::commands::search::RefreshArg::Background,
        false,
        ImportExecutionPolicy::Daemon,
    )
    .unwrap();
    assert_eq!(first.recovery_units_processed, 0);
    assert_eq!(first.recovery_units_pending, 1);

    let mut pending = first.recovery_units_pending;
    let mut completed = 0usize;
    for _ in 0..16 {
        if pending == 0 {
            break;
        }
        let outcome = crate::commands::search::refresh_sources_for_search(
            &data_root,
            Vec::new(),
            Vec::new(),
            crate::commands::search::RefreshArg::Background,
            false,
            ImportExecutionPolicy::Daemon,
        )
        .unwrap();
        completed = completed.saturating_add(outcome.recovery_units_processed);
        pending = outcome.recovery_units_pending;
    }
    assert_eq!(pending, 0);
    assert_eq!(completed, 1);
    let store = Store::open(database_path(data_root)).unwrap();
    assert!(!store.has_pending_provider_file_publications().unwrap());
    assert_eq!(store.export_archive().unwrap().events.len(), 1);
}

#[test]
fn interrupted_incremental_retirement_preserves_history_without_prior_cleanup() {
    let temp = tempdir();
    let data_root = temp.path().join("data");
    create_orphaned_pi_incremental_publication(&data_root, 130);
    let db_path = database_path(data_root);

    let mut completed = false;
    let mut attempts = 0;
    for _ in 0..32 {
        let store = Store::open(&db_path).unwrap();
        let work = store
            .list_provider_file_publication_retirement_work(1)
            .unwrap();
        assert_eq!(work.len(), 1);
        let recovery =
            recover_provider_file_publication_retirement(&store, &work[0], false).unwrap();
        assert!(recovery.maintenance_warnings.is_empty());
        assert!(recovery.made_durable_progress);
        attempts += 1;
        if recovery.completed {
            completed = true;
            break;
        }
    }

    assert!(completed, "incremental retirement did not converge");
    assert_eq!(
        attempts, 1,
        "incremental history must not be treated as an incomplete replacement"
    );
    let store = Store::open(&db_path).unwrap();
    assert!(!store.has_pending_provider_file_publications().unwrap());
    assert_eq!(store.export_archive().unwrap().events.len(), 2);
}

#[test]
fn retirement_retained_candidates_converge_with_drain_false_across_store_reopens() {
    let temp = tempdir();
    let data_root = temp.path().join("data");
    create_orphaned_pi_publication(&data_root, 130, true);
    let db_path = database_path(data_root);

    let mut completed = false;
    let mut attempts = 0;
    for _ in 0..32 {
        let store = Store::open(&db_path).unwrap();
        let work = store
            .list_provider_file_publication_retirement_work(1)
            .unwrap();
        assert_eq!(work.len(), 1);
        let recovery =
            recover_provider_file_publication_retirement(&store, &work[0], false).unwrap();
        assert!(recovery.maintenance_warnings.is_empty());
        assert!(recovery.made_durable_progress);
        attempts += 1;
        if recovery.completed {
            completed = true;
            break;
        }
    }

    assert!(
        completed,
        "bounded retained-candidate cleanup did not converge"
    );
    assert!(
        attempts >= 3,
        "131 retained events must require multiple slices"
    );
    let store = Store::open(&db_path).unwrap();
    assert!(!store.has_pending_provider_file_publications().unwrap());
    assert_eq!(store.export_archive().unwrap().events.len(), 132);
}

#[test]
fn retirement_without_marker_or_current_observation_is_not_durable_progress() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let outcome = recover_provider_file_publication_retirement(
        &store,
        &ProviderFilePublicationRetirementWork {
            provider: CaptureProvider::Pi,
            material_source_format: "pi_session_jsonl".to_owned(),
            material_source_root: "/missing/pi-root".to_owned(),
            source_path: "/missing/pi-root/session.jsonl".to_owned(),
            estimated_bytes: 0,
            last_attempt_at_ms: 0,
        },
        false,
    )
    .unwrap();

    assert!(outcome.completed);
    assert!(!outcome.made_durable_progress);
    assert!(outcome.maintenance_warnings.is_empty());
}

include!("native_tests/append_recovery.rs");
include!("native_tests/root_and_inventory.rs");
include!("native_tests/manifest_and_sqlite.rs");
