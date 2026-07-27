use serde_json::json;

use super::IndexWatchOutput;

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
