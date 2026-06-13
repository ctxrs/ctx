import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

test("workbench: once sessions are authoritative in memory, warm switches stay HTTP-idle", async ({ page, request }) => {
  const seed = await seedDummyWorkspace(request, {
    tasks: 2,
    sessionsPerTask: 1,
    turnsPerSession: 2,
    throttleMs: 5,
  });

  const isProviderNoise = (url: string) =>
    url.includes("/api/providers") ||
    url.includes("/api/sessions/web") ||
    url.includes("/api/updates/check") ||
    url.includes("/api/telemetry") ||
    url.includes("/api/health") ||
    url.includes("/api/tasks/") && (url.includes("/mark_read") || url.includes("/mark_unread")) ||
    (url.includes("/api/workspaces/") && url.includes("/providers/") && url.includes("/options"));

  const requests: Array<{ url: string; method: string; ts: number }> = [];
  page.on("request", (req) => {
    const url = req.url();
    if (!url.includes("/api/")) return;
    if (isProviderNoise(url)) return;
    requests.push({ url, method: req.method(), ts: Date.now() });
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(2);

  await rows.nth(0).click();
  await page.waitForTimeout(200);
  await rows.nth(1).click();
  await page.waitForTimeout(200);
  await rows.nth(0).click();
  await page.waitForTimeout(300);

  requests.length = 0;
  const cutoff = Date.now();

  await rows.nth(1).click();
  await page.waitForTimeout(200);
  await rows.nth(0).click();
  await page.waitForTimeout(300);

  const after = requests.filter((r) => r.ts >= cutoff);
  expect(after).toEqual([]);
});
