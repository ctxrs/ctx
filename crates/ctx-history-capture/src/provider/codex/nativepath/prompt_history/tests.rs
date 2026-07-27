use super::*;

use chrono::TimeZone;
use tempfile::tempdir;

fn prompt_lines(count: usize) -> String {
    (0..count)
        .map(|index| {
            format!(
                "{}\n",
                serde_json::to_string(&json!({
                    "session_id": "linear-prompt-session",
                    "ts": 1_785_139_200_i64 + i64::try_from(index).unwrap(),
                    "text": format!("linear prompt {index}"),
                }))
                .unwrap()
            )
        })
        .collect()
}

fn options(path: &Path, work_limit: CaptureWorkLimit) -> CodexHistoryImportOptions {
    CodexHistoryImportOptions {
        machine_id: "prompt-history-linear-scan-test".to_owned(),
        source_path: Some(path.to_path_buf()),
        imported_at: Utc.with_ymd_and_hms(2026, 7, 27, 12, 0, 0).unwrap(),
        history_record_id: None,
        capture_work_limit: work_limit,
        inventory_observation_token: None,
        import_profile: ImportProfile::CoreOnly,
    }
}

#[test]
fn drain_reads_many_pages_in_two_linear_source_passes() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("history.jsonl");
    let contents = prompt_lines(MAX_PAGE_RECORDS * 20 + 1);
    fs::write(&source, &contents).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    reset_prompt_history_io_metrics();
    let summary = import_codex_native_prompt_history(
        &source,
        &mut store,
        options(&source, CaptureWorkLimit::Drain),
    )
    .unwrap();
    let metrics = prompt_history_io_metrics();

    assert_eq!(summary.imported_events, MAX_PAGE_RECORDS * 20 + 1);
    assert_eq!(metrics.opens, 2, "{metrics:?}");
    assert_eq!(
        metrics.bytes_read,
        u64::try_from(contents.len()).unwrap() * 2,
        "{metrics:?}"
    );
}

#[test]
fn persisted_core_resume_reconstructs_prefix_once_then_drains_linearly() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("history.jsonl");
    let contents = prompt_lines(MAX_PAGE_RECORDS * 3 + 1);
    fs::write(&source, &contents).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_codex_native_prompt_history(
        &source,
        &mut store,
        options(&source, CaptureWorkLimit::OneSafeGroup),
    )
    .unwrap();
    assert_eq!(first.imported_events, MAX_PAGE_RECORDS);
    assert!(first.work_remaining);

    reset_prompt_history_io_metrics();
    let resumed = import_codex_native_prompt_history(
        &source,
        &mut store,
        options(&source, CaptureWorkLimit::Drain),
    )
    .unwrap();
    let metrics = prompt_history_io_metrics();

    assert_eq!(resumed.imported_events, MAX_PAGE_RECORDS * 2 + 1);
    assert!(!resumed.work_remaining);
    assert_eq!(metrics.opens, 2, "{metrics:?}");
    assert_eq!(
        metrics.bytes_read,
        u64::try_from(contents.len()).unwrap() * 2,
        "{metrics:?}"
    );
}

#[test]
fn prepared_page_surfaces_rejection_only_without_retained_content() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("history.jsonl");
    fs::write(&source, b"{not-json}\n").unwrap();
    let authority =
        SourceAuthority::new(&source, &source, "prompt-history-page-outcome-test").unwrap();
    let digest = digest_source(&source, None, None).unwrap();
    let cursor = plan_cursor(
        &authority,
        StoredCursor::None,
        &digest,
        source.display().to_string(),
    )
    .unwrap();
    let CursorPhase::Core {
        next_offset,
        next_ordinal,
        prefix_sha256,
    } = cursor.phase.clone()
    else {
        panic!("fresh prompt cursor must begin in Core");
    };
    let mut scanner = PromptHistoryScanner::open(
        &authority,
        &digest,
        next_offset,
        next_ordinal,
        prefix_sha256,
    )
    .unwrap();

    let page = prepare_page(&mut scanner, &digest, &cursor).unwrap();

    assert_eq!(page.outcome, PromptHistoryPageOutcome::RejectedOnly);
    assert!(page.rows.is_empty());
    assert_eq!(page.failures.len(), 1);
    assert!(page.terminal);
}
