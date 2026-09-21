import { readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";

const API = "https://api.cloudflare.com/client/v4";
const ID = /^[0-9a-f]{32}$/u;
const HEALTH_CHECK_READBACK_FIELDS = new Set([
  "created_on",
  "failure_reason",
  "id",
  "modified_on",
  "status",
  "tcp_config",
]);
const NOTIFICATION_POLICY_READBACK_FIELDS = new Set([
  "alert_interval",
  "created",
  "id",
  "modified",
]);

function matchesExactly(actual, desired) {
  if (Array.isArray(desired)) {
    return Array.isArray(actual)
      && desired.length === actual.length
      && desired.every((value, index) => matchesExactly(actual[index], value));
  }
  if (desired && typeof desired === "object") {
    if (actual == null || typeof actual !== "object" || Array.isArray(actual)) return false;
    const actualKeys = Object.keys(actual).sort();
    const desiredKeys = Object.keys(desired).sort();
    return actualKeys.length === desiredKeys.length
      && actualKeys.every((key, index) => key === desiredKeys[index])
      && desiredKeys.every((key) => matchesExactly(actual[key], desired[key]));
  }
  return Object.is(actual, desired);
}

function matchesResource(actual, desired, readbackFields) {
  if (actual == null || typeof actual !== "object" || Array.isArray(actual)) return false;
  const actualKeys = Object.keys(actual);
  if (actualKeys.some((key) => !Object.hasOwn(desired, key) && !readbackFields.has(key))) {
    return false;
  }
  return matchesExactly(
    Object.fromEntries(Object.keys(desired).map((key) => [key, actual[key]])),
    desired,
  );
}

function redact(value, token) {
  let text = typeof value === "string" ? value : JSON.stringify(value);
  text = (text || "unspecified error")
    .split(token || "\0").join("[REDACTED]")
    .replace(/Bearer\s+\S+/giu, "Bearer [REDACTED]");
  return text.length > 500 ? `${text.slice(0, 500)}…[truncated]` : text;
}

async function request(fetchImpl, token, label, path, init = {}) {
  const method = init.method ?? "GET";
  let response;
  try {
    response = await fetchImpl(`${API}${path}`, {
      ...init,
      headers: {
        authorization: `Bearer ${token}`,
        "content-type": "application/json",
      },
    });
  } catch (error) {
    throw new Error(`Cloudflare ${method} ${label} request failed: ${redact(error?.message, token)}`);
  }
  const text = await response.text();
  let body;
  try {
    body = JSON.parse(text);
  } catch {
    throw new Error(`Cloudflare ${method} ${label} returned invalid JSON (${response.status}): ${redact(text, token)}`);
  }
  if (!response.ok || body.success !== true) {
    throw new Error(`Cloudflare ${method} ${label} failed (${response.status}): ${redact(body.errors ?? body.messages, token)}`);
  }
  return body.result;
}

async function reconcileOne({ apply, desired, fetchImpl, id, label, path, readbackFields, token }) {
  const read = async () => {
    const result = await request(fetchImpl, token, label, path);
    if (result?.id !== id) {
      throw new Error(`Cloudflare ${label} response did not match pinned ID ${id}`);
    }
    return result;
  };
  if (matchesResource(await read(), desired, readbackFields)) {
    return { id, status: "in_sync", updated: false };
  }
  if (!apply) return { id, status: "drift", updated: false };
  await request(fetchImpl, token, label, path, {
    method: "PUT",
    body: JSON.stringify(desired),
  });
  if (!matchesResource(await read(), desired, readbackFields)) {
    throw new Error(`Cloudflare ${label} still has drift after update`);
  }
  return { id, status: "in_sync", updated: true };
}

export function createPinnedCloudflareMonitor({ configUrl, invalidConfigMessage, usage }) {
  function validateConfig(config) {
    if (
      config?.version !== 1
      || !ID.test(config.accountId)
      || !ID.test(config.zoneId)
      || !ID.test(config.healthCheckId)
      || !ID.test(config.notificationPolicyId)
      || typeof config.healthCheck?.http_config?.path !== "string"
      || config.notificationPolicy?.alert_type !== "health_check_status_notification"
      || config.notificationPolicy?.filters?.health_check_id?.[0] !== config.healthCheckId
      || config.notificationPolicy.filters.health_check_id.length !== 1
    ) throw new Error(invalidConfigMessage);
    return config;
  }

  async function reconcile({ apply = false, config, fetchImpl = fetch, token }) {
    validateConfig(config);
    if (!token) throw new Error("CLOUDFLARE_HEALTH_MONITOR_API_TOKEN is required");
    const healthCheck = await reconcileOne({
      apply, desired: config.healthCheck, fetchImpl, id: config.healthCheckId,
      label: "health check",
      path: `/zones/${config.zoneId}/healthchecks/${config.healthCheckId}`,
      readbackFields: HEALTH_CHECK_READBACK_FIELDS, token,
    });
    const notificationPolicy = await reconcileOne({
      apply, desired: config.notificationPolicy, fetchImpl,
      id: config.notificationPolicyId, label: "notification policy",
      path: `/accounts/${config.accountId}/alerting/v3/policies/${config.notificationPolicyId}`,
      readbackFields: NOTIFICATION_POLICY_READBACK_FIELDS, token,
    });
    return { health_check: healthCheck, notification_policy: notificationPolicy };
  }

  async function main() {
    const args = process.argv.slice(2);
    if (args.some((arg) => arg !== "--apply") || args.length > 1) {
      throw new Error(usage);
    }
    const apply = args[0] === "--apply";
    const config = JSON.parse(await readFile(configUrl, "utf8"));
    const result = await reconcile({
      apply, config, token: process.env.CLOUDFLARE_HEALTH_MONITOR_API_TOKEN,
    });
    console.log(JSON.stringify({ apply, ...result }, null, 2));
    if (Object.values(result).some(({ status }) => status !== "in_sync")) {
      process.exitCode = 1;
    }
  }

  function runIfMain(moduleUrl) {
    if (process.argv[1] && moduleUrl === pathToFileURL(process.argv[1]).href) {
      main().catch((error) => {
        console.error(redact(error?.message, process.env.CLOUDFLARE_HEALTH_MONITOR_API_TOKEN));
        process.exitCode = 1;
      });
    }
  }

  return { reconcile, runIfMain, validateConfig };
}
