import {
  CURRENT_PROVIDERS,
  isProviderId,
  normalizeProviderId,
  PROVIDERS,
} from "./provider-contract";
import { ORDINARY_BLAME_PROPERTY_KEYS, parseOrdinaryBlameProperties } from "./ordinary-blame-contract";

export { CURRENT_PROVIDERS, PROVIDERS } from "./provider-contract";

export type TelemetryScalar = string | number | boolean | null;

export class TelemetryIngestError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string) {
    super(code);
    this.status = status;
    this.code = code;
  }
}

export const MAX_EVENTS = 50;
export const MAX_EVENT_BYTES = 8 * 1024;
export const MAX_BATCH_ENVELOPE_BYTES = 8 * 1024;
export const MAX_BODY_BYTES = MAX_EVENTS * MAX_EVENT_BYTES + MAX_BATCH_ENVELOPE_BYTES;
export const MAX_DEPTH = 6;

export const V1_BATCH_KEYS = new Set([
  "client_profile_id", "data_root_id", "app_version", "os", "arch", "events",
]);
export const V1_EVENT_KEYS = new Set([
  "event_id", "event_name", "event_version", "occurred_at", "surface", "operation",
  "outcome", "duration_bucket", "install_attempt_id", "properties",
]);
export const INSTALL_STAGE_V1_KEYS = new Set([
  "event_name", "event_version", "install_attempt_id", "stage", "status", "platform", "arch",
  "script_family",
]);
export const LEGACY_INSTALL_STAGE_KEYS = new Set([
  "install_attempt_id", "stage", "status", "error_kind", "platform", "channel", "version",
]);

export const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/u;
export const UUID_V4_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/u;
export const UUID_V7_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/u;
export const INSTALL_ATTEMPT_ID_PATTERN = /^ia_[A-Za-z0-9_-]{8,128}$/u;
const UPGRADE_ATTEMPT_ID_PATTERN = /^[A-Za-z0-9._-]{1,128}$/u;
export const RELEASE_VERSION_PATTERN = /^[0-9A-Za-z][0-9A-Za-z.+_-]{0,63}$/u;

export const EVENT_NAMES = new Set([
  "analytics_delivery_observation", "operation_completed", "provider_refresh_completed",
  "runtime_observation",
]);
export const SURFACES = new Set(["cli", "mcp", "pro_host", "daemon"]);
export const OUTCOMES = new Set(["success", "failure"]);
export const OPERATING_SYSTEMS = new Set(["linux", "macos", "windows", "freebsd"]);
export const ARCHITECTURES = new Set(["x86_64", "aarch64", "x86", "arm"]);
export const DURATION_BUCKETS = new Set([
  "unknown", "lt_100ms", "lt_1s", "lt_5s", "lt_30s", "lt_2m", "lt_10m",
  "lt_1h", "gte_1h",
]);
export const COUNT_BUCKETS = new Set([
  "0", "1", "2-5", "6-20", "21-100", "101-1k", "1k+", "1k-10k", "10k-100k",
  "100k-1m", "1m+",
]);
export const BYTE_BUCKETS = new Set([
  "0", "lt_100kb", "100kb-1mb", "1mb-10mb", "10mb-100mb", "100mb-1gb", "1gb+",
  "1gb-10gb", "10gb-100gb", "1gb-2gb", "2gb-5gb", "5gb-10gb", "10gb-25gb",
  "25gb-50gb", "50gb-100gb", "100gb+",
]);
const SEARCH_COUNT_BUCKETS = new Set([
  "0", "1", "2-5", "6-20", "21-100", "101-1k", "1k-10k", "10k-100k",
  "100k-1m", "1m+",
]);
const SEARCH_BYTE_BUCKETS = new Set([
  "0", "lt_100kb", "100kb-1mb", "1mb-10mb", "10mb-100mb", "100mb-1gb",
  "1gb-2gb", "2gb-5gb", "5gb-10gb", "10gb-25gb", "25gb-50gb", "50gb-100gb",
  "100gb+",
]);
const TEXT_LENGTH_BUCKETS = new Set(["0", "1-20", "21-100", "101-500", "500+"]);

export const CURRENT_CLI_OPERATIONS = new Set([
  "setup", "semantic_enable", "semantic_status", "semantic_disable", "status", "index",
  "sources", "import", "show", "locate", "search", "docs", "integration", "upgrade",
  "doctor", "blame",
]);
const HISTORICAL_RETIRED_CLI_OPERATIONS = new Set(["sql"]);
const CLI_OPERATIONS = new Set([
  ...CURRENT_CLI_OPERATIONS,
  ...HISTORICAL_RETIRED_CLI_OPERATIONS,
]);
export const CURRENT_MCP_OPERATIONS = new Set([
  "status", "sources", "search", "show_session", "show_event", "query_events", "blame", "pro_status",
  "unknown", "missing",
]);
const HISTORICAL_RETIRED_MCP_OPERATIONS = new Set(["sql"]);
// Ingestion accepts already-emitted SQL events. These retired values are not
// a current CLI/MCP product contract and do not imply any local projection.
export const MCP_OPERATIONS = new Set([
  ...CURRENT_MCP_OPERATIONS,
  ...HISTORICAL_RETIRED_MCP_OPERATIONS,
]);
const PRO_HOST_OPERATIONS = new Set(["lifecycle", "materialize", "query", "status", "blame"]);
const DAEMON_OPERATIONS = new Set(["enable", "disable", "status", "run_once"]);
const SURFACE_OPERATIONS = new Map<string, ReadonlySet<string>>([
  ["cli", CLI_OPERATIONS],
  ["mcp", MCP_OPERATIONS],
  ["pro_host", PRO_HOST_OPERATIONS],
  ["daemon", DAEMON_OPERATIONS],
]);
const RUNTIME_OPERATIONS = new Map<string, ReadonlySet<string>>([
  ["daemon", new Set(["ready", "stopped", "recovered", "failed", "cycle", "liveness"])],
  ["mcp", new Set(["initialized", "stopped"])],
]);
const PROVIDER_REFRESH_OPERATIONS = new Set(["refresh"]);
const PROVIDER_REFRESH_SURFACES = new Set(["cli", "daemon"]);

export const SHARED_PROPERTY_KEYS = new Set([
  "install_manager", "capability_snapshot_schema", "available_parallelism_bucket",
  "host_memory_bucket", "cpu_vector_tier", "acceleration_candidate",
]);
const COMMON_OPERATION_PROPERTY_KEYS = new Set([
  "output", ...SHARED_PROPERTY_KEYS, "auto_upgrade_probe",
  "auto_upgrade_due", "auto_upgrade_spawned", "auto_upgrade_spawn_status", "auto_upgrade_channel",
  "deprecated_daemon_control", "deprecated_upgrade_control",
]);
const AUTO_UPGRADE_PROPERTY_KEYS = [
  "auto_upgrade_probe", "auto_upgrade_due", "auto_upgrade_spawned", "auto_upgrade_spawn_status",
  "auto_upgrade_channel",
] as const;
const AUTO_UPGRADE_ELIGIBLE_OPERATIONS = new Set([
  "setup", "sources", "import", "show", "locate", "search", "integration", "doctor",
]);
export const SEARCH_TERMINAL_PROPERTY_KEYS = new Set([
  "search_output_duration_bucket", "search_output_served",
  "search_retrieval_round_count_bucket", "search_query_execution_count_bucket",
  "search_candidate_rows_total_bucket", "search_candidate_records_decoded_bucket",
  "search_candidate_core_bytes_decoded_bucket", "search_final_candidate_pool_bucket",
  "search_candidate_pool_truncated", "search_stop_reason", "search_failure_phase",
  "search_candidate_session_count_bucket", "search_largest_session_candidate_share_bucket",
  "search_literal_root_concentration_availability",
  "search_candidate_literal_root_family_count_bucket",
  "search_literal_root_candidate_coverage_bucket",
  "search_largest_literal_root_candidate_share_bucket",
  "search_provider_copy_candidate_count_bucket", "search_provider_copy_candidate_share_bucket",
  "search_copy_cluster_availability", "search_diversification_status",
  "search_diversification_changed_final_top_n",
]);
export const MCP_SEARCH_SIDECAR_PROPERTY_KEYS = new Set([
  "refresh_duration_bucket", "search_refresh_status", "search_refresh_source_count_bucket",
  "query_duration_bucket", "search_backend_requested", "search_backend_effective",
]);
const OPERATION_PROPERTY_KEYS = new Map<string, ReadonlySet<string>>([
  ["blame", ORDINARY_BLAME_PROPERTY_KEYS],
  ["setup", setOf(
    "catalog_only", "no_daemon", "wait", "progress_mode", "setup_mode", "providers_detected_bucket",
    "cataloged_sessions_bucket", "inventory_sources_bucket", "inventory_source_files_bucket",
    "pending_sessions_bucket", "catalog_source_bytes_bucket", "inventory_source_bytes_bucket",
    "has_indexed_content_after_setup", ...storeKeys(), ...importResultKeys(),
  )],
  ["semantic_enable", setOf()],
  ["semantic_status", setOf()],
  ["semantic_disable", setOf()],
  ["status", setOf(
    "initialized", "indexed_items_bucket", "indexed_sessions_bucket", "indexed_events_bucket",
    "indexed_sources_bucket", "inventory_units_bucket", "pending_inventory_units_bucket",
    "failed_inventory_units_bucket", "stale_inventory_units_bucket",
  )],
  ["index", setOf(
    "index_operation", "wait_lexical", "wait_semantic", "wait_outcome", "initialized", "lexical_state",
    "semantic_state", "indexed_items_bucket", "inventory_units_bucket", "pending_inventory_units_bucket",
    "failed_inventory_units_bucket", "stale_inventory_units_bucket",
  )],
  ["sources", setOf(
    "all_sources", "show_missing", "provider_filter", "providers_detected_bucket",
    "providers_existing_bucket", "providers_importable_bucket",
  )],
  ["import", setOf(
    "resume", "all_sources", "no_daemon", "source_mode", "provider_filter", "reset_cursor",
    "progress_mode", ...importResultKeys(),
  )],
  ["show", setOf(
    "target_kind", "resource_kind", "transcript_mode", "output_format", "writes_out_file",
    "provider_lookup", "window_bucket", "events_returned_bucket",
  )],
  ["locate", setOf("target_kind", "resource_kind", "output_format", "provider_lookup")],
  ["search", setOf(
    "has_query", "has_provider_filter", "has_workspace_filter", "has_since_filter",
    "has_event_type_filter", "has_file_filter", "has_session_filter", "event_results", "primary_only",
    "include_subagents", "include_current_session", "limit_bucket", "provider_filter",
    "had_existing_store_before_search", "indexed_content_before_search_known",
    "had_indexed_content_before_search", "refresh_duration_bucket", "search_refresh_mode",
    "search_refresh_status", "search_refresh_source_count_bucket", "store_created_by_search",
    "has_indexed_content_after_search", "query_length_bucket", "query_term_count_bucket",
    "query_duration_bucket", "search_backend_requested", "search_backend_effective",
    "result_count_bucket", "citation_count_bucket", "zero_result", "render_duration_bucket",
    ...SEARCH_TERMINAL_PROPERTY_KEYS, ...storeKeys(),
  )],
  // Historical schema retained only for already-emitted CLI SQL events.
  ["sql", setOf(
    "input", "output_format", "returned_rows_bucket", "returned_columns_bucket", "rows_truncated",
    "values_truncated", "query_duration_bucket",
  )],
  ["docs", setOf(
    "docs_operation", "implicit_list", "query_length_bucket", "query_term_count_bucket",
    "result_count_bucket", "zero_result", "topic", "writes_output",
  )],
  ["integration", setOf(
    "integration_action", "integration_target", "integration_scope", "target_agent_group", "force",
    "target_agents_count_bucket", "resolved_agents_count_bucket", "integration_result",
    "modified_targets_bucket", "already_installed", "updated", "current_targets_bucket",
    "missing_targets_bucket", "conflicting_targets_bucket", "invalid_targets_bucket",
    "unsupported_targets_bucket",
  )],
  ["upgrade", setOf(
    "upgrade_mode", "upgrade_operation", "dry_run", "upgrade_status", "upgrade_applied",
    "upgrade_scheduled", "update_available", "update_was_available", "upgrade_attempt_id",
    "managed_install", "self_upgrade_allowed", "auto_upgrade_allowed",
    "upgrade_warning_count_bucket", "upgrade_channel", "upgrade_failure_kind",
  )],
  ["doctor", setOf("finding_count_bucket", "healthy")],
]);
const KNOWN_OPERATION_PROPERTY_KEYS = new Set([
  ...COMMON_OPERATION_PROPERTY_KEYS,
  ...[...OPERATION_PROPERTY_KEYS.values()].flatMap((keys) => [...keys]),
]);
const REQUIRED_OPERATION_PROPERTY_KEYS = new Map<string, readonly string[]>([
  ["setup", ["no_daemon", "wait", "progress_mode"]],
  ["sources", ["all_sources"]],
  ["import", ["resume", "all_sources", "no_daemon", "source_mode", "reset_cursor", "progress_mode"]],
  ["show", ["target_kind", "output_format", "writes_out_file", "provider_lookup"]],
  ["locate", ["target_kind", "output_format", "provider_lookup"]],
  ["search", [
    "has_query", "has_provider_filter", "has_workspace_filter", "has_since_filter",
    "has_event_type_filter", "has_file_filter", "has_session_filter", "event_results", "primary_only",
    "include_current_session", "limit_bucket",
  ]],
  ["sql", ["input", "output_format"]], // historical ingestion compatibility
  ["docs", ["implicit_list", "writes_output"]],
  ["upgrade", ["upgrade_mode", "upgrade_operation", "dry_run"]],
]);

const BOOLEAN_PROPERTIES = new Set([
  "catalog_only", "no_daemon", "wait", "has_indexed_content_after_setup", "initialized",
  "wait_lexical", "wait_semantic", "all_sources", "show_missing", "resume", "reset_cursor",
  "writes_out_file", "provider_lookup", "has_query", "has_provider_filter", "has_workspace_filter",
  "has_since_filter", "has_event_type_filter", "has_file_filter", "has_session_filter", "event_results",
  "primary_only", "include_subagents", "include_current_session", "had_existing_store_before_search",
  "indexed_content_before_search_known", "had_indexed_content_before_search", "store_created_by_search",
  "has_indexed_content_after_search", "zero_result", "rows_truncated", "values_truncated",
  "implicit_list", "writes_output", "force", "already_installed", "updated", "dry_run",
  "upgrade_applied", "upgrade_scheduled", "update_available", "update_was_available", "managed_install",
  "self_upgrade_allowed", "auto_upgrade_allowed", "healthy", "auto_upgrade_probe", "auto_upgrade_due",
  "auto_upgrade_spawned", "deprecated_daemon_control", "deprecated_upgrade_control",
  "search_output_served", "search_candidate_pool_truncated",
  "search_diversification_changed_final_top_n",
]);

const PROPERTY_ENUMS = new Map<string, ReadonlySet<string>>([
  ["output", new Set(["human", "json"])],
  ["install_manager", new Set(["ctx-hosted-installer"])],
  ["available_parallelism_bucket", new Set(["unknown", "1", "2", "3-4", "5-8", "9-16", "17-32", "33-64", "65+"])],
  ["host_memory_bucket", new Set(["unknown", "lt_4gb", "4-8gb", "8-16gb", "16-32gb", "32-64gb", "64gb+"])],
  ["cpu_vector_tier", new Set(["avx512", "avx2", "x86_baseline", "arm_neon", "other"])],
  ["acceleration_candidate", new Set(["apple_ane", "nvidia_cuda", "not_detected", "unknown"])],
  ["progress_mode", new Set(["auto", "plain", "json", "none"])],
  ["setup_mode", new Set(["catalog_only", "ready", "background"])],
  ["index_operation", new Set(["status", "mode", "watch", "wait"])],
  ["wait_outcome", new Set(["ready", "blocked", "timeout"])],
  ["lexical_state", indexStates()], ["semantic_state", indexStates()],
  ["source_mode", new Set(["explicit_format", "history_source_plugin", "explicit_path", "all_discovered", "discovered_provider", "auto_discovered"])],
  ["import_outcome", new Set(["success", "failure", "completed_with_rejections", "completed_with_source_failures", "completed_with_rejections_and_source_failures"])],
  ["import_failure_scope", new Set(["none", "record", "source", "record_and_source", "invocation"])],
  ["import_failure_type", new Set(["none", "record_rejection", "source_failure", "record_rejection_and_source_failure", "invalid_request", "store", "io", "other"])],
  ["target_kind", new Set(["session", "event", "events", "resource"])],
  ["resource_kind", new Set(["commit", "pull_request", "issue", "file", "branch", "repository"])],
  ["transcript_mode", new Set(["lite", "full", "log"])],
  ["output_format", new Set(["text", "json", "jsonl", "markdown", "csv", "raw"])],
  ["search_refresh_mode", new Set(["background", "off", "wait"])],
  ["search_refresh_status", new Set([
    "disabled", "existing_generation", "skipped", "no_sources", "daemon_background",
    "daemon_unavailable", "completed", "failed", "background", "unknown",
  ])],
  ["search_backend_requested", new Set(["hybrid", "lexical", "semantic"])],
  ["search_backend_effective", new Set(["hybrid", "lexical", "semantic"])],
  ["search_stop_reason", new Set(["decisive", "exhausted", "candidate_cap", "fixed_pool"])],
  ["search_failure_phase", new Set([
    "preparation", "refresh", "generation_open", "query_preparation", "semantic_retrieval",
    "index_query_decode", "result_projection", "render", "output",
  ])],
  ["search_largest_session_candidate_share_bucket", shareBuckets()],
  ["search_literal_root_concentration_availability", new Set([
    "observed", "not_observed_dense",
  ])],
  ["search_literal_root_candidate_coverage_bucket", shareBuckets()],
  ["search_largest_literal_root_candidate_share_bucket", shareBuckets()],
  ["search_provider_copy_candidate_share_bucket", shareBuckets()],
  ["search_copy_cluster_availability", new Set(["not_constructed_v1"])],
  ["search_diversification_status", new Set([
    "applied", "not_applicable", "indeterminate",
  ])],
  ["input", new Set(["inline", "stdin", "file", "missing"])],
  ["docs_operation", new Set(["list", "search", "show", "man_print", "man_generate"])],
  ["topic", docTopics()],
  ["integration_action", new Set(["install", "remove", "status"])],
  ["integration_target", new Set(["mcp", "skills", "slash_commands", "plugin"])],
  ["integration_scope", new Set(["global", "project"])],
  ["target_agent_group", new Set(["all", "detected", "explicit", "picker", "fallback"])],
  ["integration_result", new Set(["ok", "partial_error", "all_current", "none_current", "partially_current"])],
  ["upgrade_mode", new Set(["manual", "auto"])],
  ["upgrade_operation", new Set(["apply", "check", "status", "enable", "disable"])],
  ["upgrade_status", new Set(["available", "up_to_date", "applied", "scheduled", "dry_run", "status_checked", "auto_enabled", "auto_disabled", "locked", "skipped", "failed", "unknown"])],
  ["upgrade_channel", new Set(["stable", "beta", "canary", "dev", "other"])],
  ["upgrade_failure_kind", new Set(["lock_failed", "unmanaged_install", "metadata_fetch", "signature_verify", "metadata_invalid", "artifact_verify", "artifact_download", "policy_disallowed", "apply_failed"])],
  ["auto_upgrade_spawn_status", new Set(["auto_disabled", "ci", "background_child", "not_due", "marker_invalid", "current_exe_error", "spawned", "spawn_failed"])],
  ["auto_upgrade_channel", new Set(["stable", "beta", "canary", "dev", "other"])],
]);

const ALL_OPERATION_KEYS = new Set(
  Array.from(OPERATION_PROPERTY_KEYS.values()).flatMap((keys) => Array.from(keys)),
);
const COUNT_BUCKET_PROPERTY_KEYS = new Set([
  ...Array.from(ALL_OPERATION_KEYS).filter((key) =>
    key.endsWith("_bucket") && !key.includes("bytes") && key !== "db_size_bucket" &&
    key !== "query_length_bucket" && !key.endsWith("duration_bucket")
  ),
  "finding_count_bucket",
]);
const BYTE_BUCKET_PROPERTY_KEYS = new Set([
  "catalog_source_bytes_bucket", "inventory_source_bytes_bucket", "source_bytes_bucket", "db_size_bucket",
]);
const SEARCH_COUNT_BUCKET_PROPERTY_KEYS = new Set([
  "search_retrieval_round_count_bucket", "search_query_execution_count_bucket",
  "search_candidate_rows_total_bucket", "search_candidate_records_decoded_bucket",
  "search_final_candidate_pool_bucket", "search_refresh_source_count_bucket",
  "search_candidate_session_count_bucket",
  "search_candidate_literal_root_family_count_bucket",
  "search_provider_copy_candidate_count_bucket",
]);
const SEARCH_BYTE_BUCKET_PROPERTY_KEYS = new Set(["search_candidate_core_bytes_decoded_bucket"]);
const TEXT_BUCKET_PROPERTY_KEYS = new Set(["query_length_bucket"]);
const DURATION_BUCKET_PROPERTY_KEYS = new Set([
  "refresh_duration_bucket", "query_duration_bucket", "render_duration_bucket",
  "search_output_duration_bucket",
]);

export const INSTALL_STAGES = new Set([
  "installer", "artifact_download", "binary_install", "skill_install", "setup", "uninstall",
]);
export const INSTALL_STATUSES = new Set(["started", "completed", "failed", "skipped"]);
export const INSTALL_PLATFORMS = new Set(["linux", "macos", "windows"]);
export const INSTALL_ARCHITECTURES = new Set(["x64", "arm64"]);
export const INSTALL_SCRIPT_FAMILIES = new Set(["posix", "powershell"]);
export const LEGACY_INSTALL_STAGES = new Set([
  "script_started", "artifact_download_started", "artifact_download_completed", "binary_installed",
  "skill_launched", "skill_exited", "skill_skipped", "setup_launched", "setup_exited",
  "setup_skipped", "installer_failed",
]);
export const LEGACY_INSTALL_CHANNELS = new Set(["stable", "beta", "canary", "dev"]);
const FIXED_INSTALL_ERRORS = new Set([
  "exit", "checksum_mismatch", "decompression_failed", "interrupted", "terminated", "skill_failed",
  "setup_failed", "exception",
]);

const INSTALL_STATUS_PAIRS = new Map<string, ReadonlySet<string>>([
  ["installer", new Set(["started", "completed", "failed"])],
  ["artifact_download", new Set(["started", "completed"])],
  ["binary_install", new Set(["completed"])],
  ["skill_install", new Set(["started", "completed", "failed", "skipped"])],
  ["setup", new Set(["started", "completed", "failed", "skipped"])],
  ["uninstall", new Set(["started", "completed", "failed"])],
]);

export function parseOperationProperties(
  value: unknown,
  operation: string,
  surface: string,
  outcome: string,
): Record<string, TelemetryScalar> {
  const properties = requireRecord(value, "invalid_properties");
  if (surface !== "cli") throw schemaError("invalid_surface");
  const operationKeys = OPERATION_PROPERTY_KEYS.get(operation);
  if (!operationKeys) throw schemaError("invalid_operation");
  rejectUnknownKeys(
    properties,
    new Set([...COMMON_OPERATION_PROPERTY_KEYS, ...operationKeys]),
    "unknown_operation_property",
  );
  const automaticUpgrade = operation === "upgrade" && properties.upgrade_mode === "auto";
  if (Object.hasOwn(properties, "output")) {
    requireEnum(properties.output, new Set(["human", "json"]), "missing_output");
  } else if (!automaticUpgrade) {
    throw schemaError("missing_output");
  }
  for (const key of REQUIRED_OPERATION_PROPERTY_KEYS.get(operation) ?? []) {
    if (!Object.hasOwn(properties, key)) throw schemaError(`missing_${key}`);
  }
  if (operation === "show" || operation === "locate") {
    const targetKinds = operation === "show"
      ? new Set(["session", "event", "events", "resource"])
      : new Set(["session", "event", "resource"]);
    requireEnum(properties.target_kind, targetKinds, "invalid_target_kind");
  }
  const upgradeAttemptId = operation === "upgrade" && Object.hasOwn(properties, "upgrade_attempt_id")
    ? requireUpgradeAttemptId(properties.upgrade_attempt_id)
    : null;
  const out = validateKnownProperties(Object.fromEntries(
    Object.entries(properties).filter(([key]) =>
      key !== "upgrade_attempt_id" &&
      (operation !== "search" || !SEARCH_TERMINAL_PROPERTY_KEYS.has(key)) &&
      (operation !== "blame" || !ORDINARY_BLAME_PROPERTY_KEYS.has(key))
    ),
  ));
  if (operation === "show" && out.target_kind === "events") {
    validateListEventsProperties(out, outcome);
  }
  if (upgradeAttemptId !== null) out.upgrade_attempt_id = upgradeAttemptId;
  if (operation === "upgrade") validateUpgradeProperties(out, outcome);
  if (operation === "search") {
    Object.assign(out, validateSearchTerminalProperties(properties));
  }
  if (operation === "blame") Object.assign(out, parseOrdinaryBlameProperties(properties, outcome, true));
  validateCliOperationSidecars(out, operation, outcome);
  return out;
}

function validateListEventsProperties(
  properties: Record<string, TelemetryScalar>,
  outcome: string,
): void {
  if (
    properties.output !== "json" ||
    !new Set(["json", "jsonl"]).has(String(properties.output_format)) ||
    properties.writes_out_file !== false ||
    hasAny(properties, ["resource_kind", "transcript_mode", "window_bucket"])
  ) {
    throw schemaError("invalid_list_events_shape");
  }
  if (outcome !== "success" && Object.hasOwn(properties, "events_returned_bucket")) {
    throw schemaError("invalid_list_events_shape");
  }
}

export function validateSearchTerminalProperties(
  properties: Record<string, unknown>,
): Record<string, TelemetryScalar> {
  const out = validateKnownProperties(Object.fromEntries(
    Object.entries(properties).filter(([key]) => SEARCH_TERMINAL_PROPERTY_KEYS.has(key)),
  ));
  validateSearchConcentrationProperties(out);
  return out;
}

function validateSearchConcentrationProperties(
  properties: Record<string, TelemetryScalar>,
): void {
  const concentrationKeys = [
    "search_candidate_session_count_bucket",
    "search_largest_session_candidate_share_bucket",
    "search_literal_root_concentration_availability",
    "search_provider_copy_candidate_count_bucket",
    "search_provider_copy_candidate_share_bucket",
    "search_copy_cluster_availability",
    "search_diversification_status",
  ] as const;
  const required = ["search_final_candidate_pool_bucket", ...concentrationKeys] as const;
  const rootMeasurements = [
    "search_candidate_literal_root_family_count_bucket",
    "search_literal_root_candidate_coverage_bucket",
    "search_largest_literal_root_candidate_share_bucket",
  ] as const;
  const all = [
    ...concentrationKeys,
    ...rootMeasurements,
    "search_diversification_changed_final_top_n",
  ];
  if (!hasAny(properties, all)) return;
  if (required.some((key) => !Object.hasOwn(properties, key))) {
    throw schemaError("incomplete_search_concentration");
  }
  const rootsObserved = properties.search_literal_root_concentration_availability === "observed";
  if (
    rootMeasurements.some((key) => Object.hasOwn(properties, key) !== rootsObserved)
  ) {
    throw schemaError("inconsistent_search_literal_root_concentration");
  }
  if (
    Object.hasOwn(properties, "search_diversification_changed_final_top_n") !==
    (properties.search_diversification_status === "applied")
  ) {
    throw schemaError("inconsistent_search_diversification");
  }
  if (
    properties.search_literal_root_concentration_availability === "not_observed_dense" &&
    properties.search_diversification_status !== "not_applicable"
  ) {
    throw schemaError("inconsistent_search_diversification");
  }
}

export function validateMcpSearchSidecarProperties(
  properties: Record<string, unknown>,
): Record<string, TelemetryScalar> {
  return validateKnownProperties(Object.fromEntries(
    Object.entries(properties).filter(([key]) => MCP_SEARCH_SIDECAR_PROPERTY_KEYS.has(key)),
  ));
}

function validateUpgradeProperties(
  properties: Record<string, TelemetryScalar>,
  outcome: string,
): void {
  for (const key of ["upgrade_status", "upgrade_applied", "upgrade_scheduled"]) {
    if (!Object.hasOwn(properties, key)) throw schemaError(`missing_${key}`);
  }
  const status = String(properties.upgrade_status);
  const failed = outcome === "failure";
  if ((status === "failed") !== failed) throw schemaError("invalid_upgrade_shape");
  if (Object.hasOwn(properties, "upgrade_failure_kind") !== failed) {
    throw schemaError("invalid_upgrade_shape");
  }

  if (properties.upgrade_mode === "auto") {
    validateAutomaticUpgradeProperties(properties, status);
    return;
  }
  validateManualUpgradeProperties(properties, status, failed);
}

function validateAutomaticUpgradeProperties(
  properties: Record<string, TelemetryScalar>,
  status: string,
): void {
  if (
    properties.upgrade_operation !== "apply" ||
    properties.dry_run !== false ||
    Object.hasOwn(properties, "output") ||
    hasAny(properties, ["deprecated_daemon_control", "deprecated_upgrade_control"]) ||
    !new Set(["up_to_date", "applied", "skipped", "failed"]).has(status) ||
    properties.upgrade_applied !== (status === "applied") ||
    properties.upgrade_scheduled !== false ||
    properties.update_available !== false ||
    !Object.hasOwn(properties, "upgrade_attempt_id")
  ) {
    throw schemaError("invalid_upgrade_shape");
  }

  const planKeys = [
    "update_was_available", "managed_install", "self_upgrade_allowed",
    "auto_upgrade_allowed", "upgrade_warning_count_bucket", "upgrade_channel",
  ];
  const planKeyCount = countPresent(properties, planKeys);
  if (planKeyCount !== 0 && planKeyCount !== planKeys.length) {
    throw schemaError("invalid_upgrade_shape");
  }
  if (new Set(["up_to_date", "skipped"]).has(status) && planKeyCount === 0) {
    throw schemaError("invalid_upgrade_shape");
  }
  if (status === "up_to_date" && properties.update_was_available !== false) {
    throw schemaError("invalid_upgrade_shape");
  }
}

function validateManualUpgradeProperties(
  properties: Record<string, TelemetryScalar>,
  status: string,
  failed: boolean,
): void {
  if (!Object.hasOwn(properties, "output")) throw schemaError("missing_output");
  const operation = String(properties.upgrade_operation);
  if (new Set(["status", "enable", "disable"]).has(operation)) {
    const successStatus = new Map([
      ["status", "status_checked"],
      ["enable", "auto_enabled"],
      ["disable", "auto_disabled"],
    ]).get(operation);
    if (
      status !== (failed ? "failed" : successStatus) ||
      properties.upgrade_applied !== false ||
      properties.upgrade_scheduled !== false ||
      (properties.update_available !== false &&
        (!failed || operation !== "enable" ||
          !new Set(["unmanaged_install", "apply_failed"]).has(String(properties.upgrade_failure_kind)) ||
          Object.hasOwn(properties, "update_available"))) ||
      hasAny(properties, [
        "update_was_available", "upgrade_attempt_id", "managed_install",
        "self_upgrade_allowed", "auto_upgrade_allowed", "upgrade_warning_count_bucket",
        "upgrade_channel",
      ])
    ) {
      throw schemaError("invalid_upgrade_shape");
    }
    return;
  }
  if (operation === "check") return validateManualCheckProperties(properties, status, failed);
  if (operation === "apply") return validateManualApplyProperties(properties, status, failed);
  throw schemaError("invalid_upgrade_shape");
}

function validateManualCheckProperties(
  properties: Record<string, TelemetryScalar>,
  status: string,
  failed: boolean,
): void {
  if (
    properties.upgrade_applied !== false ||
    properties.upgrade_scheduled !== false
  ) {
    throw schemaError("invalid_upgrade_shape");
  }
  const detailKeys = [
    "update_available", "update_was_available", "upgrade_attempt_id", "managed_install",
    "self_upgrade_allowed", "auto_upgrade_allowed", "upgrade_warning_count_bucket",
    "upgrade_channel",
  ];
  const detailCount = countPresent(properties, detailKeys);
  if (failed) {
    if (
      properties.upgrade_applied !== false ||
      properties.upgrade_scheduled !== false ||
      (detailCount !== 0 && detailCount !== detailKeys.length) ||
      (detailCount !== 0 && properties.upgrade_failure_kind !== "apply_failed")
    ) {
      throw schemaError("invalid_upgrade_shape");
    }
  } else if (
    !new Set(["available", "up_to_date"]).has(status) ||
    detailCount !== detailKeys.length
  ) {
    throw schemaError("invalid_upgrade_shape");
  }
  if (
    detailCount !== 0 &&
    (
      properties.update_available !== properties.update_was_available ||
      (!failed && properties.update_available !== (status === "available"))
    )
  ) {
    throw schemaError("invalid_upgrade_shape");
  }
}

function validateManualApplyProperties(
  properties: Record<string, TelemetryScalar>,
  status: string,
  failed: boolean,
): void {
  const detailKeys = [
    "update_available", "update_was_available", "upgrade_attempt_id", "managed_install",
    "self_upgrade_allowed", "auto_upgrade_allowed", "upgrade_warning_count_bucket",
  ];
  const detailCount = countPresent(properties, detailKeys);
  if (failed) {
    if (
      (detailCount !== 0 && detailCount !== detailKeys.length) ||
      (detailCount !== 0 && properties.upgrade_failure_kind !== "apply_failed")
    ) {
      throw schemaError("invalid_upgrade_shape");
    }
    if (detailCount === 0 && Object.hasOwn(properties, "upgrade_channel")) {
      throw schemaError("invalid_upgrade_shape");
    }
    validateRecoveryDetailsWithoutChannel(properties, status);
    return;
  }
  if (
    !new Set(["up_to_date", "dry_run", "scheduled", "applied"]).has(status) ||
    detailCount !== detailKeys.length
  ) {
    throw schemaError("invalid_upgrade_shape");
  }

  const expected = new Map<string, readonly [boolean, boolean, boolean | null]>([
    ["up_to_date", [false, false, false]],
    ["dry_run", [false, false, null]],
    ["scheduled", [false, true, null]],
    ["applied", [true, false, false]],
  ]).get(status);
  if (
    !expected ||
    properties.upgrade_applied !== expected[0] ||
    properties.upgrade_scheduled !== expected[1] ||
    (status === "dry_run" && properties.dry_run !== true) ||
    (new Set(["scheduled", "applied"]).has(status) && properties.dry_run !== false) ||
    (expected[2] !== null && properties.update_available !== expected[2]) ||
    (new Set(["dry_run", "scheduled"]).has(status) &&
      properties.update_available !== properties.update_was_available) ||
    (status === "up_to_date" && properties.update_was_available !== false)
  ) {
    throw schemaError("invalid_upgrade_shape");
  }
  if (status === "up_to_date" && !Object.hasOwn(properties, "upgrade_channel")) {
    throw schemaError("invalid_upgrade_shape");
  }
  validateRecoveryDetailsWithoutChannel(properties, status);
}

function validateRecoveryDetailsWithoutChannel(
  properties: Record<string, TelemetryScalar>,
  status: string,
): void {
  if (!Object.hasOwn(properties, "upgrade_attempt_id") || Object.hasOwn(properties, "upgrade_channel")) {
    return;
  }
  const validWarningCount = status === "scheduled"
    ? properties.upgrade_warning_count_bucket === "0"
    : status === "applied"
    ? properties.upgrade_warning_count_bucket === "1"
    : new Set(["0", "1"]).has(String(properties.upgrade_warning_count_bucket));
  if (
    properties.update_available !== false ||
    properties.update_was_available !== false ||
    properties.managed_install !== false ||
    properties.self_upgrade_allowed !== false ||
    properties.auto_upgrade_allowed !== false ||
    !validWarningCount
  ) {
    throw schemaError("invalid_upgrade_shape");
  }
}

function requireUpgradeAttemptId(value: unknown): string {
  const id = requireString(value, "invalid_upgrade_attempt_id");
  if (!UPGRADE_ATTEMPT_ID_PATTERN.test(id)) throw schemaError("invalid_upgrade_attempt_id");
  return id;
}

function countPresent(
  properties: Record<string, TelemetryScalar>,
  keys: readonly string[],
): number {
  return keys.filter((key) => Object.hasOwn(properties, key)).length;
}

function hasAny(
  properties: Record<string, TelemetryScalar>,
  keys: readonly string[],
): boolean {
  return countPresent(properties, keys) !== 0;
}

function validateCliOperationSidecars(
  properties: Record<string, TelemetryScalar>,
  operation: string,
  outcome: string,
): void {
  for (const key of ["deprecated_daemon_control", "deprecated_upgrade_control"] as const) {
    if (Object.hasOwn(properties, key) && properties[key] !== true) {
      throw schemaError(`invalid_${key}`);
    }
  }

  const autoUpgradeCount = AUTO_UPGRADE_PROPERTY_KEYS.filter(
    (key) => Object.hasOwn(properties, key),
  ).length;
  if (autoUpgradeCount === 0) return;
  if (autoUpgradeCount !== AUTO_UPGRADE_PROPERTY_KEYS.length) {
    throw schemaError("incomplete_auto_upgrade");
  }
  if (outcome !== "success" || !AUTO_UPGRADE_ELIGIBLE_OPERATIONS.has(operation)) {
    throw schemaError("unexpected_auto_upgrade");
  }
  if (properties.auto_upgrade_probe !== true) throw schemaError("invalid_auto_upgrade_probe");
  const due = properties.auto_upgrade_due;
  const spawned = properties.auto_upgrade_spawned;
  const status = properties.auto_upgrade_spawn_status;
  const expected = new Map<string, readonly [boolean, boolean]>([
    ["auto_disabled", [false, false]],
    ["ci", [false, false]],
    ["background_child", [false, false]],
    ["not_due", [false, false]],
    ["marker_invalid", [true, false]],
    ["current_exe_error", [true, false]],
    ["spawned", [true, true]],
    ["spawn_failed", [true, false]],
  ]).get(String(status));
  if (!expected || due !== expected[0] || spawned !== expected[1]) {
    throw schemaError("invalid_auto_upgrade_state");
  }
}

export function validateOperation(surface: string, operation: string): void {
  if (!SURFACE_OPERATIONS.get(surface)?.has(operation)) throw schemaError("invalid_operation");
}

export function validateProviderRefreshOperation(surface: string, operation: string): void {
  if (!PROVIDER_REFRESH_OPERATIONS.has(operation)) throw schemaError("invalid_provider_refresh_operation");
  if (!PROVIDER_REFRESH_SURFACES.has(surface)) throw schemaError("invalid_provider_refresh_surface");
}

export function validateRuntimeOperation(surface: string, operation: string): void {
  if (!RUNTIME_OPERATIONS.get(surface)?.has(operation)) throw schemaError("invalid_runtime_operation");
}

export function validateInstallStatus(stage: string, status: string, legacy: boolean): void {
  if (!legacy) {
    if (!INSTALL_STATUS_PAIRS.get(stage)?.has(status)) throw schemaError("invalid_install_stage_status");
    return;
  }
  const valid = stage.endsWith("_started") || stage.endsWith("_launched") || stage === "script_started"
    ? status === "started"
    : stage.endsWith("_skipped")
    ? status === "skipped"
    : stage === "installer_failed"
    ? status === "failed"
    : status === "completed" || status === "failed";
  if (!valid) throw schemaError("invalid_install_stage_status");
}

export function validateInstallError(value: unknown): string | null {
  if (value === undefined || value === "") return null;
  const code = requireString(value, "invalid_install_error_code");
  if (!FIXED_INSTALL_ERRORS.has(code) && !/^exit_(?:[1-9]|[1-9]\d|1\d\d|2[0-4]\d|25[0-5])$/u.test(code)) {
    throw schemaError("invalid_install_error_code");
  }
  return code;
}

function validateKnownProperties(properties: Record<string, unknown>): Record<string, TelemetryScalar> {
  const out: Record<string, TelemetryScalar> = {};
  for (const [key, value] of Object.entries(properties)) {
    if (key === "capability_snapshot_schema") {
      requireExact(value, 1, "invalid_capability_snapshot_schema");
      out[key] = 1;
    } else if (key === "provider_filter") {
      out[key] = normalizeTelemetryProvider(value, "invalid_provider_filter");
    } else if (BOOLEAN_PROPERTIES.has(key)) {
      out[key] = requireBoolean(value, `invalid_${key}`);
    } else if (PROPERTY_ENUMS.has(key)) {
      out[key] = requireEnum(value, PROPERTY_ENUMS.get(key)!, `invalid_${key}`);
    } else if (SEARCH_COUNT_BUCKET_PROPERTY_KEYS.has(key)) {
      out[key] = requireEnum(value, SEARCH_COUNT_BUCKETS, `invalid_${key}`);
    } else if (SEARCH_BYTE_BUCKET_PROPERTY_KEYS.has(key)) {
      out[key] = requireEnum(value, SEARCH_BYTE_BUCKETS, `invalid_${key}`);
    } else if (COUNT_BUCKET_PROPERTY_KEYS.has(key)) {
      out[key] = requireEnum(value, COUNT_BUCKETS, `invalid_${key}`);
    } else if (BYTE_BUCKET_PROPERTY_KEYS.has(key)) {
      out[key] = requireEnum(value, BYTE_BUCKETS, `invalid_${key}`);
    } else if (TEXT_BUCKET_PROPERTY_KEYS.has(key)) {
      out[key] = requireEnum(value, TEXT_LENGTH_BUCKETS, `invalid_${key}`);
    } else if (DURATION_BUCKET_PROPERTY_KEYS.has(key)) {
      out[key] = requireEnum(value, DURATION_BUCKETS, `invalid_${key}`);
    } else {
      throw schemaError(`invalid_${key}`);
    }
  }
  return out;
}

export function validateSharedProperties(
  properties: Record<string, unknown>,
): Record<string, TelemetryScalar> {
  return validateKnownProperties(Object.fromEntries(
    Object.entries(properties).filter(([key]) => SHARED_PROPERTY_KEYS.has(key)),
  ));
}

export function requireRecord(value: unknown, code: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw schemaError(code);
  return value as Record<string, unknown>;
}

export function rejectUnknownKeys(
  value: Record<string, unknown>,
  allowed: ReadonlySet<string>,
  code: string,
): void {
  for (const key of Object.keys(value)) if (!allowed.has(key)) throw schemaError(code);
}

export function requireString(value: unknown, code: string): string {
  if (typeof value !== "string" || value.length === 0 || value.trim() !== value) throw schemaError(code);
  return value;
}

export function requireEnum(value: unknown, allowed: ReadonlySet<string>, code: string): string {
  const string = requireString(value, code);
  if (!allowed.has(string)) throw schemaError(code);
  return string;
}

export function normalizeTelemetryProvider(value: unknown, code: string): string {
  const provider = requireString(value, code);
  if (!isProviderId(provider)) throw schemaError(code);
  return normalizeProviderId(provider);
}

export function isKnownOperationPropertyKey(key: string): boolean {
  return KNOWN_OPERATION_PROPERTY_KEYS.has(key);
}

export function requireBoolean(value: unknown, code: string): boolean {
  if (typeof value !== "boolean") throw schemaError(code);
  return value;
}

export function requireInteger(
  value: unknown,
  minimum: number,
  maximum: number,
  code: string,
): number {
  if (!Number.isSafeInteger(value) || (value as number) < minimum || (value as number) > maximum) {
    throw schemaError(code);
  }
  return value as number;
}

export function requireExact(value: unknown, expected: string | number, code: string): void {
  if (value !== expected) throw schemaError(code);
}

export function requireUuid(value: unknown, code: string): string {
  const id = requireString(value, code);
  if (!UUID_PATTERN.test(id)) throw schemaError(code);
  return id;
}

export function requireUuidV4(value: unknown, code: string): string {
  const id = requireString(value, code);
  if (!UUID_V4_PATTERN.test(id)) throw schemaError(code);
  return id;
}

export function requireVersion(value: unknown, code: string): string {
  const version = requireString(value, code);
  if (!RELEASE_VERSION_PATTERN.test(version)) throw schemaError(code);
  return version;
}

export function optionalInstallAttemptId(value: unknown): string | null {
  if (value === undefined) return null;
  const id = requireString(value, "invalid_install_attempt_id");
  if (!INSTALL_ATTEMPT_ID_PATTERN.test(id)) throw schemaError("invalid_install_attempt_id");
  return id;
}

export function optionalLegacyInstallAttemptId(value: unknown): string | null {
  if (value === undefined) return null;
  const id = requireString(value, "invalid_install_attempt_id");
  if (!/^[A-Za-z0-9_-]{1,128}$/u.test(id)) throw schemaError("invalid_install_attempt_id");
  return id;
}

export function optionalLegacyEnum(
  value: unknown,
  allowed: ReadonlySet<string>,
  code: string,
): string | null {
  if (value === undefined || value === "") return null;
  return requireEnum(value, allowed, code);
}

export function optionalLegacyVersion(value: unknown): string | null {
  if (value === undefined || value === "") return null;
  return requireVersion(value, "invalid_install_version");
}

export function schemaError(code: string): TelemetryIngestError {
  return new TelemetryIngestError(422, code);
}

function setOf(...values: string[]): ReadonlySet<string> {
  return new Set(values);
}

function storeKeys(): string[] {
  return ["indexed_sessions_bucket", "indexed_events_bucket", "indexed_items_bucket", "db_size_bucket"];
}

function importResultKeys(): string[] {
  return [
    "sources_seen_bucket", "source_bytes_bucket", "source_files_bucket", "failed_sources_bucket",
    "sessions_imported_bucket", "events_imported_bucket", "edges_imported_bucket", "skipped_bucket",
    "rejected_records_bucket", "import_outcome", "import_failure_scope", "import_failure_type",
  ];
}

function indexStates(): ReadonlySet<string> {
  return new Set(["ready", "empty", "pending", "missing", "disabled", "failed", "blocked", "unknown"]);
}

function shareBuckets(): ReadonlySet<string> {
  return new Set(["not_applicable", "0", "1-25pct", "26-50pct", "51-75pct", "76-99pct", "100pct"]);
}

function docTopics(): ReadonlySet<string> {
  // `sql` remains accepted for historical docs telemetry emitted before the
  // topic and public feature were removed.
  return new Set([
    "getting-started", "first-10-minutes", "cli-reference", "docs", "search", "event-queries",
    "sql", "mcp", "blame",
    "mcp-integrations", "upgrade", "unmanaged-installs", "agent-usage", "agent-skill-install",
    "slash-command-integrations", "sdks", "json-contracts", "storage", "providers",
    "custom-history-import-format", "history-source-plugins", "provider-support",
    "provider-import-policy", "troubleshooting", "limitations",
  ]);
}
