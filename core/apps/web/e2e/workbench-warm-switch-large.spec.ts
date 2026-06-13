import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

const isLargeScale = process.env.CTX_E2E_SCALE === "large";

test("workbench: warm switching stays instant in large workspace", async ({ page, request }) => {
  test.skip(!isLargeScale, "CTX_E2E_SCALE=large only");

  const seed = await seedDummyWorkspace(request, {
    tasks: 40,
    sessionsPerTask: 1,
    turnsPerSession: 6,
    throttleMs: 2,
    includeToolSummaries: true,
    toolSummariesPerTurn: 4,
    messageBytes: 2048,
  });

  const warnings: string[] = [];
  page.on("console", (msg) => {
    const text = msg.text();
    if (text.includes("Workspace active snapshot not received")) {
      warnings.push(text);
    }
  });

  const isProviderNoise = (url: string) =>
    url.includes("/api/providers") ||
    url.includes("/api/sessions/web") ||
    (url.includes("/api/workspaces/") && url.includes("/providers/") && url.includes("/options"));

  const requests: Array<{ url: string; method: string; ts: number }> = [];
  page.on("request", (req) => {
    const url = req.url();
    if (!url.includes("/api/")) return;
    if (isProviderNoise(url)) return;
    requests.push({ url, method: req.method(), ts: Date.now() });
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const sessionView = page.getByTestId("session-view");
  const search = page.getByTestId("workbench-task-search");
  await expect(search).toBeVisible({ timeout: 30_000 });
  await search.fill("fixture task 3");
  const taskA = page.getByRole("listitem", { name: /fixture task 39/i });
  const taskB = page.getByRole("listitem", { name: /fixture task 38/i });
  await expect(taskA).toBeVisible({ timeout: 30_000 });
  await expect(taskB).toBeVisible({ timeout: 30_000 });

  const taskAMarker = /fixture msg 39\.1\./;
  const taskBMarker = /fixture msg 38\.1\./;
  await taskA.click();
  await expect(sessionView).toContainText(taskAMarker, { timeout: 20_000 });
  await taskB.click();
  await expect(sessionView).toContainText(taskBMarker, { timeout: 20_000 });

  requests.length = 0;
  const cutoff = Date.now();

  const measureSwitch = async (row: ReturnType<typeof page.getByRole>, marker: RegExp) => {
    const start = await page.evaluate(() => performance.now());
    await row.click();
    await expect(sessionView).toContainText(marker, { timeout: 20_000 });
    const end = await page.evaluate(() => performance.now());
    return end - start;
  };

  const latencyA = await measureSwitch(taskA, taskAMarker);
  const latencyB = await measureSwitch(taskB, taskBMarker);
  const maxLatencyMs = 300;
  expect(Math.max(latencyA, latencyB)).toBeLessThanOrEqual(maxLatencyMs);

  const after = requests.filter((r) => r.ts >= cutoff);
  expect(after).toEqual([]);
  expect(warnings).toEqual([]);
  await expect(sessionView.getByText(/Loading/i)).toHaveCount(0);
});
