import { describe, expect, test } from "vitest";

import { buildTelemetryIngestPlan } from "../src/telemetry-ingest";
import {
  INGEST_OPTIONS,
  OCCURRED_AT,
  providerRefreshEvent,
  v1Batch,
} from "./worker-test-fixtures";

describe("privacy-safe observability contracts", () => {
  test("preserves only closed provider failure codes and retryability", async () => {
    const failure = providerRefreshEvent({
      outcome: "failure",
      properties: {
        ...(providerRefreshEvent().properties as Record<string, unknown>),
        refresh_result: "failure",
        core_result: "failure",
        failure_scope: "system",
        failure_type: "system",
        failure_code: "index_corruption",
        retryable: false,
      },
    });
    const plan = await buildTelemetryIngestPlan(v1Batch([failure]), INGEST_OPTIONS);

    expect(plan.rows[0].properties).toMatchObject({
      failure_code: "index_corruption",
      retryable: false,
    });

    const incomplete = structuredClone(failure);
    delete (incomplete.properties as Record<string, unknown>).retryable;
    await expect(buildTelemetryIngestPlan(v1Batch([incomplete]), INGEST_OPTIONS))
      .rejects.toMatchObject({ code: "incomplete_provider_failure_diagnostics" });

    const inconsistent = structuredClone(failure);
    (inconsistent.properties as Record<string, unknown>).failure_code = "none";
    await expect(buildTelemetryIngestPlan(v1Batch([inconsistent]), INGEST_OPTIONS))
      .rejects.toMatchObject({ code: "inconsistent_provider_failure_code" });

    const openCode = structuredClone(failure);
    (openCode.properties as Record<string, unknown>).failure_code = "raw_error";
    await expect(buildTelemetryIngestPlan(v1Batch([openCode]), INGEST_OPTIONS))
      .rejects.toMatchObject({ code: "invalid_failure_code" });
  });

  test("rejects content-shaped or inconsistent analytics delivery observations", async () => {
    const delivery = {
      event_id: "965515a7-07f9-4b1f-91ba-73bda3a0c7cf",
      event_name: "analytics_delivery_observation",
      event_version: 1,
      occurred_at: OCCURRED_AT,
      surface: "cli",
      operation: "outbox",
      outcome: "failure",
      duration_bucket: "unknown",
      properties: {
        queued_count_bucket: "2-5",
        retry_attempt_count_bucket: "2-5",
        dropped_count_bucket: "1",
        oldest_queued_age_bucket: "lt_10m",
        failure_class: "transport",
      },
    };
    await expect(buildTelemetryIngestPlan(v1Batch([delivery]), INGEST_OPTIONS))
      .resolves.toMatchObject({
        rows: [{ event_name: "analytics_delivery_observation", activity_class: "operational" }],
      });

    const contentShaped = structuredClone(delivery);
    (contentShaped.properties as Record<string, unknown>).endpoint =
      "https://private.example.test/path";
    await expect(buildTelemetryIngestPlan(v1Batch([contentShaped]), INGEST_OPTIONS))
      .rejects.toMatchObject({ code: "unknown_analytics_delivery_property" });

    const inconsistent = structuredClone(delivery);
    inconsistent.outcome = "success";
    await expect(buildTelemetryIngestPlan(v1Batch([inconsistent]), INGEST_OPTIONS))
      .rejects.toMatchObject({ code: "inconsistent_analytics_delivery_outcome" });
  });
});
