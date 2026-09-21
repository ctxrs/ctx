import { readFileSync } from "node:fs";

import { describe, expect, test } from "vitest";

import { buildTelemetryIngestPlan } from "../src/telemetry-ingest";

const FIXTURE = JSON.parse(readFileSync(
  new URL(
    "./fixtures/public-telemetry-v1/upgrade_enable_precondition_failure.v1.2.4.valid.json",
    import.meta.url,
  ),
  "utf8",
));
const ENABLE_PRECONDITION_FAILURE = FIXTURE.event;
const INGEST_OPTIONS = {
  analyticsEnvironment: "staging",
  identityHmacKey: "upgrade-v124-hmac-key-with-32-bytes-minimum",
  identityKeyVersion: 1,
  now: () => new Date("2026-08-30T12:35:00.000Z"),
};

describe("ctx 1.2.4 upgrade telemetry compatibility", () => {
  test("pins the released producer source", () => {
    expect(FIXTURE.source).toEqual({
      repository: "ctxrs/ctx",
      commit: "64f673f144dc53e71bc903b3d81c528897257923",
      files: {
        "crates/ctx-cli/src/upgrade/command.rs":
          "d17d04edc92b3847e06fa5de1fe2290e55f69ad12c89bf2ee5c67256058aef51",
      },
    });
  });

  test("accepts a manual enable precondition failure without update_available", async () => {
    const { rows } = await buildTelemetryIngestPlan(
      batch(ENABLE_PRECONDITION_FAILURE),
      INGEST_OPTIONS,
    );

    expect(rows).toHaveLength(1);
    expect(rows[0].properties).toMatchObject({
      operation: "upgrade",
      outcome: "failure",
      upgrade_mode: "manual",
      upgrade_operation: "enable",
      upgrade_status: "failed",
      upgrade_applied: false,
      upgrade_scheduled: false,
      upgrade_failure_kind: "unmanaged_install",
    });
    expect(rows[0].properties).not.toHaveProperty("update_available");
  });

  test("accepts the other source-reachable early enable failure classification", async () => {
    const event = structuredClone(ENABLE_PRECONDITION_FAILURE);
    event.properties.upgrade_failure_kind = "apply_failed";

    const { rows } = await buildTelemetryIngestPlan(batch(event), INGEST_OPTIONS);
    expect(rows[0].properties.upgrade_failure_kind).toBe("apply_failed");
    expect(rows[0].properties).not.toHaveProperty("update_available");
  });

  test("keeps the typed producer shape compatible across patch versions", async () => {
    const { rows } = await buildTelemetryIngestPlan(
      batch(ENABLE_PRECONDITION_FAILURE, "1.2.5"),
      INGEST_OPTIONS,
    );
    expect(rows).toHaveLength(1);
  });

  test("rejects update_available=true on the enable precondition failure", async () => {
    const event = structuredClone(ENABLE_PRECONDITION_FAILURE);
    event.properties.update_available = true;

    await expect(buildTelemetryIngestPlan(batch(event), INGEST_OPTIONS))
      .rejects.toMatchObject({ code: "invalid_upgrade_shape" });
  });

  test.each(["status", "disable"])(
    "continues requiring update_available for manual %s failures",
    async (operation) => {
      const event = structuredClone(ENABLE_PRECONDITION_FAILURE);
      event.properties.upgrade_operation = operation;

      await expect(buildTelemetryIngestPlan(batch(event), INGEST_OPTIONS))
        .rejects.toMatchObject({ code: "invalid_upgrade_shape" });
    },
  );

  test.each([
    "lock_failed", "metadata_fetch", "signature_verify", "metadata_invalid",
    "artifact_verify", "artifact_download", "policy_disallowed",
  ])("rejects non-reachable sparse enable failure kind %s", async (failureKind) => {
    const event = structuredClone(ENABLE_PRECONDITION_FAILURE);
    event.properties.upgrade_failure_kind = failureKind;

    await expect(buildTelemetryIngestPlan(batch(event), INGEST_OPTIONS))
      .rejects.toMatchObject({ code: "invalid_upgrade_shape" });
  });

  test("continues requiring update_available for successful manual enable", async () => {
    const event = structuredClone(ENABLE_PRECONDITION_FAILURE);
    event.outcome = "success";
    event.properties.upgrade_status = "auto_enabled";
    delete event.properties.upgrade_failure_kind;

    await expect(buildTelemetryIngestPlan(batch(event), INGEST_OPTIONS))
      .rejects.toMatchObject({ code: "invalid_upgrade_shape" });
  });
});

function batch(event, appVersion = "1.2.4") {
  return {
    client_profile_id: "11111111-1111-4111-8111-111111111111",
    data_root_id: "22222222-2222-4222-8222-222222222222",
    app_version: appVersion,
    os: "linux",
    arch: "x86_64",
    events: [event],
  };
}
