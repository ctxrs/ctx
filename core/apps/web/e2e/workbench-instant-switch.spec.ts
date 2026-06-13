import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";
import type { Locator } from "playwright/test";

type InstantSwitchWindow = Window & { __cls?: number };

test("workbench: switching between active tasks is instant (no jank, no loading)", async ({ page, request }) => {
  const seed = await seedDummyWorkspace(request, {
    tasks: 2,
    sessionsPerTask: 1,
    turnsPerSession: 3,
    throttleMs: 5,
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  const taskOne = rows.filter({ hasText: "fixture task 1" }).first();
  const taskTwo = rows.filter({ hasText: "fixture task 2" }).first();
  const sessionView = page.locator(".wb-session-slot[aria-hidden=\"false\"]");
  const firstMarker = "fixture msg 1.1.1";
  const secondMarker = "fixture msg 2.1.1";
  await expect(rows).toHaveCount(2);

  await page.evaluate(() => {
    const win = window as InstantSwitchWindow;
    win.__cls = 0;
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        const shift = entry as PerformanceEntry & { hadRecentInput?: boolean; value?: number };
        if (!shift.hadRecentInput) {
          win.__cls = (win.__cls ?? 0) + (shift.value ?? 0);
        }
      }
    }).observe({ type: "layout-shift", buffered: true });
  });

  await taskOne.click();
  await expect(sessionView).toContainText(firstMarker, { timeout: 20000 });
  await taskTwo.click();
  await expect(sessionView).toContainText(secondMarker, { timeout: 20000 });

  await page.evaluate(() => {
    (window as InstantSwitchWindow).__cls = 0;
  });

  const measureSwitch = async (row: Locator, marker: string) => {
    const start = await page.evaluate(() => performance.now());
    await row.click();
    await expect(sessionView).toContainText(marker, { timeout: 20000 });
    const end = await page.evaluate(() => performance.now());
    return end - start;
  };

  const latencyA = await measureSwitch(taskOne, firstMarker);
  const latencyB = await measureSwitch(taskTwo, secondMarker);
  // Allow some variance across CI and dev machines; the core expectation is "no loading/jank",
  // which is separately asserted via CLS and absence of loading placeholders.
  const maxLatencyMs = 300;
  expect(Math.max(latencyA, latencyB)).toBeLessThanOrEqual(maxLatencyMs);

  const cls = await page.evaluate(() => (window as InstantSwitchWindow).__cls ?? 0);
  expect(cls).toBeLessThanOrEqual(0.01);
  await expect(page.locator(".wb-session >> text=Loading")).toHaveCount(0);
  await expect(page.locator(".wb-session >> text=Select a track with a session.")).toHaveCount(0);
});
