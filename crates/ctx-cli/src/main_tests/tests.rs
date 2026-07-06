use super::{
    catalog_import_checkpoint_matches, normalize_uuid_prefix, parse_event_window_limit,
    parse_search_limit, parse_since_filter, parse_sql_timeout, sha256_file_prefix_hex,
    shell_quote_arg,
};
use std::{fs, io::Write, panic};
use tempfile::tempdir;

#[test]
fn shell_quote_arg_uses_single_quotes_for_shell_metacharacters() {
    assert_eq!(shell_quote_arg("onboarding"), "onboarding");
    assert_eq!(
        shell_quote_arg("$(touch /tmp/ctx-owned)'s"),
        "'$(touch /tmp/ctx-owned)'\\''s'"
    );
}

#[test]
fn parse_since_filter_rejects_large_day_window() {
    let err = parse_since_filter("500000000d").unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("invalid --since day window"),
        "expected error about invalid day window, got: {msg}"
    );
}

#[test]
fn cli_value_parsers_do_not_panic_on_adversarial_inputs() {
    let inputs = [
        "",
        " ",
        "0",
        "-1",
        "1",
        "30d",
        "500000000d",
        "9223372036854775807d",
        "-9223372036854775808d",
        "999999999999999999999999999999d",
        "NaN",
        "inf",
        "1e309",
        "1.5d",
        "1970-01-01T00:00:00Z",
        "999999-99-99T99:99:99Z",
        "zzzzzzzz",
        "ffffffff",
        "ffffffff-ffff-ffff-ffff-ffffffffffff",
        "\0",
        "１２３",
    ];

    for input in inputs {
        assert!(
            panic::catch_unwind(|| parse_since_filter(input)).is_ok(),
            "parse_since_filter panicked for {input:?}"
        );
        assert!(
            panic::catch_unwind(|| parse_search_limit(input)).is_ok(),
            "parse_search_limit panicked for {input:?}"
        );
        assert!(
            panic::catch_unwind(|| parse_event_window_limit(input)).is_ok(),
            "parse_event_window_limit panicked for {input:?}"
        );
        assert!(
            panic::catch_unwind(|| parse_sql_timeout(input)).is_ok(),
            "parse_sql_timeout panicked for {input:?}"
        );
        assert!(
            panic::catch_unwind(|| normalize_uuid_prefix(input, "test")).is_ok(),
            "normalize_uuid_prefix panicked for {input:?}"
        );
    }
}

#[test]
fn catalog_import_checkpoint_requires_matching_hash() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("session.jsonl");
    {
        let mut file = fs::File::create(&path).unwrap();
        writeln!(file, "prefix").unwrap();
    }
    let prefix_hash = sha256_file_prefix_hex(&path, 7).unwrap();
    assert!(catalog_import_checkpoint_matches(&path, 7, Some(&prefix_hash)).unwrap());
    assert!(catalog_import_checkpoint_matches(&path, 7, None).unwrap());

    fs::write(&path, "mutated\n").unwrap();
    assert!(!catalog_import_checkpoint_matches(&path, 7, Some(&prefix_hash)).unwrap());
}
