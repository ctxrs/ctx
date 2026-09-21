import { readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { createPinnedCloudflareMonitor } from "./pinned-cloudflare-monitor.mjs";

const configUrl = new URL("../config/health-monitor.json", import.meta.url);
const invalidConfigMessage = "telemetry health monitor config is invalid";
const monitor = createPinnedCloudflareMonitor({
  configUrl, invalidConfigMessage, usage: "usage: health-monitor.mjs [--apply]",
});

export function healthMonitorConfigs(config) {
  if (config?.version !== 2 || !Array.isArray(config.signals) || config.signals.length !== 7) {
    throw new Error(invalidConfigMessage);
  }
  const signals = new Set();
  const checks = new Set();
  const policies = new Set();
  return config.signals.map((item) => {
    if (!/^[a-z][a-z_]{1,63}$/u.test(item.signal) || signals.has(item.signal)
      || checks.has(item.healthCheckId) || policies.has(item.notificationPolicyId)
      || typeof item.action !== "string" || !item.action || item.action.length > 300) {
      throw new Error(invalidConfigMessage);
    }
    signals.add(item.signal);
    checks.add(item.healthCheckId);
    policies.add(item.notificationPolicyId);
    return monitor.validateConfig({
      version: 1, accountId: config.accountId, zoneId: config.zoneId,
      healthCheckId: item.healthCheckId, notificationPolicyId: item.notificationPolicyId,
      healthCheck: {
        ...config.healthCheck,
        name: `ctx_telemetry_${item.signal}`,
        description: item.action,
        http_config: {
          ...config.healthCheck.http_config,
          path: `/functions/v1/analytics/health/${item.signal}`,
        },
      },
      notificationPolicy: {
        ...config.notificationPolicy,
        name: `ctx telemetry: ${item.signal}`,
        description: `${item.action} This category became active; other categories alert independently.`,
        filters: {
          ...config.notificationPolicy.filters,
          health_check_id: [item.healthCheckId],
        },
      },
    });
  });
}

export function validateHealthMonitorConfig(config) {
  healthMonitorConfigs(config);
  return config;
}

export async function reconcileHealthMonitor({ config, ...options }) {
  // Native state belongs to each pinned category, not to one aggregate incident.
  const configs = healthMonitorConfigs(config);
  const results = [];
  for (const [index, item] of configs.entries()) {
    results.push({ signal: config.signals[index].signal, ...await monitor.reconcile({ ...options, config: item }) });
  }
  return { monitors: results };
}

async function main() {
  const args = process.argv.slice(2);
  if (args.length > 1 || args.some((arg) => arg !== "--apply")) throw new Error("usage: health-monitor.mjs [--apply]");
  const apply = args[0] === "--apply";
  const config = JSON.parse(await readFile(configUrl, "utf8"));
  const result = await reconcileHealthMonitor({
    apply, config, token: process.env.CLOUDFLARE_HEALTH_MONITOR_API_TOKEN,
  });
  console.log(JSON.stringify({ apply, ...result }, null, 2));
  if (result.monitors.some((row) => row.health_check.status !== "in_sync" || row.notification_policy.status !== "in_sync")) {
    process.exitCode = 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    const token = process.env.CLOUDFLARE_HEALTH_MONITOR_API_TOKEN;
    console.error(String(error?.message ?? "telemetry_health_monitor_failed").split(token || "\0").join("[REDACTED]"));
    process.exitCode = 1;
  });
}
