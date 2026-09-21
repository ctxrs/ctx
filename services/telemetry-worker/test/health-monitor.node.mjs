import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { createPinnedCloudflareMonitor } from "../scripts/pinned-cloudflare-monitor.mjs";
import { definePinnedCloudflareMonitorContract } from "./support/pinned-cloudflare-monitor-contract.mjs";
import { healthMonitorConfigs, reconcileHealthMonitor, validateHealthMonitorConfig } from "../scripts/health-monitor.mjs";

const config = JSON.parse(await readFile(new URL("../config/health-monitor.json", import.meta.url)));
const generated = healthMonitorConfigs(config);
const signals = [
  "producer_configuration", "database_availability", "compatibility_rejections",
  "event_collisions", "other_rejections", "queue_health", "queue_metrics",
];

test("pins seven independent actionable email categories with existing debounce and recipient", () => {
  assert.equal(validateHealthMonitorConfig(config), config);
  assert.deepEqual(config.signals.map((item) => item.signal), signals);
  assert.equal(generated[0].healthCheckId, "4b41c9e3a36000186d35f24ba68c7a9d");
  assert.equal(generated[0].notificationPolicyId, "f7eb184b30d94763a597836cd8bf8f88");
  for (const [index, item] of generated.entries()) {
    assert.deepEqual([item.accountId, item.zoneId],
      ["f40307a99aa7ad04d63aefd1a08e6fb1", "e3ab55fb89448b5d619c4461e4b999ad"]);
    assert.deepEqual(item.healthCheck.http_config, {
      method: "GET", path: "/functions/v1/analytics/health/" + signals[index], port: 443,
      expected_body: "\"alert\":false", expected_codes: ["200"],
      follow_redirects: false, allow_insecure: false, header: { Host: ["cli.ctx.rs"] },
    });
    assert.deepEqual([item.healthCheck.interval, item.healthCheck.timeout,
      item.healthCheck.check_regions, item.healthCheck.consecutive_successes,
      item.healthCheck.consecutive_fails, item.healthCheck.suspended],
    [60, 5, ["WNAM", "ENAM", "WEU"], 2, 2, false]);
    assert.equal(item.healthCheck.name, "ctx_telemetry_" + signals[index]);
    assert.equal(item.healthCheck.description, config.signals[index].action);
    assert.match(item.notificationPolicy.description, /other categories alert independently/);
    assert.equal(item.notificationPolicy.enabled, true);
    assert.deepEqual(item.notificationPolicy.filters, {
      status: ["Unhealthy"], health_check_id: [item.healthCheckId],
    });
    assert.deepEqual(item.notificationPolicy.mechanisms, { email: [{ id: "telemetry-alerts@example.invalid" }] });
  }
});

test("rejects unbound, duplicated or malformed identities before touching Cloudflare", async () => {
  for (const change of [
    (value) => { value.version = 1; },
    (value) => { value.signals.pop(); },
    (value) => { value.signals[1].signal = value.signals[0].signal; },
    (value) => { value.signals[1].signal = "../other"; },
    (value) => { value.signals[1].healthCheckId = "UNBOUND"; },
    (value) => { value.signals[1].healthCheckId = value.signals[0].healthCheckId; },
    (value) => { value.signals[1].notificationPolicyId = value.signals[0].notificationPolicyId; },
  ]) {
    const invalid = structuredClone(config);
    change(invalid);
    await assert.rejects(reconcileHealthMonitor({
      config: invalid, token: "token", fetchImpl: () => { throw new Error("must_not_fetch"); },
    }), /telemetry health monitor config is invalid/);
  }
});

function fakeCloudflare() {
  const resources = new Map();
  for (const item of generated) {
    resources.set("https://api.cloudflare.com/client/v4/zones/" + item.zoneId + "/healthchecks/" + item.healthCheckId,
      { id: item.healthCheckId, ...structuredClone(item.healthCheck) });
    resources.set("https://api.cloudflare.com/client/v4/accounts/" + item.accountId + "/alerting/v3/policies/" + item.notificationPolicyId,
      { id: item.notificationPolicyId, ...structuredClone(item.notificationPolicy) });
  }
  const calls = [];
  const fetchImpl = async (url, init = {}) => {
    const method = init.method ?? "GET";
    assert.ok(resources.has(url), "only exact pinned resources may be touched");
    calls.push({ url, method });
    if (method === "PUT") resources.set(url, { id: resources.get(url).id, ...JSON.parse(init.body) });
    else assert.equal(method, "GET");
    return Response.json({ success: true, result: resources.get(url) });
  };
  return { resources, calls, fetchImpl };
}

test("reads all independent checks/policies and only repairs the drifting category", async () => {
  const fake = fakeCloudflare();
  const stable = await reconcileHealthMonitor({ config, token: "token", fetchImpl: fake.fetchImpl });
  assert.deepEqual(stable.monitors.map((row) => row.signal), signals);
  assert.equal(fake.calls.length, 14);
  assert.ok(fake.calls.every((call) => call.method === "GET"));
  const url = [...fake.resources.keys()].find((key) => key.endsWith(generated[3].notificationPolicyId));
  fake.resources.get(url).enabled = false;
  const drift = await reconcileHealthMonitor({ config, token: "token", fetchImpl: fake.fetchImpl });
  assert.deepEqual(drift.monitors.filter((row) => row.notification_policy.status === "drift").map((row) => row.signal),
    ["event_collisions"]);
  const fixed = await reconcileHealthMonitor({ apply: true, config, token: "token", fetchImpl: fake.fetchImpl });
  assert.equal(fixed.monitors[3].notification_policy.updated, true);
  assert.deepEqual(fake.calls.filter((call) => call.method === "PUT"), [{ url, method: "PUT" }]);
});

// Retain the shared strict-ID, GET-after-PUT, unmanaged-field and secret-redaction checks.
const pinned = createPinnedCloudflareMonitor({
  configUrl: new URL("../config/health-monitor.json", import.meta.url),
  invalidConfigMessage: "telemetry health monitor config is invalid", usage: "test",
});
definePinnedCloudflareMonitorContract({ config: generated[0], reconcile: pinned.reconcile });
