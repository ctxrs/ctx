use serde_json::json;

use super::{index_watch_human, IndexWatchOutput};

fn watch_status(
    lexical_done: usize,
    lexical_total: usize,
    daemon_reason: Option<&str>,
) -> serde_json::Value {
    let mut status = json!({
        "lexical": {
            "status": "partial",
            "inventory_units": lexical_total,
            "pending_inventory_units": lexical_total.saturating_sub(lexical_done),
        },
        "semantic": {
            "coverage": {
                "embedded_items": 0,
                "searchable_items": 12,
                "embedded_chunks": 0,
            },
        },
        "daemon": {
            "status": "running",
            "running": true,
            "jobs": {
                "semantic_index": {
                    "status": "disabled",
                },
            },
        },
    });
    if let Some(reason) = daemon_reason {
        status["daemon"]["reason"] = json!(reason);
    }
    status
}

#[test]
fn noninteractive_watch_appends_plain_frames() {
    let first = watch_status(0, 12, None);
    let second = watch_status(4, 12, None);
    let mut bytes = Vec::new();
    let mut output = IndexWatchOutput::new(&mut bytes, false);

    output.print_human(&first).unwrap();
    output.print_human(&second).unwrap();

    let rendered = String::from_utf8(bytes).unwrap();
    assert_eq!(
        rendered,
        format!(
            "{}\n\n{}\n\n",
            index_watch_human(&first),
            index_watch_human(&second)
        )
    );
    assert!(!rendered.contains('\u{1b}'));
}

#[test]
fn interactive_watch_redraws_the_existing_block() {
    let first = watch_status(0, 12, None);
    let second = watch_status(4, 12, None);
    let mut bytes = Vec::new();
    let mut output = IndexWatchOutput::new(&mut bytes, true);

    output.print_human(&first).unwrap();
    output.print_human(&second).unwrap();

    let rendered = String::from_utf8(bytes).unwrap();
    let expected_second = index_watch_human(&second)
        .lines()
        .map(|line| format!("\r\u{1b}[2K{line}\n"))
        .collect::<String>();
    assert_eq!(
        rendered,
        format!(
            "{}\n\u{1b}[3A{}",
            index_watch_human(&first),
            expected_second
        )
    );
}

#[test]
fn interactive_watch_clears_a_disappearing_reason_line() {
    let first = watch_status(0, 12, Some("daemon_starting"));
    let second = watch_status(4, 12, None);
    let mut bytes = Vec::new();
    let mut output = IndexWatchOutput::new(&mut bytes, true);

    output.print_human(&first).unwrap();
    output.print_human(&second).unwrap();

    let rendered = String::from_utf8(bytes).unwrap();
    assert!(rendered.contains("\u{1b}[4A"));
    assert!(
        rendered.ends_with("\r\u{1b}[2K\n\u{1b}[1A"),
        "the stale fourth line must be erased: {rendered:?}"
    );
}
