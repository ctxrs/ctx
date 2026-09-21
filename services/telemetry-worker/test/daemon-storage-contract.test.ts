import { describe, expect, test } from "vitest";

import { buildTelemetryIngestPlan } from "../src/telemetry-ingest";
import {
  INGEST_OPTIONS,
  daemonCycleProperties,
  daemonRunProperties,
  daemonSnapshotProperties,
  daemonStorageProperties,
  mcpRuntimeEvent,
  mcpRuntimeProperties,
  runtimeEvent,
  v1Batch,
} from "./worker-test-fixtures";

describe("daemon storage telemetry contract", () => {
  test("accepts coherent daemon storage snapshots on ready and liveness", async () => {
    const ready = runtimeEvent({
      operation: "ready",
      properties: { ...daemonRunProperties(), ...daemonStorageProperties() },
    });
    const liveness = runtimeEvent({
      event_id: "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
      properties: { ...daemonSnapshotProperties(), ...daemonStorageProperties() },
    });
    const plan = await buildTelemetryIngestPlan(v1Batch([ready, liveness]), INGEST_OPTIONS);

    expect(plan.rows.map((row) => row.properties)).toEqual([
      { ...daemonRunProperties(), ...daemonStorageProperties(), operation: "ready", outcome: "success" },
      {
        ...daemonSnapshotProperties(),
        ...daemonStorageProperties(),
        operation: "liveness",
        outcome: "success",
      },
    ]);
  });

  test.each([
    ["partial filesystem snapshot", { filesystem_total_bytes_bucket: "100gb-250gb" }, "incomplete_filesystem_storage_snapshot"],
    ["partial Core snapshot", { core_active_logical_bytes_bucket: "10gb-25gb" }, "incomplete_core_storage_snapshot"],
    [
      "missing amplification",
      withoutKeys(daemonStorageProperties(), ["core_logical_amplification_bucket"]),
      "inconsistent_core_logical_amplification",
    ],
    [
      "amplification with zero source",
      { ...daemonStorageProperties(), core_certified_source_bytes_bucket: "0" },
      "inconsistent_core_logical_amplification",
    ],
    [
      "ratio with zero active Core",
      { ...daemonStorageProperties(), core_active_logical_bytes_bucket: "0" },
      "inconsistent_filesystem_available_to_active_core_ratio",
    ],
  ] as const)("rejects incoherent daemon storage: %s", async (_name, storage, code) => {
    await expect(buildTelemetryIngestPlan(v1Batch([
      runtimeEvent({ properties: { ...daemonSnapshotProperties(), ...storage } }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code });
  });

  test("accepts optional storage groups and unavailable computed ratios", async () => {
    const filesystemOnly = withoutKeys(daemonStorageProperties(), [
      "core_active_logical_bytes_bucket",
      "core_certified_source_bytes_bucket",
      "core_logical_amplification_bucket",
      "filesystem_available_to_active_core_ratio_bucket",
    ]);
    const coreWithZeroSource = {
      core_active_logical_bytes_bucket: "5gb-10gb",
      core_certified_source_bytes_bucket: "0",
    };
    const withoutOptionalAvailableRatio = withoutKeys(daemonStorageProperties(), [
      "filesystem_available_to_active_core_ratio_bucket",
    ]);
    const plan = await buildTelemetryIngestPlan(v1Batch([
      runtimeEvent({ properties: { ...daemonSnapshotProperties(), ...filesystemOnly } }),
      runtimeEvent({
        event_id: "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
        properties: { ...daemonSnapshotProperties(), ...coreWithZeroSource },
      }),
      runtimeEvent({
        event_id: "ffffffff-ffff-4fff-8fff-ffffffffffff",
        properties: { ...daemonSnapshotProperties(), ...withoutOptionalAvailableRatio },
      }),
    ]), INGEST_OPTIONS);

    expect(plan.rows).toHaveLength(3);
  });

  test("accepts every storage bucket vocabulary value", async () => {
    const storageBytes = [
      "0", "lt_100mb", "100mb-1gb", "1gb-5gb", "5gb-10gb", "10gb-25gb",
      "25gb-50gb", "50gb-100gb", "100gb-250gb", "250gb-500gb", "500gb-1tb",
      "1tb-2tb", "2tb-5tb", "5tb+",
    ];
    const availableFractions = [
      "0", "lt_5pct", "5pct-10pct", "10pct-20pct", "20pct-40pct", "40pct-60pct",
      "60pct+",
    ];
    const amplifications = [
      "lt_0_10x", "0_10x-0_25x", "0_25x-0_35x", "0_35x-0_50x", "0_50x-1x",
      "1x-2x", "2x+",
    ];
    const availableToCoreRatios = [
      "lt_0_5x", "0_5x-1x", "1x-1_25x", "1_25x-2x", "2x-4x", "4x+",
    ];
    const variants = [
      ...storageBytes.map((value) => ({ filesystem_total_bytes_bucket: value })),
      ...availableFractions.map((value) => ({ filesystem_available_fraction_bucket: value })),
      ...amplifications.map((value) => ({ core_logical_amplification_bucket: value })),
      ...availableToCoreRatios.map((value) => ({
        filesystem_available_to_active_core_ratio_bucket: value,
      })),
    ];

    for (const variant of variants) {
      const plan = await buildTelemetryIngestPlan(v1Batch([
        runtimeEvent({
          properties: {
            ...daemonSnapshotProperties(),
            ...daemonStorageProperties(),
            ...variant,
          },
        }),
      ]), INGEST_OPTIONS);
      expect(plan.rows).toHaveLength(1);
    }
  });

  test.each(["cycle", "stopped", "failed", "recovered"])(
    "rejects storage sidecars on daemon %s",
    async (operation) => {
      await expect(buildTelemetryIngestPlan(v1Batch([
        runtimeEvent({
          operation,
          properties: operation === "cycle"
            ? { ...daemonCycleProperties(), ...daemonStorageProperties() }
            : { ...daemonSnapshotProperties(), ...daemonStorageProperties() },
        }),
      ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "unknown_daemon_runtime_property" });
    },
  );

  test("rejects storage sidecars on MCP runtime observations", async () => {
    await expect(buildTelemetryIngestPlan(v1Batch([
      mcpRuntimeEvent("initialized", {
        ...mcpRuntimeProperties(true),
        ...daemonStorageProperties(),
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: "unknown_mcp_runtime_property" });
  });

  test.each([
    ["filesystem_total_bytes_bucket", "invalid"],
    ["filesystem_available_fraction_bucket", "7pct"],
    ["core_logical_amplification_bucket", "0.5x"],
    ["filesystem_available_to_active_core_ratio_bucket", "1.5x"],
  ])("rejects an invalid storage bucket for %s", async (key, value) => {
    await expect(buildTelemetryIngestPlan(v1Batch([
      runtimeEvent({
        properties: {
          ...daemonSnapshotProperties(),
          ...daemonStorageProperties(),
          [key]: value,
        },
      }),
    ]), INGEST_OPTIONS)).rejects.toMatchObject({ code: `invalid_${key}` });
  });
});

function withoutKeys(
  properties: Record<string, unknown>,
  keys: readonly string[],
): Record<string, unknown> {
  const copy = { ...properties };
  for (const key of keys) delete copy[key];
  return copy;
}
