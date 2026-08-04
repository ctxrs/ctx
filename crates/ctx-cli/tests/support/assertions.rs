#[cfg(any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_fixtures))]
use rusqlite::Connection;
use serde_json::{json, Value};

pub(crate) fn assert_omits_keys(value: &Value, forbidden_keys: &[&str]) {
    match value {
        Value::Object(map) => {
            for key in forbidden_keys {
                assert!(
                    !map.contains_key(*key),
                    "forbidden JSON key {key} appeared in {value:#}"
                );
            }
            for nested in map.values() {
                assert_omits_keys(nested, forbidden_keys);
            }
        }
        Value::Array(items) => {
            for item in items {
                assert_omits_keys(item, forbidden_keys);
            }
        }
        _ => {}
    }
}

pub(crate) fn assert_explicit_source_publication<'a>(
    packet: &'a Value,
    provider: &str,
    source_format: &str,
) -> &'a Value {
    assert_explicit_source_publication_with_rejections(packet, provider, source_format, 0)
}

pub(crate) fn assert_explicit_source_publication_with_rejections<'a>(
    packet: &'a Value,
    provider: &str,
    source_format: &str,
    rejected_records: u64,
) -> &'a Value {
    assert_eq!(packet["schema_version"], 2, "{packet:#}");
    let has_rejections = rejected_records != 0;
    assert_eq!(
        packet["outcome"],
        if has_rejections {
            "completed_with_rejections"
        } else {
            "success"
        },
        "{packet:#}"
    );
    assert_eq!(
        packet["failure_scope"],
        if has_rejections { "record" } else { "none" },
        "{packet:#}"
    );
    assert_eq!(
        packet["failure_type"],
        if has_rejections {
            "record_rejection"
        } else {
            "none"
        },
        "{packet:#}"
    );
    let sources = packet["sources"]
        .as_array()
        .unwrap_or_else(|| panic!("missing explicit source receipts in {packet:#}"));
    assert_eq!(sources.len(), 1, "{packet:#}");
    let source = &sources[0];
    assert_eq!(source["provider"], provider, "{packet:#}");
    assert_eq!(source["source_format"], source_format, "{packet:#}");
    assert_eq!(
        source["status"],
        if has_rejections {
            "partial"
        } else {
            "published"
        },
        "{packet:#}"
    );
    assert_eq!(
        source["failure_scope"], packet["failure_scope"],
        "{packet:#}"
    );
    assert_eq!(source["failure_type"], packet["failure_type"], "{packet:#}");
    assert!(source["published_generation"].is_string(), "{packet:#}");
    for key in [
        "current_source_count",
        "current_indexed_documents",
        "current_complete_records",
        "current_retained_records",
        "current_rejected_records",
        "current_ignored_records",
        "current_certified_source_bytes",
        "current_sources_with_rejections",
        "removed_source_count",
    ] {
        assert!(
            packet["totals"][key].is_number(),
            "missing {key} in {packet:#}"
        );
        assert_eq!(packet["totals"][key], source[key], "{packet:#}");
    }
    assert_eq!(packet["totals"]["failed_sources"], 0, "{packet:#}");
    assert_eq!(
        packet["totals"]["rejected_records"], rejected_records,
        "{packet:#}"
    );
    let rejected_sources = usize::from(has_rejections);
    assert_eq!(
        packet["totals"]["sources_completed_with_rejections"], rejected_sources,
        "{packet:#}"
    );
    assert_eq!(
        packet["totals"]["rejections"],
        json!({
            "rejected_records": rejected_records,
            "sources_completed_with_rejections": rejected_sources,
        }),
        "{packet:#}"
    );
    assert_eq!(
        source["rejected_record_total"], rejected_records,
        "{packet:#}"
    );
    assert_omits_keys(
        packet,
        &["imported_sessions", "imported_events", "skipped_events"],
    );
    source
}

pub(crate) fn assert_authoritative_provider_publication(packet: &Value) -> &Value {
    assert_eq!(packet["schema_version"], 2, "{packet:#}");
    assert_eq!(packet["outcome"], "success", "{packet:#}");
    assert_eq!(packet["failure_scope"], "none", "{packet:#}");
    assert_eq!(packet["failure_type"], "none", "{packet:#}");
    let sources = packet["sources"]
        .as_array()
        .unwrap_or_else(|| panic!("missing authoritative refresh receipt in {packet:#}"));
    assert_eq!(sources.len(), 1, "{packet:#}");
    let source = &sources[0];
    assert_eq!(
        source["source_format"], "provider_authoritative_all",
        "{packet:#}"
    );
    assert_eq!(source["status"], "published", "{packet:#}");
    assert!(source["published_generation"].is_string(), "{packet:#}");
    for key in [
        "current_source_count",
        "current_indexed_documents",
        "current_complete_records",
        "current_retained_records",
        "current_rejected_records",
        "current_ignored_records",
        "current_certified_source_bytes",
        "current_sources_with_rejections",
        "removed_source_count",
    ] {
        assert!(
            packet["totals"][key].is_number(),
            "missing {key} in {packet:#}"
        );
        assert_eq!(packet["totals"][key], source[key], "{packet:#}");
    }
    assert_omits_keys(
        packet,
        &["imported_sessions", "imported_events", "skipped_events"],
    );
    assert_eq!(packet["totals"]["failed_sources"], 0, "{packet:#}");
    assert_eq!(packet["totals"]["rejected_records"], 0, "{packet:#}");
    assert_eq!(
        packet["totals"]["sources_completed_with_rejections"], 0,
        "{packet:#}"
    );
    assert!(packet["totals"]["rejections"].is_object(), "{packet:#}");
    source
}

pub(crate) fn assert_noop_publication(packet: &Value) {
    assert_eq!(packet["totals"]["change"], "no_op", "{packet:#}");
    assert_eq!(packet["sources"][0]["change"], "no_op", "{packet:#}");
    assert_eq!(
        packet["sources"][0]["generation_changed"], false,
        "{packet:#}"
    );
}

#[cfg(any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_fixtures))]
pub(crate) fn sqlite_column_text(conn: &Connection, sql: &str) -> String {
    let mut statement = conn.prepare(sql).unwrap();
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap();
    let mut text = String::new();
    for row in rows {
        text.push_str(&row.unwrap());
        text.push('\n');
    }
    text
}

#[cfg(any(all(test, not(ctx_cli_bazel_test)), ctx_cli_test_support_fixtures))]
pub(crate) fn sqlite_count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

pub(crate) fn assert_search_provider_oracle(
    packet: &Value,
    provider: &str,
    query: &str,
    expected_results: usize,
    expected_match_reason: &str,
) {
    assert_search_provider_oracle_with_scope(
        packet,
        provider,
        query,
        expected_results,
        expected_match_reason,
        "session_result",
        "session",
    );
}

pub(crate) fn assert_event_search_provider_oracle(
    packet: &Value,
    provider: &str,
    query: &str,
    expected_results: usize,
    expected_match_reason: &str,
) {
    assert_search_provider_oracle_with_scope(
        packet,
        provider,
        query,
        expected_results,
        expected_match_reason,
        "event",
        "event",
    );
}

pub(crate) fn assert_search_provider_oracle_with_scope(
    packet: &Value,
    provider: &str,
    query: &str,
    expected_results: usize,
    _expected_match_reason: &str,
    expected_result_type: &str,
    expected_scope: &str,
) {
    assert_eq!(packet["schema_version"], 1);
    assert_eq!(packet["query"], query);
    assert_eq!(packet["filters"]["provider"], provider);
    let results = packet["results"].as_array().unwrap();
    assert_eq!(
        results.len(),
        expected_results,
        "unexpected search result count in {packet:#}"
    );

    for result in results {
        assert_eq!(result["provider"], provider, "provider filter failed");
        assert!(result.get("source_exists").is_none(), "{result:#}");
        assert_eq!(result["result_type"], expected_result_type);
        assert_eq!(result["result_scope"], expected_scope);
        assert!(result["ctx_event_id"].is_string());
        assert!(result["ctx_session_id"].is_string());
        assert!(result["provider_session_id"].is_string());
        assert!(result.get("source_path").is_none(), "{result:#}");
        if expected_scope == "session" {
            assert!(result["session_importance"].is_number());
            assert!(result["more_matches_in_session"].is_number());
            assert_session_suggested_next_commands(result);
        } else {
            assert_eq!(result.get("session_importance"), None);
            assert_eq!(result.get("more_matches_in_session"), None);
            assert_event_suggested_next_commands(result);
        }
        assert_provider_citations(result, provider);
    }
}

pub(crate) fn assert_provider_citations(result: &Value, provider: &str) {
    let citations = result["citations"].as_array().unwrap();
    assert!(!citations.is_empty(), "missing citations in {result:#}");
    for citation in citations {
        assert!(
            citation["item_id"].is_string(),
            "citation needs a ctx-owned item id in {citation:#}"
        );
        assert_eq!(citation["target_type"], "event");
        assert!(citation["ctx_event_id"].is_string());
        assert!(citation["ctx_session_id"].is_string());
        assert_eq!(citation["provider"], provider, "citation provider failed");
        assert!(citation.get("source_exists").is_none(), "{citation:#}");
        assert!(citation.get("source_path").is_none(), "{citation:#}");
    }
}

pub(crate) fn assert_session_suggested_next_commands(result: &Value) {
    let commands = result["suggested_next_commands"].as_array().unwrap();
    assert!(
        commands
            .iter()
            .all(|command| !command.as_str().unwrap_or("").contains("--mode lite")),
        "lite default should not be restated in suggestions: {result:#}"
    );
    assert!(
        commands.iter().any(|command| {
            let command = command.as_str().unwrap_or("");
            command.starts_with("ctx ") && command.contains(" show session ")
        }),
        "missing show session suggestion in {result:#}"
    );
    assert!(
        commands.iter().any(|command| {
            let command = command.as_str().unwrap_or("");
            command.starts_with("ctx ")
                && command.contains(" search ")
                && command.contains(" --session ")
        }),
        "missing session event drilldown suggestion in {result:#}"
    );
    assert!(
        commands.iter().any(|command| {
            let command = command.as_str().unwrap_or("");
            command.starts_with("ctx ") && command.contains(" show event ")
        }),
        "missing representative event suggestion in {result:#}"
    );
}

pub(crate) fn assert_event_suggested_next_commands(result: &Value) {
    let commands = result["suggested_next_commands"].as_array().unwrap();
    assert!(
        commands
            .iter()
            .all(|command| !command.as_str().unwrap_or("").contains("--mode lite")),
        "lite default should not be restated in suggestions: {result:#}"
    );
    assert!(
        commands.iter().any(|command| {
            let command = command.as_str().unwrap_or("");
            command.starts_with("ctx ") && command.contains(" show event ")
        }),
        "missing show event suggestion in {result:#}"
    );
    assert!(
        !commands.iter().any(|command| command
            .as_str()
            .unwrap_or("")
            .starts_with("ctx export session ")),
        "search should not suggest exporting transcripts by default in {result:#}"
    );
    assert!(commands.iter().any(|command| {
        let command = command.as_str().unwrap_or("");
        command.starts_with("ctx ")
            && command.contains(" search ")
            && command.contains(" --session ")
    }));
}
