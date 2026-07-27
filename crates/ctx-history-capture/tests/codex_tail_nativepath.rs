use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_capture::{
    import_codex_session_jsonl, import_codex_session_jsonl_tail, CodexSessionImportOptions,
};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use tempfile::TempDir;

const TAIL_SESSION_ID: &str = "codex-nativepath-tail";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex_tail_nativepath")
        .join(name)
}

fn tempdir() -> TempDir {
    let temp_root = fs::canonicalize(std::env::temp_dir())
        .expect("system temporary directory should be canonicalizable");
    tempfile::Builder::new()
        .prefix("codex-tail-nativepath-")
        .tempdir_in(temp_root)
        .unwrap()
}

fn fixed_time(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

fn codex_options(path: &Path, imported_at: &str) -> CodexSessionImportOptions {
    CodexSessionImportOptions {
        machine_id: "codex-tail-nativepath-machine".to_owned(),
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
                .expect("Codex tail events should retain their text")
                .to_owned()
        })
        .collect()
}

#[test]
fn codex_tail_import_reports_an_incomplete_record_then_imports_it_when_completed() {
    let temp = tempdir();
    let path = temp.path().join("codex-tail.jsonl");
    fs::copy(fixture("initial.jsonl"), &path).unwrap();
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

    append(&path, &fs::read(fixture("append.jsonl")).unwrap());
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
