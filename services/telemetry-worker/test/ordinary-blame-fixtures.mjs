import { readFileSync } from "node:fs";

// The same authored examples are asserted against Rust's existing serializer.
export const BLAME_EVENTS = JSON.parse(readFileSync(new URL(
  "../../../contracts/telemetry-v1/fixtures/blame_operation_completed.valid.json",
  import.meta.url,
), "utf8"));
export const BLAME_NOW = new Date("2026-09-20T12:35:00Z");
export const BLAME_DOCS_EVENT = JSON.parse(readFileSync(new URL(
  "../../../contracts/telemetry-v1/fixtures/blame_docs_operation_completed.valid.json",
  import.meta.url,
), "utf8"));
export const BLAME_ENV = {
  TELEMETRY_ANALYTICS_ENVIRONMENT: "staging",
  TELEMETRY_DATABASE_URL: "postgresql://telemetry.example.test/db",
  TELEMETRY_IDENTITY_HMAC_KEY: "ordinary-blame-test-key-at-least-32-bytes",
  TELEMETRY_IDENTITY_KEY_VERSION: "1",
  TELEMETRY_RATE_LIMITER: { async limit() { return { success: true }; } },
};

export function ordinaryBlameBatch(events) {
  return {
    client_profile_id: "11111111-1111-4111-8111-111111111111",
    data_root_id: "22222222-2222-4222-8222-222222222222",
    app_version: "1.5.0",
    os: "linux",
    arch: "x86_64",
    events,
  };
}

export function ordinaryBlameRequest(body, route = "/functions/v1/telemetry") {
  return new Request(`https://telemetry.example.test${route}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}
