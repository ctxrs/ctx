use std::sync::{Arc, Mutex};

use super::*;
#[derive(Clone, Default)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl SharedWriter {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for SharedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn active_status() -> RefreshProgressSnapshot {
    RefreshProgressSnapshot::new(
        Some("logical-request".to_owned()),
        crate::ui::RefreshStatusKind::Logical(crate::ui::RefreshLogicalStatus {
            request_state: crate::ui::RefreshRequestState::Running,
            logical_phase: crate::ui::RefreshLogicalPhase::Direct,
            physical_attempt_id: "physical-attempt".to_owned(),
            physical_attempt_state: crate::ui::RefreshRequestState::Running,
            progress_owner_request_id: "physical-attempt".to_owned(),
            progress_owner_attempt_state: crate::ui::RefreshRequestState::Running,
            structured_outcome: None,
        }),
        crate::ui::RefreshProgress {
            phase: "refreshing".to_owned(),
            completed_sources: 1,
            total_sources: 2,
            current_source: Some("/tmp/history\ncontrol.sqlite".to_owned()),
            completed_records: Some(4_096),
            completed_bytes: Some(2_048),
            agent_histories: vec!["Codex".to_owned(), "Claude".to_owned()],
            processed_sessions: 123,
            processed_messages: 4_000,
            processed_tool_calls: 96,
            processed_bytes: 2_048,
            elapsed_millis: Some(65_000),
            whole_run_stage: crate::ui::RefreshWholeRunStage::Reading,
            estimated_remaining_millis: None,
            current_source_progress: Some(crate::ui::RefreshCurrentSourceProgress {
                stage: crate::ui::RefreshCurrentSourceProgressStage::LogicalScan,
                snapshot_pages_completed: None,
                snapshot_pages_total: None,
                snapshot_bytes_completed: None,
                snapshot_bytes_total: None,
                logical_rows_scanned: Some(4_096),
                logical_certified_bytes: Some(2_048),
            }),
        },
        true,
    )
}

fn active_transfer_status() -> RefreshProgressSnapshot {
    RefreshProgressSnapshot::new(
        Some("explicit-import-request".to_owned()),
        crate::ui::RefreshStatusKind::Logical(crate::ui::RefreshLogicalStatus {
            request_state: crate::ui::RefreshRequestState::Running,
            logical_phase: crate::ui::RefreshLogicalPhase::Attached,
            physical_attempt_id: "shared-physical-attempt".to_owned(),
            physical_attempt_state: crate::ui::RefreshRequestState::Running,
            progress_owner_request_id: "shared-physical-attempt".to_owned(),
            progress_owner_attempt_state: crate::ui::RefreshRequestState::Running,
            structured_outcome: None,
        }),
        crate::ui::RefreshProgress {
            phase: "copying".to_owned(),
            completed_sources: 1,
            total_sources: 3,
            current_source: Some("/explicit.sqlite".to_owned()),
            completed_records: Some(100),
            completed_bytes: Some(777),
            agent_histories: vec!["Codex".to_owned()],
            processed_sessions: 8,
            processed_messages: 80,
            processed_tool_calls: 20,
            processed_bytes: 777,
            elapsed_millis: Some(2_000),
            whole_run_stage: crate::ui::RefreshWholeRunStage::Reading,
            estimated_remaining_millis: None,
            current_source_progress: Some(crate::ui::RefreshCurrentSourceProgress {
                stage: crate::ui::RefreshCurrentSourceProgressStage::OnlineBackup,
                snapshot_pages_completed: None,
                snapshot_pages_total: None,
                snapshot_bytes_completed: Some(256),
                snapshot_bytes_total: Some(512),
                logical_rows_scanned: None,
                logical_certified_bytes: None,
            }),
        },
        true,
    )
}

fn terminal_status() -> RefreshProgressSnapshot {
    terminal_status_with(
        crate::ui::RefreshRequestState::Published,
        "completed",
        "completed",
        false,
    )
}

fn terminal_status_with(
    state: crate::ui::RefreshRequestState,
    code: &str,
    class: &str,
    failure: bool,
) -> RefreshProgressSnapshot {
    RefreshProgressSnapshot::new(
        Some("logical-request".to_owned()),
        crate::ui::RefreshStatusKind::Logical(crate::ui::RefreshLogicalStatus {
            request_state: state,
            logical_phase: crate::ui::RefreshLogicalPhase::Terminal,
            physical_attempt_id: "physical-attempt".to_owned(),
            physical_attempt_state: state,
            progress_owner_request_id: "physical-attempt".to_owned(),
            progress_owner_attempt_state: state,
            structured_outcome: Some(Box::new(crate::ui::RefreshStructuredOutcome {
                code: code.to_owned(),
                class: class.to_owned(),
                retryable: false,
                affected_routes: Vec::new(),
                retryable_routes: Vec::new(),
                blocked_routes: Vec::new(),
                physical_attempt_id: "physical-attempt".to_owned(),
                retained_generation: None,
                published_generation: None,
                retry_advice: None,
                detail: None,
                failure,
            })),
        }),
        crate::ui::RefreshProgress {
            phase: if state == crate::ui::RefreshRequestState::Failed {
                "failed".to_owned()
            } else {
                "committed".to_owned()
            },
            completed_sources: 2,
            total_sources: 2,
            current_source: None,
            completed_records: None,
            completed_bytes: None,
            whole_run_stage: if state == crate::ui::RefreshRequestState::Failed {
                crate::ui::RefreshWholeRunStage::Failed
            } else {
                crate::ui::RefreshWholeRunStage::Complete
            },
            ..Default::default()
        },
        true,
    )
}

fn ui_with_stderr(
    stderr: SharedWriter,
    stderr_context: crate::ui::RenderContext,
) -> (Ui, SharedWriter) {
    let stdout = SharedWriter::default();
    let stdout_capture = stdout.clone();
    let stdout_context = crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
        crate::ui::StreamKind::Stdout,
    ));
    (
        Ui::with_writers(stdout, stdout_context, stderr, stderr_context),
        stdout_capture,
    )
}

mod eta_tests;
mod notice_tests;

#[test]
fn progress_mode_matrix_uses_injected_stderr_and_keeps_stdout_clean() {
    let cases = [
        (ProgressMode::Auto, true, false, false, true),
        (ProgressMode::Auto, false, false, false, false),
        (ProgressMode::Auto, true, false, true, false),
        (ProgressMode::Auto, true, true, false, false),
        (ProgressMode::Plain, false, false, false, true),
        (ProgressMode::Plain, true, false, false, true),
        (ProgressMode::Json, false, false, false, true),
        (ProgressMode::Json, true, false, false, true),
        (ProgressMode::None, true, false, false, false),
    ];
    for (arg, stderr_tty, term_dumb, final_json, expected_output) in cases {
        let stderr = SharedWriter::default();
        let stderr_capture = stderr.clone();
        let test_context = if stderr_tty {
            crate::ui::TestContext::tty(crate::ui::StreamKind::Stderr, 80).term_dumb(term_dumb)
        } else {
            crate::ui::TestContext::pipe(crate::ui::StreamKind::Stderr)
        };
        let (mut ui, stdout_capture) =
            ui_with_stderr(stderr, crate::ui::RenderContext::for_test(test_context));
        {
            let mut reporter = ProgressReporter::new(&mut ui, arg, final_json, "import", 0);
            reporter.source_refresh(active_status()).unwrap();
        }
        assert_eq!(
            !stderr_capture.text().is_empty(),
            expected_output,
            "mode={arg:?}, tty={stderr_tty}, term_dumb={term_dumb}, final_json={final_json}"
        );
        assert!(stdout_capture.text().is_empty());
        if arg == ProgressMode::Plain {
            assert!(!stderr_capture.text().contains('\u{1b}'));
        }
        if arg == ProgressMode::Json {
            let value: serde_json::Value =
                serde_json::from_str(stderr_capture.text().trim()).unwrap();
            assert_eq!(value["type"], "ctx_progress");
            assert_eq!(value["logical_phase"], "direct");
        }
    }
}

#[test]
fn plain_refresh_progress_is_the_stable_live_document_without_internal_routes() {
    let stderr = SharedWriter::default();
    let stderr_capture = stderr.clone();
    let context = crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
        crate::ui::StreamKind::Stderr,
    ));
    let shared_document = crate::ui::refresh_progress(&context, &active_status()).render_plain();
    let (mut ui, stdout_capture) = ui_with_stderr(stderr, context);

    let mut reporter = ProgressReporter::new(&mut ui, ProgressMode::Plain, false, "setup", 0);
    reporter.source_refresh(active_status()).unwrap();

    assert_eq!(stderr_capture.text(), shared_document);
    assert_eq!(
        shared_document,
        concat!(
            "Indexing your agent history\n",
            "──────────────━━━━━━━━──────────────────────────\n",
            "\n",
            "Agent histories  Codex\n",
            "                 Claude\n",
            "Sessions         123\n",
            "Messages         4,000\n",
            "Tool calls       96\n",
            "Data scanned     2.0 KiB\n",
            "Elapsed          1m 05s\n",
            "Remaining        estimating\n",
        )
    );
    assert!(stdout_capture.text().is_empty());
    assert!(!stderr_capture.text().contains("/tmp/history"));
    assert!(!stderr_capture.text().contains("1 / 2"));
    assert!(!stderr_capture.text().contains('\u{1b}'));
}

#[test]
fn provider_rows_freeze_after_discovery_for_the_live_lifecycle() {
    let stderr = SharedWriter::default();
    let stderr_capture = stderr.clone();
    let context = crate::ui::RenderContext::for_test(crate::ui::TestContext::tty(
        crate::ui::StreamKind::Stderr,
        80,
    ));
    let (mut ui, _) = ui_with_stderr(stderr, context);
    let mut reporter = ProgressReporter::new(&mut ui, ProgressMode::Auto, false, "setup", 0);

    let mut discovery = active_status();
    discovery.progress_mut_for_test().phase = "discovering".to_owned();
    reporter
        .source_refresh_at(discovery, StdDuration::ZERO)
        .unwrap();
    assert!(!stderr_capture.text().contains("Agent histories"));

    let scan = active_status();
    reporter
        .source_refresh_at(scan.clone(), StdDuration::from_millis(100))
        .unwrap();
    let mut later = scan;
    later
        .progress_mut_for_test()
        .agent_histories
        .push("Late provider".to_owned());
    later.progress_mut_for_test().phase = "committing".to_owned();
    reporter
        .source_refresh_at(later, StdDuration::from_millis(200))
        .unwrap();
    drop(reporter);

    let output = stderr_capture.text();
    assert!(!output.contains("Late provider"), "{output:?}");
    assert_eq!(output.matches("Agent histories").count(), 1, "{output:?}");
}

#[test]
fn active_and_terminal_refresh_jsonl_contract_is_exact() {
    let active = progress_json(
        "import",
        &source_refresh_line(active_transfer_status(), 4_096),
        StdDuration::from_secs(2),
    );
    let terminal = progress_json(
        "import",
        &source_refresh_line(terminal_status(), 4_096),
        StdDuration::from_secs(2),
    );

    assert_eq!(
        active,
        r#"{"agent_histories":["Codex"],"completed_bytes":256,"completed_files":null,"completed_sources":1,"current_source":"/explicit.sqlite","current_source_progress":{"snapshot_bytes_completed":256,"snapshot_bytes_total":512,"stage":"online_backup"},"done":false,"elapsed_seconds":2.0,"estimated_remaining_millis":null,"eta_seconds":2.0,"imported_events":100,"logical_phase":"attached","logical_request_id":"explicit-import-request","message":"Refreshing history with shared work: /explicit.sqlite (1 / 3).","operation":"import","percent":50.0,"phase":"online_backup","physical_attempt_id":"shared-physical-attempt","physical_attempt_state":"running","processed_bytes":777,"processed_messages":80,"processed_sessions":8,"processed_tool_calls":20,"progress_owner_attempt_state":"running","progress_owner_request_id":"shared-physical-attempt","refresh_elapsed_millis":2000,"request_id":"explicit-import-request","request_state":"running","source_completed_bytes":777,"source_completed_records":100,"total_bytes":512,"total_files":null,"total_sources":3,"total_sources_known":true,"type":"ctx_progress","whole_run_stage":"reading"}"#
    );
    assert_eq!(
        terminal,
        r#"{"agent_histories":[],"completed_bytes":4096,"completed_files":null,"completed_sources":2,"current_source":null,"current_source_progress":null,"done":true,"elapsed_seconds":2.0,"estimated_remaining_millis":null,"eta_seconds":null,"imported_events":null,"logical_phase":"terminal","logical_request_id":"logical-request","message":"History refresh complete (2 / 2).","operation":"import","percent":100.0,"phase":"published","physical_attempt_id":"physical-attempt","physical_attempt_state":"published","processed_bytes":0,"processed_messages":0,"processed_sessions":0,"processed_tool_calls":0,"progress_owner_attempt_state":"published","progress_owner_request_id":"physical-attempt","refresh_elapsed_millis":null,"request_id":"logical-request","request_state":"published","source_completed_bytes":null,"source_completed_records":null,"structured_outcome":{"affected_routes":[],"blocked_routes":[],"class":"completed","code":"completed","physical_attempt_id":"physical-attempt","retryable":false,"retryable_routes":[]},"total_bytes":4096,"total_files":null,"total_sources":2,"total_sources_known":true,"type":"ctx_progress","whole_run_stage":"complete"}"#
    );

    let events = [&active, &terminal]
        .into_iter()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        events.iter().filter(|event| event["done"] == true).count(),
        1
    );
    assert_eq!(
        (
            events[0]["completed_bytes"].as_u64(),
            events[0]["total_bytes"].as_u64()
        ),
        (Some(256), Some(512))
    );
    assert_eq!(events[0]["percent"], 50.0);
    assert_eq!(events[0]["eta_seconds"], 2.0);
    assert_eq!(
        events[0]["estimated_remaining_millis"],
        serde_json::Value::Null
    );
    assert_ne!(
        events[0]["logical_request_id"],
        events[0]["progress_owner_request_id"]
    );
}

#[test]
fn setup_jsonl_holds_legacy_source_eta_but_never_promotes_it_to_whole_run_eta() {
    let value: serde_json::Value = serde_json::from_str(&progress_json(
        "setup",
        &source_refresh_line(active_transfer_status(), 0),
        StdDuration::from_secs(2),
    ))
    .unwrap();

    // eta_seconds is the documented legacy byte-rate field. Preserve it
    // for compatibility; the explicit whole-run field is authoritative for
    // time until setup is usable.
    assert_eq!(value["eta_seconds"], 2.0);
    assert_eq!(value["whole_run_stage"], "reading");
    assert_eq!(value["estimated_remaining_millis"], serde_json::Value::Null);
}

#[test]
fn failed_setup_snapshot_is_failed_in_json_and_live_presentation() {
    let mut snapshot = terminal_status_with(
        crate::ui::RefreshRequestState::Failed,
        "source_refresh_failed",
        "internal",
        true,
    );
    snapshot.use_setup_live_presentation();
    let context = crate::ui::RenderContext::for_test(crate::ui::TestContext::tty(
        crate::ui::StreamKind::Stderr,
        80,
    ));
    let rendered = refresh_progress(&context, &snapshot).render_plain();
    assert!(
        rendered.starts_with("History refresh failed\n"),
        "{rendered}"
    );
    assert!(!rendered.contains("Preparing"), "{rendered}");

    let json = progress_json(
        "setup",
        &source_refresh_line(snapshot, 4_096),
        StdDuration::from_secs(2),
    );
    assert_eq!(
        json,
        r#"{"agent_histories":[],"completed_bytes":4096,"completed_files":null,"completed_sources":2,"current_source":null,"current_source_progress":null,"done":true,"elapsed_seconds":2.0,"estimated_remaining_millis":null,"eta_seconds":null,"imported_events":null,"logical_phase":"terminal","logical_request_id":"logical-request","message":"History refresh failed (2 / 2).","operation":"setup","percent":100.0,"phase":"failed","physical_attempt_id":"physical-attempt","physical_attempt_state":"failed","processed_bytes":0,"processed_messages":0,"processed_sessions":0,"processed_tool_calls":0,"progress_owner_attempt_state":"failed","progress_owner_request_id":"physical-attempt","refresh_elapsed_millis":null,"request_id":"logical-request","request_state":"failed","source_completed_bytes":null,"source_completed_records":null,"structured_outcome":{"affected_routes":[],"blocked_routes":[],"class":"internal","code":"source_refresh_failed","physical_attempt_id":"physical-attempt","retryable":false,"retryable_routes":[]},"total_bytes":4096,"total_files":null,"total_sources":2,"total_sources_known":true,"type":"ctx_progress","whole_run_stage":"failed"}"#
    );
}

#[test]
fn refresh_jsonl_preserves_base_commit_and_verify_messages() {
    for (phase, expected) in [
        ("committing", "Publishing search index (1 / 2)."),
        ("verifying", "Verifying refreshed history (1 / 2)."),
    ] {
        let mut snapshot = active_status();
        snapshot.progress_mut_for_test().phase = phase.to_owned();
        snapshot.progress_mut_for_test().current_source = None;
        snapshot.progress_mut_for_test().current_source_progress = None;
        let line = source_refresh_line(snapshot, 4_096);
        let value: serde_json::Value =
            serde_json::from_str(&progress_json("setup", &line, StdDuration::from_secs(2)))
                .unwrap();

        assert_eq!(value["message"], expected);
    }
}

#[test]
fn done_progress_json_forces_complete_bytes_with_incomplete_bytes() {
    let line = ProgressLine {
        phase: "finalizing".to_owned(),
        message: "done".to_owned(),
        completed_bytes: 0,
        total_bytes: 4 * 1024,
        completed_files: None,
        total_files: None,
        imported_events: None,
        done: true,
        refresh: None,
        callout: None,
    };

    let value: serde_json::Value =
        serde_json::from_str(&progress_json("setup", &line, StdDuration::from_secs(120)))
            .expect("progress json should parse");

    assert_eq!(value["completed_bytes"], 4 * 1024);
    assert_eq!(value["total_bytes"], 4 * 1024);
    assert_eq!(value["percent"], 100.0);
    assert_eq!(value["eta_seconds"], serde_json::Value::Null);
    assert_eq!(value["done"], true);
}

#[test]
fn pre_refresh_failure_is_one_terminal_progress_event() {
    let stdout = SharedWriter::default();
    let stderr = SharedWriter::default();
    let stderr_capture = stderr.clone();
    let mut ui = Ui::with_writers(
        stdout,
        crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
            crate::ui::StreamKind::Stdout,
        )),
        stderr,
        crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
            crate::ui::StreamKind::Stderr,
        )),
    );

    ProgressReporter::new(&mut ui, ProgressMode::Json, false, "import", 0)
        .failure("failed", "Import path does not exist: /missing")
        .unwrap();

    let event: serde_json::Value = serde_json::from_str(stderr_capture.text().trim()).unwrap();
    assert_eq!(event["type"], "ctx_progress");
    assert_eq!(event["operation"], "import");
    assert_eq!(event["phase"], "failed");
    assert_eq!(event["message"], "Import path does not exist: /missing");
    assert_eq!(event["done"], true);
}

#[test]
fn progress_json_remains_exact_and_ansi_free() {
    let line = ProgressLine {
        phase: "cataloging".to_owned(),
        message: "cataloging".to_owned(),
        completed_bytes: 1024,
        total_bytes: 4096,
        completed_files: Some(1),
        total_files: Some(2),
        imported_events: Some(7),
        done: false,
        refresh: None,
        callout: None,
    };

    let rendered = progress_json("import", &line, StdDuration::from_secs(2));

    assert_eq!(
        rendered,
        concat!(
            r#"{"completed_bytes":1024,"completed_files":1,"done":false,"#,
            r#""elapsed_seconds":2.0,"eta_seconds":6.0,"imported_events":7,"#,
            r#""message":"cataloging","operation":"import","percent":25.0,"#,
            r#""phase":"cataloging","total_bytes":4096,"total_files":2,"#,
            r#""type":"ctx_progress"}"#,
        )
    );
    assert!(!rendered.contains('\u{1b}'));
}

#[test]
fn plain_and_json_progress_keep_explicit_stream_contracts() {
    let line = ProgressLine {
        phase: "indexing".to_owned(),
        message: "Indexed 2 sources".to_owned(),
        completed_bytes: 2,
        total_bytes: 4,
        completed_files: Some(2),
        total_files: Some(4),
        imported_events: None,
        done: false,
        refresh: None,
        callout: None,
    };

    let plain = match ProgressRenderMode::Plain {
        ProgressRenderMode::Plain => line.message.as_str(),
        _ => unreachable!(),
    };
    let json = match ProgressRenderMode::Json {
        ProgressRenderMode::Json => progress_json("import", &line, StdDuration::from_secs(1)),
        _ => unreachable!(),
    };

    assert_eq!(plain, "Indexed 2 sources");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&json).unwrap()["type"],
        "ctx_progress"
    );
    assert!(!plain.contains('\u{1b}'));
    assert!(!json.contains('\u{1b}'));
}

#[derive(Clone, Copy)]
enum WriterFailure {
    Write,
    Flush,
}

struct FailingWriter(WriterFailure);

impl Write for FailingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self.0 {
            WriterFailure::Write => Err(io::Error::other("injected progress write failure")),
            WriterFailure::Flush => Ok(buffer.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.0 {
            WriterFailure::Write => Ok(()),
            WriterFailure::Flush => Err(io::Error::other("injected progress flush failure")),
        }
    }
}

#[test]
fn progress_write_and_flush_failures_remain_errors() {
    let line = ProgressLine {
        phase: "logical_scan".to_owned(),
        message: "Scanning SQLite history".to_owned(),
        completed_bytes: 0,
        total_bytes: 0,
        completed_files: None,
        total_files: None,
        imported_events: None,
        done: false,
        refresh: None,
        callout: None,
    };
    for (failure, expected) in [
        (WriterFailure::Write, "injected progress write failure"),
        (WriterFailure::Flush, "injected progress flush failure"),
    ] {
        let mut writer = FailingWriter(failure);
        let context = crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
            crate::ui::StreamKind::Stderr,
        ));
        let mut output = ProgressOutput::Direct(LiveOutput::new(&mut writer, context));
        let result = write_progress(
            &mut output,
            ProgressRenderMode::Json,
            "import",
            &line,
            StdDuration::ZERO,
        );
        assert!(result
            .expect_err("progress output failure must propagate")
            .to_string()
            .contains(expected));
    }
}

#[test]
fn sqlite_logical_progress_is_typed_and_never_invents_a_total() {
    let snapshot = active_status();
    let line = source_refresh_line(snapshot, 8_192);
    assert_eq!(line.phase, "logical_scan");
    assert!(line.message.contains("history control.sqlite"));
    assert!(!line.message.contains('\n'));
    assert_eq!((line.completed_bytes, line.total_bytes), (0, 0));

    let value: serde_json::Value =
        serde_json::from_str(&progress_json("import", &line, StdDuration::from_secs(2))).unwrap();
    assert_eq!(value["percent"], 0.0);
    assert_eq!(value["eta_seconds"], serde_json::Value::Null);
    assert_eq!(value["current_source_progress"]["stage"], "logical_scan");
    assert_eq!(
        value["current_source_progress"]["logical_rows_scanned"],
        4_096
    );
    assert!(!value["current_source"].as_str().unwrap().contains('\n'));
    assert_eq!(value["logical_phase"], "direct");
    assert_eq!(value["physical_attempt_id"], "physical-attempt");
}

#[test]
fn progress_text_is_control_safe_utf8_and_bounded() {
    let text = format!("{}\n{}", "é".repeat(400), "x".repeat(400));
    let bounded = bounded_progress_text(&text, MAX_PROGRESS_MESSAGE_BYTES);
    assert!(bounded.len() <= MAX_PROGRESS_MESSAGE_BYTES);
    assert!(!bounded.contains('\n'));
    assert!(bounded.ends_with("..."));
}

#[test]
fn count_formatting_groups_the_full_u64_domain() {
    assert_eq!(format_count(999), "999");
    assert_eq!(format_count(1_000), "1,000");
    assert_eq!(format_count(u64::MAX), "18,446,744,073,709,551,615");
}
