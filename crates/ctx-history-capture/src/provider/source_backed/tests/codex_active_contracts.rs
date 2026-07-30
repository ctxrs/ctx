use std::io::Write;

use super::*;

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
    let first = VerifiedIndex::open(&index)
        .unwrap()
        .source_event_page(&source, None, 8)
        .unwrap()
        .items
        .into_iter()
        .next()
        .unwrap();
    let first_request = EventHydrationRequest::new(first.event_id, first.locator).unwrap();

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
    assert_eq!(
        registry
            .resolver_registry()
            .hydrate_event(&first_request)
            .unwrap()
            .provider_bytes,
        b"explicitfrozenmarker"
    );
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
