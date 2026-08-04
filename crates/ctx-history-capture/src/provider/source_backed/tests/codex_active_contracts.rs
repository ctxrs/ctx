use std::io::Write;

use super::*;

fn assert_cold_route_failure(
    error: SourceBackedCoordinatorError,
    class: SourceBackedSourceFailureClass,
) {
    match error {
        SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes } => {
            assert_eq!(failed_routes.len(), 1);
            assert_eq!(failed_routes[0].class, class);
            assert!(!failed_routes[0].carried_forward);
        }
        error => panic!("expected one unusable source route, got {error:?}"),
    }
}

#[test]
fn active_source_family_contract_explicit_codex_append_catches_up() {
    let temp = tempdir().unwrap();
    let selected = temp.path().join("selected.jsonl");
    let index = temp.path().join("index");
    let native_session_id = "019facf0-3333-7777-8888-000000000003";
    fs::write(
        &selected,
        codex_rollout_bytes(native_session_id, &["explicitfrozenmarker"]),
    )
    .unwrap();

    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl",
            ProviderImportSupport::Explicit,
            &selected,
        ),
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();
    let cold = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(cold.commit.indexed_documents, 1);
    let source = cold.sources[0].observation().source().clone();
    let verified = VerifiedIndex::open(&index).unwrap();
    let first = verified
        .source_event_page(&source, None, 8)
        .unwrap()
        .items
        .into_iter()
        .next()
        .unwrap();
    let first_core = verified
        .core_record_by_id(first.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(
        first_core.content.normalized_body.as_deref(),
        Some("explicitfrozenmarker")
    );

    let append = codex_rollout_bytes(native_session_id, &["discarded", "explicitappendmarker"]);
    let second_line = append
        .split(|byte| *byte == b'\n')
        .nth(2)
        .expect("fixture has a second message");
    let mut file = fs::OpenOptions::new().append(true).open(&selected).unwrap();
    file.write_all(second_line).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();

    let observed_counters = Arc::new(Mutex::new(None));
    let captured_counters = Arc::clone(&observed_counters);
    super::super::set_after_explicit_codex_stage_hook(move |counters| {
        *captured_counters.lock().unwrap() = Some(counters);
    });
    let appended = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    let counters = observed_counters
        .lock()
        .unwrap()
        .take()
        .expect("explicit Codex append must report its selected disposition");
    assert_eq!(counters.appended_sources, 1);
    assert_eq!(counters.replaced_sources, 0);
    assert_eq!(counters.cold_sources, 0);
    assert_eq!(appended.commit.indexed_documents, 2);
    assert_eq!(
        VerifiedIndex::open(&index)
            .unwrap()
            .search_event_candidates("explicitappendmarker", 8)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn active_source_family_contract_explicit_codex_defers_append_after_staging() {
    let temp = tempdir().unwrap();
    let selected = temp.path().join("selected.jsonl");
    let index = temp.path().join("index");
    let native_session_id = "019facf0-3333-7777-8888-000000000004";
    fs::write(
        &selected,
        codex_rollout_bytes(native_session_id, &["explicitfrozenmarker"]),
    )
    .unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl",
            ProviderImportSupport::Explicit,
            &selected,
        ),
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();

    let append = codex_rollout_bytes(native_session_id, &["discarded", "deferredappendmarker"]);
    let second_line = append
        .split(|byte| *byte == b'\n')
        .nth(2)
        .expect("fixture has a second message")
        .to_vec();
    let append_path = selected.clone();
    super::super::set_after_explicit_codex_stage_hook(move |_| {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(append_path)
            .unwrap();
        file.write_all(&second_line).unwrap();
        file.write_all(b"\n").unwrap();
        file.sync_all().unwrap();
    });
    let frozen = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(frozen.commit.indexed_documents, 1);
    assert!(VerifiedIndex::open(&index)
        .unwrap()
        .search_event_candidates("deferredappendmarker", 8)
        .unwrap()
        .is_empty());

    let caught_up = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(caught_up.commit.indexed_documents, 2);
    assert_eq!(
        VerifiedIndex::open(&index)
            .unwrap()
            .search_event_candidates("deferredappendmarker", 8)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn active_source_family_contract_codex_tree_defers_append_after_staging() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019facf0-3333-7777-8888-000000000005";
    let selected = sessions.join(format!("rollout-{native_session_id}.jsonl"));
    fs::write(
        &selected,
        codex_rollout_bytes(native_session_id, &["treefrozenmarker"]),
    )
    .unwrap();

    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();

    let append = codex_rollout_bytes(native_session_id, &["discarded", "treeappendmarker"]);
    let appended_line = append
        .split_inclusive(|byte| *byte == b'\n')
        .nth(2)
        .expect("fixture has a second message")
        .to_vec();
    let append_path = selected.clone();
    super::super::set_after_codex_session_tree_stage_hook(move |_| {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(append_path)
            .unwrap();
        file.write_all(&appended_line).unwrap();
        file.sync_all().unwrap();
    });

    let frozen = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(frozen.commit.indexed_documents, 1);
    assert!(VerifiedIndex::open(&index)
        .unwrap()
        .search_event_candidates("treeappendmarker", 8)
        .unwrap()
        .is_empty());

    let caught_up = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(caught_up.commit.indexed_documents, 2);
    assert_eq!(
        VerifiedIndex::open(&index)
            .unwrap()
            .search_event_candidates("treeappendmarker", 8)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn active_source_family_contract_codex_tree_admits_append_during_cold_catalog() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let archived_sessions = temp.path().join("archived_sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&archived_sessions).unwrap();
    let native_session_id = "019facf0-3333-7777-8888-000000000006";
    let selected = sessions.join(format!("rollout-{native_session_id}.jsonl"));
    fs::write(
        &selected,
        codex_rollout_bytes(native_session_id, &["catalogfrozenmarker"]),
    )
    .unwrap();

    let append = codex_rollout_bytes(native_session_id, &["discarded", "catalogappendmarker"]);
    let appended_line = append
        .split_inclusive(|byte| *byte == b'\n')
        .nth(2)
        .expect("fixture has a second message")
        .to_vec();
    let append_path = selected.clone();
    crate::provider::codex::nativepath::install_after_codex_metadata_inventory_hook(move || {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(append_path)
            .unwrap();
        file.write_all(&appended_line).unwrap();
        file.sync_all().unwrap();
    });

    let mut registry = SourceBackedProviderRegistry::new();
    super::super::register_codex_session_tree_routes(
        &mut registry,
        vec![
            fixture_provider_source_at(
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                ProviderImportSupport::Native,
                &sessions,
            ),
            fixture_provider_source_at(
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                ProviderImportSupport::Native,
                &archived_sessions,
            ),
        ],
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();

    let cold = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(cold.commit.indexed_documents, 1);
    assert_eq!(
        VerifiedIndex::open(&index)
            .unwrap()
            .search_event_candidates("catalogappendmarker", 8)
            .unwrap()
            .len(),
        0
    );
    let catch_up = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(catch_up.commit.indexed_documents, 2);
    assert_eq!(
        VerifiedIndex::open(&index)
            .unwrap()
            .search_event_candidates("catalogappendmarker", 8)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn active_source_family_contract_codex_tree_defers_new_session_after_staging() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let first_session_id = "019facf0-3333-7777-8888-000000000007";
    fs::write(
        sessions.join(format!("rollout-{first_session_id}.jsonl")),
        codex_rollout_bytes(first_session_id, &["firsttreesessionmarker"]),
    )
    .unwrap();

    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();

    let second_session_id = "019facf0-3333-7777-8888-000000000008";
    let second_path = sessions.join(format!("rollout-{second_session_id}.jsonl"));
    super::super::set_after_codex_session_tree_stage_hook(move |_| {
        fs::write(
            second_path,
            codex_rollout_bytes(second_session_id, &["deferredtreesessionmarker"]),
        )
        .unwrap();
    });

    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };
    let frozen = refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();
    assert_eq!(frozen.commit.indexed_documents, 1);
    assert!(VerifiedIndex::open(&index)
        .unwrap()
        .search_event_candidates("deferredtreesessionmarker", 8)
        .unwrap()
        .is_empty());

    let caught_up = refresh_source_backed_generation(&index, &registry, options).unwrap();
    assert_eq!(caught_up.commit.indexed_documents, 2);
    assert_eq!(
        VerifiedIndex::open(&index)
            .unwrap()
            .search_event_candidates("deferredtreesessionmarker", 8)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn active_source_family_contract_codex_tree_rejects_captured_session_removal() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019facf0-3333-7777-8888-000000000009";
    let selected = sessions.join(format!("rollout-{native_session_id}.jsonl"));
    fs::write(
        &selected,
        codex_rollout_bytes(native_session_id, &["removedtreesessionmarker"]),
    )
    .unwrap();

    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    super::super::set_after_codex_session_tree_stage_hook(move |_| {
        fs::remove_file(selected).unwrap();
    });

    let error = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap_err();
    assert_cold_route_failure(error, SourceBackedSourceFailureClass::SourceChanged);
    assert!(VerifiedIndex::open(&index).is_err());
}

#[test]
fn active_source_family_contract_codex_tree_rejects_deleted_source_reappearance() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let nested = sessions.join("nested");
    let index = temp.path().join("index");
    fs::create_dir_all(&nested).unwrap();
    let native_session_id = "019facf0-3333-7777-8888-000000000010";
    let selected = nested.join(format!("rollout-{native_session_id}.jsonl"));
    fs::write(
        &selected,
        codex_rollout_bytes(native_session_id, &["deletionbasemarker"]),
    )
    .unwrap();

    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };
    let seeded = refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();
    let seeded_generation = seeded.commit.generation_id.clone();
    fs::remove_file(&selected).unwrap();

    let recreate_path = selected.clone();
    super::super::set_after_codex_session_tree_stage_hook(move |_| {
        crate::provider::codex::nativepath::install_after_codex_directory_visit_hook(
            PathBuf::from("nested"),
            move || {
                fs::write(
                    recreate_path,
                    codex_rollout_bytes(native_session_id, &["reappearedsourcemarker"]),
                )
                .unwrap();
            },
        );
    });
    let failed = refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();
    assert_carried_route_failure(
        &failed,
        &seeded_generation,
        SourceBackedSourceFailureClass::SourceChanged,
    );
    let preserved = VerifiedIndex::open(&index).unwrap();
    assert_eq!(preserved.generation_id(), seeded_generation);
    assert_eq!(
        preserved
            .search_event_candidates("deletionbasemarker", 8)
            .unwrap()
            .len(),
        1
    );
    assert!(preserved
        .search_event_candidates("reappearedsourcemarker", 8)
        .unwrap()
        .is_empty());

    let recovered = refresh_source_backed_generation(&index, &registry, options).unwrap();
    assert_ne!(recovered.commit.generation_id, seeded_generation);
    assert_eq!(
        VerifiedIndex::open(&index)
            .unwrap()
            .search_event_candidates("reappearedsourcemarker", 8)
            .unwrap()
            .len(),
        1
    );
}

#[cfg(unix)]
#[test]
fn active_source_family_contract_codex_tree_rejects_root_replacement_with_same_leaf() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let moved_sessions = temp.path().join("moved-sessions");
    let replacement = temp.path().join("replacement");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&replacement).unwrap();
    let native_session_id = "019facf0-3333-7777-8888-000000000011";
    let file_name = format!("rollout-{native_session_id}.jsonl");
    let selected = sessions.join(&file_name);
    fs::write(
        &selected,
        codex_rollout_bytes(native_session_id, &["retainedrootmarker"]),
    )
    .unwrap();
    fs::hard_link(&selected, replacement.join(&file_name)).unwrap();

    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();

    let replace_sessions = sessions.clone();
    super::super::set_after_codex_session_tree_stage_hook(move |_| {
        fs::rename(&replace_sessions, moved_sessions).unwrap();
        fs::rename(replacement, replace_sessions).unwrap();
    });
    let error = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap_err();
    assert_cold_route_failure(error, SourceBackedSourceFailureClass::SourceChanged);
    assert!(VerifiedIndex::open(&index).is_err());
}
