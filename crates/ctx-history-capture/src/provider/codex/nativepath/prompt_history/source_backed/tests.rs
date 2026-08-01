use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use ctx_history_core::{CoreRecord, TypedKey};
use serde_json::json;

use super::*;
use crate::test_support_paths::tempdir;

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
fn append_noop_and_rewrite_preserve_lifecycle_and_stable_ids() {
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
