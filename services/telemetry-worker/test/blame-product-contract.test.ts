import { describe, expect, test } from "vitest";

import { BLAME_FAILURE_CLASSES } from "../src/blame-product-contract";
import { buildTelemetryIngestPlan, TelemetryIngestError } from "../src/telemetry-ingest";
import MINIMAL_FIXTURE from "./fixtures/blame-product-v1/minimal-current.valid.json";

const OPTIONS = {
  analyticsEnvironment: "production" as const,
  identityHmacKey: "focused-blame-product-health-test-key",
  identityKeyVersion: 7,
  now: () => new Date("2026-08-25T15:42:45Z"),
};

describe("focused Blame terminal contract", () => {
  test.each([
    ["proven", "1"],
    ["possible", "2-5"],
    ["conflicting", "6-20"],
    ["none", "0"],
  ])("counts schema V2 %s/%s as product value", async (state, countBucket) => {
    const payload = v2Fixture();
    properties(payload).blame_result_state = state;
    properties(payload).blame_result_count_bucket = countBucket;
    const plan = await buildTelemetryIngestPlan(payload, OPTIONS);

    expect(plan.rows).toHaveLength(1);
    expect(plan.rows[0]).toMatchObject({
      activity_class: "product_value",
      app_version: "1.2.3",
      arch: "x86_64",
      duration_bucket: "lt_1s",
      os: "linux",
      properties: expect.objectContaining({
        blame_result_count_bucket: countBucket,
        blame_result_state: state,
        blame_schema_version: 2,
      }),
    });
  });

  test.each(["cli", "mcp"])("admits schema V2 surface %s", async (surface) => {
    const payload = v2Fixture();
    properties(payload).blame_surface = surface;
    const plan = await buildTelemetryIngestPlan(payload, OPTIONS);

    expect(plan.rows[0]?.properties).toMatchObject({
      blame_surface: surface,
      blame_target_kind: "file",
      blame_request_kind: "first_request",
      blame_query_duration_bucket: "lt_1s",
    });
  });

  test.each(["setup", "query", "presentation"])(
    "admits schema V2 failure phase %s",
    async (failurePhase) => {
      const payload = v2FailureFixture();
      properties(payload).blame_failure_phase = failurePhase;
      if (failurePhase === "setup") {
        delete properties(payload).blame_query_duration_bucket;
      }
      const plan = await buildTelemetryIngestPlan(payload, OPTIONS);

      expect(plan.rows[0]).toMatchObject({
        activity_class: "product_activity",
        status: "failure",
        properties: expect.objectContaining({
          blame_failure_class: "repository",
          blame_failure_phase: failurePhase,
          blame_schema_version: 2,
        }),
      });
    },
  );

  test.each([
    ["semantics version", "blame_semantics_version", 2],
    ["access", "blame_access_state", "active"],
    ["delivery", "blame_output_served", true],
    ["Pro version", "blame_pro_version", "1.2.3"],
    ["protocol version", "blame_pro_protocol_version", 3],
    ["target value", "blame_target_value", "src/private.rs"],
    ["repository", "blame_repository", "private"],
    ["cursor", "blame_cursor", "opaque"],
    ["content", "blame_content", "private"],
    ["identifier", "blame_session_id", "session"],
    ["hash", "blame_target_hash", "abc123"],
  ] as const)("rejects schema V2 %s", async (_name, key, value) => {
    const payload = v2Fixture();
    properties(payload)[key] = value;
    await expect(buildTelemetryIngestPlan(payload, OPTIONS)).rejects.toMatchObject({
      code: "unknown_blame_product_property",
    });
  });

  test.each([
    ["blame_surface", "daemon", "invalid_blame_surface"],
    ["blame_target_kind", "repository", "invalid_blame_target_kind"],
    ["blame_request_kind", "retry", "invalid_blame_request_kind"],
    ["blame_query_duration_bucket", "unknown", "invalid_blame_query_duration_bucket"],
    ["blame_result_state", "failure", "invalid_blame_result_state"],
    ["blame_result_count_bucket", "1k+", "invalid_blame_result_count_bucket"],
    ["blame_freshness", "stale", "invalid_blame_freshness"],
  ])("rejects schema V2 %s outside its closed enum", async (key, value, code) => {
    const payload = v2Fixture();
    properties(payload)[key] = value;
    await expect(buildTelemetryIngestPlan(payload, OPTIONS)).rejects.toMatchObject({ code });
  });

  test.each([
    "blame_query_duration_bucket",
    "blame_result_state",
    "blame_result_count_bucket",
    "blame_freshness",
    "blame_has_more",
  ])("requires schema V2 success field %s", async (key) => {
    const payload = v2Fixture();
    delete properties(payload)[key];
    await expect(buildTelemetryIngestPlan(payload, OPTIONS)).rejects.toBeInstanceOf(
      TelemetryIngestError,
    );
  });

  test("rejects schema V2 success/failure union contradictions", async () => {
    const success = v2Fixture();
    Object.assign(properties(success), {
      blame_failure_class: "repository",
      blame_failure_phase: "query",
    });
    await expect(buildTelemetryIngestPlan(success, OPTIONS)).rejects.toMatchObject({
      code: "contradictory_blame_success",
    });

    const failure = v2FailureFixture();
    Object.assign(properties(failure), {
      blame_result_state: "none",
      blame_result_count_bucket: "0",
    });
    await expect(buildTelemetryIngestPlan(failure, OPTIONS)).rejects.toMatchObject({
      code: "contradictory_blame_failure",
    });
  });

  test.each(["blame_failure_class", "blame_failure_phase"])(
    "requires schema V2 failure field %s",
    async (key) => {
      const payload = v2FailureFixture();
      delete properties(payload)[key];
      await expect(buildTelemetryIngestPlan(payload, OPTIONS)).rejects.toBeInstanceOf(
        TelemetryIngestError,
      );
    },
  );

  test("requires query timing after setup succeeds", async () => {
    const payload = v2FailureFixture();
    delete properties(payload).blame_query_duration_bucket;
    await expect(buildTelemetryIngestPlan(payload, OPTIONS)).rejects.toMatchObject({
      code: "invalid_blame_query_duration_bucket",
    });
  });

  test("rejects work timing on a setup failure", async () => {
    const payload = v2FailureFixture();
    properties(payload).blame_failure_phase = "setup";
    await expect(buildTelemetryIngestPlan(payload, OPTIONS)).rejects.toMatchObject({
      code: "contradictory_blame_failure_phase",
    });
  });

  test("does not carry the retired output failure class into schema V2", async () => {
    const payload = v2FailureFixture();
    properties(payload).blame_failure_class = "output";
    await expect(buildTelemetryIngestPlan(payload, OPTIONS)).rejects.toMatchObject({
      code: "invalid_blame_failure_class",
    });
  });

  test.each(["proven", "possible", "conflicting", "none"])(
    "counts delivered %s as product value",
    async (state) => {
      const payload = fixture();
      properties(payload).blame_result_state = state;
      const plan = await buildTelemetryIngestPlan(payload, OPTIONS);

      expect(plan.rows).toHaveLength(1);
      expect(plan.rows[0]?.activity_class).toBe("product_value");
      expect(plan.rows[0]?.properties.blame_result_state).toBe(state);
    },
  );

  test("rejects the contradictory computed-but-unserved success shape", async () => {
    const payload = fixture();
    properties(payload).blame_output_served = false;
    await expect(buildTelemetryIngestPlan(payload, OPTIONS)).rejects.toMatchObject({
      code: "contradictory_blame_success",
    });
  });

  test.each([true, false, undefined])("rejects deferred MCP V1 with delivery %s", async (served) => {
    const payload = fixture();
    properties(payload).blame_surface = "mcp";
    if (served === undefined) delete properties(payload).blame_output_served;
    else properties(payload).blame_output_served = served;
    await expect(buildTelemetryIngestPlan(payload, OPTIONS)).rejects.toMatchObject({
      code: "invalid_blame_surface",
    });
  });

  test.each(["success", "failure"])("requires CLI delivery on %s", async (outcome) => {
    const payload = outcome === "success" ? fixture() : failureFixture();
    delete properties(payload).blame_output_served;
    await expect(buildTelemetryIngestPlan(payload, OPTIONS)).rejects.toMatchObject({
      code: "missing_blame_output_served",
    });
  });

  test("requires a stable failure class and forbids served or success-only failure facts", async () => {
    const payload = fixture();
    event(payload).outcome = "failure";
    event(payload).properties = {
      blame_schema_version: 1,
      blame_semantics_version: 1,
      blame_surface: "cli",
      blame_target_kind: "commit",
      blame_request_kind: "continuation",
      blame_failure_class: "output",
      blame_output_served: false,
    };
    const plan = await buildTelemetryIngestPlan(payload, OPTIONS);
    expect(plan.rows[0]?.activity_class).toBe("product_activity");
    expect(plan.rows[0]?.properties.blame_failure_class).toBe("output");

    for (const mutation of [
      { blame_failure_class: undefined },
      { blame_output_served: true },
      { blame_result_state: "none" },
    ]) {
      const invalid = structuredClone(payload);
      const invalidProperties = properties(invalid);
      Object.assign(invalidProperties, mutation);
      if (mutation.blame_failure_class === undefined) delete invalidProperties.blame_failure_class;
      await expect(buildTelemetryIngestPlan(invalid, OPTIONS)).rejects.toBeInstanceOf(
        TelemetryIngestError,
      );
    }
  });

  test.each(BLAME_FAILURE_CLASSES)("admits the exact failure class %s", async (failureClass) => {
    const payload = failureFixture();
    properties(payload).blame_failure_class = failureClass;
    const plan = await buildTelemetryIngestPlan(payload, OPTIONS);
    expect(plan.rows[0]?.properties.blame_failure_class).toBe(failureClass);
  });

  test.each(["file", "commit", "pull_request"])(
    "admits and preserves target kind %s without a target value",
    async (targetKind) => {
      const payload = fixture();
      properties(payload).blame_target_kind = targetKind;
      const plan = await buildTelemetryIngestPlan(payload, OPTIONS);
      expect(plan.rows[0]?.properties.blame_target_kind).toBe(targetKind);
      expect(JSON.stringify(plan.rows[0]?.properties)).not.toMatch(
        /target_(?:value|path|ref|hash|selector)/u,
      );
    },
  );

  test.each([undefined, "repository", "pull-request", 1])(
    "rejects missing or invalid target kind %s",
    async (targetKind) => {
      const payload = fixture();
      if (targetKind === undefined) delete properties(payload).blame_target_kind;
      else properties(payload).blame_target_kind = targetKind;
      await expect(buildTelemetryIngestPlan(payload, OPTIONS)).rejects.toMatchObject({
        code: "invalid_blame_target_kind",
      });
    },
  );

  test.each([
    "entitlement", "config", "spawn", "timeout", "transport", "render", "unknown",
  ])("rejects the non-contract failure class %s", async (failureClass) => {
    const payload = failureFixture();
    properties(payload).blame_failure_class = failureClass;
    await expect(buildTelemetryIngestPlan(payload, OPTIONS)).rejects.toMatchObject({
      code: "invalid_blame_failure_class",
    });
  });

  test.each([
    ["unknown field", { target_kind: "file" }],
    ["target value", { blame_target_value: "src/private.rs" }],
    ["raw path", { path: "/private/repository" }],
    ["result count", { blame_result_count_bucket: "0" }],
    ["Core version", { blame_core_version: "1.2.3" }],
    ["compatibility claim", { blame_protocol_compatibility: "compatible" }],
    ["failure on success", { blame_failure_class: "other" }],
    ["incompatible semantics", { blame_semantics_version: 2 }],
    ["schema V2 timing", { blame_query_duration_bucket: "lt_1s" }],
  ])("rejects %s", async (_name, mutation) => {
    const payload = fixture();
    Object.assign(properties(payload), mutation);
    await expect(buildTelemetryIngestPlan(payload, OPTIONS)).rejects.toBeInstanceOf(
      TelemetryIngestError,
    );
  });

  test("keeps optional exact Pro identity separate from the envelope app version", async () => {
    const payload = fixture();
    payload.app_version = "9.8.7";
    const plan = await buildTelemetryIngestPlan(payload, OPTIONS);
    expect(plan.rows[0]?.app_version).toBe("9.8.7");
    expect(plan.rows[0]?.properties.blame_pro_version).toBe("1.1.0");
    expect(plan.rows[0]?.properties.blame_pro_protocol_version).toBe(3);

    const omitted = fixture();
    delete properties(omitted).blame_pro_version;
    delete properties(omitted).blame_pro_protocol_version;
    await expect(buildTelemetryIngestPlan(omitted, OPTIONS)).resolves.toBeDefined();
  });

  test("preserves the producer's bounded exact semver suffixes", async () => {
    const payload = fixture();
    payload.app_version = "1.1.0-rc.2+build.7";
    properties(payload).blame_pro_version = "1.1.0-rc.2+build.7";
    const plan = await buildTelemetryIngestPlan(payload, OPTIONS);
    expect(plan.rows[0]?.app_version).toBe("1.1.0-rc.2+build.7");
    expect(plan.rows[0]?.properties.blame_pro_version).toBe("1.1.0-rc.2+build.7");
  });

  test.each([
    "1.1", "01.1.0", "1.1.0-", "1.1.0-rc..2", "1.1.0-01",
    "1.1.0+", "1.1.0+build_7", "1.1.0+build+7",
  ])("rejects the non-exact Pro version %s", async (version) => {
    const payload = fixture();
    properties(payload).blame_pro_version = version;
    await expect(buildTelemetryIngestPlan(payload, OPTIONS)).rejects.toMatchObject({
      code: "invalid_blame_pro_version",
    });
  });

  test("rejects a non-exact current Blame envelope app version", async () => {
    const payload = fixture();
    payload.app_version = "1.1.0+build_7";
    await expect(buildTelemetryIngestPlan(payload, OPTIONS)).rejects.toMatchObject({
      code: "invalid_app_version",
    });
  });

  test("freezes the exact Pro protocol field bytes", () => {
    const bytes = JSON.stringify(MINIMAL_FIXTURE);
    expect(bytes).toContain('"blame_pro_protocol_version"');
    expect(bytes).not.toMatch(/"blame_protocol_version"/u);
  });

  test("makes identical current replay idempotent and changed-body reuse a collision", async () => {
    const payload = fixture();
    const replay = fixture();
    event(replay).properties = Object.fromEntries(
      Object.entries(properties(replay)).reverse(),
    );
    const first = await buildTelemetryIngestPlan(payload, OPTIONS);
    const identical = await buildTelemetryIngestPlan(replay, OPTIONS);
    expect(identical.rows[0]?.payload_fingerprint).toBe(first.rows[0]?.payload_fingerprint);

    const sameBatch = fixture();
    sameBatch.events = [event(payload), event(replay)];
    await expect(buildTelemetryIngestPlan(sameBatch, OPTIONS)).resolves.toMatchObject({
      rows: [expect.objectContaining({ event_id: event(payload).event_id })],
    });

    const collision = fixture();
    properties(collision).blame_has_more = true;
    const collisionBatch = fixture();
    collisionBatch.events = [event(payload), event(collision)];
    await expect(buildTelemetryIngestPlan(collisionBatch, OPTIONS)).rejects.toMatchObject({
      code: "event_id_collision",
      status: 409,
    });
  });

  test("preserves the released legacy Blame admission and value behavior", async () => {
    const payload = fixture();
    event(payload).properties = {
      blame_target_kind: "file",
      blame_surface: "cli",
      blame_result_count_bucket: "1",
      blame_has_more: false,
    };
    const plan = await buildTelemetryIngestPlan(payload, OPTIONS);
    expect(plan.rows[0]?.activity_class).toBe("product_value");
    expect(plan.rows[0]?.properties.blame_schema_version).toBeUndefined();
  });

});

function fixture(): Record<string, unknown> {
  return structuredClone(MINIMAL_FIXTURE) as Record<string, unknown>;
}

function failureFixture(): Record<string, unknown> {
  const payload = fixture();
  event(payload).outcome = "failure";
  event(payload).properties = {
    blame_schema_version: 1,
    blame_semantics_version: 1,
    blame_surface: "cli",
    blame_target_kind: "file",
    blame_request_kind: "first_request",
    blame_failure_class: "other",
    blame_output_served: false,
    blame_pro_version: "1.1.0",
    blame_pro_protocol_version: 3,
  };
  return payload;
}

function v2Fixture(): Record<string, unknown> {
  const payload = fixture();
  event(payload).properties = {
    blame_schema_version: 2,
    blame_surface: "cli",
    blame_target_kind: "file",
    blame_request_kind: "first_request",
    blame_query_duration_bucket: "lt_1s",
    blame_result_state: "possible",
    blame_result_count_bucket: "2-5",
    blame_freshness: "current",
    blame_has_more: false,
  };
  return payload;
}

function v2FailureFixture(): Record<string, unknown> {
  const payload = v2Fixture();
  event(payload).outcome = "failure";
  event(payload).properties = {
    blame_schema_version: 2,
    blame_surface: "mcp",
    blame_target_kind: "commit",
    blame_request_kind: "continuation",
    blame_query_duration_bucket: "lt_5s",
    blame_failure_class: "repository",
    blame_failure_phase: "query",
  };
  return payload;
}

function event(payload: Record<string, unknown>): Record<string, unknown> {
  return (payload.events as Record<string, unknown>[])[0]!;
}

function properties(payload: Record<string, unknown>): Record<string, unknown> {
  return event(payload).properties as Record<string, unknown>;
}
