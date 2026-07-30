use std::{
    io,
    sync::{Arc, Mutex},
};

use serde_json::json;

use super::{index_terminal_error, index_watch_output, IndexSelection, IndexWatchOutput};
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

fn watch_status(
    lexical_done: usize,
    lexical_total: usize,
    daemon_running: bool,
) -> serde_json::Value {
    json!({
        "lexical": {
            "status": "partial",
            "indexed_sessions": lexical_done,
            "indexed_items": lexical_done.saturating_mul(10),
            "completed_source_bytes": lexical_done.saturating_mul(100),
            "total_source_bytes": lexical_total.saturating_mul(100),
            "inventory_units": lexical_total,
            "pending_inventory_units": lexical_total.saturating_sub(lexical_done),
            "failed_inventory_units": 0,
        },
        "semantic": {
            "enabled": false,
            "coverage": {
                "embedded_items": 0,
                "searchable_items": 12,
                "embedded_chunks": 0,
            },
        },
        "daemon": {
            "status": "running",
            "running": daemon_running,
            "jobs": {
                "semantic_index": {
                    "status": "disabled",
                },
            },
        },
    })
}

#[test]
fn noninteractive_watch_appends_plain_frames() {
    let first = watch_status(0, 12, true);
    let second = watch_status(4, 12, true);
    let mut bytes = Vec::new();
    let mut output = IndexWatchOutput::for_test(&mut bytes, false, 80);

    output.print_human(&first).unwrap();
    output.print_human(&second).unwrap();

    let rendered = String::from_utf8(bytes).unwrap();
    assert_eq!(rendered.matches("Indexing your history").count(), 2);
    assert!(rendered.contains("\n\nIndexing your history"));
    assert!(rendered.ends_with("\n\n"));
    assert!(!rendered.contains('\u{1b}'));
    assert!(rendered.contains("Sessions    4 indexed"));
    assert!(rendered.contains("Records     40 searchable"));
    assert!(rendered.contains("Semantic search  Off"));
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
        output.print_human(&watch_status(4, 12, true)).unwrap();
    }

    let rendered = captured.text();
    assert!(rendered.contains("\u{1b}["));
    assert!(rendered.contains("Indexing your history"));
}

#[test]
fn interactive_watch_redraws_the_existing_block() {
    let first = watch_status(0, 12, true);
    let second = watch_status(4, 12, true);
    let mut output = IndexWatchOutput::for_test(Vec::new(), true, 80);

    output.print_human(&first).unwrap();
    let first_frame = String::from_utf8(output.writer.clone()).unwrap();
    let first_lines = first_frame.lines().count();
    output.print_human(&second).unwrap();

    let rendered = String::from_utf8(output.writer).unwrap();
    assert!(rendered.starts_with(&first_frame));
    assert!(rendered.contains(&format!("\u{1b}[{first_lines}A")));
    assert!(rendered.contains("\r\u{1b}[2KSessions    4 indexed\n"));
}

#[test]
fn interactive_watch_clears_a_disappearing_warning() {
    let first = watch_status(0, 12, false);
    let second = watch_status(4, 12, true);
    let mut output = IndexWatchOutput::for_test(Vec::new(), true, 80);

    output.print_human(&first).unwrap();
    let first_frame = String::from_utf8(output.writer.clone()).unwrap();
    let first_lines = first_frame.lines().count();
    output.print_human(&second).unwrap();

    let rendered = String::from_utf8(output.writer).unwrap();
    assert!(first_frame.contains("Background indexing stopped"));
    assert!(first_frame.contains("Run `ctx doctor` for details."));
    assert!(rendered.contains(&format!("\u{1b}[{first_lines}A")));
    assert!(
        rendered.ends_with("\r\u{1b}[2K\n\r\u{1b}[2K\n\r\u{1b}[2K\n\u{1b}[3A"),
        "the stale warning rows must be erased: {rendered:?}"
    );
}

#[test]
fn watch_treats_a_stopped_failed_daemon_as_terminal() {
    let mut status = watch_status(4, 12, false);
    status["daemon"]["status"] = json!("failed");

    assert_eq!(
        index_terminal_error(&status, IndexSelection::default_for(&status)).as_deref(),
        Some(
            "background indexing stopped before the index was ready; run `ctx doctor` for details"
        )
    );
}

#[test]
fn watch_treats_failed_inventory_as_terminal_even_without_pending_units() {
    let mut status = watch_status(12, 12, false);
    status["lexical"]["status"] = json!("failed");
    status["lexical"]["failed_inventory_units"] = json!(1);
    status["daemon"]["status"] = json!("failed");

    assert_eq!(
        index_terminal_error(&status, IndexSelection::default_for(&status)).as_deref(),
        Some("one or more history files could not be indexed; run `ctx doctor` for details")
    );
}

#[test]
fn watch_does_not_treat_record_rejections_as_terminal_or_user_facing() {
    let mut status = watch_status(4, 12, true);
    status["daemon"]["jobs"]["history_refresh"] = json!({
        "status": "completed",
        "totals": {
            "sources_completed_with_rejections": 1,
            "rejected_records": 108,
            "failed_sources": 0,
        },
    });
    assert!(
        index_terminal_error(&status, IndexSelection::default_for(&status)).is_none(),
        "{status:#}"
    );

    let mut output = IndexWatchOutput::for_test(Vec::new(), false, 80);
    output.print_human(&status).unwrap();
    let rendered = String::from_utf8(output.writer).unwrap();
    assert!(!rendered.contains("rejected"), "{rendered}");
    assert!(!rendered.contains("malformed"), "{rendered}");
}
