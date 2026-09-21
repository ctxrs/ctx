import {
  BYTE_BUCKETS,
  COUNT_BUCKETS,
  DURATION_BUCKETS,
  MCP_OPERATIONS,
  MCP_SEARCH_SIDECAR_PROPERTY_KEYS,
  SEARCH_TERMINAL_PROPERTY_KEYS,
  SHARED_PROPERTY_KEYS,
  type TelemetryScalar,
  rejectUnknownKeys,
  requireBoolean,
  requireEnum,
  requireRecord,
  normalizeTelemetryProvider,
  schemaError,
  validateMcpSearchSidecarProperties,
  validateSearchTerminalProperties,
  validateSharedProperties,
} from "./telemetry-contract";
import {
  BLAME_PRODUCT_PROPERTY_KEYS,
  isCurrentBlameProductContract,
  parseCurrentBlameProductProperties,
} from "./blame-product-contract";
import {
  DAEMON_STORAGE_PROPERTY_KEYS,
  parseDaemonStorageProperties,
} from "./daemon-storage-contract";
import {
  REFRESH_FAILURE_DIAGNOSTIC_KEYS,
  parseRefreshFailureDiagnostic,
} from "./provider-refresh-diagnostics";
import { ANALYTICS_DELIVERY_PROPERTY_KEYS } from "./analytics-delivery-contract";
import { ORDINARY_BLAME_PROPERTY_KEYS, parseOrdinaryBlameProperties } from "./ordinary-blame-contract";

const PROVIDER_REFRESH_TERMINAL_HEALTH_DURATION_KEYS = [
  "refresh_queue_wait_duration_bucket", "refresh_discovery_duration_bucket",
  "refresh_scan_stage_duration_bucket", "refresh_commit_duration_bucket",
] as const;
const PROVIDER_REFRESH_TERMINAL_HEALTH_KEYS = [
  ...PROVIDER_REFRESH_TERMINAL_HEALTH_DURATION_KEYS,
  "refresh_coalesced_request_count_bucket", "refresh_successor_pending",
  "refresh_configured_indexing_mode", "refresh_daemon_trigger_kind",
  "refresh_reconciliation_demand", "refresh_retained_previous_generation",
  "refresh_processed_sessions_bucket", "refresh_processed_messages_bucket",
  "refresh_processed_tool_calls_bucket", "refresh_processed_bytes_bucket",
] as const;
const PROVIDER_REFRESH_CORPUS_STOCK_COUNT_KEYS = [
  "corpus_stock_indexed_documents_bucket", "corpus_stock_retained_records_bucket",
  "corpus_stock_rejected_records_bucket", "corpus_transition_removed_sources_bucket",
] as const;
const PROVIDER_REFRESH_CORPUS_STOCK_KEYS = [
  ...PROVIDER_REFRESH_CORPUS_STOCK_COUNT_KEYS,
  "corpus_stock_certified_source_bytes_bucket",
] as const;
const PROVIDER_REFRESH_PROPERTY_KEYS = new Set([
  ...SHARED_PROPERTY_KEYS, "provider", "trigger", "source_mode", "change", "work_remaining",
  "content_evidence", "work_kind", "refresh_result", "core_result", "canonical_pro_result",
  "output_pro_result", "failure_scope", "failure_type", "failure_code", "retryable",
  ...REFRESH_FAILURE_DIAGNOSTIC_KEYS,
  "retired_records_bucket",
  "sources_bucket", "source_files_bucket", "sessions_bucket", "events_bucket", "edges_bucket",
  "skips_bucket", "rejections_bucket", "failures_bucket", "bytes_bucket",
  "records_bucket", "logical_bytes_bucket",
  "cpu_duration_bucket", "observed_process_peak_rss_bucket",
  ...PROVIDER_REFRESH_TERMINAL_HEALTH_KEYS,
  ...PROVIDER_REFRESH_CORPUS_STOCK_KEYS,
]);
const PROVIDER_REFRESH_FINAL_REQUIRED_KEYS = [
  "refresh_result", "core_result", "failure_scope", "failure_type",
] as const;
const PROVIDER_REFRESH_FINAL_KEYS = [
  ...PROVIDER_REFRESH_FINAL_REQUIRED_KEYS, "content_evidence", "canonical_pro_result",
  "output_pro_result", "work_kind",
  "retired_records_bucket",
  "cpu_duration_bucket", "observed_process_peak_rss_bucket",
  ...REFRESH_FAILURE_DIAGNOSTIC_KEYS,
] as const;
const PROVIDER_REFRESH_COUNT_KEYS = [
  "sources_bucket", "source_files_bucket", "sessions_bucket", "events_bucket", "edges_bucket",
  "skips_bucket", "rejections_bucket", "failures_bucket", "records_bucket",
] as const;
const DAEMON_RUN_PROPERTY_KEYS = new Set(["start_mode", "supervisor", "trigger_command"]);
const DAEMON_STATE_PROPERTY_KEYS = new Set([
  "history_freshness", "semantic_backlog_bucket", "semantic_coverage", "retry_backoff",
]);
const DAEMON_CYCLE_PROPERTY_KEYS = new Set(["cycle_result", "coalesced_cycles_bucket"]);
const MCP_OPERATION_PROPERTY_KEYS = new Set([
  "method", "tool", "error_layer", "error_class", "result_count_bucket", "column_count_bucket",
  "zero_result", "result_truncated", "rows_truncated", "values_truncated", "events_truncated",
  "response_bound",
]);
const MCP_QUERY_EVENTS_PROPERTY_KEYS = new Set([
  ...SHARED_PROPERTY_KEYS, "method", "tool", "error_layer", "error_class",
  "result_count_bucket", "zero_result", "result_truncated", "response_bound",
]);
const MCP_RUNTIME_PROPERTY_KEYS = new Set([
  "initialized", "stop_reason", "request_count_bucket", "tool_request_count_bucket",
  "tool_failure_count_bucket", "malformed_request_count_bucket", "ping_count_bucket",
  "tools_list_count_bucket", "initialized_notification_count_bucket",
  "unknown_notification_count_bucket", "telemetry_dropped_count_bucket",
]);
const PRO_MATERIALIZATION_PROPERTY_KEYS = new Set([
  "materialization_commit", "materialization_freshness", "materialization_result",
  "helper_connection_outcome", "materialization_failure_bucket",
]);
const RETIRED_PRO_MATERIALIZATION_PROPERTY_KEYS = new Set([
  "materialization_mode", "materialization_batch_count_bucket",
  "materialization_input_count_bucket", "materialization_output_count_bucket",
  "materialization_lag_bucket",
]);
const V026_PRO_MATERIALIZATION_PROPERTY_KEYS = new Set([
  ...PRO_MATERIALIZATION_PROPERTY_KEYS,
  ...RETIRED_PRO_MATERIALIZATION_PROPERTY_KEYS,
]);
const PRO_LIFECYCLE_PROPERTY_KEYS = new Set([
  "lifecycle_operation", "access_state", "helper_connection_outcome", "reconcile_outcome",
  "uninstall_data_disposition", "lifecycle_failure_bucket", ...PRO_MATERIALIZATION_PROPERTY_KEYS,
]);
const V026_PRO_QUERY_PROPERTY_KEYS = new Set([
  "query_kind", "query_surface", "access_state", "helper_connection_outcome",
  "query_result_count_bucket", "query_empty", "query_truncated", "query_freshness",
  "query_auto_materialization", "query_failure_bucket", ...V026_PRO_MATERIALIZATION_PROPERTY_KEYS,
]);
const PRO_STATUS_PROPERTY_KEYS = new Set([
  "status_surface", "access_state", "helper_connection_outcome", "status_failure_bucket",
]);
const PRO_BLAME_PROPERTY_KEYS = new Set([
  "blame_target_kind", "blame_surface", "blame_result_count_bucket", "blame_has_more",
  "blame_failure_bucket",
]);
const KNOWN_SURFACE_PROPERTY_KEYS = new Set([
  ...PROVIDER_REFRESH_PROPERTY_KEYS,
  ...ANALYTICS_DELIVERY_PROPERTY_KEYS,
  ...DAEMON_RUN_PROPERTY_KEYS,
  ...DAEMON_STATE_PROPERTY_KEYS,
  ...DAEMON_CYCLE_PROPERTY_KEYS,
  ...DAEMON_STORAGE_PROPERTY_KEYS,
  ...MCP_OPERATION_PROPERTY_KEYS,
  ...MCP_RUNTIME_PROPERTY_KEYS,
  ...PRO_MATERIALIZATION_PROPERTY_KEYS,
  ...PRO_LIFECYCLE_PROPERTY_KEYS,
  ...V026_PRO_QUERY_PROPERTY_KEYS,
  ...PRO_STATUS_PROPERTY_KEYS,
  ...PRO_BLAME_PROPERTY_KEYS,
  ...BLAME_PRODUCT_PROPERTY_KEYS,
]);

const DAEMON_START_MODES = new Set(["manual", "auto"]);
const DAEMON_SUPERVISORS = new Set(["user", "cli_autostart"]);
const DAEMON_TRIGGERS = new Set(["setup", "import", "search", "semantic"]);
const DAEMON_HISTORY_FRESHNESS = new Set(["current", "pending", "backoff", "failed", "unknown"]);
const DAEMON_SEMANTIC_BACKLOG = new Set([...COUNT_BUCKETS, "unknown"]);
const DAEMON_SEMANTIC_COVERAGE = new Set(["empty", "complete", "incomplete", "dirty", "unknown"]);
const DAEMON_RETRY_BACKOFF = new Set(["none", "history", "semantic", "both"]);
const DAEMON_CYCLE_RESULTS = new Set(["work", "no_work", "failure"]);

const MCP_METHODS = new Set(["tools_call", "unknown", "missing"]);
const MCP_ERROR_LAYERS = new Set(["input", "json_rpc", "tool", "response"]);
const MCP_ERROR_CLASSES = new Set([
  "invalid_utf8", "line_too_large", "invalid_json", "invalid_request", "invalid_params",
  "server_not_initialized", "method_not_found", "missing_tool", "unknown_tool", "tool_failure",
  "response_serialize", "response_write", "response_flush",
]);
const MCP_RESPONSE_BOUNDS = new Set(["within_limit", "replaced"]);
const MCP_STOP_REASONS = new Set([
  "eof", "stdin_read_error", "response_serialize_error", "stdout_write_error", "stdout_flush_error",
]);
const MCP_RUNTIME_COUNT_KEYS = [
  "request_count_bucket", "tool_request_count_bucket", "tool_failure_count_bucket",
  "malformed_request_count_bucket", "ping_count_bucket", "tools_list_count_bucket",
  "initialized_notification_count_bucket", "unknown_notification_count_bucket",
  "telemetry_dropped_count_bucket",
] as const;

const PRO_LIFECYCLE_OPERATIONS = new Set(["setup", "manage", "status", "uninstall"]);
const PRO_ACCESS_STATES = new Set(["trial", "active", "canceling_paid", "offline_grace", "locked"]);
const PRO_HELPER_CONNECTION_OUTCOMES = new Set([
  "not_attempted", "connected", "not_installed", "authorization_failed", "protocol_mismatch",
  "timed_out", "crashed", "unavailable",
]);
const PRO_RECONCILE_OUTCOMES = new Set([
  "not_attempted", "missing", "current", "installed", "updated", "failed",
]);
const PRO_UNINSTALL_DISPOSITIONS = new Set(["delete", "preserve"]);
const V026_PRO_MATERIALIZATION_MODES = new Set([
  "no_op", "full", "incremental", "resume", "rebuild",
]);
const PRO_COMMIT_OUTCOMES = new Set(["not_committed", "no_op", "committed", "replayed", "mixed"]);
const PRO_FRESHNESS = new Set(["current", "unknown"]);
const PRO_MATERIALIZATION_RESULTS = new Set(["completed", "failed"]);
const CURRENT_COUNT_BUCKETS = new Set(
  [...COUNT_BUCKETS].filter((bucket) => bucket !== "1k+"),
);
const PRO_FAILURE_BUCKETS = new Set([
  "commercial", "installation", "authorization", "key_store", "protocol", "source", "repository",
  "stale", "ambiguous", "invalid_request", "invalid_response", "cancelled", "helper_crashed",
  "helper_timeout", "output", "other",
]);
const V026_PRO_QUERY_KINDS = new Set([
  "show", "locate", "blame", "timeline", "related", "facts", "status", "other",
]);
const V026_PRO_QUERY_SURFACES = new Set(["cli", "mcp"]);
const V026_PRO_QUERY_AUTO_MATERIALIZATION = new Set(["not_needed", "completed", "failed"]);
const V026_PRO_QUERY_COUNT_BUCKETS = new Set([
  "0", "1", "2-5", "6-20", "21-100", "101-1k", "1k+",
]);
const V026_PRO_QUERY_FRESHNESS = new Set(["current", "stale", "unknown"]);
const PRO_BLAME_TARGET_KINDS = new Set(["file", "commit", "pull_request"]);
const PRO_SURFACES = new Set(["cli", "mcp"]);
const PROVIDER_CONTENT_EVIDENCE = new Set(["none", "accepted", "mixed", "unknown"]);
const PROVIDER_WORK_KINDS = new Set([
  "no_op", "fresh", "append", "rewrite", "truncate", "replace", "retire", "mixed",
]);
const PROVIDER_REFRESH_RESULTS = new Set(["complete", "partial", "failure"]);
const PROVIDER_CORE_RESULTS = new Set(["no_op", "complete", "partial", "failure", "unknown"]);
const PROVIDER_PRO_RESULTS = new Set([
  "not_requested", "unavailable", "no_op", "complete", "partial", "behind", "failure", "unknown",
]);
const PROVIDER_FAILURE_SCOPES = new Set([
  "none", "record", "source", "system", "mixed", "unknown",
]);
const PROVIDER_FAILURE_TYPES = new Set([
  "none", "record_rejection", "unsupported_schema", "not_found", "permission",
  "source_database", "malformed_source", "store", "worker_panic", "system_io", "system", "other",
  "mixed", "unknown",
]);
const PROVIDER_FAILURE_CODES = new Set([
  "none", "source_unavailable", "explicit_source_path_missing", "source_changed",
  "malformed_source", "unsupported_schema", "source_failures", "logical_source_failures",
  "source_unclaimed", "source_refresh_failed", "source_refresh_internal",
  "resource_unavailable", "index_incompatible", "index_corruption",
  "source_refresh_admission_failed", "all_provider_terminal_coverage_unavailable", "unknown",
]);
export function parseSurfaceOperationProperties(
  value: unknown,
  operation: string,
  surface: string,
  outcome: string,
  appVersion: string,
): Record<string, TelemetryScalar> {
  const properties = requireRecord(value, "invalid_properties");
  if (surface === "daemon") return parseDaemonOperationProperties(properties, operation);
  if (surface === "mcp") return parseMcpOperationProperties(properties, operation, outcome);
  if (surface === "pro_host") {
    return parseProHostOperationProperties(
      operation === "materialize" || operation === "lifecycle"
        ? validateAndOmitRetiredProMaterializationProperties(properties, appVersion)
        : properties,
      operation,
      outcome,
      appVersion,
    );
  }
  throw schemaError("invalid_surface");
}

export function parseProviderRefreshProperties(
  value: unknown,
  outcome: string,
  surface: string,
): Record<string, TelemetryScalar> {
  const properties = requireRecord(value, "invalid_properties");
  rejectUnknownKeys(properties, PROVIDER_REFRESH_PROPERTY_KEYS, "unknown_provider_refresh_property");
  const out: Record<string, TelemetryScalar> = {
    ...validateSharedProperties(properties),
    trigger: requireEnum(
      properties.trigger,
      new Set(["setup", "import", "search", "daemon"]),
      "invalid_refresh_trigger",
    ),
    change: requireEnum(properties.change, new Set(["changed", "no_op"]), "invalid_refresh_change"),
    work_remaining: requireBoolean(properties.work_remaining, "invalid_work_remaining"),
  };
  if (Object.hasOwn(properties, "provider")) {
    out.provider = normalizeTelemetryProvider(properties.provider, "invalid_provider");
  }
  addOptionalEnum(
    out,
    properties,
    "source_mode",
    new Set(["discovered", "explicit_path", "explicit_format", "history_source_plugin"]),
  );
  for (const key of PROVIDER_REFRESH_COUNT_KEYS) {
    addOptionalEnum(out, properties, key, COUNT_BUCKETS);
  }
  addOptionalEnum(out, properties, "bytes_bucket", BYTE_BUCKETS);
  addOptionalEnum(out, properties, "logical_bytes_bucket", BYTE_BUCKETS);
  parseProviderRefreshTerminalHealthProperties(out, properties, surface, outcome);
  const hasCorpusStock = PROVIDER_REFRESH_CORPUS_STOCK_KEYS.some(
    (key) => Object.hasOwn(properties, key),
  );
  if (hasCorpusStock) {
    for (const key of PROVIDER_REFRESH_CORPUS_STOCK_KEYS) {
      if (!Object.hasOwn(properties, key)) throw schemaError("incomplete_provider_refresh_corpus_stock");
    }
    for (const key of PROVIDER_REFRESH_CORPUS_STOCK_COUNT_KEYS) {
      addOptionalEnum(out, properties, key, COUNT_BUCKETS);
    }
    addOptionalEnum(
      out,
      properties,
      "corpus_stock_certified_source_bytes_bucket",
      BYTE_BUCKETS,
    );
  }
  if (!PROVIDER_REFRESH_FINAL_KEYS.some((key) => Object.hasOwn(properties, key))) return out;
  for (const key of PROVIDER_REFRESH_FINAL_REQUIRED_KEYS) {
    if (!Object.hasOwn(properties, key)) throw schemaError("incomplete_provider_refresh_outcome");
  }
  addOptionalEnum(out, properties, "content_evidence", PROVIDER_CONTENT_EVIDENCE);
  out.refresh_result = requireEnum(
    properties.refresh_result,
    PROVIDER_REFRESH_RESULTS,
    "invalid_refresh_result",
  );
  out.core_result = requireEnum(
    properties.core_result,
    PROVIDER_CORE_RESULTS,
    "invalid_core_result",
  );
  const hasCanonicalProResult = Object.hasOwn(properties, "canonical_pro_result");
  const hasOutputProResult = Object.hasOwn(properties, "output_pro_result");
  if (hasCanonicalProResult !== hasOutputProResult) {
    throw schemaError("incomplete_provider_refresh_outcome");
  }
  addOptionalEnum(out, properties, "canonical_pro_result", PROVIDER_PRO_RESULTS);
  addOptionalEnum(out, properties, "output_pro_result", PROVIDER_PRO_RESULTS);
  out.failure_scope = requireEnum(
    properties.failure_scope,
    PROVIDER_FAILURE_SCOPES,
    "invalid_failure_scope",
  );
  out.failure_type = requireEnum(
    properties.failure_type,
    PROVIDER_FAILURE_TYPES,
    "invalid_failure_type",
  );
  const hasFailureCode = Object.hasOwn(properties, "failure_code");
  const hasRetryable = Object.hasOwn(properties, "retryable");
  if (hasFailureCode !== hasRetryable) {
    throw schemaError("incomplete_provider_failure_diagnostics");
  }
  if (hasFailureCode) {
    out.failure_code = requireEnum(
      properties.failure_code,
      PROVIDER_FAILURE_CODES,
      "invalid_failure_code",
    );
    out.retryable = requireBoolean(properties.retryable, "invalid_retryable");
    if (outcome === "failure" && out.failure_code === "none") {
      throw schemaError("inconsistent_provider_failure_code");
    }
    if (outcome === "success" && out.failure_code !== "none") {
      throw schemaError("inconsistent_provider_failure_code");
    }
  }
  Object.assign(out, parseRefreshFailureDiagnostic(properties, surface, outcome));
  addOptionalEnum(out, properties, "work_kind", PROVIDER_WORK_KINDS);
  addOptionalEnum(out, properties, "retired_records_bucket", COUNT_BUCKETS);
  const hasCpuDuration = Object.hasOwn(properties, "cpu_duration_bucket");
  const hasObservedProcessPeakRss = Object.hasOwn(
    properties,
    "observed_process_peak_rss_bucket",
  );
  if (hasObservedProcessPeakRss && !hasCpuDuration) {
    throw schemaError("orphaned_observed_process_peak_rss");
  }
  if (hasCpuDuration) {
    out.cpu_duration_bucket = requireEnum(
      properties.cpu_duration_bucket,
      DURATION_BUCKETS,
      "invalid_cpu_duration_bucket",
    );
  }
  if (hasObservedProcessPeakRss) {
    out.observed_process_peak_rss_bucket = requireEnum(
      properties.observed_process_peak_rss_bucket,
      BYTE_BUCKETS,
      "invalid_observed_process_peak_rss_bucket",
    );
  }
  if ((out.refresh_result === "failure") !== (outcome === "failure")) {
    throw schemaError("inconsistent_refresh_outcome");
  }
  if ((out.failure_scope === "none") !== (out.failure_type === "none")) {
    throw schemaError("inconsistent_provider_failure");
  }
  return out;
}

function parseProviderRefreshTerminalHealthProperties(
  out: Record<string, TelemetryScalar>,
  properties: Record<string, unknown>,
  surface: string,
  outcome: string,
): void {
  if (!PROVIDER_REFRESH_TERMINAL_HEALTH_KEYS.some((key) => Object.hasOwn(properties, key))) return;
  if (surface !== "daemon") throw schemaError("invalid_refresh_terminal_health_surface");
  if (!Object.hasOwn(properties, "refresh_successor_pending")) {
    throw schemaError("incomplete_refresh_terminal_health");
  }
  for (const key of PROVIDER_REFRESH_TERMINAL_HEALTH_DURATION_KEYS) {
    addOptionalEnum(out, properties, key, DURATION_BUCKETS);
  }
  addOptionalEnum(
    out,
    properties,
    "refresh_coalesced_request_count_bucket",
    COUNT_BUCKETS,
  );
  addOptionalEnum(
    out,
    properties,
    "refresh_configured_indexing_mode",
    new Set(["automatic", "manual"]),
  );
  addOptionalEnum(
    out,
    properties,
    "refresh_daemon_trigger_kind",
    new Set(["daemon_watch", "startup_catch_up", "periodic_reconciliation"]),
  );
  addOptionalEnum(
    out,
    properties,
    "refresh_reconciliation_demand",
    new Set(["incremental", "exhaustive"]),
  );
  addOptionalBoolean(out, properties, "refresh_retained_previous_generation");
  for (const key of [
    "refresh_processed_sessions_bucket",
    "refresh_processed_messages_bucket",
    "refresh_processed_tool_calls_bucket",
  ] as const) {
    addOptionalEnum(out, properties, key, CURRENT_COUNT_BUCKETS);
  }
  addOptionalEnum(
    out,
    properties,
    "refresh_processed_bytes_bucket",
    new Set([
      "0", "lt_100kb", "100kb-1mb", "1mb-10mb", "10mb-100mb", "100mb-1gb",
      "1gb-2gb", "2gb-5gb", "5gb-10gb", "10gb-25gb", "25gb-50gb", "50gb-100gb",
      "100gb+",
    ]),
  );
  out.refresh_successor_pending = requireBoolean(
    properties.refresh_successor_pending,
    "invalid_refresh_successor_pending",
  );
  if (out.refresh_successor_pending === true && out.work_remaining !== true) {
    throw schemaError("inconsistent_refresh_terminal_health");
  }
  if (
    Object.hasOwn(out, "refresh_daemon_trigger_kind") &&
    out.trigger !== "daemon"
  ) {
    throw schemaError("inconsistent_refresh_daemon_trigger_kind");
  }
  if (
    Object.hasOwn(out, "refresh_retained_previous_generation") &&
    outcome !== "failure"
  ) {
    throw schemaError("inconsistent_refresh_retained_generation");
  }
}

export function isKnownSurfacePropertyKey(key: string): boolean {
  return KNOWN_SURFACE_PROPERTY_KEYS.has(key);
}

export function parseRuntimeProperties(
  value: unknown,
  surface: string,
  operation: string,
): Record<string, TelemetryScalar> {
  const properties = requireRecord(value, "invalid_properties");
  if (surface === "daemon") return parseDaemonRuntimeProperties(properties, operation);
  if (surface === "mcp") return parseMcpRuntimeProperties(properties, operation);
  throw schemaError("invalid_runtime_surface");
}

export function validateSharedPropertyConsistency(
  properties: Record<string, TelemetryScalar>,
  hasInstallAttemptId: boolean,
): void {
  const capabilityKeys = [
    "capability_snapshot_schema", "available_parallelism_bucket", "host_memory_bucket",
    "cpu_vector_tier", "acceleration_candidate",
  ];
  const capabilityCount = capabilityKeys.filter((key) => Object.hasOwn(properties, key)).length;
  if (capabilityCount !== 0 && capabilityCount !== capabilityKeys.length) {
    throw schemaError("incomplete_capability_snapshot");
  }
  if (Object.hasOwn(properties, "install_manager") !== hasInstallAttemptId) {
    throw schemaError("inconsistent_install_manager");
  }
}

function parseDaemonOperationProperties(
  properties: Record<string, unknown>,
  operation: string,
): Record<string, TelemetryScalar> {
  const allowed = operation === "run_once"
    ? new Set([...SHARED_PROPERTY_KEYS, ...DAEMON_RUN_PROPERTY_KEYS])
    : SHARED_PROPERTY_KEYS;
  rejectUnknownKeys(properties, allowed, "unknown_daemon_operation_property");
  const out = validateSharedProperties(properties);
  if (operation === "run_once") parseDaemonRunProperties(out, properties);
  return out;
}

function parseDaemonRuntimeProperties(
  properties: Record<string, unknown>,
  operation: string,
): Record<string, TelemetryScalar> {
  const needsState = operation !== "ready";
  const allowed = new Set([
    ...SHARED_PROPERTY_KEYS,
    ...DAEMON_RUN_PROPERTY_KEYS,
    ...(needsState ? DAEMON_STATE_PROPERTY_KEYS : []),
    ...(operation === "cycle" ? DAEMON_CYCLE_PROPERTY_KEYS : []),
    ...(operation === "ready" || operation === "liveness"
      ? DAEMON_STORAGE_PROPERTY_KEYS
      : []),
  ]);
  rejectUnknownKeys(properties, allowed, "unknown_daemon_runtime_property");
  const out = validateSharedProperties(properties);
  parseDaemonRunProperties(out, properties);
  if (needsState) parseDaemonStateProperties(out, properties);
  if (operation === "cycle") {
    out.cycle_result = requireEnum(
      properties.cycle_result,
      DAEMON_CYCLE_RESULTS,
      "invalid_cycle_result",
    );
    out.coalesced_cycles_bucket = requireEnum(
      properties.coalesced_cycles_bucket,
      COUNT_BUCKETS,
      "invalid_coalesced_cycles_bucket",
    );
  }
  if (operation === "ready" || operation === "liveness") {
    parseDaemonStorageProperties(out, properties);
  }
  return out;
}

function parseDaemonRunProperties(
  out: Record<string, TelemetryScalar>,
  properties: Record<string, unknown>,
): void {
  out.start_mode = requireEnum(properties.start_mode, DAEMON_START_MODES, "invalid_start_mode");
  out.supervisor = requireEnum(properties.supervisor, DAEMON_SUPERVISORS, "invalid_supervisor");
  addOptionalEnum(out, properties, "trigger_command", DAEMON_TRIGGERS);
}

function parseDaemonStateProperties(
  out: Record<string, TelemetryScalar>,
  properties: Record<string, unknown>,
): void {
  out.history_freshness = requireEnum(
    properties.history_freshness,
    DAEMON_HISTORY_FRESHNESS,
    "invalid_history_freshness",
  );
  out.semantic_backlog_bucket = requireEnum(
    properties.semantic_backlog_bucket,
    DAEMON_SEMANTIC_BACKLOG,
    "invalid_semantic_backlog_bucket",
  );
  out.semantic_coverage = requireEnum(
    properties.semantic_coverage,
    DAEMON_SEMANTIC_COVERAGE,
    "invalid_semantic_coverage",
  );
  out.retry_backoff = requireEnum(
    properties.retry_backoff,
    DAEMON_RETRY_BACKOFF,
    "invalid_retry_backoff",
  );
}

function parseMcpOperationProperties(
  properties: Record<string, unknown>,
  operation: string,
  outcome: string,
): Record<string, TelemetryScalar> {
  if (operation === "query_events") {
    rejectUnknownKeys(
      properties,
      MCP_QUERY_EVENTS_PROPERTY_KEYS,
      "invalid_query_events_shape",
    );
  }
  rejectUnknownKeys(
    properties,
    new Set([
      ...SHARED_PROPERTY_KEYS,
      ...MCP_OPERATION_PROPERTY_KEYS,
      ...(operation === "blame" ? ORDINARY_BLAME_PROPERTY_KEYS : []),
      ...(operation === "search" ? [
        ...MCP_SEARCH_SIDECAR_PROPERTY_KEYS,
        ...SEARCH_TERMINAL_PROPERTY_KEYS,
      ] : []),
    ]),
    "unknown_mcp_operation_property",
  );
  const out = validateSharedProperties(properties);
  if (operation === "search") {
    Object.assign(out, validateMcpSearchSidecarProperties(properties));
    Object.assign(out, validateSearchTerminalProperties(properties));
  }
  if (operation === "blame") Object.assign(out, parseOrdinaryBlameProperties(properties, outcome, false));
  const method = requireEnum(properties.method, MCP_METHODS, "invalid_mcp_method");
  const tool = requireEnum(properties.tool, MCP_OPERATIONS, "invalid_mcp_tool");
  if (tool !== operation) throw schemaError("mcp_tool_operation_mismatch");
  if (
    operation === "missing"
    && method !== "missing"
    && method !== "unknown"
    && method !== "tools_call"
  ) {
    throw schemaError("invalid_mcp_method");
  }
  if (operation === "unknown" && method !== "unknown" && method !== "tools_call") {
    throw schemaError("invalid_mcp_method");
  }
  if (operation !== "missing" && operation !== "unknown" && method !== "tools_call") {
    throw schemaError("invalid_mcp_method");
  }
  out.method = method;
  out.tool = tool;
  const hasErrorLayer = Object.hasOwn(properties, "error_layer");
  const hasErrorClass = Object.hasOwn(properties, "error_class");
  if (hasErrorLayer !== hasErrorClass) throw schemaError("incomplete_mcp_error");
  addOptionalEnum(out, properties, "error_layer", MCP_ERROR_LAYERS);
  addOptionalEnum(out, properties, "error_class", MCP_ERROR_CLASSES);
  addOptionalEnum(out, properties, "result_count_bucket", COUNT_BUCKETS);
  addOptionalEnum(out, properties, "column_count_bucket", COUNT_BUCKETS);
  addOptionalBoolean(out, properties, "zero_result");
  addOptionalBoolean(out, properties, "result_truncated");
  addOptionalBoolean(out, properties, "rows_truncated");
  addOptionalBoolean(out, properties, "values_truncated");
  addOptionalBoolean(out, properties, "events_truncated");
  addOptionalEnum(out, properties, "response_bound", MCP_RESPONSE_BOUNDS);
  if (operation === "query_events") validateMcpQueryEventsProperties(out, outcome);
  return out;
}

function validateMcpQueryEventsProperties(
  properties: Record<string, TelemetryScalar>,
  outcome: string,
): void {
  const succeeded = outcome === "success";
  const resultKeys = ["result_count_bucket", "zero_result", "result_truncated"] as const;
  const hasError = Object.hasOwn(properties, "error_layer");
  if (hasError === succeeded) throw schemaError("invalid_query_events_shape");
  if (succeeded) {
    if (
      resultKeys.some((key) => !Object.hasOwn(properties, key)) ||
      properties.response_bound !== "within_limit" ||
      properties.zero_result !== (properties.result_count_bucket === "0")
    ) {
      throw schemaError("invalid_query_events_shape");
    }
    return;
  }
  if (resultKeys.some((key) => Object.hasOwn(properties, key))) {
    throw schemaError("invalid_query_events_shape");
  }
}

function parseMcpRuntimeProperties(
  properties: Record<string, unknown>,
  operation: string,
): Record<string, TelemetryScalar> {
  rejectUnknownKeys(
    properties,
    new Set([...SHARED_PROPERTY_KEYS, ...MCP_RUNTIME_PROPERTY_KEYS]),
    "unknown_mcp_runtime_property",
  );
  const out = validateSharedProperties(properties);
  const initialized = requireBoolean(properties.initialized, "invalid_initialized");
  if (operation === "initialized" && !initialized) throw schemaError("invalid_initialized");
  out.initialized = initialized;
  if (operation === "stopped") {
    out.stop_reason = requireEnum(properties.stop_reason, MCP_STOP_REASONS, "invalid_stop_reason");
  } else if (Object.hasOwn(properties, "stop_reason")) {
    throw schemaError("unexpected_stop_reason");
  }
  for (const key of MCP_RUNTIME_COUNT_KEYS) {
    out[key] = requireEnum(properties[key], COUNT_BUCKETS, `invalid_${key}`);
  }
  return out;
}

function parseProHostOperationProperties(
  properties: Record<string, unknown>,
  operation: string,
  outcome: string,
  appVersion: string,
): Record<string, TelemetryScalar> {
  if (operation === "materialize") {
    rejectUnknownKeys(
      properties,
      new Set([
        ...SHARED_PROPERTY_KEYS,
        ...(appVersion === "0.26.0"
          ? V026_PRO_MATERIALIZATION_PROPERTY_KEYS
          : PRO_MATERIALIZATION_PROPERTY_KEYS),
      ]),
      "unknown_pro_materialization_property",
    );
    const out = validateSharedProperties(properties);
    if (appVersion === "0.26.0") {
      parseV026ProMaterializationProperties(
        out,
        properties,
        COUNT_BUCKETS,
        PRO_FRESHNESS,
      );
    } else {
      parseProMaterializationProperties(out, properties);
    }
    return out;
  }
  if (operation === "lifecycle") return parseProLifecycleProperties(properties);
  if (operation === "query") return parseV026ProQueryProperties(properties);
  if (operation === "status") return parseProStatusProperties(properties);
  if (operation === "blame") {
    return isCurrentBlameProductContract(properties)
      ? parseCurrentBlameProductProperties(properties, outcome)
      : parseProBlameProperties(properties);
  }
  throw schemaError("invalid_operation");
}

function parseProLifecycleProperties(
  properties: Record<string, unknown>,
): Record<string, TelemetryScalar> {
  rejectUnknownKeys(
    properties,
    new Set([...SHARED_PROPERTY_KEYS, ...PRO_LIFECYCLE_PROPERTY_KEYS]),
    "unknown_pro_lifecycle_property",
  );
  const out = validateSharedProperties(properties);
  out.lifecycle_operation = requireEnum(
    properties.lifecycle_operation,
    PRO_LIFECYCLE_OPERATIONS,
    "invalid_lifecycle_operation",
  );
  addOptionalEnum(out, properties, "access_state", PRO_ACCESS_STATES);
  out.helper_connection_outcome = requireEnum(
    properties.helper_connection_outcome,
    PRO_HELPER_CONNECTION_OUTCOMES,
    "invalid_helper_connection_outcome",
  );
  out.reconcile_outcome = requireEnum(
    properties.reconcile_outcome,
    PRO_RECONCILE_OUTCOMES,
    "invalid_reconcile_outcome",
  );
  addOptionalEnum(out, properties, "uninstall_data_disposition", PRO_UNINSTALL_DISPOSITIONS);
  addOptionalEnum(out, properties, "lifecycle_failure_bucket", PRO_FAILURE_BUCKETS);
  if (hasProMaterializationDetails(properties)) parseProMaterializationProperties(out, properties);
  return out;
}

function parseV026ProQueryProperties(
  properties: Record<string, unknown>,
): Record<string, TelemetryScalar> {
  rejectUnknownKeys(
    properties,
    new Set([...SHARED_PROPERTY_KEYS, ...V026_PRO_QUERY_PROPERTY_KEYS]),
    "unknown_pro_query_property",
  );
  const out = validateSharedProperties(properties);
  out.query_kind = requireEnum(
    properties.query_kind,
    V026_PRO_QUERY_KINDS,
    "invalid_query_kind",
  );
  out.query_surface = requireEnum(
    properties.query_surface,
    V026_PRO_QUERY_SURFACES,
    "invalid_query_surface",
  );
  addOptionalEnum(out, properties, "access_state", PRO_ACCESS_STATES);
  out.helper_connection_outcome = requireEnum(
    properties.helper_connection_outcome,
    PRO_HELPER_CONNECTION_OUTCOMES,
    "invalid_helper_connection_outcome",
  );
  addOptionalEnum(
    out,
    properties,
    "query_result_count_bucket",
    V026_PRO_QUERY_COUNT_BUCKETS,
  );
  addOptionalBoolean(out, properties, "query_empty");
  addOptionalBoolean(out, properties, "query_truncated");
  addOptionalEnum(out, properties, "query_freshness", V026_PRO_QUERY_FRESHNESS);
  out.query_auto_materialization = requireEnum(
    properties.query_auto_materialization,
    V026_PRO_QUERY_AUTO_MATERIALIZATION,
    "invalid_query_auto_materialization",
  );
  addOptionalEnum(out, properties, "query_failure_bucket", PRO_FAILURE_BUCKETS);
  if (hasProMaterializationDetails(properties)) {
    parseV026ProMaterializationProperties(
      out,
      properties,
    );
  }
  return out;
}

function parseProStatusProperties(
  properties: Record<string, unknown>,
): Record<string, TelemetryScalar> {
  rejectUnknownKeys(
    properties,
    new Set([...SHARED_PROPERTY_KEYS, ...PRO_STATUS_PROPERTY_KEYS]),
    "unknown_pro_status_property",
  );
  const out = validateSharedProperties(properties);
  out.status_surface = requireEnum(properties.status_surface, PRO_SURFACES, "invalid_status_surface");
  addOptionalEnum(out, properties, "access_state", PRO_ACCESS_STATES);
  out.helper_connection_outcome = requireEnum(
    properties.helper_connection_outcome,
    PRO_HELPER_CONNECTION_OUTCOMES,
    "invalid_helper_connection_outcome",
  );
  addOptionalEnum(out, properties, "status_failure_bucket", PRO_FAILURE_BUCKETS);
  return out;
}

function parseProBlameProperties(
  properties: Record<string, unknown>,
): Record<string, TelemetryScalar> {
  rejectUnknownKeys(
    properties,
    new Set([...SHARED_PROPERTY_KEYS, ...PRO_BLAME_PROPERTY_KEYS]),
    "unknown_pro_blame_property",
  );
  const out = validateSharedProperties(properties);
  addOptionalEnum(out, properties, "blame_target_kind", PRO_BLAME_TARGET_KINDS);
  out.blame_surface = requireEnum(properties.blame_surface, PRO_SURFACES, "invalid_blame_surface");
  addOptionalEnum(out, properties, "blame_result_count_bucket", COUNT_BUCKETS);
  addOptionalBoolean(out, properties, "blame_has_more");
  addOptionalEnum(out, properties, "blame_failure_bucket", PRO_FAILURE_BUCKETS);
  return out;
}

function parseProMaterializationProperties(
  out: Record<string, TelemetryScalar>,
  properties: Record<string, unknown>,
  freshness: ReadonlySet<string> = PRO_FRESHNESS,
): void {
  out.materialization_commit = requireEnum(
    properties.materialization_commit,
    PRO_COMMIT_OUTCOMES,
    "invalid_materialization_commit",
  );
  out.materialization_freshness = requireEnum(
    properties.materialization_freshness,
    freshness,
    "invalid_materialization_freshness",
  );
  out.materialization_result = requireEnum(
    properties.materialization_result,
    PRO_MATERIALIZATION_RESULTS,
    "invalid_materialization_result",
  );
  out.helper_connection_outcome = requireEnum(
    properties.helper_connection_outcome,
    PRO_HELPER_CONNECTION_OUTCOMES,
    "invalid_helper_connection_outcome",
  );
  addOptionalEnum(out, properties, "materialization_failure_bucket", PRO_FAILURE_BUCKETS);
}

function parseV026ProMaterializationProperties(
  out: Record<string, TelemetryScalar>,
  properties: Record<string, unknown>,
  countBuckets: ReadonlySet<string> = V026_PRO_QUERY_COUNT_BUCKETS,
  freshness: ReadonlySet<string> = V026_PRO_QUERY_FRESHNESS,
): void {
  addOptionalEnum(
    out,
    properties,
    "materialization_mode",
    V026_PRO_MATERIALIZATION_MODES,
  );
  out.materialization_commit = requireEnum(
    properties.materialization_commit,
    PRO_COMMIT_OUTCOMES,
    "invalid_materialization_commit",
  );
  out.materialization_freshness = requireEnum(
    properties.materialization_freshness,
    freshness,
    "invalid_materialization_freshness",
  );
  out.materialization_result = requireEnum(
    properties.materialization_result,
    PRO_MATERIALIZATION_RESULTS,
    "invalid_materialization_result",
  );
  addOptionalEnum(
    out,
    properties,
    "materialization_batch_count_bucket",
    countBuckets,
  );
  addOptionalEnum(
    out,
    properties,
    "materialization_input_count_bucket",
    countBuckets,
  );
  addOptionalEnum(
    out,
    properties,
    "materialization_output_count_bucket",
    countBuckets,
  );
  addOptionalEnum(
    out,
    properties,
    "materialization_lag_bucket",
    countBuckets,
  );
  out.helper_connection_outcome = requireEnum(
    properties.helper_connection_outcome,
    PRO_HELPER_CONNECTION_OUTCOMES,
    "invalid_helper_connection_outcome",
  );
  addOptionalEnum(out, properties, "materialization_failure_bucket", PRO_FAILURE_BUCKETS);
}

function hasProMaterializationDetails(properties: Record<string, unknown>): boolean {
  return [...V026_PRO_MATERIALIZATION_PROPERTY_KEYS].some(
    (key) => key !== "helper_connection_outcome" && Object.hasOwn(properties, key),
  );
}

function validateAndOmitRetiredProMaterializationProperties(
  properties: Record<string, unknown>,
  appVersion: string,
): Record<string, unknown> {
  if (appVersion !== "1.1.0") return properties;
  if (Object.hasOwn(properties, "materialization_mode")) {
    requireEnum(
      properties.materialization_mode,
      V026_PRO_MATERIALIZATION_MODES,
      "invalid_materialization_mode",
    );
  }
  for (const key of [
    "materialization_batch_count_bucket",
    "materialization_input_count_bucket",
    "materialization_output_count_bucket",
    "materialization_lag_bucket",
  ]) {
    if (Object.hasOwn(properties, key)) {
      requireEnum(properties[key], COUNT_BUCKETS, `invalid_${key}`);
    }
  }
  return Object.fromEntries(Object.entries(properties).filter(
    ([key]) => !RETIRED_PRO_MATERIALIZATION_PROPERTY_KEYS.has(key),
  ));
}

function addOptionalEnum(
  out: Record<string, TelemetryScalar>,
  properties: Record<string, unknown>,
  key: string,
  allowed: ReadonlySet<string>,
): void {
  if (!Object.hasOwn(properties, key)) return;
  out[key] = requireEnum(properties[key], allowed, `invalid_${key}`);
}

function addOptionalBoolean(
  out: Record<string, TelemetryScalar>,
  properties: Record<string, unknown>,
  key: string,
): void {
  if (!Object.hasOwn(properties, key)) return;
  out[key] = requireBoolean(properties[key], `invalid_${key}`);
}
