import { test, expect } from "./fixtures";
import type { APIRequestContext, Page, TestInfo } from "@playwright/test";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

const scrollSelector = ".wb-thread-scroller";
const activeComposerSelector = "textarea.wb-active-textarea";
const overscrollVerbose = process.env.CTX_E2E_OVERSCROLL_VERBOSE === "1";
const stressLoops = Math.max(1, Number(process.env.CTX_E2E_OVERSCROLL_STRESS_LOOPS ?? "4") || 4);
const viewportWidth = Math.max(800, Number(process.env.CTX_E2E_OVERSCROLL_VIEWPORT_WIDTH ?? "1440") || 1440);
const viewportHeight = Math.max(600, Number(process.env.CTX_E2E_OVERSCROLL_VIEWPORT_HEIGHT ?? "900") || 900);

type OverscrollMetrics = {
  scrollTop: number;
  clientHeight: number;
  scrollHeight: number;
  maxScrollTop: number;
  distanceFromMaxScrollPx: number;
  blankTailPx: number | null;
  lastItemId: string | null;
  lastItemTopPx: number | null;
  lastItemBottomPx: number | null;
  lastItemOffscreenAbove: boolean;
  impossibleTail: boolean;
  renderedItemCount: number;
};

type MessageListDebugWindow = Window & {
  __wbSessionMessageListDebug?: {
    seq: number;
    entries: Array<Record<string, unknown>>;
  };
};

async function addLongMessages(request: APIRequestContext, sessionId: string, label: string) {
  const longText = Array.from({ length: 120 }, (_, i) => `${label} line ${i + 1}`).join("\n");
  for (let i = 0; i < 8; i += 1) {
    const response = await request.post(`/api/sessions/${sessionId}/messages`, {
      data: { content: `${longText}\nblock ${i + 1}`, delivery: "immediate" },
    });
    expect(response.ok(), `failed to seed long message ${label} ${i}`).toBeTruthy();
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  await new Promise((resolve) => setTimeout(resolve, 400));
}

async function readOverscrollMetrics(page: Page): Promise<OverscrollMetrics | null> {
  return page.evaluate((selector) => {
    const scroller = document.querySelector(selector) as HTMLElement | null;
    if (!scroller) return null;
    const items = scroller.querySelectorAll<HTMLElement>("[role=\"listitem\"]");
    const lastItem = items.length > 0 ? items.item(items.length - 1) : null;
    const scrollerRect = scroller.getBoundingClientRect();
    const lastItemRect = lastItem?.getBoundingClientRect() ?? null;
    const maxScrollTop = Math.max(0, scroller.scrollHeight - scroller.clientHeight);
    const distanceFromMaxScrollPx = Math.max(0, maxScrollTop - scroller.scrollTop);
    const blankTailPx = lastItemRect ? Math.max(0, scrollerRect.bottom - lastItemRect.bottom) : null;
    const lastItemTopPx = lastItemRect ? lastItemRect.top - scrollerRect.top : null;
    const lastItemBottomPx = lastItemRect ? lastItemRect.bottom - scrollerRect.top : null;
    const lastItemOffscreenAbove = lastItemRect ? lastItemRect.bottom < scrollerRect.top - 1 : false;
    const impossibleTail =
      distanceFromMaxScrollPx <= 2 && ((blankTailPx != null && blankTailPx > 96) || lastItemOffscreenAbove);
    return {
      scrollTop: scroller.scrollTop,
      clientHeight: scroller.clientHeight,
      scrollHeight: scroller.scrollHeight,
      maxScrollTop,
      distanceFromMaxScrollPx,
      blankTailPx,
      lastItemId: lastItem?.getAttribute("data-thread-item-id") ?? null,
      lastItemTopPx,
      lastItemBottomPx,
      lastItemOffscreenAbove,
      impossibleTail,
      renderedItemCount: items.length,
    };
  }, scrollSelector);
}

async function readDebugEntries(page: Page) {
  return page.evaluate(() => {
    const win = window as MessageListDebugWindow;
    const store = win.__wbSessionMessageListDebug;
    return store ? { seq: store.seq, entries: store.entries.slice(-80) } : { seq: 0, entries: [] };
  });
}

async function clearDebugEntries(page: Page) {
  await page.evaluate(() => {
    const win = window as MessageListDebugWindow;
    win.__wbSessionMessageListDebug = { seq: 0, entries: [] };
  });
}

async function forceBottom(page: Page) {
  await page.locator(scrollSelector).first().evaluate((el) => {
    el.scrollTop = el.scrollHeight;
  });
}

async function setScrollFraction(page: Page, fraction: number) {
  await page.locator(scrollSelector).first().evaluate(
    (el, ratio) => {
      const max = Math.max(0, el.scrollHeight - el.clientHeight);
      el.scrollTop = max * ratio;
    },
    fraction,
  );
}

async function setComposerLines(page: Page, label: string, lines: number) {
  const text = Array.from({ length: lines }, (_, i) => `${label} ${i + 1}`).join("\n");
  const composer = page.locator(activeComposerSelector).first();
  await expect(composer).toBeVisible({ timeout: 20_000 });
  await composer.fill(text);
  await page.waitForTimeout(120);
}

async function clearActiveComposer(page: Page) {
  const composer = page.locator(activeComposerSelector).first();
  await composer.fill("");
  await page.waitForTimeout(100);
}

async function clickTaskAndWait(page: Page, taskNumber: number, expectedSessionId: string) {
  const row = page.locator(".wb-task-row").filter({ hasText: `fixture task ${taskNumber}` }).first();
  await row.click();
  await expect(page.locator('[data-testid="session-view"]').first()).toHaveAttribute(
    "data-session-id",
    expectedSessionId,
    {
      timeout: 20_000,
    },
  );
  await expect(page.locator(scrollSelector).first()).toBeVisible({ timeout: 20_000 });
  await page.waitForTimeout(160);
}

async function assertNoImpossibleTail(page: Page, testInfo: TestInfo, label: string) {
  for (let attempt = 0; attempt < 3; attempt += 1) {
    await forceBottom(page);
    await page.waitForTimeout(120);
    const metrics = await readOverscrollMetrics(page);
    if (
      metrics &&
      !metrics.impossibleTail &&
      !metrics.lastItemOffscreenAbove &&
      (metrics.blankTailPx ?? 0) <= 64 &&
      metrics.distanceFromMaxScrollPx <= 2
    ) {
      if (overscrollVerbose) {
        // eslint-disable-next-line no-console
        console.log(
          `[overscroll-check] ${label} blankTail=${metrics.blankTailPx} dist=${metrics.distanceFromMaxScrollPx} lastTop=${metrics.lastItemTopPx} lastBottom=${metrics.lastItemBottomPx} items=${metrics.renderedItemCount}`,
        );
      }
      return metrics;
    }
  }

  const metrics = await readOverscrollMetrics(page);
  const debug = await readDebugEntries(page);
  await testInfo.attach(`${label}-overscroll.png`, {
    body: await page.screenshot({ fullPage: false }),
    contentType: "image/png",
  });
  await testInfo.attach(`${label}-overscroll-debug.json`, {
    body: JSON.stringify({ label, metrics, debug }, null, 2),
    contentType: "application/json",
  });

  expect(metrics, `${label}: missing scroller metrics`).not.toBeNull();
  expect(metrics?.distanceFromMaxScrollPx ?? Number.POSITIVE_INFINITY, `${label}: failed to settle at max scroll`).toBeLessThanOrEqual(2);
  expect(metrics?.blankTailPx ?? Number.POSITIVE_INFINITY, `${label}: blank tail too large`).toBeLessThanOrEqual(64);
  expect(metrics?.lastItemOffscreenAbove ?? true, `${label}: last item ended above viewport`).toBeFalsy();
  expect(metrics?.impossibleTail ?? true, `${label}: impossible tail heuristic fired`).toBeFalsy();
}

test("workbench: session thread never develops impossible blank tail after mixed switching and composer growth", async ({
  page,
  request,
}, testInfo) => {
  test.setTimeout(180000);
  await page.setViewportSize({ width: viewportWidth, height: viewportHeight });

  const seed = await seedDummyWorkspace(request, {
    tasks: 3,
    sessionsPerTask: 1,
    turnsPerSession: 12,
    throttleMs: 5,
  });
  const sessionIdByTaskNumber = new Map<number, string>();
  seed.taskIds.forEach((taskId, index) => {
    const sessionId = seed.sessionIdsByTask[taskId]?.[0];
    if (sessionId) sessionIdByTaskNumber.set(index + 1, sessionId);
  });

  for (const [taskId, sessionIds] of Object.entries(seed.sessionIdsByTask)) {
    const sessionId = sessionIds?.[0];
    if (!taskId || !sessionId) continue;
    await addLongMessages(request, sessionId, taskId);
  }

  const params = new URLSearchParams();
  params.set("debug", "1");
  await page.goto(`/workspaces/${seed.workspaceId}?${params.toString()}`, { waitUntil: "domcontentloaded" });
  await expect(page.locator(".wb-task-row")).toHaveCount(3, { timeout: 20_000 });
  await clearDebugEntries(page);

  const task1SessionId = sessionIdByTaskNumber.get(1) ?? "";
  const task2SessionId = sessionIdByTaskNumber.get(2) ?? "";
  const task3SessionId = sessionIdByTaskNumber.get(3) ?? "";
  expect(task1SessionId).toBeTruthy();
  expect(task2SessionId).toBeTruthy();
  expect(task3SessionId).toBeTruthy();

  await clickTaskAndWait(page, 1, task1SessionId);
  await assertNoImpossibleTail(page, testInfo, "initial-task-1");

  await test.step("scenario 1: non-bottom restore after tall composer switch", async () => {
    await setComposerLines(page, "task-1 tall composer", 6);
    await setScrollFraction(page, 0.42);
    await page.waitForTimeout(120);
    await clickTaskAndWait(page, 2, task2SessionId);
    await setComposerLines(page, "task-2 short composer", 2);
    await assertNoImpossibleTail(page, testInfo, "scenario-1-task-2");
    await clickTaskAndWait(page, 1, task1SessionId);
    await assertNoImpossibleTail(page, testInfo, "scenario-1-return-task-1");
  });

  await test.step("scenario 2: shrink and regrow composer across third-session hop", async () => {
    await clearActiveComposer(page);
    await setComposerLines(page, "task-1 regrow", 8);
    await setScrollFraction(page, 0.18);
    await page.waitForTimeout(120);
    await clickTaskAndWait(page, 3, task3SessionId);
    await setComposerLines(page, "task-3 medium composer", 4);
    await setScrollFraction(page, 0.56);
    await page.waitForTimeout(120);
    await clickTaskAndWait(page, 1, task1SessionId);
    await clearActiveComposer(page);
    await assertNoImpossibleTail(page, testInfo, "scenario-2-return-task-1");
  });

  await test.step("scenario 3: detour through new-task composer and back", async () => {
    await setComposerLines(page, "task-1 before new task", 5);
    await setScrollFraction(page, 0.35);
    await page.waitForTimeout(120);
    await page.getByRole("button", { name: "New Task" }).click();
    const newTaskComposer = page.locator("textarea.wb-new-composer-textarea").first();
    await expect(newTaskComposer).toBeVisible({ timeout: 20_000 });
    await newTaskComposer.fill(
      Array.from({ length: 7 }, (_, i) => `new task composer line ${i + 1}`).join("\n"),
    );
    await page.waitForTimeout(140);
    await clickTaskAndWait(page, 1, task1SessionId);
    await assertNoImpossibleTail(page, testInfo, "scenario-3-return-task-1");
  });

  await test.step("scenario 4: repeat mixed switches in a short stress loop", async () => {
    const order = [2, 1, 3, 1, 2, 1];
    const lineCounts = [3, 7, 2, 6, 4, 1];
    const fractions = [0.25, 0.48, 0.2, 0.6, 0.4, 0.1];
    for (let loop = 0; loop < stressLoops; loop += 1) {
      for (let i = 0; i < order.length; i += 1) {
        const taskNumber = order[i] ?? 1;
        const sessionId = sessionIdByTaskNumber.get(taskNumber) ?? task1SessionId;
        await clickTaskAndWait(page, taskNumber, sessionId);
        await setComposerLines(page, `stress-${loop}-${taskNumber}-${i}`, lineCounts[i] ?? 3);
        await setScrollFraction(page, fractions[i] ?? 0.25);
        await page.waitForTimeout(100);
        await assertNoImpossibleTail(page, testInfo, `stress-loop-${loop}-${i}-task-${taskNumber}`);
      }
    }
  });

  const debug = await readDebugEntries(page);
  await testInfo.attach("final-message-list-debug.json", {
    body: JSON.stringify(debug, null, 2),
    contentType: "application/json",
  });
});
