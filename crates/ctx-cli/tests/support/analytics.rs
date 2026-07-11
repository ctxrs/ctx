use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

pub(crate) const CAPABILITY_PROPERTY_KEYS: [&str; 5] = [
    "capability_snapshot_schema",
    "available_parallelism_bucket",
    "host_memory_bucket",
    "cpu_vector_tier",
    "acceleration_candidate",
];

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

pub(crate) fn expected_device_path(_home: &Path, _state: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        _state.join("ctx").join("device.json")
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
        _state.join("ctx").join("device.json")
    }
}

pub(crate) fn expected_capability_marker_path(home: &Path, state: &Path) -> PathBuf {
    expected_device_path(home, state).with_file_name("execution-capabilities-v1.reported")
}

pub(crate) fn expected_capability_claim_path(home: &Path, state: &Path) -> PathBuf {
    expected_device_path(home, state).with_file_name("execution-capabilities-v1.claim")
}

pub(crate) fn assert_no_capability_state(home: &Path, state: &Path) {
    assert!(!expected_capability_marker_path(home, state).exists());
    assert!(!expected_capability_claim_path(home, state).exists());
}

pub(crate) fn assert_capability_snapshot_is_coarse(properties: &serde_json::Map<String, Value>) {
    assert_eq!(properties["capability_snapshot_schema"], 1);
    assert_string_property_is_one_of(
        properties,
        "available_parallelism_bucket",
        &[
            "unknown", "1", "2", "3-4", "5-8", "9-16", "17-32", "33-64", "65+",
        ],
    );
    assert_string_property_is_one_of(
        properties,
        "host_memory_bucket",
        &[
            "unknown", "lt_4gb", "4-8gb", "8-16gb", "16-32gb", "32-64gb", "64gb+",
        ],
    );
    assert_string_property_is_one_of(
        properties,
        "cpu_vector_tier",
        &["avx512", "avx2", "x86_baseline", "arm_neon", "other"],
    );
    assert_string_property_is_one_of(
        properties,
        "acceleration_candidate",
        &["apple_ane", "nvidia_cuda", "not_detected", "unknown"],
    );

    for raw_key in [
        "available_parallelism",
        "host_memory_bucket_raw",
        "host_memory_bytes",
        "cpu_model",
        "cpu_name",
        "gpu_model",
        "gpu_name",
        "cuda_device_name",
        "hardware_id",
        "serial_number",
    ] {
        assert!(
            !properties.contains_key(raw_key),
            "analytics exposed raw hardware property {raw_key}: {properties:#?}"
        );
    }
}

fn assert_string_property_is_one_of(
    properties: &serde_json::Map<String, Value>,
    key: &str,
    allowed: &[&str],
) {
    let value = properties[key]
        .as_str()
        .unwrap_or_else(|| panic!("capability property {key} must be a string: {properties:#?}"));
    assert!(
        allowed.contains(&value),
        "unexpected capability property {key}={value:?}: {properties:#?}"
    );
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
        "acceleration_candidate",
        "action",
        "all_sources",
        "analytics_client",
        "available_parallelism_bucket",
        "available_sources_bucket",
        "auto_upgrade_allowed",
        "auto_upgrade_due",
        "auto_upgrade_probe",
        "auto_upgrade_spawn_status",
        "auto_upgrade_spawned",
        "background",
        "capability_snapshot_schema",
        "catalog_only",
        "catalog_source_bytes_bucket",
        "cataloged_sessions_bucket",
        "citation_count_bucket",
        "cpu_vector_tier",
        "db_size_bucket",
        "daemon_command",
        "dry_run",
        "edges_imported_bucket",
        "event_results",
        "failed_bucket",
        "failed_sources_bucket",
        "failure_kind",
        "finding_count_bucket",
        "force",
        "has_event_type_filter",
        "has_file_filter",
        "has_indexed_content_after_setup",
        "has_indexed_content_after_search",
        "has_provider_filter",
        "has_query",
        "has_session_filter",
        "has_since_filter",
        "has_workspace_filter",
        "host_memory_bucket",
        "had_existing_store_before_search",
        "had_indexed_content_before_search",
        "include_current_session",
        "include_subagents",
        "indexed_content_before_search_known",
        "indexed_events_bucket",
        "indexed_items_bucket",
        "indexed_sessions_bucket",
        "indexed_sources_bucket",
        "install_manager",
        "initialized",
        "inventory_source_bytes_bucket",
        "inventory_source_files_bucket",
        "inventory_sources_bucket",
        "json_output",
        "limit_bucket",
        "native_sources_bucket",
        "no_daemon",
        "once",
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
        "search_backend_effective",
        "search_backend_requested",
        "sessions_imported_bucket",
        "setup_completed",
        "setup_result",
        "skipped_bucket",
        "source_files_bucket",
        "source_mode",
        "start_mode",
        "store_created_by_search",
        "target_kind",
        "transcript_mode",
        "trigger_command",
        "managed_install",
        "self_upgrade_allowed",
        "update_available",
        "upgrade_applied",
        "upgrade_channel",
        "upgrade_failure_kind",
        "upgrade_mode",
        "upgrade_operation",
        "upgrade_scheduled",
        "upgrade_status",
        "upgrade_warning_count_bucket",
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
