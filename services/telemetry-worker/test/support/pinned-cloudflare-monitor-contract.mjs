import assert from "node:assert/strict";
import test from "node:test";

const API = "https://api.cloudflare.com/client/v4";

export function definePinnedCloudflareMonitorContract({ config, reconcile }) {
  const checkUrl = `${API}/zones/${config.zoneId}/healthchecks/${config.healthCheckId}`;
  const policyUrl = `${API}/accounts/${config.accountId}/alerting/v3/policies/${config.notificationPolicyId}`;
  const headers = { authorization: "Bearer token", "content-type": "application/json" };

  function fakeCloudflare({ check = {}, ignorePuts = false, policy = {} } = {}) {
    const state = {
      check: { id: config.healthCheckId, ...structuredClone(config.healthCheck), ...check },
      policy: { id: config.notificationPolicyId, ...structuredClone(config.notificationPolicy), ...policy },
    };
    const calls = [];
    const fetchImpl = async (url, init = {}) => {
      const method = init.method ?? "GET";
      calls.push({ body: init.body, headers: init.headers, method, url });
      const key = url === checkUrl ? "check" : url === policyUrl ? "policy" : null;
      if (!key) return Response.json({ success: false, errors: ["not found"] }, { status: 404 });
      if (method === "PUT" && !ignorePuts) {
        state[key] = { id: state[key].id, ...JSON.parse(init.body) };
      }
      return Response.json({ success: true, result: state[key] });
    };
    return { calls, fetchImpl };
  }

  test("read-only mode reports in-sync and drift without mutation", async () => {
    const current = fakeCloudflare();
    const inSync = await reconcile({ config, fetchImpl: current.fetchImpl, token: "token" });
    assert.deepEqual([inSync.health_check.status, inSync.notification_policy.status], ["in_sync", "in_sync"]);
    assert.deepEqual(current.calls, [
      { body: undefined, headers, method: "GET", url: checkUrl },
      { body: undefined, headers, method: "GET", url: policyUrl },
    ]);

    const changed = fakeCloudflare({ check: { interval: 120 }, policy: { enabled: false } });
    const drift = await reconcile({ config, fetchImpl: changed.fetchImpl, token: "token" });
    assert.deepEqual([drift.health_check.status, drift.notification_policy.status], ["drift", "drift"]);
    assert.deepEqual(changed.calls.map(({ method }) => method), ["GET", "GET"]);
  });

  test("read-only mode independently rejects unmanaged nested settings", async (t) => {
    await t.test("health check header", async () => {
      const httpConfig = structuredClone(config.healthCheck.http_config);
      httpConfig.header.Authorization = ["Bearer unexpected"];
      const fake = fakeCloudflare({ check: { http_config: httpConfig } });
      const result = await reconcile({ config, fetchImpl: fake.fetchImpl, token: "token" });
      assert.deepEqual([result.health_check.status, result.notification_policy.status], ["drift", "in_sync"]);
    });

    await t.test("notification mechanisms", async () => {
      const mechanisms = structuredClone(config.notificationPolicy.mechanisms);
      mechanisms.webhooks = [{ id: "unexpected-webhook" }];
      mechanisms.pagerduty = [{ id: "unexpected-pagerduty" }];
      const fake = fakeCloudflare({ policy: { mechanisms } });
      const result = await reconcile({ config, fetchImpl: fake.fetchImpl, token: "token" });
      assert.deepEqual([result.health_check.status, result.notification_policy.status], ["in_sync", "drift"]);
    });

    await t.test("notification filters", async () => {
      const filters = structuredClone(config.notificationPolicy.filters);
      filters.zones = [config.zoneId];
      const fake = fakeCloudflare({ policy: { filters } });
      const result = await reconcile({ config, fetchImpl: fake.fetchImpl, token: "token" });
      assert.deepEqual([result.health_check.status, result.notification_policy.status], ["in_sync", "drift"]);
    });
  });

  test("read-only mode allows only documented Cloudflare resource fields", async () => {
    const documented = fakeCloudflare({
      check: {
        created_on: "2026-08-21T00:00:00Z", failure_reason: "",
        modified_on: "2026-08-21T00:00:00Z", status: "Healthy", tcp_config: { port: 0 },
      },
      policy: {
        alert_interval: "5m", created: "2026-08-21T00:00:00Z",
        modified: "2026-08-21T00:00:00Z",
      },
    });
    const inSync = await reconcile({ config, fetchImpl: documented.fetchImpl, token: "token" });
    assert.deepEqual([inSync.health_check.status, inSync.notification_policy.status], ["in_sync", "in_sync"]);

    const unknown = fakeCloudflare({
      check: { undocumented_default: true },
      policy: { undocumented_metadata: "unexpected" },
    });
    const drift = await reconcile({ config, fetchImpl: unknown.fetchImpl, token: "token" });
    assert.deepEqual([drift.health_check.status, drift.notification_policy.status], ["drift", "drift"]);
  });

  test("apply PUTs exact desired bytes only to pinned URLs and verifies with GET", async () => {
    const fake = fakeCloudflare({ check: { interval: 120 }, policy: { enabled: false } });
    const result = await reconcile({ apply: true, config, fetchImpl: fake.fetchImpl, token: "token" });

    assert.deepEqual([result.health_check.updated, result.notification_policy.updated], [true, true]);
    assert.deepEqual(fake.calls, [
      { body: undefined, headers, method: "GET", url: checkUrl },
      { body: JSON.stringify(config.healthCheck), headers, method: "PUT", url: checkUrl },
      { body: undefined, headers, method: "GET", url: checkUrl },
      { body: undefined, headers, method: "GET", url: policyUrl },
      { body: JSON.stringify(config.notificationPolicy), headers, method: "PUT", url: policyUrl },
      { body: undefined, headers, method: "GET", url: policyUrl },
    ]);
  });

  test("apply fails closed when GET-after-PUT still observes drift", async () => {
    const fake = fakeCloudflare({ check: { interval: 120 }, ignorePuts: true });

    await assert.rejects(
      reconcile({ apply: true, config, fetchImpl: fake.fetchImpl, token: "token" }),
      /Cloudflare health check still has drift after update/,
    );
    assert.deepEqual(fake.calls.map(({ method, url }) => [method, url]), [
      ["GET", checkUrl], ["PUT", checkUrl], ["GET", checkUrl],
    ]);
  });

  test("missing credentials, resources, and pinned-ID mismatches fail closed", async () => {
    await assert.rejects(reconcile({ config, fetchImpl: fetch, token: "" }),
      /CLOUDFLARE_HEALTH_MONITOR_API_TOKEN is required/);
    const missing = async () => Response.json({ success: false, errors: ["not found"] }, { status: 404 });
    await assert.rejects(reconcile({ config, fetchImpl: missing, token: "token" }),
      /GET health check failed \(404\).*not found/);
    const wrongId = fakeCloudflare({ check: { id: "0".repeat(32) } });
    await assert.rejects(reconcile({ config, fetchImpl: wrongId.fetchImpl, token: "token" }),
      new RegExp(`health check response did not match pinned ID ${config.healthCheckId}`));
  });

  test("Cloudflare failures adversarially redact and bound credentials", async (t) => {
    const token = "secret-monitor-token";
    const failures = {
      "API errors": async () => Response.json({
        success: false, errors: [`Bearer ${token} ${token} ${"x".repeat(2_000)}`],
      }, { status: 403 }),
      "invalid JSON": async () => new Response(`${token} ${"x".repeat(2_000)}`, { status: 502 }),
      "network errors": async () => { throw new Error(`Bearer ${token} ${token} ${"x".repeat(2_000)}`); },
    };
    for (const [name, failure] of Object.entries(failures)) {
      await t.test(name, async () => {
        let message;
        await assert.rejects(reconcile({ config, fetchImpl: failure, token }), (error) => {
          message = error.message;
          return true;
        });
        assert.doesNotMatch(message, new RegExp(token));
        assert.match(message, /\[REDACTED\].*\[truncated\]/);
        assert.ok(message.length < 650);
      });
    }
  });
}
