mod support;

use support::*;

fn source_search(temp: &TempDir, filters: &[&str]) -> Value {
    let mut arguments = vec!["search", "parser test", "--refresh", "off", "--format=json"];
    arguments.extend_from_slice(filters);
    json_output(ctx(temp).args(arguments))
}

fn assert_result_count(value: &Value, expected: usize) {
    assert_eq!(
        value["results"].as_array().map(Vec::len),
        Some(expected),
        "{value:#}"
    );
    assert_eq!(value["retrieval"]["index"], "core", "{value:#}");
}

#[test]
fn cli_source_identity_filters_are_exact_conjunctive_and_fail_closed() {
    let temp = tempdir();
    let (_daemon, _) = import_custom_history_fixture_source_backed(&temp, "basic.jsonl");

    let history_source = source_search(&temp, &["--history-source", "demo-agent/demo-source"]);
    assert_result_count(&history_source, 1);
    assert_eq!(history_source["filters"]["provider"], "custom");
    assert_eq!(
        history_source["filters"]["history_source"],
        "demo-agent/demo-source"
    );

    let exact_parts = source_search(
        &temp,
        &["--provider-key", "demo-agent", "--source-id", "demo-source"],
    );
    assert_result_count(&exact_parts, 1);
    assert_eq!(exact_parts["filters"]["provider"], "custom");
    assert_eq!(exact_parts["filters"]["provider_key"], "demo-agent");
    assert_eq!(exact_parts["filters"]["source_id"], "demo-source");

    let all = source_search(
        &temp,
        &[
            "--history-source",
            "demo-agent/demo-source",
            "--provider-key",
            "demo-agent",
            "--source-id",
            "demo-source",
        ],
    );
    assert_result_count(&all, 1);

    for filters in [
        &["--history-source", "unknown/source"][..],
        &["--provider-key", "unknown"][..],
        &["--source-id", "unknown"][..],
        &[
            "--history-source",
            "demo-agent/demo-source",
            "--source-id",
            "other-source",
        ][..],
    ] {
        assert_result_count(&source_search(&temp, filters), 0);
    }

    let malformed = failure_stderr(ctx(&temp).args([
        "search",
        "parser test",
        "--history-source",
        "missing-separator",
        "--refresh",
        "off",
    ]));
    assert!(
        malformed.contains("--history-source expects plugin/source or provider_key/source_id"),
        "{malformed}"
    );

    let incompatible = failure_stderr(ctx(&temp).args([
        "search",
        "parser test",
        "--provider",
        "codex",
        "--provider-key",
        "demo-agent",
        "--refresh",
        "off",
    ]));
    assert!(
        incompatible
            .contains("custom history source filters can only be combined with --provider custom"),
        "{incompatible}"
    );
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "source identity search must not create previous-epoch storage"
    );
}
