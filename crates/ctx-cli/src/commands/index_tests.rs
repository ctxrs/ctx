use std::{
    io,
    sync::{Arc, Mutex},
};

use serde_json::json;

use super::{
    index_ready, index_terminal_error, index_wait_json, index_watch_output, IndexSelection,
    IndexWaitArgs, IndexWaitHumanOutput, IndexWatchOutput,
};
use crate::output::JsonOutputFormat;
use crate::ui::{ColorMode, RenderContext, StreamKind, TestContext, Ui};

#[derive(Clone, Default)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl SharedWriter {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl io::Write for SharedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn readiness(
    refresh_status: &str,
    completed_sources: u64,
    daemon_running: bool,
) -> serde_json::Value {
    json!({
        "schema_version": 1,
        "initialized": true,
        "lexical": {
            "status": "ready",
            "generation_id": "generation-1",
            "indexed_sessions": 4,
            "indexed_items": 40,
            "indexed_sources": 12,
            "certified_source_bytes": 1200,
        },
        "refresh": {
            "status": refresh_status,
            "reason": if refresh_status == "ready" { serde_json::Value::Null } else { json!("core_refresh_pending") },
            "request_state": if refresh_status == "ready" { "published" } else { "running" },
            "published_generation": "generation-1",
            "generation_id": "generation-1",
            "generation_matches": refresh_status == "ready",
            "progress": {
                "phase": if refresh_status == "ready" { "published" } else { "scanning_provider_sources" },
                "completed_sources": completed_sources,
                "total_sources": 12,
            },
        },
        "semantic": {
            "status": "disabled",
            "enabled": false,
            "coverage": {},
        },
        "daemon": {
            "status": if daemon_running { "running" } else { "failed" },
            "running": daemon_running,
            "jobs": {"semantic_index": {"status": "disabled"}},
        },
        "local_only": true,
        "read_only": true,
    })
}

fn first_publication_pending() -> serde_json::Value {
    let mut status = readiness("pending", 0, true);
    status["initialized"] = json!(false);
    status["lexical"] = json!({
        "status": "pending",
        "reason": "generation_not_published",
    });
    status
}

fn wait_selection(lexical: bool, semantic: bool, all: bool) -> IndexSelection {
    IndexSelection::from_wait_args(&IndexWaitArgs {
        format: JsonOutputFormat::Text,
        lexical,
        semantic,
        all,
        timeout_seconds: None,
        interval_seconds: 1,
    })
    .expect("explicit wait selection")
}

#[test]
fn first_publication_pending_is_not_collapsed_to_missing() {
    let status = first_publication_pending();
    let selection = IndexSelection::default_for(&status);
    assert!(!index_ready(&status, selection));
    assert!(
        index_terminal_error(&status, selection).is_none(),
        "{status:#}"
    );
}

#[test]
fn machine_snapshot_contains_only_authoritative_readiness_units() {
    let status = first_publication_pending();
    let mut output = IndexWatchOutput::for_test(Vec::new(), false, 32);
    output.print_json(&status).unwrap();
    let rendered = String::from_utf8(output.writer).unwrap();
    let rendered: serde_json::Value = serde_json::from_str(rendered.trim()).unwrap();
    assert_eq!(rendered["refresh"]["progress"]["completed_sources"], 0);
    assert_eq!(rendered["refresh"]["progress"]["total_sources"], 12);
    for obsolete in [
        "inventory_units",
        "pending_inventory_units",
        "failed_inventory_units",
        "stale_inventory_units",
    ] {
        assert!(!rendered.to_string().contains(obsolete), "{rendered:#}");
    }
}

#[test]
fn explicit_lexical_wait_accepts_a_verified_generation_during_refresh() {
    let pending = readiness("pending", 4, true);
    let selection = wait_selection(true, false, false);
    assert!(index_ready(&pending, selection));
    assert!(index_terminal_error(&pending, selection).is_none());

    let mut failed = pending;
    failed["refresh"] = json!({
        "status": "unavailable",
        "reason": "core_refresh_failed",
        "request_state": "failed",
    });
    failed["daemon"]["running"] = json!(false);
    assert!(index_ready(&failed, selection));
    assert!(index_terminal_error(&failed, selection).is_none());
}

#[test]
fn default_and_all_waits_preserve_refresh_convergence() {
    let pending = readiness("pending", 4, true);
    let ready = readiness("ready", 12, true);
    let selection = IndexSelection::default_for(&pending);
    assert!(!index_ready(&pending, selection));
    assert!(index_ready(&ready, selection));

    let all = wait_selection(false, false, true);
    assert!(!index_ready(&pending, all));
    assert!(index_ready(&ready, all));
}

#[test]
fn lexical_wait_accepts_a_verified_generation_when_no_refresh_is_active() {
    let mut status = readiness("ready", 12, false);
    status["refresh"] = json!({
        "status": "unavailable",
        "reason": "daemon_unavailable",
    });
    status["daemon"]["status"] = json!("disabled");

    assert!(index_ready(&status, IndexSelection::default_for(&status)));
    assert!(index_terminal_error(&status, IndexSelection::default_for(&status)).is_none());
}

#[test]
fn failed_refresh_is_terminal_only_when_refresh_convergence_is_selected() {
    let mut status = readiness("ready", 12, true);
    status["refresh"] = json!({
        "status": "unavailable",
        "reason": "core_refresh_failed",
    });

    assert_eq!(
        index_terminal_error(&status, IndexSelection::default_for(&status)).as_deref(),
        Some("history refresh is unavailable; run `ctx doctor` for details")
    );
    assert!(index_terminal_error(&status, wait_selection(true, false, false)).is_none());
}

#[test]
fn wait_json_names_the_readiness_payload() {
    let status = readiness("ready", 12, true);
    let output = index_wait_json(status, IndexSelection::all(), "ready");
    assert!(output.get("readiness").is_some());
    assert!(output.get("index").is_none());
}

#[test]
fn wait_human_output_prints_a_changed_final_snapshot() {
    let stdout = SharedWriter::default();
    let captured = stdout.clone();
    let stdout_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stdout));
    let stderr_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));
    let mut ui = Ui::with_writers(stdout, stdout_context, Vec::new(), stderr_context);
    let mut output = IndexWaitHumanOutput::default();

    output
        .print(&mut ui, &readiness("pending", 0, true))
        .unwrap();
    output
        .print_final(&mut ui, &readiness("pending", 4, true))
        .unwrap();

    let rendered = captured.text();
    assert_eq!(rendered.matches("Your history is searchable").count(), 2);
    assert!(rendered.contains("0 / 12"), "{rendered}");
    assert!(rendered.contains("4 / 12"), "{rendered}");
}

#[test]
fn model_cache_missing_semantic_job_is_terminal() {
    let mut status = readiness("ready", 12, false);
    status["semantic"] = json!({
        "status": "pending",
        "enabled": true,
        "coverage": {},
    });
    status["daemon"]["jobs"]["semantic_index"] = json!({
        "status": "skipped",
        "reason": "model_cache_missing",
    });

    assert_eq!(
        index_terminal_error(&status, IndexSelection::default_for(&status)).as_deref(),
        Some("semantic indexing is skipped because the local embedding model cache is missing")
    );
}

#[test]
fn noninteractive_watch_appends_plain_frames() {
    let mut bytes = Vec::new();
    let mut output = IndexWatchOutput::for_test(&mut bytes, false, 80);
    output.print_human(&readiness("pending", 0, true)).unwrap();
    output.print_human(&readiness("pending", 4, true)).unwrap();

    let rendered = String::from_utf8(bytes).unwrap();
    assert_eq!(rendered.matches("Your history is searchable").count(), 2);
    assert!(rendered.contains("\n\n✓ Your history is searchable"));
    assert!(rendered.ends_with("\n\n"));
    assert!(!rendered.contains('\u{1b}'));
}

#[test]
fn watch_writes_through_the_ui_selected_stdout_adapter() {
    let stdout = SharedWriter::default();
    let captured = stdout.clone();
    let stdout_context =
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Always));
    let stderr_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));
    let mut ui = Ui::with_writers(stdout, stdout_context, Vec::new(), stderr_context);

    {
        let mut output = index_watch_output(&mut ui);
        output.print_human(&readiness("pending", 4, true)).unwrap();
    }

    let rendered = captured.text();
    assert!(rendered.contains("\u{1b}["));
    assert!(rendered.contains("Refreshing history"));
}

#[test]
fn interactive_watch_redraws_the_existing_block() {
    let first = readiness("pending", 0, true);
    let second = readiness("pending", 4, true);
    let mut output = IndexWatchOutput::for_test(Vec::new(), true, 80);

    output.print_human(&first).unwrap();
    let first_frame = String::from_utf8(output.writer.clone()).unwrap();
    let first_lines = first_frame.lines().count();
    output.print_human(&second).unwrap();

    let rendered = String::from_utf8(output.writer).unwrap();
    assert!(rendered.starts_with(&first_frame));
    assert!(rendered.contains(&format!("\u{1b}[{first_lines}A")));
    assert!(rendered.contains("4 / 12"));
}

#[test]
fn stopped_refresh_is_terminal_without_fabricated_failure_counts() {
    let status = readiness("pending", 4, false);
    assert_eq!(
        index_terminal_error(&status, IndexSelection::default_for(&status)).as_deref(),
        Some(
            "background indexing stopped before the index was ready; run `ctx doctor` for details"
        )
    );
}

#[test]
fn stopped_queued_or_running_refresh_is_terminal_despite_stale_daemon_status() {
    for request_state in ["queued", "running"] {
        let mut status = readiness("pending", 4, false);
        status["refresh"]["request_state"] = json!(request_state);
        status["daemon"]["status"] = json!("running");

        assert_eq!(
            index_terminal_error(&status, IndexSelection::default_for(&status)).as_deref(),
            Some(
                "background indexing stopped before the index was ready; run `ctx doctor` for details"
            ),
            "request_state={request_state}"
        );
    }
}

#[test]
fn stopped_queued_or_running_semantic_work_is_terminal_despite_stale_daemon_status() {
    for semantic_state in ["queued", "running"] {
        let mut status = readiness("ready", 12, false);
        status["semantic"] = json!({
            "status": "pending",
            "enabled": true,
            "coverage": {},
        });
        status["daemon"]["status"] = json!("running");
        status["daemon"]["jobs"]["semantic_index"] = json!({
            "status": semantic_state,
        });

        assert_eq!(
            index_terminal_error(&status, wait_selection(false, true, false)).as_deref(),
            Some(
                "background indexing stopped before the index was ready; run `ctx doctor` for details"
            ),
            "semantic_state={semantic_state}"
        );
    }
}
