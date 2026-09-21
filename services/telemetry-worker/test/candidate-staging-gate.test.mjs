import { describe, expect, test, vi } from "vitest";

import {
  minuteTimestamp,
  parseArgs,
  runCandidateStagingGate,
} from "../scripts/candidate-staging-gate.mjs";

describe("candidate staging telemetry gate", () => {
  test("posts one canonical terminal and reports only the observed HTTP success", async () => {
    const requests = [];
    const uuids = [
      "11111111-1111-4111-8111-111111111111",
      "22222222-2222-4222-8222-222222222222",
      "33333333-3333-4333-8333-333333333333",
    ];

    const report = await runCandidateStagingGate({
      fetchImpl: async (url, init) => {
        requests.push([url, init]);
        return new Response(null, { status: 204 });
      },
      makeUuid: () => uuids.shift(),
      now: () => new Date("2026-08-21T15:42:51.123Z"),
    });

    expect(report).toEqual({
      app_version: "1.1.0",
      event_family: "operation_completed",
      http_204_observed: true,
      http_status: 204,
      neon_commit_verified: false,
    });
    expect(report).not.toHaveProperty("storage_committed");
    expect(report).not.toHaveProperty("durable_queue_admitted");
    expect(requests).toHaveLength(1);
    const payload = JSON.parse(requests[0][1].body);
    expect(payload).toMatchObject({
      app_version: "1.1.0",
      arch: "x86_64",
      client_profile_id: "11111111-1111-4111-8111-111111111111",
      data_root_id: "22222222-2222-4222-8222-222222222222",
      os: "linux",
    });
    expect(payload.events).toEqual([expect.objectContaining({
      event_id: "33333333-3333-4333-8333-333333333333",
      event_name: "operation_completed",
      occurred_at: "2026-08-21T15:42:00Z",
      operation: "search",
      outcome: "success",
      properties: expect.objectContaining({
        has_query: true,
        output: "human",
      }),
      surface: "cli",
    })]);
    expect(JSON.stringify(payload)).not.toContain("materialization_");
  });

  test("fails closed before a noncanonical endpoint or after a non-204 response", async () => {
    const fetchImpl = vi.fn();
    await expect(runCandidateStagingGate({
      endpoint: "https://api.ctx.rs/functions/v1/telemetry",
      fetchImpl,
    })).rejects.toThrow("staging_endpoint_must_be_canonical");
    expect(fetchImpl).not.toHaveBeenCalled();

    await expect(runCandidateStagingGate({
      fetchImpl: async () => new Response(null, { status: 500 }),
    })).rejects.toThrow("staging_http_500");
  });

  test("has no source checkout or read-database arguments", () => {
    expect(parseArgs([])).toEqual({
      endpoint: "https://telemetry-staging.ctx.rs/functions/v1/telemetry",
    });
    expect(() => parseArgs(["--public-repo", "/tmp/ctx"])).toThrow(
      "unsupported_argument:--public-repo",
    );
    expect(minuteTimestamp(new Date("2026-08-21T15:42:51.123Z")))
      .toBe("2026-08-21T15:42:00Z");
  });
});
