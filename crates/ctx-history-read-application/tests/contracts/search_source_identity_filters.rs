#[path = "../support/mod.rs"]
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
    assert_eq!(history_source["results"][0]["agent_scope"], "primary");
    assert_eq!(history_source["results"][0]["provider_key"], "demo-agent");
    assert_eq!(history_source["results"][0]["source_id"], "demo-source");
    assert_eq!(
        history_source["results"][0]["provider_session_id"],
        "demo-session"
    );
    let ctx_event_id = history_source["results"][0]["ctx_event_id"]
        .as_str()
        .unwrap();
    let ctx_session_id = history_source["results"][0]["ctx_session_id"]
        .as_str()
        .unwrap();
    assert_eq!(history_source["filters"]["provider"], "custom");
    assert_eq!(
        history_source["filters"]["history_source"],
        "demo-agent/demo-source"
    );

    let human_search = ctx(&temp)
        .args([
            "search",
            "parser test",
            "--history-source",
            "demo-agent/demo-source",
            "--refresh",
            "off",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human_search = String::from_utf8(human_search).unwrap();
    assert!(human_search.contains("demo-agent/demo-source"), "{human_search}");

    let located_session = json_output(ctx(&temp).args([
        "locate",
        "session",
        ctx_session_id,
        "--format=json",
    ]));
    assert_eq!(located_session["provider_key"], "demo-agent");
    assert_eq!(located_session["source_id"], "demo-source");
    let located_event = json_output(ctx(&temp).args([
        "locate",
        "event",
        ctx_event_id,
        "--format=json",
    ]));
    assert_eq!(located_event["provider_key"], "demo-agent");
    assert_eq!(located_event["source_id"], "demo-source");

    let shown_session = json_output(ctx(&temp).args([
        "show",
        "session",
        ctx_session_id,
        "--format=json",
    ]));
    assert_eq!(shown_session["provider_key"], "demo-agent");
    assert_eq!(shown_session["source_id"], "demo-source");
    assert_eq!(shown_session["session"]["provider_key"], "demo-agent");
    assert_eq!(shown_session["session"]["source_id"], "demo-source");
    assert!(
        shown_session["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["provider_key"] == "demo-agent"
                && event["source_id"] == "demo-source"),
        "{shown_session:#}"
    );

    let listed = json_output(ctx(&temp).args([
        "list",
        "events",
        "--session",
        ctx_session_id,
        "--content",
        "full",
        "--format=json",
    ]));
    let listed_events = listed["events"].as_array().unwrap();
    assert_eq!(listed_events.len(), 2, "{listed:#}");
    assert!(
        listed_events.iter().all(|event| {
            event["provider_key"] == "demo-agent"
                && event["source_id"] == "demo-source"
                && event["provider_session_id"] == "demo-session"
        }),
        "{listed:#}"
    );

    let shown_jsonl = ctx(&temp)
        .args([
            "show",
            "session",
            ctx_session_id,
            "--format=jsonl",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let shown_jsonl = String::from_utf8(shown_jsonl).unwrap();
    for line in shown_jsonl.lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        assert_eq!(row["provider_key"], "demo-agent", "{row:#}");
        assert_eq!(row["source_id"], "demo-source", "{row:#}");
    }

    for arguments in [
        vec!["locate", "session", ctx_session_id],
        vec!["show", "session", ctx_session_id],
    ] {
        let output = ctx(&temp)
            .args(arguments)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let output = String::from_utf8(output).unwrap();
        assert!(
            output
                .lines()
                .any(|line| line.starts_with("Provider key") && line.ends_with("demo-agent")),
            "{output}"
        );
        assert!(
            output
                .lines()
                .any(|line| line.starts_with("Source ID") && line.ends_with("demo-source")),
            "{output}"
        );
    }

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

#[test]
fn provider_session_lookup_uses_the_complete_custom_route_tuple() {
    let temp = tempdir();
    let (_daemon, _) =
        import_custom_history_fixture_source_backed(&temp, "multiple-routes.jsonl");

    let ambiguous = failure_stderr(ctx(&temp).args([
        "locate",
        "session",
        "--provider-session",
        "shared-provider-session",
        "--provider",
        "custom",
    ]));
    assert!(ambiguous.contains("ambiguous between custom routes"), "{ambiguous}");
    assert!(ambiguous.contains("amp/threads"), "{ambiguous}");
    assert!(ambiguous.contains("internal-agent/archive"), "{ambiguous}");

    for (provider_key, source_id, expected_text) in [
        ("amp", "threads", "multi route collision oracle amp"),
        (
            "internal-agent",
            "archive",
            "multi route collision oracle internal",
        ),
    ] {
        let located = json_output(ctx(&temp).args([
            "locate",
            "session",
            "--provider-session",
            "shared-provider-session",
            "--provider-key",
            provider_key,
            "--source-id",
            source_id,
            "--format=json",
        ]));
        assert_eq!(located["provider"], "custom", "{located:#}");
        assert_eq!(located["provider_key"], provider_key, "{located:#}");
        assert_eq!(located["source_id"], source_id, "{located:#}");
        assert_eq!(
            located["provider_session_id"],
            "shared-provider-session",
            "{located:#}"
        );

        let shown = json_output(ctx(&temp).args([
            "show",
            "session",
            "--provider-session",
            "shared-provider-session",
            "--provider-key",
            provider_key,
            "--source-id",
            source_id,
            "--mode=full",
            "--format=json",
        ]));
        assert_eq!(shown["provider_key"], provider_key, "{shown:#}");
        assert_eq!(shown["source_id"], source_id, "{shown:#}");
        assert!(
            shown["events"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains(expected_text)),
            "{shown:#}"
        );
    }
}
