import { test, expect } from "./fixtures";
import { seedDummyWorkspace, startStreamingMessages } from "./utils/seedDummyWorkspace";

const isLargeScale = process.env.CTX_E2E_SCALE === "large";

type E2EWindow = Window & {
  __ctxE2E?: {
    workspaceStream?: {
      close?: () => void;
      getConnectionState?: () => string | null;
    };
  };
};

test("ws: recovers from a single stream drop in a large workspace", async ({ page, request }) => {
  test.skip(!isLargeScale, "CTX_E2E_SCALE=large only");

  const seed = await seedDummyWorkspace(request, {
    tasks: 30,
    sessionsPerTask: 1,
    turnsPerSession: 5,
    throttleMs: 2,
    includeToolSummaries: true,
    toolSummariesPerTurn: 4,
    messageBytes: 2048,
  });

  const warnings: Array<{ text: string; ts: number; reason?: string }> = [];
  page.on("console", (msg) => {
    const text = msg.text();
    if (text.includes("Workspace active snapshot not received")) {
      const match = text.match(/Workspace active snapshot not received over WS \(([^)]+)\)/);
      warnings.push({ text, ts: Date.now(), reason: match?.[1] });
    }
  });

  const requests: Array<{ url: string; ts: number }> = [];
  page.on("request", (req) => {
    const url = req.url();
    if (!url.includes("/api/workspaces/")) return;
    if (!url.includes("/active_snapshot") && !url.includes("/active_heads")) return;
    requests.push({ url, ts: Date.now() });
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const search = page.getByTestId("workbench-task-search");
  await expect(search).toBeVisible({ timeout: 30_000 });
  await search.fill(`fixture task ${seed.taskIds.length}`);
  await expect(
    page.getByRole("listitem", { name: new RegExp(`fixture task ${seed.taskIds.length}`, "i") }),
  ).toBeVisible({ timeout: 30_000 });

  await expect
    .poll(async () =>
      page.evaluate(() => typeof (window as E2EWindow).__ctxE2E?.workspaceStream?.getConnectionState === "function"),
    )
    .toBe(true);

  await expect
    .poll(async () => page.evaluate(() => (window as E2EWindow).__ctxE2E?.workspaceStream?.getConnectionState?.()))
    .toBe("connected");

  const dropTime = Date.now();
  await page.evaluate(() => {
    (window as E2EWindow).__ctxE2E?.workspaceStream?.close?.();
  });

  await expect
    .poll(async () => page.evaluate(() => (window as E2EWindow).__ctxE2E?.workspaceStream?.getConnectionState?.()))
    .toBe("disconnected");

  await expect
    .poll(async () => page.evaluate(() => (window as E2EWindow).__ctxE2E?.workspaceStream?.getConnectionState?.()))
    .toBe("connected");

  const sessionView = page.getByTestId("session-view");

  const targetTaskIndex = seed.taskIds.length;
  const targetTaskId = seed.taskIds[targetTaskIndex - 1];
  const sessionId = seed.sessionIdsByTask[targetTaskId][0];
  const targetTask = page.getByRole("listitem", {
    name: new RegExp(`fixture task ${targetTaskIndex}`, "i"),
  });
  await targetTask.click();
  const targetMarker = new RegExp(`fixture msg ${targetTaskIndex}\\.1\\.`, "i");
  await expect(sessionView).toContainText(targetMarker, { timeout: 20_000 });
  await sessionView.evaluate((node) => node.scrollTo(0, node.scrollHeight));
  const streamer = startStreamingMessages(request, {
    sessionIds: [sessionId],
    intervalMs: 300,
    durationMs: 8_000,
    includeToolSummaries: true,
    toolSummariesPerTurn: 2,
    messageBytes: 1024,
    messagePrefix: "stream msg",
  });

  await page.waitForTimeout(9_000);
  await streamer.stop();

  await sessionView.evaluate((node) => node.scrollTo(0, node.scrollHeight));
  await expect(sessionView).toContainText("stream msg", { timeout: 20_000 });

  const afterDrop = requests.filter((entry) => entry.ts >= dropTime);
  expect(afterDrop).toEqual([]);
  const unexpectedWarnings = warnings.filter((warning) => {
    if (warning.reason !== "ws_open") return true;
    if (!dropTime) return true;
    return warning.ts > dropTime + 2000;
  });
  expect(unexpectedWarnings).toEqual([]);
});
