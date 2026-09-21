import { expect, test } from "vitest";
import monitor from "../config/health-monitor.json";
import { ENV, workerHarness } from "./worker-test-fixtures";

test("native per-category debounce can notify B while A remains active, without repeating A", async () => {
  const snapshot = {
    compatibilityRejectionMax: 0n, eventCollisionCount: 0n, otherRejectionCount: 0n,
    providerRefreshFailureCount: 500n, deliveryDegradedCount: 500n, deliveryDroppedCount: 100n,
  };
  const harness = workerHarness({ healthSnapshot: snapshot });
  // Test model of the configured native transitions, not a production email sender.
  const states = monitor.signals.map(() => ({ unhealthy: false, candidate: false, consecutive: 0 }));
  async function sample() {
    const emails: string[] = [];
    for (const [index, item] of monitor.signals.entries()) {
      const response = await harness.worker.fetch(new Request(
        "https://cli.ctx.rs/functions/v1/analytics/health/" + item.signal,
      ), ENV);
      const body = await response.text();
      const unhealthy = !(monitor.healthCheck.http_config.expected_codes.includes(String(response.status))
        && body.includes(monitor.healthCheck.http_config.expected_body));
      const state = states[index];
      state.consecutive = unhealthy === state.candidate ? state.consecutive + 1 : 1;
      state.candidate = unhealthy;
      const threshold = unhealthy ? monitor.healthCheck.consecutive_fails : monitor.healthCheck.consecutive_successes;
      if (state.consecutive >= threshold && state.unhealthy !== unhealthy) {
        state.unhealthy = unhealthy;
        if (unhealthy && monitor.notificationPolicy.filters.status.includes("Unhealthy")) emails.push(item.signal);
      }
    }
    return emails;
  }

  expect(await sample()).toEqual([]);
  expect(await sample()).toEqual([]); // Old-client failures do not trip service alerts.
  snapshot.compatibilityRejectionMax = 100n;
  expect(await sample()).toEqual([]);
  expect(await sample()).toEqual(["compatibility_rejections"]);
  expect(await sample()).toEqual([]);
  snapshot.eventCollisionCount = 1n;
  expect(await sample()).toEqual([]);
  expect(await sample()).toEqual(["event_collisions"]);
  expect(await sample()).toEqual([]); // Both remain failed; neither repeats.
  snapshot.eventCollisionCount = 0n;
  expect(await sample()).toEqual([]);
  expect(await sample()).toEqual([]); // No recovery emails.
  snapshot.eventCollisionCount = 1n;
  expect(await sample()).toEqual([]);
  expect(await sample()).toEqual(["event_collisions"]); // A genuine recurrence re-arms B.
  expect(monitor.notificationPolicy.filters.status).toEqual(["Unhealthy"]);
});
