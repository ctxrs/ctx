#!/usr/bin/env node

import { randomUUID } from "node:crypto";
import { pathToFileURL } from "node:url";

const DEFAULT_ENDPOINT = "https://telemetry-staging.ctx.rs/functions/v1/telemetry";
const APP_VERSION = "1.1.0";

export async function runCandidateStagingGate({
  endpoint = DEFAULT_ENDPOINT,
  fetchImpl = globalThis.fetch,
  makeUuid = randomUUID,
  now = () => new Date(),
} = {}) {
  if (endpoint !== DEFAULT_ENDPOINT) {
    throw new Error("staging_endpoint_must_be_canonical");
  }
  if (typeof fetchImpl !== "function") throw new Error("fetch_unavailable");

  const occurredAt = minuteTimestamp(now());
  const payload = canonicalTerminalPayload({ makeUuid, occurredAt });
  const response = await fetchImpl(endpoint, {
    body: JSON.stringify(payload),
    headers: { "content-type": "application/json; charset=utf-8" },
    method: "POST",
  });
  if (response.status !== 204) throw new Error(`staging_http_${response.status}`);

  return Object.freeze({
    app_version: APP_VERSION,
    event_family: "operation_completed",
    http_status: response.status,
    http_204_observed: true,
    neon_commit_verified: false,
  });
}

export function canonicalTerminalPayload({ makeUuid, occurredAt }) {
  return {
    app_version: APP_VERSION,
    arch: "x86_64",
    client_profile_id: makeUuid(),
    data_root_id: makeUuid(),
    events: [{
      duration_bucket: "lt_1s",
      event_id: makeUuid(),
      event_name: "operation_completed",
      event_version: 1,
      occurred_at: occurredAt,
      operation: "search",
      outcome: "success",
      properties: {
        event_results: false,
        has_event_type_filter: false,
        has_file_filter: false,
        has_provider_filter: false,
        has_query: true,
        has_session_filter: false,
        has_since_filter: false,
        has_workspace_filter: false,
        include_current_session: false,
        include_subagents: false,
        limit_bucket: "21-100",
        output: "human",
        primary_only: true,
        zero_result: false,
      },
      surface: "cli",
    }],
    os: "linux",
  };
}

export function minuteTimestamp(value) {
  if (!(value instanceof Date) || !Number.isFinite(value.getTime())) {
    throw new Error("invalid_candidate_time");
  }
  const minute = new Date(value);
  minute.setUTCSeconds(0, 0);
  return minute.toISOString().replace(".000Z", "Z");
}

export function parseArgs(argv) {
  if (argv.length > 0) throw new Error(`unsupported_argument:${argv[0]}`);
  return { endpoint: DEFAULT_ENDPOINT };
}

async function main() {
  const result = await runCandidateStagingGate(parseArgs(process.argv.slice(2)));
  process.stdout.write(`${JSON.stringify(result)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    const code = error instanceof Error && /^[a-z0-9_:.-]+$/u.test(error.message)
      ? error.message
      : "candidate_staging_gate_failed";
    process.stderr.write(`error: ${code}\n`);
    process.exitCode = 1;
  });
}
