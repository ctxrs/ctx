// Adapted from Sift tests, MIT; see this crate’s NOTICE.
// Authored synthetic input plus an independent byte-range removal oracle.
// No parser, product formatter, or substring-based retention oracle is used.
pub fn cargo_fixture(failed: bool, tail: &str) -> (String, String) {
    let names = [
        "accepts_unicode_identifiers",
        "rejects_stale_generation",
        "handles_empty_input",
        "preserves_nested_numbers",
        "reports_missing_artifact",
        "allows_quoted_paths",
        "checks_network_disabled",
        "retains_timeout_reason",
        "orders_commit_states",
        "compares_boundary_bytes",
        "detects_duplicate_records",
        "keeps_multibyte_suffix",
    ];
    let mut text = format!(
        "warning: authored build diagnostic 🦀\n    Running tests/fixture.rs\nrunning {} tests\n",
        13 + usize::from(failed)
    );
    let mut removed = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let start = text.len();
        text.push_str(&format!("test checks::{name} ... ok\n"));
        removed.push(start..text.len());
        if i == 4 && failed {
            text.push_str("test checks::broken_value ... FAILED\n");
        }
        if i == 7 {
            text.push_str("test checks::unavailable_service ... ignored, needs a server\n");
        }
    }
    if failed {
        text.push_str("\nfailures:\n\n---- checks::broken_value stdout ----\nthread 'checks::broken_value' panicked at src/lib.rs:17:9:\nassertion failed: left = 7, right = 9\ntest quoted_diagnostic ... ok\n\nfailures:\n    checks::broken_value\n");
    }
    text.push_str(&format!("\ntest result: {}. 12 passed; {} failed; 1 ignored; 0 measured; 3 filtered out; finished in 0.02s\n", if failed {"FAILED"} else {"ok"}, usize::from(failed)));
    text.push_str(tail);
    let mut expected = text[..removed[0].start].to_owned();
    expected.push_str("[12 passing test lines omitted]\n");
    let mut cursor = removed[0].start;
    for range in removed {
        expected.push_str(&text[cursor..range.start]);
        cursor = range.end;
    }
    expected.push_str(&text[cursor..]);
    (text, expected)
}
