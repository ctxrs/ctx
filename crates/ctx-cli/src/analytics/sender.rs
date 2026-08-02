use std::{env, path::Path};

use anyhow::Result;
use chrono::Timelike;
use ctx_history_core::{utc_now, CaptureProvider};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::{
    config::AppConfig,
    execution_capabilities::{self, CapabilitySnapshotV1},
    install_marker, net,
};

use super::*;

const MAX_EVENTS_PER_REQUEST: usize = 50;

pub(crate) fn send_batch(data_root: &Path, config: &AppConfig, events: &[PublicEventV1]) {
    if events.is_empty()
        || !config.analytics.enabled
        || env::var_os("CTX_ANALYTICS_DRY_RUN").is_some()
    {
        return;
    }
    if let Err(err) = send_batch_inner(data_root, config, events) {
        if env::var_os("CTX_ANALYTICS_DEBUG").is_some() {
            eprintln!("ctx analytics delivery failed: {err:#}");
        }
    }
}

fn send_batch_inner(data_root: &Path, config: &AppConfig, events: &[PublicEventV1]) -> Result<()> {
    let client_profile_id = crate::identity::device_id(data_root)?;
    let data_root_id = crate::identity::installation_id(data_root)?;
    let install_marker = install_marker::current_exe_install_marker();
    let mut capability_snapshot = execution_capabilities::pending(data_root).ok().flatten();
    let occurred_at = minute_rounded_now();
    let serialized = events
        .iter()
        .enumerate()
        .map(|(index, event)| {
            serialize_event(
                event,
                occurred_at,
                (index < MAX_EVENTS_PER_REQUEST)
                    .then(|| {
                        capability_snapshot
                            .as_ref()
                            .map(|pending| pending.snapshot())
                    })
                    .flatten(),
                install_marker
                    .as_ref()
                    .map(|marker| marker.install_attempt_id.as_str()),
            )
        })
        .collect::<Vec<_>>();
    let snapshot_attached = capability_snapshot.is_some();
    post_event_chunks(
        &serialized,
        snapshot_attached,
        |chunk| {
            let body = serialize_batch_body(&client_profile_id, &data_root_id, chunk)?;
            net::post_telemetry_json(&config.analytics.endpoint, &body)
        },
        || {
            if let Some(snapshot) = capability_snapshot.take() {
                snapshot.mark_reported()?;
            }
            Ok(())
        },
    )
}

fn serialize_batch_body(
    client_profile_id: &str,
    data_root_id: &str,
    events: &[Value],
) -> Result<Vec<u8>> {
    let payload = json!({
        "client_profile_id": client_profile_id,
        "data_root_id": data_root_id,
        "app_version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "events": events,
    });
    Ok(serde_json::to_vec(&payload)?)
}

fn post_event_chunks(
    events: &[Value],
    snapshot_attached: bool,
    mut post: impl FnMut(&[Value]) -> Result<()>,
    mut acknowledge_snapshot: impl FnMut() -> Result<()>,
) -> Result<()> {
    for (index, chunk) in events.chunks(MAX_EVENTS_PER_REQUEST).enumerate() {
        post(chunk)?;
        if index == 0 && snapshot_attached {
            acknowledge_snapshot()?;
        }
    }
    Ok(())
}

fn minute_rounded_now() -> chrono::DateTime<chrono::Utc> {
    utc_now()
        .with_second(0)
        .and_then(|value| value.with_nanosecond(0))
        .expect("rounding a valid UTC timestamp to a minute must succeed")
}

pub(super) fn serialize_event(
    event: &PublicEventV1,
    occurred_at: chrono::DateTime<chrono::Utc>,
    capability_snapshot: Option<&CapabilitySnapshotV1>,
    install_attempt_id: Option<&str>,
) -> Value {
    let (event_name, surface, operation, outcome, duration, mut properties) = match event {
        PublicEventV1::OperationCompleted(event) => {
            let mut properties = operation_properties(event);
            if let Some(output) = event.output {
                properties.insert("output".to_owned(), json!(output.as_str()));
            }
            if event.deprecated_daemon_control {
                properties.insert("deprecated_daemon_control".to_owned(), json!(true));
            }
            if event.deprecated_upgrade_control {
                properties.insert("deprecated_upgrade_control".to_owned(), json!(true));
            }
            (
                "operation_completed",
                event.payload.surface(),
                event.payload.name(),
                event.outcome,
                event.duration,
                properties,
            )
        }
        PublicEventV1::ProviderRefreshCompleted(event) => {
            let mut properties = Map::new();
            if let Some(foreground) = event.foreground {
                insert_provider_refresh_properties(&mut properties, &foreground);
            }
            (
                "provider_refresh_completed",
                event.surface,
                "refresh",
                event.outcome,
                event.duration,
                properties,
            )
        }
        PublicEventV1::RuntimeObservation(event) => {
            let mut properties = Map::new();
            event.kind.insert_properties(&mut properties);
            (
                "runtime_observation",
                event.kind.surface(),
                event.kind.name(),
                event.outcome,
                event.duration,
                properties,
            )
        }
    };
    if let Some(snapshot) = capability_snapshot {
        insert_capability_properties(&mut properties, snapshot);
    }
    if install_attempt_id.is_some() {
        properties.insert("install_manager".to_owned(), json!("ctx-hosted-installer"));
    }
    let mut value = json!({
        "event_id": event_id(event),
        "event_name": event_name,
        "event_version": 1,
        "occurred_at": occurred_at,
        "surface": surface.as_str(),
        "operation": operation,
        "outcome": outcome.as_str(),
        "duration_bucket": duration.as_str(),
        "properties": properties,
    });
    if let Some(install_attempt_id) = install_attempt_id {
        value["install_attempt_id"] = json!(install_attempt_id);
    }
    value
}

fn event_id(event: &PublicEventV1) -> String {
    let _ = event;
    Uuid::new_v4().to_string()
}

fn operation_properties(event: &OperationCompletedV1) -> Map<String, Value> {
    let mut properties = Map::new();
    match &event.payload {
        OperationPayloadV1::Cli(operation) => {
            insert_client_operation_properties(&mut properties, operation)
        }
        OperationPayloadV1::Mcp(operation) => operation.insert_properties(&mut properties),
        OperationPayloadV1::ProHost(operation) => operation.insert_properties(&mut properties),
        OperationPayloadV1::Daemon(operation) => operation.insert_properties(&mut properties),
    }
    properties
}

fn insert_provider_refresh_properties(
    properties: &mut Map<String, Value>,
    refresh: &ForegroundProviderRefreshV1,
) {
    if let Some(provider) = refresh.provider {
        insert_str(properties, "provider", provider.as_str());
    }
    insert_str(properties, "trigger", refresh.trigger.as_str());
    insert_optional_str(
        properties,
        "source_mode",
        refresh.source_mode.map(ProviderRefreshSourceMode::as_str),
    );
    insert_str(properties, "change", refresh.change.as_str());
    insert_str(
        properties,
        "content_evidence",
        refresh.content_evidence.as_str(),
    );
    insert_optional_str(
        properties,
        "work_kind",
        refresh.work_kind.map(ProviderRefreshWorkKind::as_str),
    );
    insert_str(
        properties,
        "refresh_result",
        refresh.refresh_result.as_str(),
    );
    insert_str(properties, "core_result", refresh.core_result.as_str());
    insert_str(
        properties,
        "canonical_pro_result",
        refresh.canonical_pro_result.as_str(),
    );
    insert_str(
        properties,
        "output_pro_result",
        refresh.output_pro_result.as_str(),
    );
    insert_str(properties, "failure_scope", refresh.failure_scope.as_str());
    insert_str(properties, "failure_type", refresh.failure_type.as_str());
    insert_bool(properties, "work_remaining", refresh.work_remaining);
    insert_optional_count(
        properties,
        "retired_records_bucket",
        refresh.retired_records,
    );
    if let Some(counts) = refresh.counts {
        insert_optional_count(properties, "sources_bucket", Some(counts.sources));
        insert_optional_count(properties, "source_files_bucket", Some(counts.source_files));
        insert_optional_count(properties, "sessions_bucket", Some(counts.sessions));
        insert_optional_count(properties, "events_bucket", Some(counts.events));
        insert_optional_count(properties, "edges_bucket", Some(counts.edges));
        insert_optional_count(properties, "skips_bucket", Some(counts.skips));
        insert_optional_count(properties, "rejections_bucket", Some(counts.rejections));
        insert_optional_count(properties, "failures_bucket", Some(counts.failures));
        insert_optional_bytes(properties, "bytes_bucket", Some(counts.bytes));
    }
    if let Some(performance) = refresh.performance {
        insert_optional_duration(
            properties,
            "cpu_duration_bucket",
            Some(performance.cpu_duration),
        );
        insert_optional_bytes(
            properties,
            "observed_process_peak_rss_bucket",
            performance.observed_process_peak_rss,
        );
    }
}

fn insert_client_operation_properties(
    properties: &mut Map<String, Value>,
    operation: &ClientOperationV1,
) {
    match operation {
        ClientOperationV1::Setup(value) => {
            insert_bool(properties, "catalog_only", value.catalog_only);
            insert_bool(properties, "no_daemon", value.no_daemon);
            insert_bool(properties, "wait", value.wait);
            insert_str(properties, "progress_mode", value.progress_mode.as_str());
            insert_optional_str(properties, "setup_mode", value.mode.map(SetupMode::as_str));
            insert_optional_count(
                properties,
                "providers_detected_bucket",
                value.providers_detected,
            );
            insert_optional_count(
                properties,
                "cataloged_sessions_bucket",
                value.cataloged_sessions,
            );
            insert_optional_count(
                properties,
                "inventory_sources_bucket",
                value.inventory_sources,
            );
            insert_optional_count(
                properties,
                "inventory_source_files_bucket",
                value.inventory_source_files,
            );
            insert_optional_count(
                properties,
                "pending_sessions_bucket",
                value.pending_sessions,
            );
            insert_optional_bytes(
                properties,
                "catalog_source_bytes_bucket",
                value.catalog_source_bytes,
            );
            insert_optional_bytes(
                properties,
                "inventory_source_bytes_bucket",
                value.inventory_source_bytes,
            );
            insert_optional_bool(
                properties,
                "has_indexed_content_after_setup",
                value.has_indexed_content,
            );
            insert_import_result_properties(properties, &value.import);
        }
        ClientOperationV1::Status(value) => {
            insert_optional_bool(properties, "initialized", value.initialized);
            insert_optional_count(properties, "indexed_items_bucket", value.indexed_items);
            insert_optional_count(
                properties,
                "indexed_sessions_bucket",
                value.indexed_sessions,
            );
            insert_optional_count(properties, "indexed_events_bucket", value.indexed_events);
            insert_optional_count(properties, "indexed_sources_bucket", value.indexed_sources);
        }
        ClientOperationV1::Index(value) => {
            insert_optional_str(
                properties,
                "index_operation",
                value.operation.map(IndexOperation::as_str),
            );
            insert_optional_bool(properties, "wait_lexical", value.wait_lexical);
            insert_optional_bool(properties, "wait_semantic", value.wait_semantic);
            insert_optional_str(
                properties,
                "wait_outcome",
                value.wait_outcome.map(WaitOutcome::as_str),
            );
            insert_optional_bool(properties, "initialized", value.initialized);
            insert_optional_str(
                properties,
                "lexical_state",
                value.lexical_state.map(IndexState::as_str),
            );
            insert_optional_str(
                properties,
                "semantic_state",
                value.semantic_state.map(IndexState::as_str),
            );
            insert_optional_count(properties, "indexed_items_bucket", value.indexed_items);
        }
        ClientOperationV1::Sources(value) => {
            insert_bool(properties, "all_sources", value.all);
            insert_bool(properties, "show_missing", value.show_missing);
            insert_optional_provider(properties, "provider_filter", value.provider_filter);
            insert_optional_count(
                properties,
                "providers_detected_bucket",
                value.providers_detected,
            );
            insert_optional_count(
                properties,
                "providers_existing_bucket",
                value.providers_existing,
            );
            insert_optional_count(
                properties,
                "providers_importable_bucket",
                value.providers_importable,
            );
        }
        ClientOperationV1::Import(value) => insert_import_properties(properties, value),
        ClientOperationV1::Show(value) => {
            insert_str(properties, "target_kind", value.target_kind.as_str());
            insert_optional_str(
                properties,
                "transcript_mode",
                value.transcript_mode.map(TranscriptModeKind::as_str),
            );
            insert_str(properties, "output_format", value.output_format.as_str());
            insert_bool(properties, "writes_out_file", value.writes_out_file);
            insert_bool(properties, "provider_lookup", value.provider_lookup);
            insert_optional_count(properties, "window_bucket", value.window);
            insert_optional_count(properties, "events_returned_bucket", value.events_returned);
        }
        ClientOperationV1::Locate(value) => {
            insert_str(properties, "target_kind", value.target_kind.as_str());
            insert_str(properties, "output_format", value.output_format.as_str());
            insert_bool(properties, "provider_lookup", value.provider_lookup);
        }
        ClientOperationV1::Search(value) => insert_search_properties(properties, value),
        ClientOperationV1::Docs(value) => {
            insert_optional_str(
                properties,
                "docs_operation",
                value.operation.map(DocsOperation::as_str),
            );
            insert_bool(properties, "implicit_list", value.implicit_list);
            insert_optional_text_length(properties, "query_length_bucket", value.query_length);
            insert_optional_count(
                properties,
                "query_term_count_bucket",
                value.query_term_count,
            );
            insert_optional_count(properties, "result_count_bucket", value.result_count);
            insert_optional_bool(properties, "zero_result", value.zero_result);
            insert_optional_str(properties, "topic", value.topic.map(DocTopicId::as_str));
            insert_bool(properties, "writes_output", value.writes_output);
        }
        ClientOperationV1::Integration(value) => insert_integration_properties(properties, value),
        ClientOperationV1::Upgrade(value) => insert_upgrade_properties(properties, value),
        ClientOperationV1::Doctor(value) => {
            insert_optional_count(properties, "finding_count_bucket", value.finding_count);
            insert_optional_bool(properties, "healthy", value.healthy);
        }
    }
}

fn insert_import_properties(properties: &mut Map<String, Value>, value: &ImportTelemetry) {
    insert_bool(properties, "resume", value.resume);
    insert_bool(properties, "all_sources", value.all_sources);
    insert_bool(properties, "no_daemon", value.no_daemon);
    insert_str(properties, "source_mode", value.source_mode.as_str());
    insert_optional_provider(properties, "provider_filter", value.provider_filter);
    insert_bool(properties, "reset_cursor", value.reset_cursor);
    insert_str(properties, "progress_mode", value.progress_mode.as_str());
    insert_import_result_properties(properties, value);
}

fn insert_import_result_properties(properties: &mut Map<String, Value>, value: &ImportTelemetry) {
    insert_optional_count(properties, "sources_seen_bucket", value.sources_seen);
    insert_optional_bytes(properties, "source_bytes_bucket", value.source_bytes);
    insert_optional_count(properties, "source_files_bucket", value.source_files);
    insert_optional_count(properties, "failed_sources_bucket", value.failed_sources);
    insert_optional_count(
        properties,
        "sessions_imported_bucket",
        value.sessions_imported,
    );
    insert_optional_count(properties, "events_imported_bucket", value.events_imported);
    insert_optional_count(properties, "edges_imported_bucket", value.edges_imported);
    insert_optional_count(properties, "skipped_bucket", value.skipped);
    insert_optional_count(
        properties,
        "rejected_records_bucket",
        value.rejected_records,
    );
    insert_optional_str(
        properties,
        "import_outcome",
        value.outcome.map(ImportOutcome::as_str),
    );
    insert_optional_str(
        properties,
        "import_failure_scope",
        value.failure_scope.map(ImportFailureScope::as_str),
    );
    insert_optional_str(
        properties,
        "import_failure_type",
        value.failure_type.map(ImportFailureType::as_str),
    );
}

fn insert_search_properties(properties: &mut Map<String, Value>, value: &SearchTelemetry) {
    insert_bool(properties, "has_query", value.has_query);
    insert_bool(properties, "has_provider_filter", value.has_provider_filter);
    insert_bool(
        properties,
        "has_workspace_filter",
        value.has_workspace_filter,
    );
    insert_bool(properties, "has_since_filter", value.has_since_filter);
    insert_bool(
        properties,
        "has_event_type_filter",
        value.has_event_type_filter,
    );
    insert_bool(properties, "has_file_filter", value.has_file_filter);
    insert_bool(properties, "has_session_filter", value.has_session_filter);
    insert_bool(properties, "event_results", value.event_results);
    insert_bool(properties, "primary_only", value.primary_only);
    insert_bool(properties, "include_subagents", value.include_subagents);
    insert_bool(
        properties,
        "include_current_session",
        value.include_current_session,
    );
    insert_str(properties, "limit_bucket", value.limit.as_str());
    insert_optional_provider(properties, "provider_filter", value.provider_filter);
    insert_optional_duration(
        properties,
        "refresh_duration_bucket",
        value.refresh_duration,
    );
    insert_optional_str(
        properties,
        "search_refresh_mode",
        value.refresh_mode.map(|mode| mode.as_str()),
    );
    insert_optional_str(
        properties,
        "search_refresh_status",
        value.refresh_status.map(RefreshStatus::as_str),
    );
    insert_optional_count(
        properties,
        "search_refresh_source_count_bucket",
        value.refresh_source_count,
    );
    insert_optional_bool(
        properties,
        "has_indexed_content_after_search",
        value.has_indexed_content_after,
    );
    insert_optional_text_length(properties, "query_length_bucket", value.query_length);
    insert_optional_count(
        properties,
        "query_term_count_bucket",
        value.query_term_count,
    );
    insert_optional_duration(properties, "query_duration_bucket", value.query_duration);
    insert_optional_str(
        properties,
        "search_backend_requested",
        value.backend_requested.map(|backend| backend.as_str()),
    );
    insert_optional_str(
        properties,
        "search_backend_effective",
        value.backend_effective.map(|backend| backend.as_str()),
    );
    insert_optional_count(properties, "result_count_bucket", value.result_count);
    insert_optional_count(properties, "citation_count_bucket", value.citation_count);
    insert_optional_bool(properties, "zero_result", value.zero_result);
    insert_optional_duration(properties, "render_duration_bucket", value.render_duration);
}

fn insert_integration_properties(
    properties: &mut Map<String, Value>,
    value: &IntegrationTelemetry,
) {
    insert_optional_str(
        properties,
        "integration_action",
        value.action.map(IntegrationAction::as_str),
    );
    insert_optional_str(
        properties,
        "integration_target",
        value.target.map(IntegrationTarget::as_str),
    );
    insert_optional_str(
        properties,
        "integration_scope",
        value.scope.map(IntegrationScope::as_str),
    );
    insert_optional_str(
        properties,
        "target_agent_group",
        value.selection.map(TargetSelection::as_str),
    );
    insert_optional_bool(properties, "force", value.force);
    insert_optional_count(
        properties,
        "target_agents_count_bucket",
        value.target_agents,
    );
    insert_optional_count(
        properties,
        "resolved_agents_count_bucket",
        value.resolved_agents,
    );
    insert_optional_str(
        properties,
        "integration_result",
        value.result.map(IntegrationResult::as_str),
    );
    insert_optional_count(
        properties,
        "modified_targets_bucket",
        value.modified_targets,
    );
    insert_optional_bool(properties, "already_installed", value.already_installed);
    insert_optional_bool(properties, "updated", value.updated);
    insert_optional_count(properties, "current_targets_bucket", value.current_targets);
    insert_optional_count(properties, "missing_targets_bucket", value.missing_targets);
    insert_optional_count(
        properties,
        "conflicting_targets_bucket",
        value.conflicting_targets,
    );
    insert_optional_count(properties, "invalid_targets_bucket", value.invalid_targets);
    insert_optional_count(
        properties,
        "unsupported_targets_bucket",
        value.unsupported_targets,
    );
}

fn insert_upgrade_properties(properties: &mut Map<String, Value>, value: &UpgradeTelemetry) {
    insert_str(properties, "upgrade_mode", value.mode.as_str());
    insert_str(properties, "upgrade_operation", value.operation.as_str());
    insert_bool(properties, "dry_run", value.dry_run);
    insert_optional_str(
        properties,
        "upgrade_status",
        value.status.map(UpgradeStatus::as_str),
    );
    insert_optional_bool(properties, "upgrade_applied", value.applied);
    insert_optional_bool(properties, "upgrade_scheduled", value.scheduled);
    insert_optional_bool(properties, "update_available", value.update_available);
    insert_optional_bool(
        properties,
        "update_was_available",
        value.update_was_available,
    );
    insert_optional_str(
        properties,
        "upgrade_attempt_id",
        value.upgrade_attempt_id.as_deref(),
    );
    insert_optional_bool(properties, "managed_install", value.managed_install);
    insert_optional_bool(
        properties,
        "self_upgrade_allowed",
        value.self_upgrade_allowed,
    );
    insert_optional_bool(
        properties,
        "auto_upgrade_allowed",
        value.auto_upgrade_allowed,
    );
    insert_optional_count(
        properties,
        "upgrade_warning_count_bucket",
        value.warning_count,
    );
    insert_optional_str(
        properties,
        "upgrade_channel",
        value.channel.map(UpgradeChannel::as_str),
    );
    insert_optional_str(
        properties,
        "upgrade_failure_kind",
        value.failure_kind.map(UpgradeFailureKind::as_str),
    );
}

fn insert_capability_properties(
    properties: &mut Map<String, Value>,
    snapshot: &CapabilitySnapshotV1,
) {
    properties.insert("capability_snapshot_schema".to_owned(), json!(1));
    insert_str(
        properties,
        "available_parallelism_bucket",
        snapshot.available_parallelism.as_str(),
    );
    insert_str(
        properties,
        "host_memory_bucket",
        snapshot.host_memory.as_str(),
    );
    insert_str(properties, "cpu_vector_tier", snapshot.cpu_vector.as_str());
    insert_str(
        properties,
        "acceleration_candidate",
        snapshot.acceleration.as_str(),
    );
}

fn insert_str(properties: &mut Map<String, Value>, key: &'static str, value: &str) {
    properties.insert(key.to_owned(), Value::String(value.to_owned()));
}

fn insert_bool(properties: &mut Map<String, Value>, key: &'static str, value: bool) {
    properties.insert(key.to_owned(), Value::Bool(value));
}

fn insert_optional_str(
    properties: &mut Map<String, Value>,
    key: &'static str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        insert_str(properties, key, value);
    }
}

fn insert_optional_bool(
    properties: &mut Map<String, Value>,
    key: &'static str,
    value: Option<bool>,
) {
    if let Some(value) = value {
        insert_bool(properties, key, value);
    }
}

fn insert_optional_count(
    properties: &mut Map<String, Value>,
    key: &'static str,
    value: Option<CountBucket>,
) {
    insert_optional_str(properties, key, value.map(CountBucket::as_str));
}

fn insert_optional_bytes(
    properties: &mut Map<String, Value>,
    key: &'static str,
    value: Option<BytesBucket>,
) {
    insert_optional_str(properties, key, value.map(BytesBucket::as_str));
}

fn insert_optional_text_length(
    properties: &mut Map<String, Value>,
    key: &'static str,
    value: Option<TextLengthBucket>,
) {
    insert_optional_str(properties, key, value.map(TextLengthBucket::as_str));
}

fn insert_optional_duration(
    properties: &mut Map<String, Value>,
    key: &'static str,
    value: Option<DurationBucket>,
) {
    insert_optional_str(properties, key, value.map(DurationBucket::as_str));
}

fn insert_optional_provider(
    properties: &mut Map<String, Value>,
    key: &'static str,
    value: Option<CaptureProvider>,
) {
    if let Some(value) = value {
        properties.insert(key.to_owned(), Value::String(value.as_str().to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered_events(count: usize) -> Vec<Value> {
        (0..count).map(|index| json!({ "index": index })).collect()
    }

    #[test]
    fn outbound_payloads_never_exceed_fifty_events_and_preserve_order() {
        for (event_count, expected_chunk_sizes) in [
            (1, vec![1]),
            (49, vec![49]),
            (50, vec![50]),
            (51, vec![50, 1]),
            (100, vec![50, 50]),
            (101, vec![50, 50, 1]),
            (123, vec![50, 50, 23]),
        ] {
            let events = numbered_events(event_count);
            let mut payloads = Vec::new();

            post_event_chunks(
                &events,
                false,
                |chunk| {
                    let body = serialize_batch_body("client", "root", chunk)?;
                    payloads.push(serde_json::from_slice::<Value>(&body)?);
                    Ok(())
                },
                || panic!("a batch without a capability snapshot must not acknowledge one"),
            )
            .unwrap();

            assert_eq!(
                payloads
                    .iter()
                    .map(|payload| payload["events"].as_array().unwrap().len())
                    .collect::<Vec<_>>(),
                expected_chunk_sizes
            );
            assert!(payloads.iter().all(|payload| {
                payload["events"].as_array().unwrap().len() <= MAX_EVENTS_PER_REQUEST
            }));
            assert_eq!(
                payloads
                    .iter()
                    .flat_map(|payload| payload["events"].as_array().unwrap())
                    .map(|event| event["index"].as_u64().unwrap())
                    .collect::<Vec<_>>(),
                (0..event_count as u64).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn capability_ack_tracks_the_snapshot_bearing_chunk_not_later_chunks() {
        for (failure_on_post, expected_posts, expected_acks, should_succeed) in [
            (Some(1), 1, 0, false),
            (Some(2), 2, 1, false),
            (None, 3, 1, true),
        ] {
            let events = numbered_events(101);
            let mut posts = 0;
            let mut acknowledgements = 0;
            let result = post_event_chunks(
                &events,
                true,
                |_chunk| {
                    posts += 1;
                    if failure_on_post == Some(posts) {
                        return Err(anyhow::anyhow!("injected post failure"));
                    }
                    Ok(())
                },
                || {
                    acknowledgements += 1;
                    Ok(())
                },
            );

            assert_eq!(result.is_ok(), should_succeed);
            assert_eq!(posts, expected_posts);
            assert_eq!(acknowledgements, expected_acks);
        }
    }
}
