import { describe, expect, test } from "vitest";

import { createTelemetryWorker } from "../src/worker";
import { withCapturedQueue } from "./queue-capture.mjs";

const LEGACY_RELEASE_VERSIONS = Object.freeze(
  Array.from({ length: 25 }, (_, index) => `0.${index + 1}.0`),
);
const LEGACY_ENDPOINTS = Object.freeze([
  "/functions/v1/analytics",
  "/functions/v1/telemetry",
]);
const LEGACY_RELEASE_REPLAYS = Object.freeze(
  LEGACY_RELEASE_VERSIONS.flatMap((version) =>
    LEGACY_ENDPOINTS.map((endpoint) => ({ endpoint, version })),
  ),
);
const NOW = new Date("2026-07-23T12:00:00.000Z");
const INSTALL_ID = "123e4567-e89b-42d3-a456-426614174000";
const DEVICE_ID = "923e4567-e89b-42d3-a456-426614174000";
const ENV = {
  TELEMETRY_ANALYTICS_ENVIRONMENT: "production",
  TELEMETRY_DATABASE_URL: "postgresql://telemetry.example.test/db",
  TELEMETRY_IDENTITY_HMAC_KEY: "released-cli-hmac-key-with-32-bytes-minimum",
  TELEMETRY_IDENTITY_KEY_VERSION: "1",
  TELEMETRY_RATE_LIMITER: { async limit() { return { success: true }; } },
};

describe("released cli_invocation@1 family", () => {
  test("keeps the complete frozen 0.1.0 through 0.25.0 release range", () => {
    expect(Object.isFrozen(LEGACY_RELEASE_VERSIONS)).toBe(true);
    expect(LEGACY_RELEASE_VERSIONS).toHaveLength(25);
    expect(LEGACY_RELEASE_VERSIONS[0]).toBe("0.1.0");
    expect(LEGACY_RELEASE_VERSIONS.at(-1)).toBe("0.25.0");
    expect(new Set(LEGACY_RELEASE_VERSIONS).size).toBe(25);
  });

  test.each(LEGACY_RELEASE_REPLAYS)(
    "accepts representative telemetry from $version on $endpoint",
    async ({ endpoint, version }) => {
      const harness = workerHarness();
      const minor = releaseMinor(version);

      const response = await harness.worker.fetch(
        jsonRequest(endpoint, releasedBatch(version)),
        ENV,
      );

      expect(response.status).toBe(204);
      expect(harness.telemetryWrites).toHaveLength(1);
      expect(harness.telemetryWrites[0]).toHaveLength(1);
      const row = harness.telemetryWrites[0][0];
      expect(row).toMatchObject({
        activity_class: "product_value",
        app_version: version,
        event_name: "cli_invocation",
        schema_version: null,
        source: "ctx-cli",
        success: true,
        traffic_class: "unclassified_public",
      });
      expect(row.data_root_id_hash).toMatch(/^[0-9a-f]{64}$/u);
      expect(row.client_profile_id_hash === null).toBe(minor < 14);
      expect(row.properties).toMatchObject({
        action: "show",
        operation: "show",
        outcome: "success",
      });
    },
  );

  test("accepts the shipped install-attempt generation starting at 0.18.0", async () => {
    const harness = workerHarness();
    const batch = releasedBatch("0.18.0");
    batch.events[0].install_attempt_id = "ia_release_018";
    batch.events[0].properties.install_manager = "ctx-hosted-installer";

    const response = await harness.worker.fetch(jsonRequest(undefined, batch), ENV);

    expect(response.status).toBe(204);
    expect(harness.telemetryWrites[0][0].properties.install_attempt_id_hash)
      .toMatch(/^[0-9a-f]{64}$/u);
  });

  test("accepts the released long-running daemon duration wire range", async () => {
    const harness = workerHarness();
    const batch = releasedBatch("0.24.0", {
      action: "daemon",
      includeAutoUpgrade: false,
    });
    batch.events[0].duration_ms = 9_223_372_036_854_776_000;
    batch.events[0].duration_bucket = "gte_30s";

    const response = await harness.worker.fetch(jsonRequest(undefined, batch), ENV);

    expect(response.status).toBe(204);
  });

  test("accepts the released v0.25 capability snapshot", async () => {
    const harness = workerHarness();
    const batch = releasedBatch("0.25.0");
    Object.assign(batch.events[0].properties, {
      capability_snapshot_schema: 1,
      available_parallelism_bucket: "5-8",
      host_memory_bucket: "16-32gb",
      cpu_vector_tier: "avx2",
      acceleration_candidate: "not_detected",
    });

    const response = await harness.worker.fetch(jsonRequest(undefined, batch), ENV);

    expect(response.status).toBe(204);
    expect(harness.telemetryWrites[0][0].properties.capability_snapshot_schema).toBe(1);
  });

  test("accepts the released pre-v0.25 JSON-output auto-upgrade state", async () => {
    const harness = workerHarness();
    const batch = releasedBatch("0.24.0");
    batch.events[0].properties.json_output = true;
    batch.events[0].properties.auto_upgrade_spawn_status = "json_output";

    const response = await harness.worker.fetch(jsonRequest(undefined, batch), ENV);

    expect(response.status).toBe(204);
  });

  test("accepts historical failure telemetry without weakening failure semantics", async () => {
    const harness = workerHarness();
    const batch = releasedBatch("0.1.0");
    Object.assign(batch.events[0], { status: "error", success: false });
    batch.events[0].properties.failure_kind = "command_error";

    const response = await harness.worker.fetch(jsonRequest(undefined, batch), ENV);

    expect(response.status).toBe(204);
    expect(harness.telemetryWrites[0][0]).toMatchObject({
      activity_class: "product_activity",
      status: "failure",
      success: false,
    });
  });

  test("accepts the always-emitted list limit on a failed historical command", async () => {
    const harness = workerHarness();
    const batch = releasedBatch("0.1.0", {
      action: "list",
      properties: { limit_bucket: "21-100" },
      includeAutoUpgrade: false,
    });
    Object.assign(batch.events[0], { status: "error", success: false });
    batch.events[0].properties.failure_kind = "command_error";

    const response = await harness.worker.fetch(jsonRequest(undefined, batch), ENV);

    expect(response.status).toBe(204);
    expect(harness.telemetryWrites[0][0].properties.limit_bucket).toBe("21-100");
  });

  test.each([
    ["0.1.0", "status", {
      initialized: true,
      indexed_items_bucket: "21-100",
      indexed_sources_bucket: "2-5",
    }],
    ["0.1.0", "search", {
      has_repo_filter: true,
      result_count_bucket: "2-5",
    }],
    ["0.14.0", "search", {
      search_refresh_mode: "strict",
      search_refresh_status: "completed",
    }],
    ["0.16.0", "search", {
      has_provider_filter: true,
      provider_filter: "openclaw",
    }],
    ["0.6.0", "research", {
      has_query: true,
      has_provider_filter: false,
      has_workspace_filter: false,
      has_since_filter: false,
      has_event_type_filter: false,
      has_file_filter: false,
      primary_only: false,
      include_subagents: false,
      include_current_session: false,
      limit_bucket: "21-100",
      result_count_bucket: "2-5",
    }],
    ["0.11.0", "upgrade", {
      dry_run: false,
      background: false,
    }],
    ["0.18.0", "setup_started", {
      catalog_only: false,
      progress_mode: "auto",
    }],
    ["0.20.0", "skill", {
      skill_name: "ctx-agent-history-search",
      skill_action: "status",
      skill_scope: "global",
      target_agent_group: "detected",
      target_agents_count_bucket: "1",
    }],
    ["0.22.0", "integrations", {
      integration_action: "status",
      integration_name: "mcp",
      integration_scope: "global",
      target_agent_group: "detected",
      target_agents_count_bucket: "1",
    }],
    ["0.24.0", "daemon", {
      daemon_command: "run",
      once: true,
      force: false,
    }],
    ["0.24.0", "search", {
      has_provider_filter: true,
      provider_filter: "mimocode",
      search_refresh_mode: "background",
      search_refresh_status: "daemon_background",
    }],
  ])("accepts released historical vocabulary in %s %s", async (version, action, properties) => {
    const harness = workerHarness();
    const batch = releasedBatch(version, {
      action,
      properties,
    });

    const response = await harness.worker.fetch(jsonRequest(undefined, batch), ENV);

    expect(response.status).toBe(204);
    expect(harness.telemetryWrites[0][0].properties).toMatchObject({ action, ...properties });
  });
});

describe("legacy generation boundaries", () => {
  test.each([
    ["0.13 rejects future device identity", () => {
      const batch = releasedBatch("0.13.0");
      batch.broker_device_id = DEVICE_ID;
      batch.events[0].origin_device_id = DEVICE_ID;
      return batch;
    }],
    ["0.14 requires device identity", () => {
      const batch = releasedBatch("0.14.0");
      delete batch.broker_device_id;
      delete batch.events[0].origin_device_id;
      return batch;
    }],
    ["0.17 rejects future install correlation", () => {
      const batch = releasedBatch("0.17.0");
      batch.events[0].install_attempt_id = "ia_release_017";
      batch.events[0].properties.install_manager = "ctx-hosted-installer";
      return batch;
    }],
    ["0.24 rejects future capabilities", () => {
      const batch = releasedBatch("0.24.0");
      Object.assign(batch.events[0].properties, {
        capability_snapshot_schema: 1,
        available_parallelism_bucket: "5-8",
        host_memory_bucket: "16-32gb",
        cpu_vector_tier: "avx2",
        acceleration_candidate: "not_detected",
      });
      return batch;
    }],
    ["device identities must match", () => {
      const batch = releasedBatch("0.14.0");
      batch.events[0].origin_device_id = "823e4567-e89b-42d3-a456-426614174000";
      return batch;
    }],
    ["failure rows require the shipped failure kind", () => {
      const batch = releasedBatch("0.1.0");
      Object.assign(batch.events[0], { status: "error", success: false });
      return batch;
    }],
    ["failed historical list still requires its emitted limit", () => {
      const batch = releasedBatch("0.1.0", {
        action: "list",
        includeAutoUpgrade: false,
      });
      Object.assign(batch.events[0], { status: "error", success: false });
      batch.events[0].properties.failure_kind = "command_error";
      delete batch.events[0].properties.limit_bucket;
      return batch;
    }],
    ["0.17 rejects future setup_started", () =>
      releasedBatch("0.17.0", {
        action: "setup_started",
        properties: {},
        includeAutoUpgrade: false,
      })],
    ["0.21 rejects retired status telemetry", () =>
      releasedBatch("0.21.0", {
        action: "status",
        properties: {},
        includeAutoUpgrade: false,
      })],
    ["0.22 rejects retired skill action", () =>
      releasedBatch("0.22.0", {
        action: "skill",
        properties: {},
        includeAutoUpgrade: false,
      })],
    ["historical setup_started requires its emitted initial shape", () => {
      const batch = releasedBatch("0.18.0", {
        action: "setup_started",
        includeAutoUpgrade: false,
      });
      delete batch.events[0].properties.progress_mode;
      return batch;
    }],
    ["historical show variants cannot mix session and event fields", () => {
      const batch = releasedBatch("0.8.0");
      batch.events[0].properties.transcript_mode = "full";
      return batch;
    }],
    ["successful historical search requires its emitted result shape", () => {
      const batch = releasedBatch("0.14.0", {
        action: "search",
        includeAutoUpgrade: false,
      });
      delete batch.events[0].properties.query_duration_bucket;
      return batch;
    }],
    ["successful v0.6 research requires its emitted initial shape", () => {
      const batch = releasedBatch("0.6.0", {
        action: "research",
        includeAutoUpgrade: false,
      });
      delete batch.events[0].properties.include_current_session;
      return batch;
    }],
    ["successful v0.6 research requires its emitted result shape", () => {
      const batch = releasedBatch("0.6.0", {
        action: "research",
        includeAutoUpgrade: false,
      });
      delete batch.events[0].properties.result_count_bucket;
      return batch;
    }],
    ["successful v0.6 research requires the emitted non-empty query state", () => {
      const batch = releasedBatch("0.6.0", {
        action: "research",
        includeAutoUpgrade: false,
      });
      batch.events[0].properties.has_query = false;
      return batch;
    }],
    ["v0.11-v0.20 background upgrades were not emitted", () =>
      releasedBatch("0.20.0", {
        action: "upgrade",
        properties: { dry_run: false, background: true },
        includeAutoUpgrade: false,
      })],
    ["v0.25 rejects the retired JSON-output auto-upgrade state", () => {
      const batch = releasedBatch("0.25.0");
      batch.events[0].properties.json_output = true;
      batch.events[0].properties.auto_upgrade_spawn_status = "json_output";
      return batch;
    }],
    ["legacy batches contain exactly one emitted event", () => {
      const batch = releasedBatch("0.25.0");
      const second = structuredClone(batch.events[0]);
      second.event_id = "018f1f2e-7b3c-7ac1-8def-000000000026";
      batch.events.push(second);
      return batch;
    }],
  ])("%s", async (_name, makeBatch) => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(
      jsonRequest(undefined, makeBatch()),
      ENV,
    );
    expect(response.status).toBe(422);
    expect(harness.telemetryWrites).toHaveLength(0);
  });

  test.each([
    ["0.1 search rejects list-only result fields", "0.1.0", "search", {
      items_returned_bucket: "2-5",
    }],
    ["0.1 show rejects search-only fields", "0.1.0", "show", {
      has_query: true,
    }],
    ["0.13 search rejects future refresh fields", "0.13.0", "search", {
      search_refresh_mode: "strict",
    }],
    ["0.14 search rejects a future provider", "0.14.0", "search", {
      has_provider_filter: true,
      provider_filter: "openclaw",
    }],
    ["0.15 search rejects the future openclaw provider", "0.15.0", "search", {
      has_provider_filter: true,
      provider_filter: "openclaw",
    }],
    ["0.15 search rejects the future custom provider", "0.15.0", "search", {
      has_provider_filter: true,
      provider_filter: "custom",
    }],
    ["0.1 search rejects a non-provider source kind", "0.1.0", "search", {
      has_provider_filter: true,
      provider_filter: "shell",
    }],
    ["0.19 search rejects a future provider", "0.19.0", "search", {
      has_provider_filter: true,
      provider_filter: "kilo",
    }],
    ["0.23 search rejects a future provider", "0.23.0", "search", {
      has_provider_filter: true,
      provider_filter: "mimocode",
    }],
    ["0.23 search rejects the future background refresh mode", "0.23.0", "search", {
      search_refresh_mode: "background",
    }],
    ["0.24 search rejects the retired strict refresh mode", "0.24.0", "search", {
      search_refresh_mode: "strict",
    }],
    ["0.23 search rejects the future daemon refresh status", "0.23.0", "search", {
      search_refresh_status: "daemon_background",
    }],
    ["0.6 research rejects search-only fields", "0.6.0", "research", {
      has_session_filter: false,
    }],
    ["0.15 import rejects future cursor reset", "0.15.0", "import", {
      reset_cursor: false,
    }],
    ["0.15 import rejects future source modes", "0.15.0", "import", {
      source_mode: "explicit_format",
    }],
    ["0.19 rejects future skill properties", "0.19.0", "setup", {
      skill_name: "ctx-agent-history-search",
    }],
    ["0.20 rejects future integration properties", "0.20.0", "skill", {
      integration_action: "install",
    }],
    ["0.20 successful skill rejects the pre-resolution default target", "0.20.0", "skill", {
      target_agent_group: "default",
    }],
    ["0.23 rejects future daemon properties", "0.23.0", "setup", {
      daemon_command: "run",
    }],
    ["0.24 setup rejects daemon-only fields", "0.24.0", "setup", {
      daemon_command: "enable",
    }],
  ])("%s", async (_name, version, action, properties) => {
    const harness = workerHarness();
    const batch = releasedBatch(version, {
      action,
      properties,
      includeAutoUpgrade: false,
    });

    const response = await harness.worker.fetch(jsonRequest(undefined, batch), ENV);

    expect(response.status).toBe(422);
    expect(harness.telemetryWrites).toHaveLength(0);
  });

  test.each([
    "0.0.0", "0.25.1", "0.26.0", "0.25.0-rc.1", "1.0.0",
  ])("rejects unshipped version %s", async (version) => {
    const harness = workerHarness();
    const batch = releasedBatch("0.25.0");
    batch.broker_app_version = version;
    batch.events[0].app_version = version;
    const response = await harness.worker.fetch(jsonRequest(undefined, batch), ENV);
    expect(response.status).toBe(422);
    expect(harness.telemetryWrites).toHaveLength(0);
  });

  test.each([
    "prompt", "query_text", "transcript", "repository_url", "file_path", "raw_output",
  ])("rejects content-shaped property %s across the historical family", async (key) => {
    const harness = workerHarness();
    const batch = releasedBatch("0.1.0");
    batch.events[0].properties[key] = "private value";
    const response = await harness.worker.fetch(jsonRequest(undefined, batch), ENV);
    expect(response.status).toBe(422);
    expect(harness.telemetryWrites).toHaveLength(0);
  });

  test("validates the complete historical batch before writing", async () => {
    const harness = workerHarness();
    const batch = releasedBatch("0.1.0");
    const invalid = structuredClone(batch.events[0]);
    invalid.event_id = "018f1f2e-7b3c-7ac1-8def-000000000002";
    invalid.properties.raw_output = "private value";
    batch.events.push(invalid);

    const response = await harness.worker.fetch(jsonRequest(undefined, batch), ENV);

    expect(response.status).toBe(422);
    expect(harness.telemetryWrites).toHaveLength(0);
  });
});

function releasedBatch(version, options = {}) {
  const minor = releaseMinor(version);
  const action = options.action ?? "show";
  const includeAutoUpgrade = options.includeAutoUpgrade ??
    (minor >= 21 && new Set([
      "setup", "sources", "import", "show", "locate", "search", "skill", "integrations", "doctor",
    ]).has(action));
  const properties = {
    action,
    json_output: false,
    analytics_client: "ctx-cli",
    ...releasedActionProperties(minor, action),
    ...options.properties,
    ...(includeAutoUpgrade ? autoUpgradeProperties() : {}),
  };
  const event = {
    event_id: `018f1f2e-7b3c-7ac1-8def-${minor.toString(16).padStart(12, "0")}`,
    event_name: "cli_invocation",
    event_version: 1,
    occurred_at: "2026-07-23T12:00:00.000Z",
    plane: "product",
    delivery: "remote",
    origin_runtime: "cli",
    origin_install_id: INSTALL_ID,
    app_version: version,
    os: "linux",
    arch: "x86_64",
    surface: "cli",
    source: "ctx-cli",
    duration_ms: 50,
    duration_bucket: "lt_100ms",
    status: "ok",
    success: true,
    properties,
  };
  const batch = {
    broker_install_id: INSTALL_ID,
    broker_runtime: "cli",
    broker_app_version: version,
    broker_os: "linux",
    broker_arch: "x86_64",
    events: [event],
  };
  if (minor >= 14) {
    batch.broker_device_id = DEVICE_ID;
    event.origin_device_id = DEVICE_ID;
  }
  return batch;
}

function releasedActionProperties(minor, action) {
  if (action === "show") {
    return {
      target_kind: "event",
      output_format: "jsonl",
      window_bucket: "2-5",
      events_returned_bucket: "2-5",
    };
  }
  if (action === "status") {
    return {
      initialized: true,
      indexed_items_bucket: "21-100",
      indexed_sources_bucket: "2-5",
      cataloged_sessions_bucket: "21-100",
      ...(minor >= 14 ? {
        indexed_sessions_bucket: "21-100",
        indexed_events_bucket: "101-1k",
        db_size_bucket: "1mb-10mb",
      } : {}),
    };
  }
  if (action === "search") {
    return {
      has_query: true,
      has_provider_filter: false,
      ...(minor <= 5 ? {
        has_repo_filter: false,
      } : {
        has_workspace_filter: false,
        has_session_filter: false,
        event_results: false,
        include_current_session: false,
      }),
      has_since_filter: false,
      has_event_type_filter: false,
      has_file_filter: false,
      primary_only: false,
      include_subagents: false,
      limit_bucket: "21-100",
      ...(minor >= 21 ? {
        had_existing_store_before_search: true,
        indexed_content_before_search_known: true,
        had_indexed_content_before_search: true,
        store_created_by_search: false,
        has_indexed_content_after_search: true,
      } : {}),
      ...(minor >= 14 ? {
        refresh_duration_bucket: "lt_100ms",
        search_refresh_mode: minor >= 24 ? "background" : "auto",
        search_refresh_status: "completed",
        search_refresh_source_count_bucket: "2-5",
        db_size_bucket: "1mb-10mb",
        indexed_sessions_bucket: "21-100",
        indexed_events_bucket: "101-1k",
        indexed_items_bucket: "101-1k",
        query_length_bucket: "1-20",
        query_term_count_bucket: "1",
        query_duration_bucket: "lt_100ms",
        result_count_bucket: "2-5",
        citation_count_bucket: "2-5",
        zero_result: false,
        render_duration_bucket: "lt_100ms",
      } : {
        result_count_bucket: "2-5",
        citation_count_bucket: "2-5",
      }),
      ...(minor >= 24 ? {
        search_backend_requested: "lexical",
        search_backend_effective: "lexical",
      } : {}),
    };
  }
  if (action === "research") {
    return {
      has_query: true,
      has_provider_filter: false,
      has_workspace_filter: false,
      has_since_filter: false,
      has_event_type_filter: false,
      has_file_filter: false,
      primary_only: false,
      include_subagents: false,
      include_current_session: false,
      limit_bucket: "21-100",
      result_count_bucket: "2-5",
    };
  }
  if (action === "list") {
    return {
      limit_bucket: "21-100",
      items_returned_bucket: "2-5",
    };
  }
  if (action === "import") {
    return {
      resume: false,
      all_sources: true,
      ...(minor >= 24 ? { no_daemon: false } : {}),
      source_mode: "all_discovered",
      ...(minor >= 16 ? { reset_cursor: false } : {}),
      progress_mode: "auto",
      sources_seen_bucket: "2-5",
      source_bytes_bucket: "1mb-10mb",
      source_files_bucket: "2-5",
      failed_sources_bucket: "0",
      sessions_imported_bucket: "21-100",
      events_imported_bucket: "101-1k",
      edges_imported_bucket: "101-1k",
      skipped_bucket: "0",
      failed_bucket: "0",
    };
  }
  if (action === "setup_started") {
    return {
      catalog_only: false,
      ...(minor >= 24 ? { no_daemon: false } : {}),
      progress_mode: "auto",
    };
  }
  if (action === "skill") {
    return {
      skill_name: "ctx-agent-history-search",
      skill_action: "status",
      skill_scope: "global",
      target_agent_group: "detected",
      target_agents_count_bucket: "1",
      status_result: "all_current",
      current_targets_bucket: "1",
    };
  }
  if (action === "integrations") {
    return {
      integration_action: "status",
      integration_name: "mcp",
      integration_scope: "global",
      target_agent_group: "detected",
      target_agents_count_bucket: "1",
    };
  }
  if (action === "daemon") {
    return { daemon_command: "run", once: true, force: false };
  }
  return {};
}

function autoUpgradeProperties() {
  return {
    auto_upgrade_probe: true,
    auto_upgrade_due: false,
    auto_upgrade_spawned: false,
    auto_upgrade_spawn_status: "auto_disabled",
    upgrade_channel: "stable",
  };
}

function releaseMinor(version) {
  return Number.parseInt(version.split(".")[1], 10);
}

function jsonRequest(path = "/functions/v1/telemetry", body) {
  return new Request(`https://api.example.test${path}`, {
    method: "POST",
    body: JSON.stringify(body),
    headers: { "content-type": "application/json; charset=utf-8" },
  });
}

function workerHarness() {
  const telemetryWrites = [];
  const database = {
    async insertTelemetryRows(rows) {
      telemetryWrites.push(rows);
    },
    async insertInstallStageRow() {},
  };
  return {
    telemetryWrites,
    worker: withCapturedQueue(createTelemetryWorker({
      createDatabaseClient: () => database,
      now: () => NOW,
    }), { telemetryWrites }),
  };
}
