import {
  type TelemetryScalar,
  rejectUnknownKeys,
  requireBoolean,
  requireEnum,
  requireExact,
  requireRecord,
  schemaError,
} from "./telemetry-contract";
import { providersForMinor } from "./legacy-cli-providers";
import {
  V025_TRANSITION_IMPORT_KEYS, V025_TRANSITION_PROPERTY_ENUMS,
} from "./legacy-cli-v025-transition-contract";

const BASE_BATCH_KEYS = [
  "broker_install_id", "broker_runtime", "broker_app_version", "broker_os", "broker_arch", "events",
] as const;
const BASE_EVENT_KEYS = [
  "event_id", "event_name", "event_version", "occurred_at", "plane", "delivery",
  "origin_runtime", "origin_install_id", "app_version", "os", "arch", "surface", "source",
  "duration_ms", "duration_bucket", "status", "success", "properties",
] as const;

export type LegacyCliRelease = {
  version: string;
  minor: number;
  hasDeviceIdentity: boolean;
  supportsInstallAttempt: boolean;
  batchKeys: ReadonlySet<string>;
  eventKeys: ReadonlySet<string>;
  actions: ReadonlySet<string>;
};

const RELEASE_MINORS = new Map<string, number>([
  ["0.1.0", 1], ["0.2.0", 2], ["0.3.0", 3], ["0.4.0", 4], ["0.5.0", 5],
  ["0.6.0", 6], ["0.7.0", 7], ["0.8.0", 8], ["0.9.0", 9], ["0.10.0", 10],
  ["0.11.0", 11], ["0.12.0", 12], ["0.13.0", 13], ["0.14.0", 14],
  ["0.15.0", 15], ["0.16.0", 16], ["0.17.0", 17], ["0.18.0", 18],
  ["0.19.0", 19], ["0.20.0", 20], ["0.21.0", 21], ["0.22.0", 22],
  ["0.23.0", 23], ["0.24.0", 24], ["0.25.0", 25],
]);

export function legacyCliRelease(version: string): LegacyCliRelease {
  const minor = RELEASE_MINORS.get(version);
  if (minor === undefined) throw schemaError("invalid_legacy_app_version");
  const hasDeviceIdentity = minor >= 14;
  const supportsInstallAttempt = minor >= 18;
  return {
    version,
    minor,
    hasDeviceIdentity,
    supportsInstallAttempt,
    batchKeys: new Set([
      ...BASE_BATCH_KEYS,
      ...(hasDeviceIdentity ? ["broker_device_id"] : []),
    ]),
    eventKeys: new Set([
      ...BASE_EVENT_KEYS,
      ...(hasDeviceIdentity ? ["origin_device_id"] : []),
      ...(supportsInstallAttempt ? ["install_attempt_id"] : []),
    ]),
    actions: new Set(propertySchemasForMinor(minor).keys()),
  };
}

const V025_AUTO_UPGRADE_ACTIONS = new Set([
  "setup", "sources", "import", "show", "locate", "search", "integrations", "doctor",
]);
const HISTORICAL_AUTO_UPGRADE_ACTIONS = new Set([
  "setup", "sources", "import", "show", "locate", "search", "skill", "integrations", "doctor",
]);
const CAPABILITY_KEYS = [
  "capability_snapshot_schema", "available_parallelism_bucket", "host_memory_bucket",
  "cpu_vector_tier", "acceleration_candidate",
] as const;
const AUTO_UPGRADE_KEYS = [
  "auto_upgrade_probe", "auto_upgrade_due", "auto_upgrade_spawned", "auto_upgrade_spawn_status",
  "upgrade_channel",
] as const;
const V025_COMMON_PROPERTY_KEYS = new Set([
  "action", "json_output", "analytics_client", "install_manager", "failure_kind",
  ...CAPABILITY_KEYS,
]);
const V025_ACTION_PROPERTY_KEYS = new Map<string, ReadonlySet<string>>([
  ["setup_started", setOf("catalog_only", "no_daemon", "progress_mode")],
  ["setup", setOf(
    "catalog_only", "no_daemon", "progress_mode", "setup_completed", "setup_result",
    "providers_detected_bucket", "cataloged_sessions_bucket", "inventory_sources_bucket",
    "inventory_source_files_bucket", "pending_sessions_bucket", "catalog_source_bytes_bucket",
    "inventory_source_bytes_bucket", "has_indexed_content_after_setup", "indexed_sessions_bucket",
    "indexed_events_bucket", "indexed_items_bucket", "sources_seen_bucket", "source_bytes_bucket",
    "source_files_bucket", "failed_sources_bucket", "sessions_imported_bucket",
    "events_imported_bucket", "edges_imported_bucket", "skipped_bucket", "failed_bucket",
    ...V025_TRANSITION_IMPORT_KEYS,
  )],
  ["sources", setOf(
    "providers_detected_bucket", "providers_existing_bucket", "providers_importable_bucket",
  )],
  ["import", setOf(
    "resume", "all_sources", "no_daemon", "source_mode", "provider_filter", "reset_cursor",
    "progress_mode", "sources_seen_bucket", "source_bytes_bucket", "source_files_bucket",
    "failed_sources_bucket", "sessions_imported_bucket", "events_imported_bucket",
    "edges_imported_bucket", "skipped_bucket", "failed_bucket",
    ...V025_TRANSITION_IMPORT_KEYS,
  )],
  ["show", setOf(
    "target_kind", "transcript_mode", "output_format", "writes_out_file", "provider_lookup",
    "window_bucket", "events_returned_bucket",
  )],
  ["locate", setOf("target_kind", "output_format", "provider_lookup")],
  ["search", setOf(
    "has_query", "has_provider_filter", "has_workspace_filter", "has_since_filter",
    "has_event_type_filter", "has_file_filter", "has_session_filter", "event_results",
    "primary_only", "include_subagents", "include_current_session", "limit_bucket",
    "provider_filter", "had_existing_store_before_search", "indexed_content_before_search_known",
    "had_indexed_content_before_search", "refresh_duration_bucket", "search_refresh_mode",
    "search_refresh_status", "search_refresh_source_count_bucket", "db_size_bucket",
    "store_created_by_search", "indexed_sessions_bucket", "indexed_events_bucket",
    "indexed_items_bucket", "has_indexed_content_after_search", "query_length_bucket",
    "query_term_count_bucket", "query_duration_bucket", "search_backend_requested",
    "search_backend_effective", "result_count_bucket", "citation_count_bucket", "zero_result",
    "render_duration_bucket",
  )],
  ["docs", setOf()],
  ["integrations", setOf(
    "integration_action", "integration_name", "integration_target", "integration_scope",
    "skill_name", "skill_action", "skill_scope", "slash_command_scope", "target_agent_group",
    "target_agents_count_bucket", "resolved_agents_count_bucket",
    "slash_command_target_agents_count_bucket", "force", "install_result",
    "modified_targets_bucket", "already_installed", "updated", "status_result",
    "current_targets_bucket",
  )],
  ["daemon", setOf("daemon_command", "once", "force", "start_mode", "trigger_command")],
  ["upgrade", setOf(
    "dry_run", "background", "upgrade_mode", "upgrade_operation", "upgrade_status",
    "upgrade_applied", "upgrade_scheduled", "update_available", "managed_install",
    "self_upgrade_allowed", "auto_upgrade_allowed", "upgrade_warning_count_bucket",
    "upgrade_channel", "upgrade_failure_kind",
  )],
  ["doctor", setOf("finding_count_bucket")],
]);
const V025_REQUIRED_PROPERTIES = new Map<string, readonly string[]>([
  ["setup_started", ["catalog_only", "no_daemon", "progress_mode"]],
  ["setup", [
    "catalog_only", "no_daemon", "progress_mode", "setup_completed", "setup_result",
  ]],
  ["import", ["resume", "all_sources", "no_daemon", "source_mode", "reset_cursor", "progress_mode"]],
  ["search", [
    "has_query", "has_provider_filter", "has_workspace_filter", "has_since_filter",
    "has_event_type_filter", "has_file_filter", "has_session_filter", "event_results",
    "primary_only", "include_subagents", "include_current_session", "limit_bucket",
  ]],
  ["integrations", ["integration_action", "target_agent_group", "target_agents_count_bucket"]],
  ["daemon", ["daemon_command"]],
  ["upgrade", [
    "dry_run", "background", "upgrade_mode", "upgrade_operation", "upgrade_status",
    "upgrade_applied", "upgrade_scheduled",
  ]],
]);
const V025_SUCCESS_PROPERTIES = new Map<string, readonly string[]>([
  ["setup", [
    "providers_detected_bucket", "cataloged_sessions_bucket", "inventory_sources_bucket",
    "inventory_source_files_bucket", "pending_sessions_bucket", "catalog_source_bytes_bucket",
    "inventory_source_bytes_bucket", "indexed_sessions_bucket", "indexed_events_bucket",
    "indexed_items_bucket", "has_indexed_content_after_setup",
  ]],
  ["sources", [
    "providers_detected_bucket", "providers_existing_bucket", "providers_importable_bucket",
  ]],
  ["show", ["events_returned_bucket"]],
  ["import", [
    "sources_seen_bucket", "source_bytes_bucket", "source_files_bucket", "failed_sources_bucket",
    "sessions_imported_bucket", "events_imported_bucket", "edges_imported_bucket",
    "skipped_bucket", "failed_bucket",
  ]],
  ["doctor", ["finding_count_bucket"]],
  ["upgrade", ["update_available"]],
  ["search", [
    "had_existing_store_before_search", "indexed_content_before_search_known",
    "had_indexed_content_before_search", "refresh_duration_bucket", "search_refresh_mode",
    "search_refresh_status", "search_refresh_source_count_bucket", "db_size_bucket",
    "store_created_by_search", "indexed_sessions_bucket", "indexed_events_bucket",
    "indexed_items_bucket", "has_indexed_content_after_search", "query_length_bucket",
    "query_term_count_bucket", "query_duration_bucket", "search_backend_requested",
    "search_backend_effective", "result_count_bucket", "citation_count_bucket", "zero_result",
    "render_duration_bucket",
  ]],
]);

const IMPORT_RESULT_KEYS = [
  "sources_seen_bucket", "source_bytes_bucket", "source_files_bucket", "failed_sources_bucket",
  "sessions_imported_bucket", "events_imported_bucket", "edges_imported_bucket",
  "skipped_bucket", "failed_bucket",
] as const;
const SETUP_RESULT_KEYS = [
  "providers_detected_bucket", "cataloged_sessions_bucket", "pending_sessions_bucket",
  "catalog_source_bytes_bucket", ...IMPORT_RESULT_KEYS,
] as const;
const SEARCH_INITIAL_V001_KEYS = [
  "has_query", "has_provider_filter", "has_repo_filter", "has_since_filter",
  "has_event_type_filter", "has_file_filter", "primary_only", "include_subagents",
  "limit_bucket", "provider_filter",
] as const;
const SEARCH_INITIAL_V006_KEYS = [
  "has_query", "has_provider_filter", "has_workspace_filter", "has_since_filter",
  "has_event_type_filter", "has_file_filter", "has_session_filter", "event_results",
  "primary_only", "include_subagents", "include_current_session", "limit_bucket",
  "provider_filter",
] as const;
const SEARCH_RESULT_V001_KEYS = ["result_count_bucket", "citation_count_bucket"] as const;
const RESEARCH_V006_KEYS = [
  "has_query", "has_provider_filter", "has_workspace_filter", "has_since_filter",
  "has_event_type_filter", "has_file_filter", "primary_only", "include_subagents",
  "include_current_session", "limit_bucket", "provider_filter", "result_count_bucket",
] as const;
const SEARCH_RESULT_V014_KEYS = [
  "refresh_duration_bucket", "search_refresh_mode", "search_refresh_status",
  "search_refresh_source_count_bucket", "db_size_bucket", "indexed_sessions_bucket",
  "indexed_events_bucket", "indexed_items_bucket", "query_length_bucket",
  "query_term_count_bucket", "query_duration_bucket", "result_count_bucket",
  "citation_count_bucket", "zero_result", "render_duration_bucket",
] as const;
const STATUS_V001_KEYS = [
  "initialized", "indexed_items_bucket", "indexed_sources_bucket", "cataloged_sessions_bucket",
] as const;
const STATUS_V014_KEYS = [
  ...STATUS_V001_KEYS, "indexed_sessions_bucket", "indexed_events_bucket", "db_size_bucket",
] as const;
const SHOW_V001_KEYS = [
  "target_kind", "transcript_mode", "output_format", "provider_lookup", "window_bucket",
  "events_returned_bucket",
] as const;
const SHOW_V008_KEYS = [...SHOW_V001_KEYS, "writes_out_file"] as const;
const LOCATE_KEYS = ["target_kind", "output_format", "provider_lookup"] as const;
const SKILL_KEYS = [
  "skill_name", "skill_action", "skill_scope", "target_agent_group",
  "target_agents_count_bucket", "install_result", "already_installed", "updated",
  "status_result", "current_targets_bucket",
] as const;
const UPGRADE_V021_KEYS = [
  "dry_run", "background", "upgrade_mode", "upgrade_operation", "upgrade_status",
  "upgrade_applied", "upgrade_scheduled", "update_available", "managed_install",
  "self_upgrade_allowed", "auto_upgrade_allowed", "upgrade_warning_count_bucket",
  "upgrade_channel", "upgrade_failure_kind",
] as const;

const V001_PROPERTY_SCHEMAS = actionSchemas([
  ["setup", ["progress_mode", ...SETUP_RESULT_KEYS]],
  ["status", STATUS_V001_KEYS],
  ["sources", [
    "providers_detected_bucket", "providers_existing_bucket", "providers_importable_bucket",
  ]],
  ["import", [
    "resume", "all_sources", "source_mode", "provider_filter", "progress_mode",
    ...IMPORT_RESULT_KEYS,
  ]],
  ["list", ["limit_bucket", "items_returned_bucket"]],
  ["show", SHOW_V001_KEYS],
  ["locate", LOCATE_KEYS],
  ["export", [
    "target_kind", "transcript_mode", "output_format", "writes_out_file", "provider_lookup",
    "events_returned_bucket",
  ]],
  ["search", [...SEARCH_INITIAL_V001_KEYS, ...SEARCH_RESULT_V001_KEYS]],
  ["doctor", ["finding_count_bucket"]],
  ["validate", ["finding_count_bucket"]],
]);
const V002_PROPERTY_SCHEMAS = replaceActionSchema(
  V001_PROPERTY_SCHEMAS,
  "setup",
  ["catalog_only", "progress_mode", ...SETUP_RESULT_KEYS],
);
const V006_PROPERTY_SCHEMAS = replaceActionSchema(
  addActionSchema(V002_PROPERTY_SCHEMAS, "research", RESEARCH_V006_KEYS),
  "search",
  [...SEARCH_INITIAL_V006_KEYS, ...SEARCH_RESULT_V001_KEYS],
);
const V007_PROPERTY_SCHEMAS = removeActionSchema(V006_PROPERTY_SCHEMAS, "research");
const V008_PROPERTY_SCHEMAS = replaceActionSchema(
  removeActionSchema(
    removeActionSchema(removeActionSchema(V007_PROPERTY_SCHEMAS, "list"), "export"),
    "validate",
  ),
  "show",
  SHOW_V008_KEYS,
);
const V011_PROPERTY_SCHEMAS = addActionSchema(
  addActionSchema(V008_PROPERTY_SCHEMAS, "docs", []),
  "upgrade",
  ["dry_run", "background"],
);
const V014_PROPERTY_SCHEMAS = replaceActionSchema(
  replaceActionSchema(V011_PROPERTY_SCHEMAS, "status", STATUS_V014_KEYS),
  "search",
  [...SEARCH_INITIAL_V006_KEYS, ...SEARCH_RESULT_V014_KEYS],
);
const V016_PROPERTY_SCHEMAS = replaceActionSchema(
  V014_PROPERTY_SCHEMAS,
  "import",
  [
    "resume", "all_sources", "source_mode", "provider_filter", "reset_cursor", "progress_mode",
    ...IMPORT_RESULT_KEYS,
  ],
);
const V018_PROPERTY_SCHEMAS = addActionSchema(
  V016_PROPERTY_SCHEMAS,
  "setup_started",
  ["catalog_only", "progress_mode"],
);
const V020_PROPERTY_SCHEMAS = addActionSchema(V018_PROPERTY_SCHEMAS, "skill", SKILL_KEYS);
const V021_PROPERTY_SCHEMAS = replaceActionSchema(
  replaceActionSchema(
    replaceActionSchema(
      removeActionSchema(V020_PROPERTY_SCHEMAS, "status"),
      "setup",
      [
        "catalog_only", "progress_mode", "setup_completed", "setup_result",
        "inventory_sources_bucket", "inventory_source_files_bucket",
        "inventory_source_bytes_bucket", "indexed_sessions_bucket", "indexed_events_bucket",
        "indexed_items_bucket", "has_indexed_content_after_setup", ...SETUP_RESULT_KEYS,
      ],
    ),
    "search",
    [
      ...SEARCH_INITIAL_V006_KEYS, "had_existing_store_before_search",
      "indexed_content_before_search_known", "had_indexed_content_before_search",
      "store_created_by_search", "has_indexed_content_after_search",
      ...SEARCH_RESULT_V014_KEYS,
    ],
  ),
  "upgrade",
  UPGRADE_V021_KEYS,
);
const V022_PROPERTY_SCHEMAS = addActionSchema(
  removeActionSchema(V021_PROPERTY_SCHEMAS, "skill"),
  "integrations",
  [...(V025_ACTION_PROPERTY_KEYS.get("integrations") ?? [])],
);
const V024_PROPERTY_SCHEMAS = new Map(V025_ACTION_PROPERTY_KEYS);

function propertySchemasForMinor(minor: number): ReadonlyMap<string, ReadonlySet<string>> {
  if (minor === 1) return V001_PROPERTY_SCHEMAS;
  if (minor <= 5) return V002_PROPERTY_SCHEMAS;
  if (minor === 6) return V006_PROPERTY_SCHEMAS;
  if (minor === 7) return V007_PROPERTY_SCHEMAS;
  if (minor <= 10) return V008_PROPERTY_SCHEMAS;
  if (minor <= 13) return V011_PROPERTY_SCHEMAS;
  if (minor <= 15) return V014_PROPERTY_SCHEMAS;
  if (minor <= 17) return V016_PROPERTY_SCHEMAS;
  if (minor <= 19) return V018_PROPERTY_SCHEMAS;
  if (minor === 20) return V020_PROPERTY_SCHEMAS;
  if (minor === 21) return V021_PROPERTY_SCHEMAS;
  if (minor <= 23) return V022_PROPERTY_SCHEMAS;
  return V024_PROPERTY_SCHEMAS;
}

const BOOLEAN_PROPERTIES = new Set([
  "json_output", "catalog_only", "no_daemon", "setup_completed", "has_indexed_content_after_setup",
  "resume", "all_sources", "reset_cursor", "writes_out_file", "provider_lookup", "has_query",
  "has_provider_filter", "has_repo_filter", "has_workspace_filter", "has_since_filter",
  "has_event_type_filter", "has_file_filter", "has_session_filter", "event_results", "primary_only",
  "include_subagents", "include_current_session", "had_existing_store_before_search",
  "indexed_content_before_search_known", "had_indexed_content_before_search",
  "store_created_by_search", "has_indexed_content_after_search", "zero_result", "force",
  "already_installed", "updated", "once", "dry_run", "background", "upgrade_applied",
  "upgrade_scheduled", "update_available", "managed_install", "self_upgrade_allowed",
  "auto_upgrade_allowed", "auto_upgrade_probe", "auto_upgrade_due", "auto_upgrade_spawned",
  "initialized",
]);
const COUNT_BUCKET_PROPERTIES = new Set([
  "providers_detected_bucket", "cataloged_sessions_bucket", "inventory_sources_bucket",
  "inventory_source_files_bucket", "pending_sessions_bucket", "indexed_sessions_bucket",
  "indexed_events_bucket", "indexed_items_bucket", "indexed_sources_bucket", "items_returned_bucket",
  "sources_seen_bucket", "source_files_bucket", "failed_sources_bucket",
  "sessions_imported_bucket", "events_imported_bucket", "edges_imported_bucket", "skipped_bucket",
  "failed_bucket", "providers_existing_bucket", "providers_importable_bucket", "window_bucket",
  "events_returned_bucket", "limit_bucket", "search_refresh_source_count_bucket",
  "query_term_count_bucket", "result_count_bucket", "citation_count_bucket",
  "target_agents_count_bucket", "resolved_agents_count_bucket",
  "slash_command_target_agents_count_bucket", "modified_targets_bucket", "current_targets_bucket",
  "upgrade_warning_count_bucket", "finding_count_bucket", "rejected_records_bucket",
]);
const BYTE_BUCKET_PROPERTIES = new Set([
  "catalog_source_bytes_bucket", "inventory_source_bytes_bucket", "source_bytes_bucket", "db_size_bucket",
]);
const DURATION_BUCKET_PROPERTIES = new Set([
  "refresh_duration_bucket", "query_duration_bucket", "render_duration_bucket",
]);
const V014_SEARCH_REFRESH_MODES = new Set(["auto", "off", "strict"]);
const V024_SEARCH_REFRESH_MODES = new Set(["background", "off", "wait"]);
const V014_SEARCH_REFRESH_STATUSES = new Set(["skipped", "no_sources", "completed", "failed"]);
const V024_SEARCH_REFRESH_STATUSES = new Set([
  ...V014_SEARCH_REFRESH_STATUSES, "daemon_background",
]);
const PROPERTY_ENUMS = new Map<string, ReadonlySet<string>>([
  ["available_parallelism_bucket", new Set([
    "unknown", "1", "2", "3-4", "5-8", "9-16", "17-32", "33-64", "65+",
  ])],
  ["host_memory_bucket", new Set([
    "unknown", "lt_4gb", "4-8gb", "8-16gb", "16-32gb", "32-64gb", "64gb+",
  ])],
  ["cpu_vector_tier", new Set(["avx512", "avx2", "x86_baseline", "arm_neon", "other"])],
  ["acceleration_candidate", new Set(["apple_ane", "nvidia_cuda", "not_detected", "unknown"])],
  ["progress_mode", new Set(["auto", "plain", "json", "none"])],
  ["setup_result", new Set(["success", "failure"])],
  ["source_mode", new Set([
    "explicit_format", "history_source_plugin", "explicit_path", "all_discovered",
    "discovered_provider", "auto_discovered",
  ])],
  ["target_kind", new Set(["session", "event"])],
  ["transcript_mode", new Set(["lite", "full", "log"])],
  ["output_format", new Set(["text", "markdown", "json", "jsonl"])],
  ["search_backend_requested", new Set(["hybrid", "lexical", "semantic"])],
  ["search_backend_effective", new Set(["hybrid", "lexical", "semantic"])],
  ["integration_action", new Set(["install", "status"])],
  ["integration_name", new Set(["mcp"])],
  ["integration_target", new Set(["skills", "slash_commands"])],
  ["integration_scope", new Set(["global", "project"])],
  ["skill_name", new Set(["ctx-agent-history-search"])],
  ["skill_action", new Set(["install", "status"])],
  ["skill_scope", new Set(["global", "project"])],
  ["slash_command_scope", new Set(["global", "project"])],
  ["target_agent_group", new Set([
    "all", "detected", "explicit", "default", "picker", "fallback",
  ])],
  ["install_result", new Set(["ok", "partial_error"])],
  ["status_result", new Set(["all_current", "none_current", "partially_current"])],
  ["daemon_command", new Set(["run", "enable", "disable"])],
  ["start_mode", new Set(["auto", "manual"])],
  ["trigger_command", new Set(["setup", "import", "search"])],
  ["upgrade_mode", new Set(["manual", "auto"])],
  ["upgrade_operation", new Set(["apply", "check", "status", "enable", "disable"])],
  ["upgrade_status", new Set([
    "available", "up_to_date", "applied", "scheduled", "dry_run", "locked", "status_checked",
    "auto_enabled", "auto_disabled", "skipped", "failed",
  ])],
  ["upgrade_channel", new Set(["stable", "beta", "canary", "dev", "other"])],
  ["upgrade_failure_kind", new Set([
    "lock_failed", "unmanaged_install", "metadata_fetch", "signature_verify", "metadata_invalid",
    "artifact_verify", "artifact_download", "policy_disallowed", "apply_failed",
  ])],
  ["auto_upgrade_spawn_status", new Set([
    "json_output", "auto_disabled", "ci", "env_disabled", "background_child", "not_due",
    "marker_invalid", "current_exe_error", "spawned", "spawn_failed",
  ])],
  ...V025_TRANSITION_PROPERTY_ENUMS,
]);
const COUNT_BUCKETS = new Set(["0", "1", "2-5", "6-20", "21-100", "101-1k", "1k+"]);
const BYTE_BUCKETS = new Set([
  "0", "lt_100kb", "100kb-1mb", "1mb-10mb", "10mb-100mb", "100mb-1gb", "1gb+",
]);
export const LEGACY_DURATION_BUCKETS = new Set([
  "lt_100ms", "lt_1s", "lt_5s", "lt_30s", "gte_30s",
]);
const TEXT_LENGTH_BUCKETS = new Set(["0", "1-20", "21-100", "101-500", "500+"]);
export function parseLegacyCliProperties(
  release: LegacyCliRelease,
  value: unknown,
  success: boolean,
  hasInstallAttemptId: boolean,
): Record<string, TelemetryScalar> {
  if (release.minor === 25) {
    return parseV025Properties(value, success, hasInstallAttemptId);
  }
  return parseHistoricalProperties(release, value, success, hasInstallAttemptId);
}

function parseHistoricalProperties(
  release: LegacyCliRelease,
  value: unknown,
  success: boolean,
  hasInstallAttemptId: boolean,
): Record<string, TelemetryScalar> {
  const properties = requireRecord(value, "invalid_properties");
  const action = requireEnum(properties.action, release.actions, "invalid_legacy_action");
  const hasAutoUpgrade = release.minor >= 21 &&
    success && HISTORICAL_AUTO_UPGRADE_ACTIONS.has(action);
  const actionSchema = propertySchemasForMinor(release.minor).get(action);
  if (!actionSchema) throw schemaError("invalid_legacy_action");
  const allowed = new Set(["action", "json_output", "analytics_client", ...actionSchema]);
  if (hasInstallAttemptId) allowed.add("install_manager");
  if (!success) allowed.add("failure_kind");
  if (hasAutoUpgrade) for (const key of AUTO_UPGRADE_KEYS) allowed.add(key);
  rejectUnknownKeys(properties, allowed, "unknown_legacy_property");

  const out = parseKnownProperties(properties, action, release.minor);
  validateCommonProperties(properties, out, success, hasInstallAttemptId);
  validateHistoricalActionProperties(release.minor, properties, action, success);
  if (hasAutoUpgrade) validateAutoUpgrade(properties, release.minor);
  return out;
}

function parseV025Properties(
  value: unknown,
  success: boolean,
  hasInstallAttemptId: boolean,
): Record<string, TelemetryScalar> {
  const properties = requireRecord(value, "invalid_properties");
  const action = requireEnum(
    properties.action,
    new Set(V024_PROPERTY_SCHEMAS.keys()),
    "invalid_legacy_action",
  );
  const hasAutoUpgrade = success && V025_AUTO_UPGRADE_ACTIONS.has(action);
  rejectUnknownKeys(
    properties,
    new Set([
      ...V025_COMMON_PROPERTY_KEYS,
      ...(V025_ACTION_PROPERTY_KEYS.get(action) ?? []),
      ...(hasAutoUpgrade ? AUTO_UPGRADE_KEYS : []),
    ]),
    "unknown_legacy_property",
  );

  const out = parseKnownProperties(properties, action, 25);
  requireProperties(properties, V025_REQUIRED_PROPERTIES.get(action) ?? []);
  if (success) requireProperties(properties, V025_SUCCESS_PROPERTIES.get(action) ?? []);
  validateCapabilitySnapshot(properties);
  validateCommonProperties(properties, out, success, hasInstallAttemptId);
  validateV025ActionProperties(properties, action, success);
  if (hasAutoUpgrade) validateAutoUpgrade(properties, 25);
  return out;
}

function parseKnownProperties(
  properties: Record<string, unknown>,
  action: string,
  minor: number,
): Record<string, TelemetryScalar> {
  const out: Record<string, TelemetryScalar> = { action };
  out.json_output = requireBoolean(properties.json_output, "invalid_json_output");
  requireExact(properties.analytics_client, "ctx-cli", "invalid_analytics_client");
  out.analytics_client = "ctx-cli";
  for (const [key, entry] of Object.entries(properties)) {
    if (new Set(["action", "analytics_client", "install_manager", "failure_kind"]).has(key)) continue;
    if (key === "capability_snapshot_schema") {
      requireExact(entry, 1, "invalid_capability_snapshot_schema");
      out[key] = 1;
    } else if (BOOLEAN_PROPERTIES.has(key)) {
      out[key] = requireBoolean(entry, `invalid_${key}`);
    } else if (key === "provider_filter") {
      out[key] = requireEnum(entry, providersForMinor(minor), "invalid_provider_filter");
    } else if (key === "search_refresh_mode") {
      out[key] = requireEnum(
        entry,
        minor >= 24 ? V024_SEARCH_REFRESH_MODES : V014_SEARCH_REFRESH_MODES,
        "invalid_search_refresh_mode",
      );
    } else if (key === "search_refresh_status") {
      out[key] = requireEnum(
        entry,
        minor >= 24 ? V024_SEARCH_REFRESH_STATUSES : V014_SEARCH_REFRESH_STATUSES,
        "invalid_search_refresh_status",
      );
    } else if (PROPERTY_ENUMS.has(key)) {
      out[key] = requireEnum(entry, PROPERTY_ENUMS.get(key)!, `invalid_${key}`);
    } else if (COUNT_BUCKET_PROPERTIES.has(key)) {
      out[key] = requireEnum(entry, COUNT_BUCKETS, `invalid_${key}`);
    } else if (BYTE_BUCKET_PROPERTIES.has(key)) {
      out[key] = requireEnum(entry, BYTE_BUCKETS, `invalid_${key}`);
    } else if (DURATION_BUCKET_PROPERTIES.has(key)) {
      out[key] = requireEnum(entry, LEGACY_DURATION_BUCKETS, `invalid_${key}`);
    } else if (key === "query_length_bucket") {
      out[key] = requireEnum(entry, TEXT_LENGTH_BUCKETS, "invalid_query_length_bucket");
    } else {
      throw schemaError(`invalid_${key}`);
    }
  }
  return out;
}

function validateCommonProperties(
  properties: Record<string, unknown>,
  out: Record<string, TelemetryScalar>,
  success: boolean,
  hasInstallAttemptId: boolean,
): void {
  if (hasInstallAttemptId) {
    requireExact(properties.install_manager, "ctx-hosted-installer", "invalid_install_manager");
    out.install_manager = "ctx-hosted-installer";
  } else if (Object.hasOwn(properties, "install_manager")) {
    throw schemaError("inconsistent_install_manager");
  }
  if (success) {
    if (Object.hasOwn(properties, "failure_kind")) throw schemaError("unexpected_failure_kind");
  } else {
    requireExact(properties.failure_kind, "command_error", "invalid_failure_kind");
    out.failure_kind = "command_error";
  }
}

function validateV025ActionProperties(
  properties: Record<string, unknown>,
  action: string,
  success: boolean,
): void {
  if (action === "setup_started" && !success) throw schemaError("invalid_setup_started_outcome");
  if (action === "setup") {
    if (properties.setup_completed !== success) throw schemaError("inconsistent_setup_completed");
    if (properties.setup_result !== (success ? "success" : "failure")) {
      throw schemaError("inconsistent_setup_result");
    }
  } else if (action === "import") {
    // Released v0.25 clients emitted provider_filter as an independent import
    // sidecar. Its value and source_mode remain closed and validated above,
    // but their presence cannot be constrained relative to each other.
  } else if (action === "show") {
    validateShow(properties);
  } else if (action === "locate") {
    validateLocate(properties);
  } else if (action === "search") {
    const hasProvider = properties.has_provider_filter === true;
    if (Object.hasOwn(properties, "provider_filter") !== hasProvider) {
      throw schemaError("inconsistent_provider_filter");
    }
  } else if (action === "integrations") {
    validateIntegration(properties, success);
  } else if (action === "daemon") {
    validateDaemon(properties);
  } else if (action === "upgrade") {
    validateUpgrade(properties, success);
  }
}

function validateHistoricalActionProperties(
  minor: number,
  properties: Record<string, unknown>,
  action: string,
  success: boolean,
): void {
  if (minor === 24) {
    requireProperties(properties, V025_REQUIRED_PROPERTIES.get(action) ?? []);
    if (success) requireProperties(properties, V025_SUCCESS_PROPERTIES.get(action) ?? []);
    validateV025ActionProperties(properties, action, success);
    return;
  }
  if (action === "setup_started") {
    requireProperties(properties, ["catalog_only", "progress_mode"]);
    if (!success) throw schemaError("invalid_setup_started_outcome");
  } else if (action === "setup") {
    requireProperties(properties, [
      ...(minor >= 2 ? ["catalog_only"] : []),
      "progress_mode",
    ]);
    if (minor >= 21) {
      requireProperties(properties, ["setup_completed", "setup_result"]);
      if (properties.setup_completed !== success) throw schemaError("inconsistent_setup_completed");
      if (properties.setup_result !== (success ? "success" : "failure")) {
        throw schemaError("inconsistent_setup_result");
      }
    }
    if (success) {
      requireProperties(properties, SETUP_RESULT_KEYS.slice(0, 4));
      if (minor === 1 || properties.catalog_only === false) {
        requireProperties(properties, IMPORT_RESULT_KEYS);
      } else {
        rejectPresentProperties(properties, IMPORT_RESULT_KEYS);
      }
      if (minor >= 21) {
        requireProperties(properties, [
          "inventory_sources_bucket", "inventory_source_files_bucket",
          "inventory_source_bytes_bucket", "indexed_sessions_bucket", "indexed_events_bucket",
          "indexed_items_bucket", "has_indexed_content_after_setup",
        ]);
      }
    }
  } else if (action === "status" && success) {
    requireProperties(properties, minor >= 14 ? STATUS_V014_KEYS : STATUS_V001_KEYS);
  } else if (action === "sources" && success) {
    requireProperties(properties, [
      "providers_detected_bucket", "providers_existing_bucket", "providers_importable_bucket",
    ]);
  } else if (action === "import") {
    requireProperties(properties, [
      "resume", "all_sources", ...(minor >= 24 ? ["no_daemon"] : []), "source_mode",
      ...(minor >= 16 ? ["reset_cursor"] : []), "progress_mode",
    ]);
    requireEnum(
      properties.source_mode,
      minor >= 16
        ? new Set([
          "explicit_format", "history_source_plugin", "explicit_path", "all_discovered",
          "discovered_provider", "auto_discovered",
        ])
        : new Set(["explicit_path", "all_discovered", "discovered_provider", "auto_discovered"]),
      "invalid_source_mode",
    );
    if (Object.hasOwn(properties, "provider_filter") !==
      (properties.source_mode === "discovered_provider")) {
      throw schemaError("inconsistent_provider_filter");
    }
    if (success) requireProperties(properties, IMPORT_RESULT_KEYS);
  } else if (action === "list") {
    requireProperties(properties, ["limit_bucket"]);
    if (success) requireProperties(properties, ["items_returned_bucket"]);
  } else if (action === "show") {
    validateHistoricalShow(properties, minor);
    if (success) requireProperties(properties, ["events_returned_bucket"]);
  } else if (action === "locate") {
    validateLocate(properties);
  } else if (action === "export") {
    requireProperties(properties, [
      "target_kind", "transcript_mode", "output_format", "writes_out_file", "provider_lookup",
    ]);
    requireExact(properties.target_kind, "session", "invalid_target_kind");
    if (success) requireProperties(properties, ["events_returned_bucket"]);
  } else if (action === "search") {
    requireProperties(properties, minor <= 5
      ? SEARCH_INITIAL_V001_KEYS.filter((key) => key !== "provider_filter")
      : SEARCH_INITIAL_V006_KEYS.filter((key) => key !== "provider_filter"));
    if (Object.hasOwn(properties, "provider_filter") !==
      (properties.has_provider_filter === true)) {
      throw schemaError("inconsistent_provider_filter");
    }
    if (success) requireProperties(
      properties,
      minor >= 14 ? SEARCH_RESULT_V014_KEYS : SEARCH_RESULT_V001_KEYS,
    );
  } else if (action === "research") {
    requireProperties(
      properties,
      RESEARCH_V006_KEYS.filter((key) =>
        key !== "provider_filter" && key !== "result_count_bucket"
      ),
    );
    if (Object.hasOwn(properties, "provider_filter") !==
      (properties.has_provider_filter === true)) {
      throw schemaError("inconsistent_provider_filter");
    }
    if (success) {
      requireProperties(properties, ["result_count_bucket"]);
      if (properties.has_query !== true) throw schemaError("invalid_research_query_state");
    }
  } else if ((action === "doctor" || action === "validate") && success) {
    requireProperties(properties, ["finding_count_bucket"]);
  } else if (action === "skill") {
    validateSkill(properties, success);
  } else if (action === "integrations") {
    requireProperties(properties, [
      "integration_action", "target_agent_group", "target_agents_count_bucket",
    ]);
    validateIntegration(properties, success);
  } else if (action === "daemon") {
    requireProperties(properties, ["daemon_command"]);
    validateDaemon(properties);
  } else if (action === "upgrade") {
    requireProperties(properties, [
      "dry_run", "background", ...(minor >= 21 ? [
        "upgrade_mode", "upgrade_operation", "upgrade_status", "upgrade_applied",
        "upgrade_scheduled",
      ] : []),
    ]);
    if (minor >= 21) {
      if (success) requireProperties(properties, ["update_available"]);
      validateUpgrade(properties, success);
    } else if (properties.background !== false) {
      throw schemaError("unexpected_background_upgrade");
    }
  }
}

function validateHistoricalShow(properties: Record<string, unknown>, minor: number): void {
  if (properties.target_kind === "session") {
    requireProperties(properties, [
      "target_kind", "transcript_mode", "output_format",
      ...(minor >= 8 ? ["writes_out_file"] : []), "provider_lookup",
    ]);
    rejectPresentProperties(properties, ["window_bucket"]);
  } else {
    requireProperties(properties, ["target_kind", "output_format", "window_bucket"]);
    rejectPresentProperties(properties, [
      "transcript_mode", "writes_out_file", "provider_lookup",
    ]);
  }
}

function validateSkill(properties: Record<string, unknown>, success: boolean): void {
  requireProperties(properties, [
    "skill_name", "skill_action", "skill_scope", "target_agent_group",
    "target_agents_count_bucket",
  ]);
  if (success && properties.target_agent_group === "default") {
    throw schemaError("invalid_target_agent_group");
  }
  if (properties.skill_action === "install") {
    rejectPresentProperties(properties, ["status_result", "current_targets_bucket"]);
    if (success) requireProperties(properties, ["install_result", "already_installed", "updated"]);
  } else {
    rejectPresentProperties(properties, ["install_result", "already_installed", "updated"]);
    if (success) requireProperties(properties, ["status_result", "current_targets_bucket"]);
  }
}

function validateShow(properties: Record<string, unknown>): void {
  if (properties.target_kind === "session") {
    requireProperties(properties, [
      "target_kind", "transcript_mode", "output_format", "writes_out_file", "provider_lookup",
    ]);
    rejectPresentProperties(properties, ["window_bucket"]);
  } else {
    requireProperties(properties, ["target_kind", "output_format", "window_bucket"]);
    rejectPresentProperties(properties, ["transcript_mode", "writes_out_file", "provider_lookup"]);
  }
}

function validateLocate(properties: Record<string, unknown>): void {
  requireProperties(properties, ["target_kind", "output_format"]);
  if (properties.target_kind === "session") {
    requireProperties(properties, ["provider_lookup"]);
  } else {
    rejectPresentProperties(properties, ["provider_lookup"]);
  }
  if (properties.output_format !== "text" && properties.output_format !== "json") {
    throw schemaError("invalid_output_format");
  }
}

function validateIntegration(properties: Record<string, unknown>, success: boolean): void {
  const action = String(properties.integration_action);
  const isMcp = Object.hasOwn(properties, "integration_name");
  const target = properties.integration_target;
  if (isMcp === Object.hasOwn(properties, "integration_target")) {
    throw schemaError("invalid_legacy_integration_target");
  }
  if (isMcp) {
    requireProperties(properties, ["integration_scope"]);
    requireEnum(
      properties.target_agent_group,
      new Set(["all", "detected", "explicit"]),
      "invalid_target_agent_group",
    );
    rejectPresentProperties(properties, [
      "skill_name", "skill_action", "skill_scope", "slash_command_scope",
      "slash_command_target_agents_count_bucket", "already_installed", "updated", "status_result",
      "current_targets_bucket",
    ]);
    if (action === "install") {
      requireProperties(properties, ["force"]);
      if (success) {
        requireProperties(properties, [
          "resolved_agents_count_bucket", "install_result", "modified_targets_bucket",
        ]);
      }
    } else {
      rejectPresentProperties(properties, [
        "force", "resolved_agents_count_bucket", "install_result", "modified_targets_bucket",
      ]);
    }
    return;
  }
  if (target === "skills") {
    requireProperties(properties, ["skill_name", "skill_action", "skill_scope"]);
    if (properties.skill_action !== action) throw schemaError("inconsistent_skill_action");
    requireEnum(
      properties.target_agent_group,
      new Set(["all", "detected", "explicit", "default", "picker", "fallback"]),
      "invalid_target_agent_group",
    );
    rejectPresentProperties(properties, [
      "integration_scope", "slash_command_scope", "slash_command_target_agents_count_bucket",
      "resolved_agents_count_bucket", "modified_targets_bucket", "force",
    ]);
    if (success && properties.target_agent_group === "default") {
      throw schemaError("invalid_target_agent_group");
    }
    if (action === "install") {
      rejectPresentProperties(properties, ["status_result", "current_targets_bucket"]);
      if (success) {
        requireProperties(properties, ["install_result", "already_installed", "updated"]);
      }
    } else {
      rejectPresentProperties(properties, ["force", "install_result", "already_installed", "updated"]);
      if (success) requireProperties(properties, ["status_result", "current_targets_bucket"]);
    }
    return;
  }
  if (target !== "slash_commands" || action !== "install") {
    throw schemaError("invalid_legacy_integration_target");
  }
  requireProperties(properties, ["slash_command_scope"]);
  requireEnum(
    properties.target_agent_group,
    new Set(["all", "detected", "explicit"]),
    "invalid_target_agent_group",
  );
  rejectPresentProperties(properties, [
    "integration_scope", "skill_name", "skill_action", "skill_scope", "resolved_agents_count_bucket",
    "modified_targets_bucket", "status_result", "current_targets_bucket", "force",
  ]);
  if (success) {
    requireProperties(properties, [
      "slash_command_target_agents_count_bucket", "install_result", "already_installed", "updated",
    ]);
  }
}

function validateDaemon(properties: Record<string, unknown>): void {
  if (properties.daemon_command === "run") {
    requireProperties(properties, ["once", "force"]);
  } else {
    rejectPresentProperties(properties, ["once", "force", "start_mode", "trigger_command"]);
  }
}

function validateUpgrade(properties: Record<string, unknown>, success: boolean): void {
  if ((properties.background === true) !== (properties.upgrade_mode === "auto")) {
    throw schemaError("inconsistent_upgrade_mode");
  }
  if (properties.background === true && properties.upgrade_operation !== "apply") {
    throw schemaError("invalid_background_upgrade_operation");
  }
  if (success) {
    if (properties.upgrade_status === "failed") throw schemaError("inconsistent_upgrade_status");
    if (Object.hasOwn(properties, "upgrade_failure_kind")) {
      throw schemaError("unexpected_upgrade_failure_kind");
    }
  } else {
    if (properties.upgrade_status !== "failed") throw schemaError("inconsistent_upgrade_status");
    requireProperties(properties, ["upgrade_failure_kind"]);
  }
}

function validateCapabilitySnapshot(properties: Record<string, unknown>): void {
  const count = CAPABILITY_KEYS.filter((key) => Object.hasOwn(properties, key)).length;
  if (count !== 0 && count !== CAPABILITY_KEYS.length) {
    throw schemaError("incomplete_capability_snapshot");
  }
}

function validateAutoUpgrade(properties: Record<string, unknown>, minor: number): void {
  requireProperties(properties, AUTO_UPGRADE_KEYS);
  if (properties.auto_upgrade_probe !== true) throw schemaError("invalid_auto_upgrade_probe");
  if (minor === 25 && properties.auto_upgrade_spawn_status === "json_output") {
    throw schemaError("invalid_auto_upgrade_state");
  }
  const expected = new Map<string, readonly [boolean, boolean]>([
    ["json_output", [false, false]], ["auto_disabled", [false, false]],
    ["ci", [false, false]], ["env_disabled", [false, false]],
    ["background_child", [false, false]], ["not_due", [false, false]],
    ["marker_invalid", [true, false]], ["current_exe_error", [true, false]],
    ["spawned", [true, true]], ["spawn_failed", [true, false]],
  ]).get(String(properties.auto_upgrade_spawn_status));
  if (
    !expected ||
    properties.auto_upgrade_due !== expected[0] ||
    properties.auto_upgrade_spawned !== expected[1]
  ) {
    throw schemaError("invalid_auto_upgrade_state");
  }
}

function requireProperties(properties: Record<string, unknown>, keys: readonly string[]): void {
  for (const key of keys) {
    if (!Object.hasOwn(properties, key)) throw schemaError(`missing_${key}`);
  }
}

function rejectPresentProperties(properties: Record<string, unknown>, keys: readonly string[]): void {
  for (const key of keys) {
    if (Object.hasOwn(properties, key)) throw schemaError(`unexpected_${key}`);
  }
}

function actionSchemas(
  entries: readonly (readonly [string, readonly string[]])[],
): ReadonlyMap<string, ReadonlySet<string>> {
  return new Map(entries.map(([action, keys]) => [action, new Set(keys)]));
}

function addActionSchema(
  base: ReadonlyMap<string, ReadonlySet<string>>,
  action: string,
  keys: readonly string[],
): ReadonlyMap<string, ReadonlySet<string>> {
  const next = new Map(base);
  next.set(action, new Set(keys));
  return next;
}

function replaceActionSchema(
  base: ReadonlyMap<string, ReadonlySet<string>>,
  action: string,
  keys: readonly string[],
): ReadonlyMap<string, ReadonlySet<string>> {
  if (!base.has(action)) throw new Error(`cannot replace absent legacy action schema: ${action}`);
  return addActionSchema(base, action, keys);
}

function removeActionSchema(
  base: ReadonlyMap<string, ReadonlySet<string>>,
  action: string,
): ReadonlyMap<string, ReadonlySet<string>> {
  const next = new Map(base);
  if (!next.delete(action)) throw new Error(`cannot remove absent legacy action schema: ${action}`);
  return next;
}

function setOf(...values: string[]): ReadonlySet<string> {
  return new Set(values);
}
