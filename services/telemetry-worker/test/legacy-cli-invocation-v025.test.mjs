import { readFileSync } from "node:fs";

import { describe, expect, test } from "vitest";

import { createTelemetryWorker } from "../src/worker";
import { withCapturedQueue } from "./queue-capture.mjs";

const FIXTURE_ROOT = new URL("./fixtures/legacy-cli-invocation-v0.25/", import.meta.url);
const NOW = new Date("2026-07-22T18:35:00.000Z");
const ENV = {
  TELEMETRY_ANALYTICS_ENVIRONMENT: "production",
  TELEMETRY_DATABASE_URL: "postgresql://telemetry.example.test/db",
  TELEMETRY_IDENTITY_HMAC_KEY: "legacy-fixture-hmac-key-with-32-bytes-minimum",
  TELEMETRY_IDENTITY_KEY_VERSION: "1",
  TELEMETRY_RATE_LIMITER: { async limit() { return { success: true }; } },
};

const fixtures = new Map([
  ["setup-started.valid.json", "setup"],
  ["setup-completed.valid.json", "setup"],
  ["integrations-mcp-install.valid.json", "setup"],
  ["integrations-skills-status.valid.json", "status"],
  ["upgrade-background.valid.json", "automatic"],
].map(([name, activity]) => [name, { activity, batch: readFixture(name) }]));

const releasedActionCases = [
  ["sources", "status", {
    providers_detected_bucket: "2-5",
    providers_existing_bucket: "1",
    providers_importable_bucket: "1",
    ...autoUpgradeProperties(),
  }],
  ["import", "product_activity", {
    resume: false,
    all_sources: true,
    no_daemon: false,
    source_mode: "all_discovered",
    reset_cursor: false,
    progress_mode: "plain",
    sources_seen_bucket: "1",
    source_bytes_bucket: "lt_100kb",
    source_files_bucket: "2-5",
    failed_sources_bucket: "0",
    sessions_imported_bucket: "1",
    events_imported_bucket: "6-20",
    edges_imported_bucket: "0",
    skipped_bucket: "0",
    failed_bucket: "0",
    ...autoUpgradeProperties(),
  }],
  ["show", "product_value", {
    target_kind: "event",
    output_format: "jsonl",
    window_bucket: "2-5",
    events_returned_bucket: "2-5",
    ...autoUpgradeProperties(),
  }],
  ["locate", "product_value", {
    target_kind: "session",
    output_format: "json",
    provider_lookup: true,
    ...autoUpgradeProperties(),
  }],
  ["search", "product_value", {
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
    limit_bucket: "6-20",
    had_existing_store_before_search: true,
    indexed_content_before_search_known: true,
    had_indexed_content_before_search: true,
    refresh_duration_bucket: "lt_100ms",
    search_refresh_mode: "off",
    search_refresh_status: "skipped",
    search_refresh_source_count_bucket: "0",
    db_size_bucket: "100kb-1mb",
    store_created_by_search: false,
    indexed_sessions_bucket: "21-100",
    indexed_events_bucket: "101-1k",
    indexed_items_bucket: "101-1k",
    has_indexed_content_after_search: true,
    query_length_bucket: "21-100",
    query_term_count_bucket: "2-5",
    query_duration_bucket: "lt_100ms",
    search_backend_requested: "lexical",
    search_backend_effective: "lexical",
    result_count_bucket: "2-5",
    citation_count_bucket: "2-5",
    zero_result: false,
    render_duration_bucket: "lt_100ms",
    ...autoUpgradeProperties(),
  }],
  ["docs", "product_activity", {}],
  ["daemon", "automatic", {
    daemon_command: "run",
    once: true,
    force: false,
    start_mode: "auto",
    trigger_command: "search",
  }],
  ["doctor", "status", {
    finding_count_bucket: "1",
    ...autoUpgradeProperties(),
  }],
];

describe("released v0.25 cli_invocation compatibility", () => {
  test.each([...fixtures])("accepts released golden payload %s", async (_name, fixture) => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(jsonRequest(fixture.batch), ENV);

    expect(response.status).toBe(204);
    expect(harness.telemetryWrites).toHaveLength(1);
    expect(harness.telemetryWrites[0]).toHaveLength(1);
    expect(harness.telemetryWrites[0][0]).toMatchObject({
      activity_class: fixture.activity,
      app_version: "0.25.0",
      event_name: "cli_invocation",
      schema_version: null,
    });
    expect(harness.telemetryWrites[0][0].properties.action)
      .toBe(fixture.batch.events[0].properties.action);
  });

  test.each(releasedActionCases)("accepts released %s property vocabulary", async (
    action,
    activity,
    actionProperties,
  ) => {
    const harness = workerHarness();
    const batch = releasedBatch(action, actionProperties);

    const response = await harness.worker.fetch(jsonRequest(batch), ENV);

    expect(response.status).toBe(204);
    expect(harness.telemetryWrites[0][0]).toMatchObject({ activity_class: activity });
  });

  test("accepts the observed released import provider sidecar", async () => {
    const importProperties = structuredClone(
      releasedActionCases.find(([action]) => action === "import")[2],
    );
    importProperties.provider_filter = "codex";
    const harness = workerHarness();

    const response = await harness.worker.fetch(
      jsonRequest(releasedBatch("import", importProperties)),
      ENV,
    );

    expect(response.status, await response.text()).toBe(204);
    expect(harness.telemetryWrites[0][0].properties).toMatchObject({
      provider_filter: "codex",
      source_mode: "all_discovered",
    });
  });

  test("retains the exact released count and duration ceiling buckets", async () => {
    const searchProperties = structuredClone(
      releasedActionCases.find(([action]) => action === "search")[2],
    );
    searchProperties.indexed_sessions_bucket = "1k+";
    searchProperties.indexed_events_bucket = "1k+";
    searchProperties.indexed_items_bucket = "1k+";
    searchProperties.result_count_bucket = "1k+";
    const batch = releasedBatch("search", searchProperties);
    batch.events[0].duration_ms = 30_000;
    batch.events[0].duration_bucket = "gte_30s";
    const harness = workerHarness();

    const response = await harness.worker.fetch(jsonRequest(batch), ENV);

    expect(response.status).toBe(204);
    expect(harness.telemetryWrites[0][0]).toMatchObject({
      app_version: "0.25.0",
      duration_bucket: "gte_30s",
    });
    expect(harness.telemetryWrites[0][0].properties).toMatchObject({
      indexed_sessions_bucket: "1k+",
      result_count_bucket: "1k+",
    });
  });

  test.each([
    ["post-v0.25 count bucket", "indexed_sessions_bucket", "1k-10k"],
    ["post-v0.25 duration bucket", "query_duration_bucket", "lt_2m"],
  ])("rejects %s", async (_name, property, value) => {
    const searchProperties = structuredClone(
      releasedActionCases.find(([action]) => action === "search")[2],
    );
    searchProperties[property] = value;
    const harness = workerHarness();

    const response = await harness.worker.fetch(
      jsonRequest(releasedBatch("search", searchProperties)),
      ENV,
    );

    expect(response.status).toBe(422);
    expect(harness.telemetryWrites).toHaveLength(0);
  });

  test("accepts the released setup failure completion", async () => {
    const batch = releasedBatch("setup", {
      catalog_only: false,
      no_daemon: false,
      progress_mode: "none",
      setup_completed: false,
      setup_result: "failure",
      failure_kind: "command_error",
    }, { status: "error", success: false });
    const harness = workerHarness();

    const response = await harness.worker.fetch(jsonRequest(batch), ENV);

    expect(response.status).toBe(204);
    expect(harness.telemetryWrites[0][0]).toMatchObject({ activity_class: "setup", success: false });
  });

  test("accepts the bounded post-tag v0.25 import failure classification", async () => {
    const batch = releasedBatch("import", {
      resume: false,
      all_sources: true,
      no_daemon: false,
      source_mode: "all_discovered",
      reset_cursor: false,
      progress_mode: "plain",
      sources_seen_bucket: "1",
      source_bytes_bucket: "lt_100kb",
      import_outcome: "failure",
      import_failure_scope: "source",
      import_failure_type: "not_found",
      failure_kind: "command_error",
    }, { status: "error", success: false });
    const harness = workerHarness();

    const response = await harness.worker.fetch(jsonRequest(batch), ENV);

    expect(response.status, await response.text()).toBe(204);
    expect(harness.telemetryWrites[0][0].properties).toMatchObject({
      import_outcome: "failure",
      import_failure_scope: "source",
      import_failure_type: "not_found",
    });
  });

  test("accepts the bounded post-tag v0.25 completed import and setup sidecars", async () => {
    const importProperties = structuredClone(
      releasedActionCases.find(([action]) => action === "import")[2],
    );
    Object.assign(importProperties, {
      rejected_records_bucket: "0",
      import_outcome: "success",
      import_failure_scope: "none",
      import_failure_type: "none",
    });
    const setupBatch = mutate("setup-completed.valid.json", (batch) => {
      Object.assign(batch.events[0].properties, {
        rejected_records_bucket: "1",
        import_outcome: "completed_with_rejections",
        import_failure_scope: "record",
        import_failure_type: "record_rejection",
      });
    });
    const harness = workerHarness();

    const importResponse = await harness.worker.fetch(
      jsonRequest(releasedBatch("import", importProperties)),
      ENV,
    );
    const setupResponse = await harness.worker.fetch(jsonRequest(setupBatch), ENV);

    expect(importResponse.status, await importResponse.text()).toBe(204);
    expect(setupResponse.status, await setupResponse.text()).toBe(204);
    expect(harness.telemetryWrites).toHaveLength(2);
  });

  test("keeps the post-tag v0.25 transition enums closed", async () => {
    const batch = releasedBatch("import", {
      resume: false,
      all_sources: true,
      no_daemon: false,
      source_mode: "all_discovered",
      reset_cursor: false,
      progress_mode: "plain",
      import_outcome: "partial_success",
      import_failure_scope: "source",
      import_failure_type: "other",
      failure_kind: "command_error",
    }, { status: "error", success: false });
    const harness = workerHarness();

    const response = await harness.worker.fetch(jsonRequest(batch), ENV);

    expect(response.status).toBe(422);
    expect(harness.telemetryWrites).toHaveLength(0);
  });

  test("keeps the post-tag v0.25 transition fields scoped to setup and import", async () => {
    const searchProperties = structuredClone(
      releasedActionCases.find(([action]) => action === "search")[2],
    );
    searchProperties.import_outcome = "success";
    const harness = workerHarness();

    const response = await harness.worker.fetch(
      jsonRequest(releasedBatch("search", searchProperties)),
      ENV,
    );

    expect(response.status).toBe(422);
    expect(harness.telemetryWrites).toHaveLength(0);
  });

  test.each([
    ["unreleased status action", mutate("setup-completed.valid.json", (batch) => {
      batch.events[0].properties.action = "status";
    })],
    ["singular current-v1 integration action", mutate("integrations-mcp-install.valid.json", (batch) => {
      batch.events[0].properties.action = "integration";
    })],
    ["current-v1 setup field", mutate("setup-completed.valid.json", (batch) => {
      batch.events[0].properties.wait = false;
    })],
    ["missing setup completion", mutate("setup-completed.valid.json", (batch) => {
      delete batch.events[0].properties.setup_completed;
    })],
    ["current-v1 MCP target alias", mutate("integrations-mcp-install.valid.json", (batch) => {
      delete batch.events[0].properties.integration_name;
      batch.events[0].properties.integration_target = "mcp";
    })],
    ["current-v1 auto-upgrade channel", mutate("setup-completed.valid.json", (batch) => {
      delete batch.events[0].properties.upgrade_channel;
      batch.events[0].properties.auto_upgrade_channel = "stable";
    })],
    ["post-release deprecated-control sidecar", mutate("setup-started.valid.json", (batch) => {
      batch.events[0].properties.deprecated_control_ids = "CTX_DAEMON_OFF";
    })],
    ["nonreleased app version", mutate("setup-started.valid.json", (batch) => {
      batch.broker_app_version = "0.25.1";
      batch.events[0].app_version = "0.25.1";
    })],
    ["duration bucket mismatch", mutate("upgrade-background.valid.json", (batch) => {
      batch.events[0].duration_bucket = "lt_1s";
    })],
  ])("rejects %s", async (_name, batch) => {
    const harness = workerHarness();
    const response = await harness.worker.fetch(jsonRequest(batch), ENV);

    expect(response.status).toBe(422);
    expect(harness.telemetryWrites).toHaveLength(0);
  });

  test("validates every legacy event before writing the batch", async () => {
    const batch = structuredClone(fixtures.get("setup-started.valid.json").batch);
    const invalid = structuredClone(batch.events[0]);
    invalid.event_id = "018f1f2e-7b3c-7ac1-8def-0123456789ab";
    invalid.properties.action = "status";
    batch.events.push(invalid);
    const harness = workerHarness();

    const response = await harness.worker.fetch(jsonRequest(batch), ENV);

    expect(response.status).toBe(422);
    expect(harness.telemetryWrites).toHaveLength(0);
  });
});

function readFixture(name) {
  return JSON.parse(readFileSync(new URL(name, FIXTURE_ROOT), "utf8"));
}

function mutate(name, change) {
  const batch = readFixture(name);
  change(batch);
  return batch;
}

function autoUpgradeProperties() {
  return {
    auto_upgrade_probe: true,
    auto_upgrade_due: false,
    auto_upgrade_spawned: false,
    auto_upgrade_spawn_status: "env_disabled",
    upgrade_channel: "stable",
  };
}

function releasedBatch(action, actionProperties, eventOverrides = {}) {
  const batch = readFixture("setup-started.valid.json");
  const event = batch.events[0];
  const index = releasedActionCases.findIndex(([name]) => name === action);
  event.event_id = `018f1f2e-7b3c-7ad${index < 0 ? 8 : index}-8def-0123456789ab`;
  event.properties = {
    action,
    json_output: false,
    analytics_client: "ctx-cli",
    ...actionProperties,
  };
  Object.assign(event, eventOverrides);
  return batch;
}

function jsonRequest(body) {
  return new Request("https://api.example.test/functions/v1/telemetry", {
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
  const worker = withCapturedQueue(createTelemetryWorker({
    createDatabaseClient: () => database,
    now: () => NOW,
  }), { telemetryWrites });
  return { telemetryWrites, worker };
}
