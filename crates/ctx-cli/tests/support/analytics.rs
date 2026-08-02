use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command as StdCommand, Stdio},
    time::{Duration, Instant},
};

use super::{bind_test_ctx_binary, ctx, TempDir};

pub(crate) struct SourceRefreshDaemon {
    child: Option<Child>,
}

impl Drop for SourceRefreshDaemon {
    fn drop(&mut self) {
        if let Err(error) =
            super::terminate_and_reap_test_child(&mut self.child, "analytics source-refresh daemon")
        {
            if std::thread::panicking() {
                eprintln!("analytics daemon teardown also failed: {error}");
            } else {
                panic!("analytics daemon teardown failed: {error}");
            }
        }
    }
}

pub(crate) fn start_source_refresh_daemon(
    temp: &TempDir,
    data_root: &Path,
    home: &Path,
    state: &Path,
) -> SourceRefreshDaemon {
    fs::create_dir_all(data_root).unwrap();
    fs::write(
        data_root.join("config.toml"),
        "[daemon]\nenabled = true\nmode = \"source-refresh-only\"\n\n[search]\nsemantic = false\n",
    )
    .unwrap();
    bind_test_ctx_binary(temp);
    let prepared = ctx(temp);
    let mut command = StdCommand::new(prepared.get_program());
    for (name, value) in prepared.get_envs() {
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }
    command
        .args([
            "daemon",
            "run",
            "--force",
            "--idle-exit-seconds",
            "600",
            "--loop-interval-seconds",
            "600",
        ])
        .env("CTX_DATA_ROOT", data_root)
        .env("HOME", home)
        .env("XDG_STATE_HOME", state)
        .env("LOCALAPPDATA", state)
        .env("CTX_DAEMON_MODE", "source-refresh-only")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .unwrap_or_else(|error| panic!("start isolated source-refresh daemon: {error}"));
    let mut daemon = SourceRefreshDaemon { child: Some(child) };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(exit) = daemon.child.as_mut().unwrap().try_wait().unwrap() {
            let mut stderr = String::new();
            daemon
                .child
                .as_mut()
                .unwrap()
                .stderr
                .as_mut()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            panic!("source-refresh daemon exited before becoming ready ({exit}): {stderr}");
        }
        let status = ctx(temp)
            .args(["daemon", "status", "--format=json"])
            .env("CTX_DATA_ROOT", data_root)
            .env("HOME", home)
            .env("XDG_STATE_HOME", state)
            .env("LOCALAPPDATA", state)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok());
        if status.as_ref().is_some_and(|status| {
            status["daemon"]["running"] == true
                && status["daemon"]["core_refresh_endpoint"]["available"] == true
        }) {
            ctx(temp)
                .args([
                    "import",
                    "--all",
                    "--no-daemon",
                    "--format=json",
                    "--progress",
                    "none",
                ])
                .env("CTX_DATA_ROOT", data_root)
                .env("HOME", home)
                .env("XDG_STATE_HOME", state)
                .env("LOCALAPPDATA", state)
                .assert()
                .success();
            return daemon;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for source-refresh daemon readiness: {status:#?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

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
    analytics_cli_event(event)["properties"]
        .as_object()
        .unwrap()
}

pub(crate) fn analytics_cli_event(event: &Value) -> &Value {
    event["events"]
        .as_array()
        .and_then(|events| {
            events.iter().find(|event| {
                event["event_name"] == "operation_completed" && event["surface"] == "cli"
            })
        })
        .unwrap_or_else(|| panic!("analytics batch has no CLI operation event: {event:#}"))
}

pub(crate) fn assert_operation_event(event: &Value, operation: &str, outcome: &str) {
    let event = analytics_cli_event(event);
    assert_eq!(event["event_name"], "operation_completed");
    assert_eq!(event["event_version"], 1);
    assert_eq!(event["surface"], "cli");
    assert_eq!(event["operation"], operation);
    assert_eq!(event["outcome"], outcome);
    assert!(event["event_id"].as_str().is_some_and(|value| {
        uuid::Uuid::parse_str(value).is_ok_and(|id| id.get_version_num() == 4)
    }));
    assert!(event.get("duration_ms").is_none());
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
        "all_sources",
        "already_installed",
        "available_parallelism_bucket",
        "auto_upgrade_allowed",
        "capability_snapshot_schema",
        "catalog_only",
        "catalog_source_bytes_bucket",
        "cataloged_sessions_bucket",
        "citation_count_bucket",
        "cpu_vector_tier",
        "conflicting_targets_bucket",
        "current_targets_bucket",
        "deprecated_daemon_control",
        "deprecated_upgrade_control",
        "docs_operation",
        "dry_run",
        "edges_imported_bucket",
        "events_imported_bucket",
        "event_results",
        "rejected_records_bucket",
        "failed_sources_bucket",
        "events_returned_bucket",
        "finding_count_bucket",
        "force",
        "healthy",
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
        "include_current_session",
        "include_subagents",
        "indexed_events_bucket",
        "indexed_items_bucket",
        "indexed_sessions_bucket",
        "indexed_sources_bucket",
        "index_operation",
        "input",
        "install_manager",
        "integration_action",
        "integration_result",
        "integration_scope",
        "integration_target",
        "invalid_targets_bucket",
        "initialized",
        "inventory_source_bytes_bucket",
        "inventory_source_files_bucket",
        "inventory_sources_bucket",
        "import_failure_scope",
        "import_failure_type",
        "import_outcome",
        "implicit_list",
        "lexical_state",
        "limit_bucket",
        "missing_targets_bucket",
        "modified_targets_bucket",
        "no_daemon",
        "output",
        "output_format",
        "pending_sessions_bucket",
        "primary_only",
        "progress_mode",
        "provider_filter",
        "provider_lookup",
        "providers_detected_bucket",
        "providers_existing_bucket",
        "providers_importable_bucket",
        "query_duration_bucket",
        "query_length_bucket",
        "query_term_count_bucket",
        "refresh_duration_bucket",
        "render_duration_bucket",
        "result_count_bucket",
        "reset_cursor",
        "resume",
        "search_refresh_mode",
        "search_refresh_source_count_bucket",
        "search_refresh_status",
        "search_backend_effective",
        "search_backend_requested",
        "sessions_imported_bucket",
        "resolved_agents_count_bucket",
        "resource_kind",
        "returned_columns_bucket",
        "returned_rows_bucket",
        "rows_truncated",
        "semantic_state",
        "setup_mode",
        "show_missing",
        "skipped_bucket",
        "source_files_bucket",
        "source_bytes_bucket",
        "source_mode",
        "sources_seen_bucket",
        "target_agent_group",
        "target_agents_count_bucket",
        "target_kind",
        "topic",
        "transcript_mode",
        "unsupported_targets_bucket",
        "managed_install",
        "self_upgrade_allowed",
        "update_available",
        "update_was_available",
        "upgrade_applied",
        "upgrade_attempt_id",
        "upgrade_channel",
        "upgrade_failure_kind",
        "upgrade_mode",
        "upgrade_operation",
        "upgrade_scheduled",
        "upgrade_status",
        "upgrade_warning_count_bucket",
        "updated",
        "values_truncated",
        "wait",
        "wait_lexical",
        "wait_outcome",
        "wait_semantic",
        "window_bucket",
        "writes_out_file",
        "writes_output",
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
