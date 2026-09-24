// Adapted from Sift tests, MIT; see this crate’s NOTICE.
// Authored flat-Tape facts and byte spans, independent of the presentation parser.
// Only 1..2, 4..5 and 7..12 may disappear. Row 3 owns YAML; row 6 owns a comment.
pub fn tape_fixture(tail: &str) -> (String, String) {
    let names = [
        "accepts Unicode identifiers without changing the original spelling",
        "rejects a stale generation while retaining its previous stored value",
        "deliberately unequal scalar at fixture.js:19:7",
        "preserves nested decimal numbers across independent round trips",
        "reports a missing artifact with its complete relative file location",
        "keeps the final assertion before the next named group comment",
        "allows quoted filenames containing ordinary platform separators",
        "checks that the network-disabled state is visible to the caller",
        "retains the timeout reason supplied by the remote test harness",
        "orders staged and unstaged commit states without merging paths",
        "detects duplicate records without silently dropping the original",
        "keeps every multibyte suffix of independently supplied input text",
    ];
    let mut text = String::from("TAP version 13\n# first group\n");
    let mut spans = Vec::new();
    for (i, name) in names.iter().enumerate() {
        if i == 6 {
            text.push_str("# second group\n");
        }
        let start = text.len();
        text.push_str(&format!(
            "{} {} {name}\n",
            if i == 2 { "not ok" } else { "ok" },
            i + 1
        ));
        spans.push(start..text.len());
        if i == 2 {
            text.push_str("  ---\n    operator: equal\n    expected: 17\n    actual:   19\n    at: fixture.js:19:7\n    stack: |-\n      ok 900 is diagnostic data\n  ...\n");
        }
    }
    text.push_str("\n1..12\n# tests 12\n# pass  11\n# fail  1\n\n");
    text.push_str(tail);
    let mut expected = String::new();
    let mut cursor = 0;
    for (first, last) in [(1, 2), (4, 5), (7, 12)] {
        let start = spans[first - 1].start;
        expected.push_str(&text[cursor..start]);
        expected.push_str(&format!("# omitted {} ordinary passing names/rows: tests {first}..{last} (human view, not TAP)\n", last - first + 1));
        cursor = spans[last - 1].end;
    }
    expected.push_str(&text[cursor..]);
    (text, expected)
}
