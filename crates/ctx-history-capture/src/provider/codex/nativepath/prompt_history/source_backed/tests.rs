use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::{Arc, Barrier},
};

use ctx_history_core::{CoreRecord, TypedKey};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::json;

use super::*;
use crate::provider::source_backed::{
    family::jsonl::jsonl_family_driver, refresh_source_backed_generation,
    SourceBackedProviderRegistry, SourceBackedRoute, SourceBackedSelectorAuthority,
    SourceBackedSourceFailureClass,
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

fn collect(
    input: &CodexPromptHistorySourceBackedInputV0,
    prior: Option<&CertifiedSource>,
) -> (
    CodexPromptHistorySourceBackedScanV0,
    Vec<CoreRecord>,
    Vec<(usize, usize)>,
) {
    let source = observe_codex_prompt_history_source_backed_explicit_v0(input).unwrap();
    let mut records = Vec::new();
    let mut pages = Vec::new();
    let scan = scan_codex_prompt_history_source_backed_v0(source, prior, |page| {
        pages.push((page.records.len(), page.retained_bytes));
        records.extend(page.records);
        Ok(())
    })
    .unwrap();
    (scan, records, pages)
}

fn core_body(record: &CoreRecord) -> &str {
    record.content.normalized_body.as_deref().unwrap()
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
    let adapter = CodexPromptHistoryJsonlFamilyAdapterV0::new(input).unwrap();
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
    adapter.set_after_scan_hook(move || {
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

#[test]
fn active_source_family_contract_prompt_history_rejects_inflight_disappearance_then_deletes() {
    let temp = tempdir().unwrap();
    let history = temp.path().join("history.jsonl");
    write_lines(
        &history,
        &[prompt_line("session", 1_700_000_000, "retained prompt")],
    );
    let input = CodexPromptHistorySourceBackedInputV0::explicit(&history, [18; 32]);
    let adapter = CodexPromptHistoryJsonlFamilyAdapterV0::new(input).unwrap();
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
    adapter.set_after_scan_hook(move || fs::remove_file(removed).unwrap());

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
    let adapter = CodexPromptHistoryJsonlFamilyAdapterV0::new(input).unwrap();
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
    adapter.set_after_scan_hook(move || {
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
fn cold_scan_emits_complete_self_contained_core_records() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("history.jsonl");
    let long = "complete prompt body ".repeat(8_000);
    write_lines(
        &path,
        &[
            prompt_line("session-a", 1_700_000_000, &long),
            prompt_line("session-a", 1_700_000_001, "second prompt"),
        ],
    );
    let input = CodexPromptHistorySourceBackedInputV0::explicit(&path, [7; 32]);
    let (scan, records, pages) = collect(&input, None);

    assert!(matches!(
        scan.disposition,
        CodexPromptHistorySourceBackedDispositionV0::Cold
    ));
    assert_eq!(records.len(), 2);
    assert_eq!(core_body(&records[0]), long);
    assert_eq!(records[0].provider_session_id.as_deref(), Some("session-a"));
    assert_eq!(records[0].native_event_id, Some(TypedKey::U64(0)));
    assert_eq!(records[0].occurred_at_unix_ms, Some(1_700_000_000_000));
    assert_eq!(records[0].role.as_deref(), Some("user"));
    assert!(records
        .iter()
        .all(|record| record.validate_contract().is_ok()));
    assert!(pages
        .iter()
        .all(|(count, bytes)| *count <= PAGE_MAX_DOCUMENTS && *bytes <= PAGE_MAX_RETAINED_BYTES));
}

#[test]
fn active_source_family_contract_prompt_history_preserves_append_noop_and_rewrite_lifecycle() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("history.jsonl");
    write_lines(&path, &[prompt_line("s", 1_700_000_000, "one")]);
    let input = CodexPromptHistorySourceBackedInputV0::explicit(&path, [8; 32]);
    let (cold, cold_records, _) = collect(&input, None);

    append(&path, &prompt_line("s", 1_700_000_001, "two"));
    let (appended, appended_records, _) = collect(&input, Some(&cold.certificate));
    assert!(matches!(
        appended.disposition,
        CodexPromptHistorySourceBackedDispositionV0::Append
    ));
    assert_eq!(appended_records.len(), 1);
    assert_eq!(core_body(&appended_records[0]), "two");
    assert_eq!(appended_records[0].event_sequence, 1);

    let (unchanged, unchanged_records, _) = collect(&input, Some(&appended.certificate));
    assert!(matches!(
        unchanged.disposition,
        CodexPromptHistorySourceBackedDispositionV0::Unchanged
    ));
    assert!(unchanged_records.is_empty());

    write_lines(
        &path,
        &[
            prompt_line("s", 1_700_000_000, "rewritten one"),
            prompt_line("s", 1_700_000_001, "two"),
        ],
    );
    let (replacement, replacement_records, _) = collect(&input, Some(&appended.certificate));
    assert!(matches!(
        replacement.disposition,
        CodexPromptHistorySourceBackedDispositionV0::Replacement
    ));
    assert_eq!(replacement_records.len(), 2);
    assert_eq!(core_body(&replacement_records[0]), "rewritten one");
    assert_eq!(replacement_records[0].event_id, cold_records[0].event_id);
}

#[test]
fn incomplete_tail_is_deferred_until_terminated() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("history.jsonl");
    let complete = prompt_line("s", 1_700_000_000, "one");
    let mut partial = prompt_line("s", 1_700_000_001, "two");
    partial.pop();
    write_lines(&path, &[complete, partial]);
    let input = CodexPromptHistorySourceBackedInputV0::explicit(&path, [9; 32]);

    let (cold, records, _) = collect(&input, None);
    assert_eq!(records.len(), 1);
    assert!(!cold.terminal);
    append(&path, b"\n");
    let (appended, records, _) = collect(&input, Some(&cold.certificate));
    assert!(matches!(
        appended.disposition,
        CodexPromptHistorySourceBackedDispositionV0::Append
    ));
    assert_eq!(records.len(), 1);
    assert_eq!(core_body(&records[0]), "two");
    assert!(appended.terminal);
}

#[test]
fn pages_remain_bounded() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("history.jsonl");
    let lines = (0..(PAGE_MAX_DOCUMENTS + 3))
        .map(|index| {
            prompt_line(
                "s",
                1_700_000_000 + index as i64,
                &format!("prompt {index}"),
            )
        })
        .collect::<Vec<_>>();
    write_lines(&path, &lines);
    let input = CodexPromptHistorySourceBackedInputV0::explicit(&path, [10; 32]);
    let (_, records, pages) = collect(&input, None);
    assert_eq!(records.len(), PAGE_MAX_DOCUMENTS + 3);
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].0, PAGE_MAX_DOCUMENTS);
    assert!(pages
        .iter()
        .all(|(_, bytes)| *bytes <= PAGE_MAX_RETAINED_BYTES));
}
