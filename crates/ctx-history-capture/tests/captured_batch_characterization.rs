use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_capture::{
    import_codex_session_jsonl, import_codex_session_jsonl_tail, import_provider_fixture_jsonl,
    CodexSessionImportOptions, ProviderAdapterContext, ProviderCaptureAdapter,
    ProviderFixtureImportOptions, ProviderFixtureJsonlAdapter,
};
use ctx_history_core::{CaptureProvider, Fidelity};
use ctx_history_store::Store;
use tempfile::TempDir;

const SOURCE_FORMAT: &str = "captured_batch_characterization_jsonl";
const NORMALIZED_SOURCE_PATH: &str = "fixture://captured-batch/normalized";
const TAIL_SESSION_ID: &str = "captured-batch-tail";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/captured_batch")
        .join(name)
}

fn tempdir() -> TempDir {
    let temp_root = fs::canonicalize(std::env::temp_dir())
        .expect("system temporary directory should be canonicalizable");
    tempfile::Builder::new()
        .prefix("captured-batch-characterization-")
        .tempdir_in(temp_root)
        .unwrap()
}

fn owned_fixture(temp: &TempDir, name: &str) -> PathBuf {
    let destination = temp.path().join(name);
    fs::copy(fixture(name), &destination).unwrap();
    destination
}

fn fixed_time(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

fn fixture_adapter() -> ProviderFixtureJsonlAdapter {
    ProviderFixtureJsonlAdapter {
        expected_provider: Some(CaptureProvider::Codex),
        source_format: SOURCE_FORMAT.to_owned(),
        fidelity: Fidelity::Imported,
    }
}

fn adapter_context(source_path: &str) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "captured-batch-machine".to_owned(),
        source_path: Some(PathBuf::from(source_path)),
        source_root: None,
        imported_at: fixed_time("2026-07-17T12:30:00Z"),
    }
}

fn fixture_import_options(source_path: &str) -> ProviderFixtureImportOptions {
    ProviderFixtureImportOptions {
        machine_id: "captured-batch-machine".to_owned(),
        source_path: Some(PathBuf::from(source_path)),
        imported_at: fixed_time("2026-07-17T12:30:00Z"),
        expected_provider: Some(CaptureProvider::Codex),
        source_format: SOURCE_FORMAT.to_owned(),
        fidelity: Fidelity::Imported,
        ..ProviderFixtureImportOptions::default()
    }
}

fn codex_options(path: &Path, imported_at: &str) -> CodexSessionImportOptions {
    CodexSessionImportOptions {
        machine_id: "captured-batch-machine".to_owned(),
        source_path: Some(path.to_path_buf()),
        imported_at: fixed_time(imported_at),
        ..CodexSessionImportOptions::default()
    }
}

fn append(path: &Path, bytes: &[u8]) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

fn stored_event_texts(store: &Store, provider_session_id: &str) -> Vec<String> {
    let sessions = store
        .sessions_by_external_session_limited(CaptureProvider::Codex, provider_session_id, 10)
        .unwrap();
    assert_eq!(sessions.len(), 1);
    store
        .events_for_session(sessions[0].id)
        .unwrap()
        .into_iter()
        .map(|event| {
            event.payload["body"]["text"]
                .as_str()
                .expect("characterization events should retain their normalized text")
                .to_owned()
        })
        .collect()
}

#[test]
fn repeated_jsonl_normalization_and_reimport_keep_exact_output_and_ids() {
    let temp = tempdir();
    let source = owned_fixture(&temp, "normalized.jsonl");
    let source_before = fs::read(&source).unwrap();
    let adapter = fixture_adapter();
    let context = adapter_context(NORMALIZED_SOURCE_PATH);

    let normalized_first = adapter.normalize_path(&source, &context).unwrap();
    let normalized_second = adapter.normalize_path(&source, &context).unwrap();

    assert_eq!(normalized_first.summary, normalized_second.summary);
    assert_eq!(normalized_first.captures, normalized_second.captures);
    assert_eq!(
        normalized_first.files_touched,
        normalized_second.files_touched
    );
    assert_eq!(normalized_first.summary.failed, 0);
    assert_eq!(normalized_first.captures.len(), 3);
    for (index, (line, capture)) in normalized_first.captures.iter().enumerate() {
        assert_eq!(*line, index + 1);
        assert_eq!(
            capture.source.idempotency_key.as_deref(),
            Some("provider-source:codex:captured_batch_characterization_jsonl:captured-batch-normalized")
        );
        assert_eq!(
            capture.session.idempotency_key.as_deref(),
            Some("provider-session:codex:captured-batch-normalized")
        );
        assert_eq!(
            capture.event.as_ref().unwrap().idempotency_key,
            Some(format!(
                "provider-event:codex:captured-batch-normalized:{index}"
            ))
        );
    }

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let first = import_provider_fixture_jsonl(
        &source,
        &mut store,
        fixture_import_options(NORMALIZED_SOURCE_PATH),
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported, 4);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 3);
    assert_eq!(first.skipped, 0);
    assert_eq!(fs::read(&source).unwrap(), source_before);

    let sessions_before = store
        .sessions_by_external_session_limited(
            CaptureProvider::Codex,
            "captured-batch-normalized",
            10,
        )
        .unwrap();
    assert_eq!(sessions_before.len(), 1);
    let events_before = store.events_for_session(sessions_before[0].id).unwrap();

    let second = import_provider_fixture_jsonl(
        &source,
        &mut store,
        fixture_import_options(NORMALIZED_SOURCE_PATH),
    )
    .unwrap();

    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported, 0);
    assert_eq!(second.skipped, 4);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 3);
    assert_eq!(fs::read(&source).unwrap(), source_before);
    assert_eq!(
        store
            .sessions_by_external_session_limited(
                CaptureProvider::Codex,
                "captured-batch-normalized",
                10,
            )
            .unwrap(),
        sessions_before
    );
    assert_eq!(
        store.events_for_session(sessions_before[0].id).unwrap(),
        events_before
    );
}

#[test]
fn malformed_jsonl_reports_every_record_and_keeps_valid_siblings() {
    let temp = tempdir();
    let source = owned_fixture(&temp, "malformed_mixed.jsonl");
    let logical_source = "fixture://captured-batch/malformed-mixed";
    let normalization = fixture_adapter()
        .normalize_path(&source, &adapter_context(logical_source))
        .unwrap();

    assert_eq!(normalization.summary.failed, 2);
    assert_eq!(normalization.summary.failures.len(), 2);
    assert_eq!(
        normalization
            .summary
            .failures
            .iter()
            .map(|failure| failure.line)
            .collect::<Vec<_>>(),
        vec![2, 4]
    );
    assert_eq!(
        normalization
            .captures
            .iter()
            .map(|(line, _)| *line)
            .collect::<Vec<_>>(),
        vec![1, 3, 5]
    );

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary =
        import_provider_fixture_jsonl(&source, &mut store, fixture_import_options(logical_source))
            .unwrap();

    assert_eq!(summary.failed, 2);
    assert_eq!(summary.failures, normalization.summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 3);
    assert!(summary.has_accepted_content());
    assert_eq!(
        stored_event_texts(&store, "captured-batch-malformed-mixed"),
        vec![
            "valid before malformed records",
            "valid between malformed records",
            "valid after malformed records",
        ]
    );
}

#[test]
fn codex_tail_import_reports_an_incomplete_record_then_imports_it_when_completed() {
    let temp = tempdir();
    let path = temp.path().join("codex-tail.jsonl");
    fs::copy(fixture("codex_tail_initial.jsonl"), &path).unwrap();
    let initial_end = fs::metadata(&path).unwrap().len();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let initial = import_codex_session_jsonl(
        &path,
        &mut store,
        codex_options(&path, "2026-07-17T12:30:00Z"),
    )
    .unwrap();
    assert_eq!(initial.failed, 0, "{:?}", initial.failures);
    assert_eq!(initial.imported_sessions, 1);
    assert_eq!(initial.imported_events, 1);

    append(
        &path,
        &fs::read(fixture("codex_tail_append.jsonl")).unwrap(),
    );
    let incomplete_start = fs::metadata(&path).unwrap().len();
    append(
        &path,
        br#"{"timestamp":"2026-07-17T12:00:03Z","type":"response_item","payload":{"type":"message","role":"assistant","content":["#,
    );
    let incomplete_source = fs::read(&path).unwrap();

    let incomplete = import_codex_session_jsonl_tail(
        &path,
        initial_end,
        &mut store,
        codex_options(&path, "2026-07-17T12:31:00Z"),
    )
    .unwrap();

    assert_eq!(incomplete.failed, 1, "{:?}", incomplete.failures);
    assert_eq!(incomplete.failures[0].line, 4);
    assert_eq!(incomplete.imported_sessions, 0);
    assert_eq!(incomplete.skipped_sessions, 1);
    assert_eq!(incomplete.imported_events, 1);
    assert_eq!(fs::read(&path).unwrap(), incomplete_source);
    assert_eq!(
        stored_event_texts(&store, TAIL_SESSION_ID),
        vec!["tail initial", "tail complete append"]
    );

    append(
        &path,
        br#"{"type":"output_text","text":"tail completed after retry"}]}}
"#,
    );
    let completed_source = fs::read(&path).unwrap();
    let completed = import_codex_session_jsonl_tail(
        &path,
        incomplete_start,
        &mut store,
        codex_options(&path, "2026-07-17T12:32:00Z"),
    )
    .unwrap();

    assert_eq!(completed.failed, 0, "{:?}", completed.failures);
    assert_eq!(completed.imported_sessions, 0);
    assert_eq!(completed.skipped_sessions, 1);
    assert_eq!(completed.imported_events, 1);
    assert_eq!(fs::read(&path).unwrap(), completed_source);
    assert_eq!(
        stored_event_texts(&store, TAIL_SESSION_ID),
        vec![
            "tail initial",
            "tail complete append",
            "tail completed after retry",
        ]
    );
}
