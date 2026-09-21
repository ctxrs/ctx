import { describe, expect, test, vi } from "vitest";

import {
  NeonTelemetryDatabase,
  TelemetryEventCollisionError,
  type NeonQueryClient,
  type TelemetryDatabase,
  type TelemetryIngestRejection,
} from "../src/database";
import { hmacSha256Hex, sha256Hex } from "../src/hash";
import {
  CURRENT_PROVIDER_IDS,
  HISTORICAL_PROVIDER_IDS,
} from "../src/provider-contract";
import {
  buildInstallStageRow,
  buildTelemetryIngestPlan,
  TelemetryIngestError,
  type InstallStageRow,
  type TelemetryRow,
} from "../src/telemetry-ingest";
import {
  BYTE_BUCKETS,
  COUNT_BUCKETS,
  DURATION_BUCKETS,
  MAX_BODY_BYTES,
  MAX_EVENT_BYTES,
  PROVIDERS,
} from "../src/telemetry-contract";
import {
  createTelemetryWorker,
  type Env,
  type TelemetryQueueProducer,
  type TelemetryRejectionObservation,
} from "../src/worker";
import type { BlameProductReceipt } from "../src/blame-product-receipt";
import {
  decodeTelemetryQueueMessage,
  type TelemetryQueueMessage,
} from "../src/telemetry-queue";

export const NOW = new Date("2026-07-22T18:34:45.000Z");
export const OCCURRED_AT = "2026-07-22T18:34:00Z";
export const HMAC_KEY = "telemetry-test-hmac-key-with-32-bytes-minimum";
export const CLIENT_PROFILE_ID = "11111111-1111-4111-8111-111111111111";
export const DATA_ROOT_ID = "22222222-2222-4222-8222-222222222222";
export const EVENT_ID = "33333333-3333-4333-8333-333333333333";
export const INSTALL_ATTEMPT_ID = "ia_0123456789abcdef";
export const CURRENT_PROVIDER_WIRE_NAMES = CURRENT_PROVIDER_IDS;
export const HISTORICAL_PROVIDER_WIRE_NAMES = HISTORICAL_PROVIDER_IDS;
export const CLOSED_PROVIDERS = [
  ...CURRENT_PROVIDER_WIRE_NAMES,
  ...HISTORICAL_PROVIDER_WIRE_NAMES,
] as const;

export const ENV: Env = {
  TELEMETRY_ANALYTICS_ENVIRONMENT: "production",
  TELEMETRY_DATABASE_URL: "postgresql://telemetry.example.test/db",
  TELEMETRY_IDENTITY_HMAC_KEY: HMAC_KEY,
  TELEMETRY_IDENTITY_KEY_VERSION: "7",
  TELEMETRY_RATE_LIMITER: { async limit() { return { success: true }; } },
};

export const INGEST_OPTIONS = {
  analyticsEnvironment: "production" as const,
  identityHmacKey: HMAC_KEY,
  identityKeyVersion: 7,
  now: () => NOW,
};

export function v1Batch(events: Record<string, unknown>[]): Record<string, unknown> {
  return {
    client_profile_id: CLIENT_PROFILE_ID,
    data_root_id: DATA_ROOT_ID,
    app_version: "0.26.0",
    os: "linux",
    arch: "x86_64",
    events,
  };
}

export function operationEvent(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    event_id: EVENT_ID,
    event_name: "operation_completed",
    event_version: 1,
    occurred_at: OCCURRED_AT,
    surface: "cli",
    operation: "search",
    outcome: "success",
    duration_bucket: "lt_1s",
    properties: {
      output: "human",
      has_query: true,
      has_provider_filter: false,
      has_workspace_filter: false,
      has_since_filter: false,
      has_event_type_filter: false,
      has_file_filter: false,
      has_session_filter: false,
      event_results: false,
      primary_only: true,
      include_subagents: false,
      include_current_session: false,
      limit_bucket: "21-100",
      zero_result: false,
    },
    ...overrides,
  };
}

export function providerRefreshEvent(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    event_id: EVENT_ID,
    event_name: "provider_refresh_completed",
    event_version: 1,
    occurred_at: OCCURRED_AT,
    surface: "cli",
    operation: "refresh",
    outcome: "success",
    duration_bucket: "lt_5s",
    properties: {
      provider: "codex",
      trigger: "search",
      source_mode: "discovered",
      change: "changed",
      content_evidence: "accepted",
      work_kind: "append",
      refresh_result: "complete",
      core_result: "complete",
      canonical_pro_result: "not_requested",
      output_pro_result: "not_requested",
      failure_scope: "none",
      failure_type: "none",
      work_remaining: false,
      sources_bucket: "1",
      source_files_bucket: "1",
      sessions_bucket: "2-5",
      events_bucket: "6-20",
      edges_bucket: "0",
      skips_bucket: "0",
      rejections_bucket: "0",
      failures_bucket: "0",
      bytes_bucket: "lt_100kb",
    },
    ...overrides,
  };
}

export function providerRefreshProperties(
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    provider: "codex",
    trigger: "search",
    source_mode: "discovered",
    change: "changed",
    content_evidence: "accepted",
    work_kind: "append",
    refresh_result: "complete",
    core_result: "complete",
    canonical_pro_result: "no_op",
    output_pro_result: "complete",
    failure_scope: "none",
    failure_type: "none",
    work_remaining: false,
    retired_records_bucket: "0",
    sources_bucket: "1",
    source_files_bucket: "6-20",
    sessions_bucket: "2-5",
    events_bucket: "6-20",
    edges_bucket: "0",
    skips_bucket: "0",
    rejections_bucket: "0",
    failures_bucket: "0",
    bytes_bucket: "lt_100kb",
    ...overrides,
  };
}

export function batchEventId(index: number): string {
  return `00000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}`;
}

export function maximalSearchEvent(index: number): Record<string, unknown> {
  return operationEvent({
    event_id: batchEventId(index),
    duration_bucket: "gte_1h",
    install_attempt_id: `ia_${"A".repeat(128)}`,
    properties: {
      output: "json",
      install_manager: "ctx-hosted-installer",
      capability_snapshot_schema: 1,
      available_parallelism_bucket: "65+",
      host_memory_bucket: "64gb+",
      cpu_vector_tier: "avx512",
      acceleration_candidate: "nvidia_cuda",
      auto_upgrade_probe: true,
      auto_upgrade_due: true,
      auto_upgrade_spawned: false,
      auto_upgrade_spawn_status: "current_exe_error",
      auto_upgrade_channel: "canary",
      deprecated_daemon_control: true,
      deprecated_upgrade_control: true,
      has_query: true,
      has_provider_filter: true,
      has_workspace_filter: true,
      has_since_filter: true,
      has_event_type_filter: true,
      has_file_filter: true,
      has_session_filter: true,
      event_results: true,
      primary_only: true,
      include_subagents: true,
      include_current_session: true,
      limit_bucket: "100k-1m",
      provider_filter: "factory_ai_droid",
      had_existing_store_before_search: true,
      indexed_content_before_search_known: true,
      had_indexed_content_before_search: true,
      refresh_duration_bucket: "gte_1h",
      search_refresh_mode: "background",
      search_refresh_status: "daemon_background",
      search_refresh_source_count_bucket: "100k-1m",
      store_created_by_search: true,
      has_indexed_content_after_search: true,
      query_length_bucket: "500+",
      query_term_count_bucket: "100k-1m",
      query_duration_bucket: "gte_1h",
      search_backend_requested: "semantic",
      search_backend_effective: "semantic",
      result_count_bucket: "100k-1m",
      citation_count_bucket: "100k-1m",
      zero_result: false,
      render_duration_bucket: "gte_1h",
      indexed_sessions_bucket: "100k-1m",
      indexed_events_bucket: "100k-1m",
      indexed_items_bucket: "100k-1m",
      db_size_bucket: "100gb+",
      search_output_duration_bucket: "gte_1h",
      search_output_served: true,
      search_retrieval_round_count_bucket: "100k-1m",
      search_query_execution_count_bucket: "100k-1m",
      search_candidate_rows_total_bucket: "100k-1m",
      search_candidate_records_decoded_bucket: "100k-1m",
      search_candidate_core_bytes_decoded_bucket: "100gb+",
      search_final_candidate_pool_bucket: "100k-1m",
      search_candidate_pool_truncated: true,
      search_stop_reason: "candidate_cap",
      search_failure_phase: "query_preparation",
      search_candidate_session_count_bucket: "100k-1m",
      search_largest_session_candidate_share_bucket: "76-99pct",
      search_literal_root_concentration_availability: "observed",
      search_candidate_literal_root_family_count_bucket: "100k-1m",
      search_literal_root_candidate_coverage_bucket: "76-99pct",
      search_largest_literal_root_candidate_share_bucket: "76-99pct",
      search_provider_copy_candidate_count_bucket: "100k-1m",
      search_provider_copy_candidate_share_bucket: "76-99pct",
      search_copy_cluster_availability: "not_constructed_v1",
      search_diversification_status: "applied",
      search_diversification_changed_final_top_n: true,
    },
  });
}

export function runtimeEvent(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    event_id: EVENT_ID,
    event_name: "runtime_observation",
    event_version: 1,
    occurred_at: OCCURRED_AT,
    surface: "daemon",
    operation: "liveness",
    outcome: "success",
    duration_bucket: "lt_100ms",
    properties: daemonSnapshotProperties(),
    ...overrides,
  };
}

export function daemonRunProperties(): Record<string, unknown> {
  return {
    start_mode: "auto",
    supervisor: "cli_autostart",
    trigger_command: "search",
  };
}

export function daemonSnapshotProperties(): Record<string, unknown> {
  return {
    ...daemonRunProperties(),
    history_freshness: "current",
    semantic_backlog_bucket: "0",
    semantic_coverage: "complete",
    retry_backoff: "none",
  };
}

export function daemonStorageProperties(): Record<string, unknown> {
  return {
    filesystem_total_bytes_bucket: "100gb-250gb",
    filesystem_available_bytes_bucket: "25gb-50gb",
    filesystem_available_fraction_bucket: "20pct-40pct",
    core_active_logical_bytes_bucket: "10gb-25gb",
    core_certified_source_bytes_bucket: "25gb-50gb",
    core_logical_amplification_bucket: "0_35x-0_50x",
    filesystem_available_to_active_core_ratio_bucket: "1_25x-2x",
  };
}

export function daemonCycleProperties(): Record<string, unknown> {
  return {
    ...daemonSnapshotProperties(),
    cycle_result: "no_work",
    coalesced_cycles_bucket: "2-5",
  };
}

export function mcpOperationEvent(
  operation: string,
  properties: Record<string, unknown>,
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    event_id: EVENT_ID,
    event_name: "operation_completed",
    event_version: 1,
    occurred_at: OCCURRED_AT,
    surface: "mcp",
    operation,
    outcome: "success",
    duration_bucket: "lt_100ms",
    properties,
    ...overrides,
  };
}

export function mcpRuntimeProperties(initialized: boolean): Record<string, unknown> {
  return {
    initialized,
    request_count_bucket: "2-5",
    tool_request_count_bucket: "1",
    tool_failure_count_bucket: "0",
    malformed_request_count_bucket: "0",
    ping_count_bucket: "1",
    tools_list_count_bucket: "1",
    initialized_notification_count_bucket: "1",
    unknown_notification_count_bucket: "0",
    telemetry_dropped_count_bucket: "0",
  };
}

export function mcpRuntimeEvent(
  operation: "initialized" | "stopped",
  properties: Record<string, unknown>,
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    event_id: EVENT_ID,
    event_name: "runtime_observation",
    event_version: 1,
    occurred_at: OCCURRED_AT,
    surface: "mcp",
    operation,
    outcome: "success",
    duration_bucket: "lt_1s",
    properties,
    ...overrides,
  };
}

export function proOperationEvent(
  operation: "lifecycle" | "materialize" | "query" | "status" | "blame",
  properties: Record<string, unknown>,
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    event_id: EVENT_ID,
    event_name: "operation_completed",
    event_version: 1,
    occurred_at: OCCURRED_AT,
    surface: "pro_host",
    operation,
    outcome: "success",
    duration_bucket: "lt_1s",
    properties,
    ...overrides,
  };
}

export function installStage(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    event_name: "install_stage",
    event_version: 1,
    install_attempt_id: INSTALL_ATTEMPT_ID,
    stage: "installer",
    status: "started",
    platform: "linux",
    arch: "x64",
    script_family: "posix",
    ...overrides,
  };
}

export function jsonRequest(
  path: string,
  body: unknown,
  headers: Record<string, string> = {},
): Request {
  return new Request(`https://api.example.test${path}`, {
    method: "POST",
    body: JSON.stringify(body),
    headers: { "content-type": "application/json; charset=utf-8", ...headers },
  });
}

export function workerHarness(options: {
  healthError?: Error;
  healthSnapshot?: {
    compatibilityRejectionMax: bigint;
    deliveryDegradedCount: bigint;
    deliveryDroppedCount: bigint;
    eventCollisionCount: bigint;
    otherRejectionCount: bigint;
    providerRefreshFailureCount: bigint;
  };
  rejectionError?: Error;
  rejectionPromise?: Promise<void>;
  telemetryError?: Error;
  telemetryPromise?: Promise<void>;
  blameReceiptError?: Error;
  blameReceiptPromise?: Promise<void>;
  installError?: Error;
  installPromise?: Promise<void>;
  queueError?: Error;
  queuePromise?: Promise<void>;
  queueHealthError?: Error;
  queueHealthy?: boolean;
  now?: Date;
} = {}) {
  const observeRejection = vi.fn((_observation: TelemetryRejectionObservation) => {});
  const recordIngestRejection = vi.fn(async (_rejection: TelemetryIngestRejection) => {
    if (options.rejectionError) throw options.rejectionError;
    await options.rejectionPromise;
  });
  const insertTelemetryRows = vi.fn(async (_rows: readonly TelemetryRow[]) => {
    if (options.telemetryError) throw options.telemetryError;
    await options.telemetryPromise;
  });
  const insertBlameProductReceipt = vi.fn(async (_receipt: BlameProductReceipt) => {
    if (options.blameReceiptError) throw options.blameReceiptError;
    await options.blameReceiptPromise;
  });
  const insertInstallStageRow = vi.fn(async (_row: InstallStageRow) => {
    if (options.installError) throw options.installError;
    await options.installPromise;
  });
  const readIngestHealthSnapshot = vi.fn(async () => {
    if (options.healthError) throw options.healthError;
    return options.healthSnapshot ?? {
      compatibilityRejectionMax: 0n,
      deliveryDegradedCount: 0n,
      deliveryDroppedCount: 0n,
      eventCollisionCount: 0n,
      otherRejectionCount: 0n,
      providerRefreshFailureCount: 0n,
    };
  });
  const database: TelemetryDatabase = {
    insertBlameProductReceipt,
    insertInstallStageRow,
    insertTelemetryRows,
    readIngestHealthSnapshot,
    recordIngestRejection,
  };
  const createDatabaseClient = vi.fn(() => database);
  const readQueueHealth = vi.fn(async () => {
    if (options.queueHealthError) throw options.queueHealthError;
    return options.queueHealthy ?? true;
  });
  const implementation = createTelemetryWorker({
    createDatabaseClient,
    now: () => options.now ?? NOW,
    observeRejection,
    readQueueHealth,
  });
  const queueBodies: Uint8Array<ArrayBuffer>[] = [];
  const queueBatches: Uint8Array<ArrayBuffer>[][] = [];
  const queueSendBatch = vi.fn(async (entries: Iterable<{
    readonly body: Uint8Array<ArrayBuffer>;
    readonly contentType: "bytes";
  }>) => {
    const batch = Array.from(entries);
    queueBatches.push(batch.map((entry) => entry.body));
    queueBodies.push(...batch.map((entry) => entry.body));
    if (options.queueError) throw options.queueError;
    await options.queuePromise;
  });
  const queue: TelemetryQueueProducer = { sendBatch: queueSendBatch };
  const worker = {
    fetch(request: Request, env: Env, context?: Parameters<typeof implementation.fetch>[2]) {
      return implementation.fetch(request, { ...env, TELEMETRY_INGEST_QUEUE: queue }, context);
    },
    queue(batch: Parameters<typeof implementation.queue>[0], env: Env) {
      return implementation.queue(batch, { ...env, TELEMETRY_INGEST_QUEUE: queue });
    },
    scheduled: implementation.scheduled,
  };
  return {
    createDatabaseClient,
    insertBlameProductReceipt,
    insertInstallStageRow,
    insertTelemetryRows,
    observeRejection,
    readIngestHealthSnapshot,
    readQueueHealth,
    recordIngestRejection,
    queueBatches,
    queueBodies,
    queueSendBatch,
    async queueMessages(): Promise<TelemetryQueueMessage[]> {
      const messages = await Promise.all(queueBodies.map(decodeTelemetryQueueMessage));
      if (messages.some((message) => message === null)) {
        throw new Error("invalid_test_queue_message");
      }
      return messages as TelemetryQueueMessage[];
    },
    worker,
  };
}

export function neonHarness(options: { transactionError?: unknown } = {}): {
  calls: [string, readonly unknown[]][];
  client: NeonQueryClient;
} {
  const calls: [string, readonly unknown[]][] = [];
  const query = async <T extends Record<string, unknown> = Record<string, unknown>>(
    sql: string,
    params: readonly (string | number | boolean | null)[] = [],
  ): Promise<T[]> => {
    calls.push([sql, params]);
    return [];
  };
  const client: NeonQueryClient = {
    query,
    async transaction(build) {
      if (options.transactionError) throw options.transactionError;
      return Promise.all(build({ query }));
    },
  };
  return { calls, client };
}

export function canonicalJson(value: unknown): string {
  const canonicalValue = (entry: unknown): unknown => {
    if (Array.isArray(entry)) return entry.map(canonicalValue);
    if (typeof entry === "object" && entry !== null) {
      return Object.fromEntries(
        Object.entries(entry as Record<string, unknown>)
          .sort(([left], [right]) => left.localeCompare(right))
          .map(([key, child]) => [key, canonicalValue(child)]),
      );
    }
    return entry;
  };
  return JSON.stringify(canonicalValue(value));
}

export function uuidV4FromFingerprint(fingerprint: string): string {
  const bytes = fingerprint.slice(0, 32).split("");
  bytes[12] = "4";
  bytes[16] = ["8", "9", "a", "b"][Number.parseInt(bytes[16], 16) % 4];
  const hex = bytes.join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
