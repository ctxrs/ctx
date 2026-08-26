use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::{Arc, Barrier},
};

use ctx_history_core::TypedKey;
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::*;
use crate::provider::codex::nativepath::{
    CodexPromptHistoryJsonlFamilyAdapterV0, CodexPromptHistoryProjector,
    CodexPromptHistorySourceBackedInputV0,
};
use crate::provider::source_backed::family::CaptureProviderRuntime;
use ctx_history_provider_runtime::JsonlFamilyProjector;

const SOURCE_FORMAT: &str = "codex_history_jsonl";
use crate::provider::source_backed::{
    family::jsonl::{
        jsonl_family_driver, set_after_jsonl_append_observation_route_binding_hook,
        set_before_jsonl_terminal_physical_revalidation_hook,
    },
    refresh_source_backed_generation, SourceBackedProviderRegistry, SourceBackedRoute,
    SourceBackedSelectorAuthority, SourceBackedSourceFailureClass,
};
use crate::test_support_paths::tempdir;
use crate::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};

fn prompt_line(session_id: &str, ts: i64, text: &str) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&json!({
        "session_id": session_id,
        "ts": ts,
        "text": text,
    }))
    .unwrap();
    bytes.push(b'\n');
    bytes
}

fn write_lines(path: &Path, lines: &[Vec<u8>]) {
    fs::write(path, lines.concat()).unwrap();
}

fn append(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

#[test]
fn active_source_family_contract_prompt_history_rejects_same_content_pathname_replacement() {
    let temp = tempdir().unwrap();
    let provider_root = temp.path().join("provider");
    fs::create_dir(&provider_root).unwrap();
    let history = provider_root.join("history.jsonl");
    write_lines(
        &history,
        &[prompt_line("session", 1_700_000_000, "first prompt")],
    );
    let input = CodexPromptHistorySourceBackedInputV0::explicit(&history, [6; 32]);
    let adapter =
        CodexPromptHistoryJsonlFamilyAdapterV0::<CaptureProviderRuntime>::new(input).unwrap();
    let driver = jsonl_family_driver(Arc::new(adapter.clone()), history.clone());
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(
        SourceBackedRoute::automatic(
            ProviderSource {
                provider: CaptureProvider::Codex,
                path: history.clone(),
                exists: true,
                source_format: SOURCE_FORMAT,
                source_kind: ProviderSourceKind::NativeHistory,
                import_support: ProviderImportSupport::Native,
                catalog_support: ProviderCatalogSupport::None,
                status: ProviderSourceStatus::Available,
                unsupported_reason: None,
                route_provenance: Default::default(),
            },
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver,
        )
        .unwrap(),
    );
    let index_root = temp.path().join("index");
    let initial =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();

    append(
        &history,
        &prompt_line("session", 1_700_000_001, "staged prompt"),
    );
    let replacement = provider_root.join("replacement.jsonl");
    fs::write(&replacement, fs::read(&history).unwrap()).unwrap();
    let moved = provider_root.join("scanned-history.jsonl");
    let hook_history = history.clone();
    set_before_jsonl_terminal_physical_revalidation_hook(history.clone(), move || {
        fs::rename(&hook_history, moved).unwrap();
        fs::rename(replacement, hook_history).unwrap();
    });

    let refreshed =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_eq!(refreshed.commit.generation_id, initial.commit.generation_id);
    assert_eq!(refreshed.failed_routes.len(), 1);
    assert_eq!(
        refreshed.failed_routes[0].class,
        SourceBackedSourceFailureClass::SourceChanged
    );
    assert!(refreshed.failed_routes[0].carried_forward);
    let retained = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(retained.generation_id(), initial.commit.generation_id);
    assert_eq!(retained.document_count(), 1);
}

#[cfg(unix)]
#[test]
fn active_source_family_contract_prompt_history_rejects_scanner_leaf_open_same_length_inode_swap() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().unwrap();
    let provider_root = temp.path().join("provider");
    fs::create_dir(&provider_root).unwrap();
    let history = provider_root.join("history.jsonl");
    write_lines(
        &history,
        &[prompt_line("session", 1_700_000_000, "retained seed")],
    );
    let input = CodexPromptHistorySourceBackedInputV0::explicit(&history, [19; 32]);
    let adapter =
        CodexPromptHistoryJsonlFamilyAdapterV0::<CaptureProviderRuntime>::new(input).unwrap();
    let driver = jsonl_family_driver(Arc::new(adapter.clone()), history.clone());
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(
        SourceBackedRoute::automatic(
            ProviderSource {
                provider: CaptureProvider::Codex,
                path: history.clone(),
                exists: true,
                source_format: SOURCE_FORMAT,
                source_kind: ProviderSourceKind::NativeHistory,
                import_support: ProviderImportSupport::Native,
                catalog_support: ProviderCatalogSupport::None,
                status: ProviderSourceStatus::Available,
                unsupported_reason: None,
                route_provenance: Default::default(),
            },
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver,
        )
        .unwrap(),
    );
    let index_root = temp.path().join("index");
    let initial =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();

    let scanner_bytes = prompt_line("session", 1_700_000_001, "old descriptor");
    let replacement_bytes = prompt_line("session", 1_700_000_001, "new descriptor");
    assert_eq!(scanner_bytes.len(), replacement_bytes.len());
    fs::write(&history, scanner_bytes).unwrap();
    let replacement = provider_root.join("replacement.jsonl");
    fs::write(&replacement, replacement_bytes).unwrap();
    assert_ne!(
        fs::metadata(&history).unwrap().ino(),
        fs::metadata(&replacement).unwrap().ino()
    );
    let moved = provider_root.join("scanner-opened.jsonl");
    let swap_path = history.clone();
    set_after_jsonl_append_observation_route_binding_hook(history.clone(), move || {
        fs::rename(&swap_path, moved).unwrap();
        fs::rename(replacement, swap_path).unwrap();
    });

    let failed =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_eq!(failed.commit.generation_id, initial.commit.generation_id);
    assert_eq!(failed.failed_routes.len(), 1);
    assert_eq!(
        failed.failed_routes[0].class,
        SourceBackedSourceFailureClass::SourceChanged
    );
    assert!(failed.failed_routes[0].carried_forward);
    let retained = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(retained.document_count(), 1);
    assert_eq!(
        retained
            .search_event_candidates("retained seed", 8)
            .unwrap()
            .len(),
        1
    );
    assert!(retained
        .search_event_candidates("descriptor", 8)
        .unwrap()
        .is_empty());
}

#[test]
fn active_source_family_contract_prompt_history_rejects_inflight_disappearance_then_deletes() {
    let temp = tempdir().unwrap();
    let history = temp.path().join("history.jsonl");
    write_lines(
        &history,
        &[prompt_line("session", 1_700_000_000, "retained prompt")],
    );
    let input = CodexPromptHistorySourceBackedInputV0::explicit(&history, [18; 32]);
    let adapter =
        CodexPromptHistoryJsonlFamilyAdapterV0::<CaptureProviderRuntime>::new(input).unwrap();
    let driver = jsonl_family_driver(Arc::new(adapter.clone()), history.clone());
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(
        SourceBackedRoute::automatic(
            ProviderSource {
                provider: CaptureProvider::Codex,
                path: history.clone(),
                exists: true,
                source_format: SOURCE_FORMAT,
                source_kind: ProviderSourceKind::NativeHistory,
                import_support: ProviderImportSupport::Native,
                catalog_support: ProviderCatalogSupport::None,
                status: ProviderSourceStatus::Available,
                unsupported_reason: None,
                route_provenance: Default::default(),
            },
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver,
        )
        .unwrap(),
    );
    let index_root = temp.path().join("index");
    let initial =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    append(
        &history,
        &prompt_line("session", 1_700_000_001, "discarded prompt"),
    );
    let removed = history.clone();
    set_after_jsonl_append_observation_route_binding_hook(history.clone(), move || {
        fs::remove_file(removed).unwrap();
    });

    let failed =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_eq!(failed.commit.generation_id, initial.commit.generation_id);
    assert_eq!(failed.failed_routes.len(), 1);
    assert_eq!(
        failed.failed_routes[0].class,
        SourceBackedSourceFailureClass::SourceChanged
    );
    assert!(failed.failed_routes[0].carried_forward);
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().document_count(),
        1
    );

    let deleted =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert!(
        deleted.failed_routes.is_empty(),
        "unexpected deletion failure: {:?}",
        deleted.failed_routes
    );
    assert_ne!(deleted.commit.generation_id, initial.commit.generation_id);
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().document_count(),
        0
    );
}

#[test]
fn active_source_family_contract_prompt_history_defers_live_suffix_exactly_once() {
    let temp = tempdir().unwrap();
    let history = temp.path().join("history.jsonl");
    write_lines(
        &history,
        &[prompt_line("session", 1_700_000_000, "frozen prompt")],
    );
    let input = CodexPromptHistorySourceBackedInputV0::explicit(&history, [14; 32]);
    let adapter =
        CodexPromptHistoryJsonlFamilyAdapterV0::<CaptureProviderRuntime>::new(input).unwrap();
    let driver = jsonl_family_driver(Arc::new(adapter.clone()), history.clone());
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(
        SourceBackedRoute::automatic(
            ProviderSource {
                provider: CaptureProvider::Codex,
                path: history.clone(),
                exists: true,
                source_format: SOURCE_FORMAT,
                source_kind: ProviderSourceKind::NativeHistory,
                import_support: ProviderImportSupport::Native,
                catalog_support: ProviderCatalogSupport::None,
                status: ProviderSourceStatus::Available,
                unsupported_reason: None,
                route_provenance: Default::default(),
            },
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver,
        )
        .unwrap(),
    );
    let index_root = temp.path().join("index");
    let cold =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let worker_barrier = Arc::clone(&barrier);
    let append_path = history.clone();
    let worker = std::thread::spawn(move || {
        worker_barrier.wait();
        append(
            &append_path,
            &prompt_line("session", 1_700_000_001, "deferred prompt"),
        );
        worker_barrier.wait();
    });
    set_before_jsonl_terminal_physical_revalidation_hook(history.clone(), move || {
        barrier.wait();
        barrier.wait();
    });

    let deferred =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    worker.join().unwrap();
    assert!(
        deferred.failed_routes.is_empty(),
        "unexpected route failures: {:?}",
        deferred.failed_routes
    );
    assert_eq!(deferred.commit.generation_id, cold.commit.generation_id);
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().document_count(),
        1
    );

    let caught_up =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert!(caught_up.failed_routes.is_empty());
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().document_count(),
        2
    );

    let no_op =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_eq!(no_op.commit.generation_id, caught_up.commit.generation_id);
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().document_count(),
        2
    );
}

#[test]
fn projector_preserves_prompt_identity_and_rejections() {
    use crate::provider::source_backed::family::jsonl::{JsonlFamilyWorkerContext, JsonlRecordRef};

    let input = CodexPromptHistorySourceBackedInputV0::explicit("history.jsonl", [7; 32]);
    let mut projector =
        CodexPromptHistoryProjector::<CaptureProviderRuntime>::for_test(input).unwrap();
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut records = Vec::new();
    let mut bytes = prompt_line("session-a", 1_700_000_000, "complete prompt body");
    bytes.pop();
    projector
        .project(
            JsonlRecordRef::for_test(&bytes, 4),
            &mut worker,
            &mut |record| {
                records.push(record);
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(
        record.content.normalized_body.as_deref(),
        Some("complete prompt body")
    );
    assert_eq!(record.provider_session_id.as_deref(), Some("session-a"));
    assert_eq!(record.native_event_id, Some(TypedKey::U64(4)));
    assert_eq!(record.event_sequence, 4);
    assert_eq!(record.occurred_at_unix_ms, Some(1_700_000_000_000));
    assert_eq!(record.role.as_deref(), Some("user"));
    assert!(record.validate_contract().is_ok());
    assert_eq!(
        format!("{:x}", Sha256::digest(record.encode_stored().unwrap())),
        "cf0c1b68ee1596cbb20215b77fed1bbb59c1ec8259cd723008523e7c57f0cdde"
    );

    for invalid in [
        b"not json".as_slice(),
        br#"{"session_id":"","ts":1,"text":"x"}"#,
    ] {
        projector
            .project(
                JsonlRecordRef::for_test(invalid, 5),
                &mut worker,
                &mut |_| Ok(()),
            )
            .unwrap();
    }
    assert_eq!(projector.rejected_records(), 2);
}

#[test]
fn active_source_family_contract_prompt_history_preserves_append_noop_rewrite_and_tail() {
    let temp = tempdir().unwrap();
    let history = temp.path().join("history.jsonl");
    write_lines(&history, &[prompt_line("s", 1_700_000_000, "one")]);
    let input = CodexPromptHistorySourceBackedInputV0::explicit(&history, [8; 32]);
    let adapter =
        CodexPromptHistoryJsonlFamilyAdapterV0::<CaptureProviderRuntime>::new(input).unwrap();
    let driver = jsonl_family_driver(Arc::new(adapter), history.clone());
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(
        SourceBackedRoute::automatic(
            ProviderSource {
                provider: CaptureProvider::Codex,
                path: history.clone(),
                exists: true,
                source_format: SOURCE_FORMAT,
                source_kind: ProviderSourceKind::NativeHistory,
                import_support: ProviderImportSupport::Native,
                catalog_support: ProviderCatalogSupport::None,
                status: ProviderSourceStatus::Available,
                unsupported_reason: None,
                route_provenance: Default::default(),
            },
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver,
        )
        .unwrap(),
    );
    let index_root = temp.path().join("index");

    let cold =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    let cold_id = cold.commit.generation_id;
    let index = VerifiedIndex::open(&index_root).unwrap();
    let one = index.search_event_candidates("one", 8).unwrap();
    assert_eq!(one.len(), 1);
    let first_event_id = one[0].event.event_id;

    append(&history, &prompt_line("s", 1_700_000_001, "two"));
    let appended =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_ne!(appended.commit.generation_id, cold_id);
    let appended_id = appended.commit.generation_id;
    let index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(index.document_count(), 2);
    assert_eq!(index.search_event_candidates("two", 8).unwrap().len(), 1);

    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert_eq!(unchanged.commit.generation_id, appended_id);

    write_lines(
        &history,
        &[
            prompt_line("s", 1_700_000_000, "rewritten one"),
            prompt_line("s", 1_700_000_001, "two"),
        ],
    );
    refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    let index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(index.document_count(), 2);
    let rewritten = index.search_event_candidates("rewritten", 8).unwrap();
    assert_eq!(rewritten.len(), 1);
    assert_eq!(rewritten[0].event.event_id, first_event_id);

    let mut tail = prompt_line("s", 1_700_000_002, "deferred tail");
    tail.pop();
    append(&history, &tail);
    refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    assert!(VerifiedIndex::open(&index_root)
        .unwrap()
        .search_event_candidates("deferred", 8)
        .unwrap()
        .is_empty());
    append(&history, b"\n");
    refresh_source_backed_generation(&index_root, &registry, WriterOptions::default()).unwrap();
    let index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(index.document_count(), 3);
    assert_eq!(
        index.search_event_candidates("deferred", 8).unwrap().len(),
        1
    );
}
