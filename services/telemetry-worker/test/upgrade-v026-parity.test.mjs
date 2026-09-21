import { readFileSync } from "node:fs";

import { describe, expect, test } from "vitest";

import { hmacSha256Hex } from "../src/hash";
import { buildTelemetryIngestPlan } from "../src/telemetry-ingest";

const VALID = JSON.parse(readFileSync(
  new URL("./fixtures/public-telemetry-v1/upgrade-v026.valid.json", import.meta.url),
  "utf8",
));
const INVALID = JSON.parse(readFileSync(
  new URL("./fixtures/public-telemetry-v1/upgrade-v026.invalid.json", import.meta.url),
  "utf8",
));
const HMAC_KEY = "upgrade-parity-hmac-key-with-32-bytes-minimum";
const NOW = new Date("2026-07-26T12:35:00.000Z");
const OCCURRED_AT = "2026-07-26T12:34:00Z";
const INGEST_OPTIONS = {
  analyticsEnvironment: "staging",
  identityHmacKey: HMAC_KEY,
  identityKeyVersion: 1,
  now: () => NOW,
};

describe("ctx 0.26 upgrade telemetry parity", () => {
  test("pins the final producer files and every reachable terminal vocabulary", () => {
    expect(VALID.source).toEqual({
      repository: "ctxrs/ctx",
      commit: "5a4ccb39d0cb4016bef2d22c25f714dc4543c6f9",
      files: {
        "crates/ctx-cli/src/analytics/operation.rs":
          "79a50bf1201d57f5106faf282031e2e232fed112244dc85feeb761e3b94ca6a6",
        "crates/ctx-cli/src/analytics/product.rs":
          "90c84843faf1550d417da4e7e77134c6a2fc9d6425018b6b0c458be74e358387",
        "crates/ctx-cli/src/analytics/sender.rs":
          "c1847d5f051ff8ffe1aed5dd9166dee3e27bb9d2d302ddfadf10e63e5d318785",
        "crates/ctx-cli/src/upgrade/command.rs":
          "e1db9eaf842d524d57d3938c1fa7bebb844723ad07e09b002b249c3df934feed",
        "crates/ctx-cli/src/upgrade/command/daemon.rs":
          "548cc098fb7d95f80c839728e56bec7555665a1e9e5d13da082678fa3e7fe31e",
        "crates/ctx-cli/src/upgrade/state.rs":
          "46121dd6d616c03d953e9d218b20502f0ee5996f9549ac44d1b83686cf9a22b1",
      },
    });
    expect(VALID.variants).toHaveLength(40);
    expect(new Set(VALID.variants.map(({ outcome }) => outcome)))
      .toEqual(new Set(["success", "failure"]));
    expect(new Set(VALID.variants.map(({ properties }) => properties.upgrade_mode)))
      .toEqual(new Set(["manual", "auto"]));
    expect(new Set(VALID.variants.map(({ properties }) => properties.upgrade_operation)))
      .toEqual(new Set(["apply", "check", "status", "enable", "disable"]));
    expect(new Set(VALID.variants.map(({ properties }) => properties.upgrade_status)))
      .toEqual(new Set([
        "available", "up_to_date", "applied", "scheduled", "dry_run", "status_checked",
        "auto_enabled", "auto_disabled", "skipped", "failed",
      ]));
    expect(new Set(VALID.variants
      .map(({ properties }) => properties.upgrade_failure_kind)
      .filter(Boolean)))
      .toEqual(new Set([
        "lock_failed", "unmanaged_install", "metadata_fetch", "signature_verify",
        "metadata_invalid", "artifact_verify", "artifact_download", "policy_disallowed",
        "apply_failed",
      ]));
    expect(VALID.variants
      .filter(({ properties }) => properties.upgrade_mode === "auto")
      .every(({ properties }) => !Object.hasOwn(properties, "output")))
      .toBe(true);
  });

  test("accepts every reachable manual and automatic shape and hashes attempt identities", async () => {
    const events = VALID.variants.map((variant, index) => event(
      index,
      variant.outcome,
      variant.properties,
    ));
    const { rows } = await buildTelemetryIngestPlan(batch(events), INGEST_OPTIONS);

    expect(rows).toHaveLength(VALID.variants.length);
    for (const [index, variant] of VALID.variants.entries()) {
      const inputAttemptId = variant.properties.upgrade_attempt_id;
      const row = rows[index];
      expect(row.properties.operation, variant.name).toBe("upgrade");
      expect(row.properties.outcome, variant.name).toBe(variant.outcome);
      expect(row.properties.upgrade_attempt_id, variant.name).toBeUndefined();
      if (inputAttemptId) {
        expect(row.properties.upgrade_attempt_id_hash, variant.name).toBe(
          await hmacSha256Hex(
            HMAC_KEY,
            "ctx.telemetry.upgrade-attempt.v1",
            inputAttemptId,
          ),
        );
        expect(JSON.stringify(row), variant.name).not.toContain(inputAttemptId);
      } else {
        expect(row.properties.upgrade_attempt_id_hash, variant.name).toBeUndefined();
      }
    }
    expect(rows
      .filter(({ properties }) => properties.upgrade_mode === "auto")
      .every(({ activity_class }) => activity_class === "automatic"))
      .toBe(true);
  });

  test("rejects impossible combinations, cross-operation fields, and prose", async () => {
    const variants = new Map(VALID.variants.map((variant) => [variant.name, variant]));
    for (const [index, invalid] of INVALID.cases.entries()) {
      const base = variants.get(invalid.base);
      expect(base, invalid.name).toBeDefined();
      const properties = structuredClone(base.properties);
      Object.assign(properties, invalid.set ?? {});
      for (const key of invalid.delete ?? []) delete properties[key];

      await expect(
        buildTelemetryIngestPlan(
          batch([event(index, base.outcome, properties, invalid.operation ?? "upgrade")]),
          INGEST_OPTIONS,
        ),
        invalid.name,
      ).rejects.toMatchObject({ code: invalid.expected });
    }
  });
});

function batch(events) {
  return {
    client_profile_id: "11111111-1111-4111-8111-111111111111",
    data_root_id: "22222222-2222-4222-8222-222222222222",
    app_version: "0.26.0",
    os: "linux",
    arch: "x86_64",
    events,
  };
}

function event(index, outcome, properties, operation = "upgrade") {
  return {
    event_id: `30000000-0000-4000-8000-${String(index + 1).padStart(12, "0")}`,
    event_name: "operation_completed",
    event_version: 1,
    occurred_at: OCCURRED_AT,
    surface: "cli",
    operation,
    outcome,
    duration_bucket: "lt_5s",
    properties,
  };
}
