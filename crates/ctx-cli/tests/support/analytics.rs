use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

pub(crate) fn read_analytics_events(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

pub(crate) fn analytics_event_properties(event: &Value) -> &serde_json::Map<String, Value> {
    event["events"][0]["properties"].as_object().unwrap()
}

pub(crate) fn analytics_cli_event(event: &Value) -> &Value {
    &event["events"][0]
}

pub(crate) fn expected_device_path(_home: &Path, state: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        state.join("ctx").join("device.json")
    }
    #[cfg(target_os = "macos")]
    {
        _home
            .join("Library")
            .join("Application Support")
            .join("ctx")
            .join("device.json")
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        state.join("ctx").join("device.json")
    }
}

pub(crate) fn assert_no_json_string_contains(value: &Value, forbidden: &[&str]) {
    match value {
        Value::String(text) => {
            for needle in forbidden {
                assert!(
                    !text.contains(needle),
                    "analytics leaked forbidden string {needle:?} in {text:?}"
                );
            }
        }
        Value::Array(values) => {
            for value in values {
                assert_no_json_string_contains(value, forbidden);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                assert_no_json_string_contains(value, forbidden);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

pub(crate) fn assert_analytics_properties_are_allowlisted(
    properties: &serde_json::Map<String, Value>,
) {
    let allowed = [
        "action",
        "all_sources",
        "analytics_client",
        "available_sources_bucket",
        "background",
        "catalog_only",
        "catalog_source_bytes_bucket",
        "cataloged_sessions_bucket",
        "citation_count_bucket",
        "db_size_bucket",
        "dry_run",
        "edges_imported_bucket",
        "event_results",
        "failed_bucket",
        "failed_sources_bucket",
        "failure_kind",
        "finding_count_bucket",
        "has_event_type_filter",
        "has_file_filter",
        "has_provider_filter",
        "has_query",
        "has_session_filter",
        "has_since_filter",
        "has_workspace_filter",
        "include_current_session",
        "include_subagents",
        "indexed_events_bucket",
        "indexed_items_bucket",
        "indexed_sessions_bucket",
        "indexed_sources_bucket",
        "install_manager",
        "initialized",
        "json_output",
        "limit_bucket",
        "native_sources_bucket",
        "output_format",
        "pending_sessions_bucket",
        "primary_only",
        "progress_mode",
        "provider_filter",
        "provider_lookup",
        "providers_detected_bucket",
        "query_duration_bucket",
        "query_length_bucket",
        "query_term_count_bucket",
        "refresh_duration_bucket",
        "render_duration_bucket",
        "result_count_bucket",
        "resume",
        "search_refresh_mode",
        "search_refresh_source_count_bucket",
        "search_refresh_status",
        "sessions_imported_bucket",
        "skipped_bucket",
        "source_files_bucket",
        "source_mode",
        "target_kind",
        "transcript_mode",
        "window_bucket",
        "writes_out_file",
        "zero_result",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();

    for key in properties.keys() {
        assert!(
            allowed.contains(key.as_str()),
            "unexpected analytics property {key}: {properties:#?}"
        );
    }
}
