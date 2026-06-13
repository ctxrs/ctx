import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

const scrollSelector = ".wb-session-slot[aria-hidden=\"false\"] .wb-thread-scroller";

type ComposerJankSample = {
  t: number;
  top: number;
  height: number;
  clientHeight: number;
  bottomOffset: number;
};

type ComposerJankShiftEntry = {
  startTime: number;
  value: number;
  hadRecentInput: boolean;
};

type ComposerJankWindow = Window & {
  __composerJankSamples?: ComposerJankSample[];
  __composerJankShiftEntries?: ComposerJankShiftEntry[];
  __composerJankStart?: number;
  __composerJankCleanup?: () => void;
  __composerJankObserver?: PerformanceObserver;
};

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

async function waitForSessionIdle(request: Parameters<typeof test>[0]["request"], sessionId: string, timeoutMs = 20_000) {
  const startedAt = Date.now();
  while (true) {
    const resp = await request.get(`/api/sessions/${sessionId}/snapshot?limit=1`);
    if (!resp.ok()) {
      throw new Error(`failed to read session snapshot for ${sessionId}: ${resp.status()}`);
    }
    const snapshot = (await resp.json()) as {
      head?: {
        turns?: Array<{ status?: string | null }>;
      };
    };
    const turns = Array.isArray(snapshot?.head?.turns) ? snapshot.head.turns : [];
    const lastStatus = typeof turns.at(-1)?.status === "string" ? turns.at(-1)?.status : null;
    if (turns.length === 0 || lastStatus === "completed" || lastStatus === "done") {
      return;
    }
    if (Date.now() - startedAt > timeoutMs) {
      throw new Error(`session ${sessionId} did not become idle within ${timeoutMs}ms`);
    }
    await sleep(50);
  }
}

test("workbench: composer jank stays stable on third line", async ({ page, request }, testInfo) => {
  test.setTimeout(120000);
  await page.setViewportSize({ width: 1400, height: 900 });

  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 12,
    messageBytes: { min: 180, max: 240 },
    messagePrefix: "composer-jank",
  });
  const seedTaskId = seed.taskIds[0] ?? "";
  const seedSessionId = seedTaskId ? seed.sessionIdsByTask[seedTaskId]?.[0] ?? "" : "";
  if (!seedSessionId) {
    throw new Error("failed to resolve seeded session id");
  }
  await waitForSessionIdle(request, seedSessionId, 60_000);

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1, { timeout: 20000 });
  await rows.first().click();

  const composer = page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea");
  await expect(composer).toBeVisible({ timeout: 20000 });

  const scroller = page.locator(scrollSelector).first();
  await expect(scroller).toBeVisible({ timeout: 20000 });
  for (let attempt = 0; attempt < 8; attempt += 1) {
    const overflow = await scroller.evaluate((el) => el.scrollHeight - el.clientHeight);
    if (overflow > 80) break;
    const extraResp = await request.post(`/api/sessions/${seedSessionId}/messages`, {
      data: {
        content: `composer-jank-overflow-seed ${attempt} ${"x".repeat(400)}`,
        delivery: "immediate",
      },
    });
    expect(extraResp.ok(), `failed to append overflow seed ${attempt}`).toBeTruthy();
    await waitForSessionIdle(request, seedSessionId);
    await page.waitForTimeout(120);
  }
  await expect
    .poll(async () => scroller.evaluate((el) => el.scrollHeight - el.clientHeight), { timeout: 20000 })
    .toBeGreaterThan(80);
  const scrollbarMetrics = await page.evaluate(() => {
    const scroller = document.querySelector(
      ".wb-session-slot[aria-hidden=\"false\"] .wb-thread-scroller",
    ) as HTMLElement | null;
    const scrollbar = document.querySelector(
      ".wb-session-slot[aria-hidden=\"false\"] .wb-scrollbar",
    ) as HTMLElement | null;
    if (!scroller || !scrollbar) return null;
    const scrollerRect = scroller.getBoundingClientRect();
    const scrollbarRect = scrollbar.getBoundingClientRect();
    return {
      hidden: scrollbar.classList.contains("is-hidden"),
      rightGapPx: Math.abs(scrollerRect.right - scrollbarRect.right),
    };
  });
  expect(scrollbarMetrics).not.toBeNull();
  if (scrollbarMetrics) {
    expect(scrollbarMetrics.hidden).toBe(false);
    expect(scrollbarMetrics.rightGapPx).toBeLessThanOrEqual(4);
  }

  await scroller.evaluate((el) => {
    el.scrollTop = el.scrollHeight;
  });

  await composer.fill("line one\nline two\n");
  await scroller.evaluate((el) => {
    el.scrollTop = el.scrollHeight;
  });
  const layout = await page.evaluate(() => {
    const threadStack = document.querySelector(".wb-thread-stack") as HTMLElement | null;
    const composer = document.querySelector(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea") as HTMLTextAreaElement | null;
    if (!threadStack || !composer) return null;
    const stackRect = threadStack.getBoundingClientRect();
    const composerRect = composer.getBoundingClientRect();
    return {
      stackBottom: stackRect.bottom,
      composerTop: composerRect.top,
      composerBottom: composerRect.bottom,
      viewportHeight: window.innerHeight,
    };
  });
  expect(layout).not.toBeNull();
  if (layout) {
    expect(layout.stackBottom).toBeLessThanOrEqual(layout.composerTop + 1);
    expect(layout.composerBottom).toBeLessThanOrEqual(layout.viewportHeight + 1);
  }
  await expect
    .poll(async () => scroller.evaluate((el) => el.scrollHeight - (el.scrollTop + el.clientHeight)), {
      timeout: 10000,
    })
    .toBeLessThanOrEqual(8);
  await waitForSessionIdle(request, seedSessionId);
  await page.waitForTimeout(100);

  await page.evaluate(() => {
    const w = window as ComposerJankWindow;
    w.__composerJankSamples = [];
    w.__composerJankShiftEntries = [];
    w.__composerJankStart = performance.now();

    const scroller = document.querySelector(
      ".wb-session-slot[aria-hidden=\"false\"] .wb-thread-scroller",
    ) as HTMLElement | null;
    const composer = document.querySelector(
      ".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea",
    ) as HTMLTextAreaElement | null;
    if (!scroller || !composer) return;

    const recordSample = () => {
      w.__composerJankSamples.push({
        t: performance.now(),
        top: scroller.scrollTop,
        height: scroller.scrollHeight,
        clientHeight: scroller.clientHeight,
        bottomOffset: Math.max(0, scroller.scrollHeight - (scroller.scrollTop + scroller.clientHeight)),
      });
    };

    requestAnimationFrame(recordSample);
    composer.addEventListener("input", recordSample);
    w.__composerJankCleanup = () => {
      composer.removeEventListener("input", recordSample);
    };

    w.__composerJankObserver?.disconnect?.();
    const shiftObserver = new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        const shiftEntry = entry as PerformanceEntry & { value?: number; hadRecentInput?: boolean };
        w.__composerJankShiftEntries.push({
          startTime: shiftEntry.startTime,
          value: shiftEntry.value ?? 0,
          hadRecentInput: shiftEntry.hadRecentInput ?? false,
        });
      }
    });
    shiftObserver.observe({ type: "layout-shift", buffered: true });
    w.__composerJankObserver = shiftObserver;
  });

  await composer.focus();
  await page.keyboard.type("stable-input", { delay: 30 });
  await page.waitForTimeout(150);

  const metrics = await page.evaluate(() => {
    const w = window as ComposerJankWindow;
    w.__composerJankCleanup?.();
    w.__composerJankObserver?.disconnect?.();
    const samples = Array.isArray(w.__composerJankSamples) ? w.__composerJankSamples : [];
    const entries = Array.isArray(w.__composerJankShiftEntries) ? w.__composerJankShiftEntries : [];
    const startAt = Number(w.__composerJankStart ?? 0);
    const deltas: number[] = [];
    for (let i = 1; i < samples.length; i += 1) {
      const prev = samples[i - 1]?.top ?? 0;
      const next = samples[i]?.top ?? 0;
      deltas.push(next - prev);
    }
    const clsTotal = entries
      .filter((entry) => entry.startTime >= startAt)
      .reduce((sum, entry) => sum + entry.value, 0);
    const clsNoInput = entries
      .filter((entry) => entry.startTime >= startAt && !entry.hadRecentInput)
      .reduce((sum, entry) => sum + entry.value, 0);
    const maxDelta = deltas.reduce((max: number, value: number) => Math.max(max, Math.abs(value)), 0);
    return {
      samples,
      entries,
      deltas,
      clsTotal,
      clsNoInput,
      maxDelta,
    };
  });

  await testInfo.attach("composer-jank-metrics.json", {
    body: JSON.stringify(metrics, null, 2),
    contentType: "application/json",
  });

  expect(metrics.samples.length).toBeGreaterThan(5);
  expect(metrics.maxDelta).toBeLessThanOrEqual(2);
  expect(metrics.samples.every((sample) => sample.bottomOffset <= 8)).toBeTruthy();
  expect(metrics.clsTotal).toBeLessThan(0.02);
  expect(metrics.clsNoInput).toBeLessThan(0.001);
});

test("workbench: composer stays visible when expanding long messages", async ({ page, request }) => {
  test.setTimeout(120000);
  await page.setViewportSize({ width: 1400, height: 900 });

  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 6,
    messageBytes: { min: 2000, max: 2200 },
    messagePrefix: "composer-expand",
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1, { timeout: 20000 });
  await rows.first().click();

  const composer = page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea");
  await expect(composer).toBeVisible({ timeout: 20000 });

  const collapsedHeader = page.locator(".wb-turn-header[aria-expanded=\"false\"]").first();
  await expect(collapsedHeader).toBeVisible({ timeout: 20000 });
  const collapsedHeaderHandle = await collapsedHeader.elementHandle();
  if (!collapsedHeaderHandle) {
    throw new Error("Expected a collapsed turn header handle");
  }
  await collapsedHeaderHandle.evaluate((node) => {
    const element = node as HTMLElement;
    element.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true, button: 0 }));
    element.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, cancelable: true, button: 0 }));
    element.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true, button: 0 }));
  });
  await expect
    .poll(
      () => collapsedHeaderHandle.evaluate((node) => node.getAttribute("aria-expanded")),
      { timeout: 20000 },
    )
    .toBe("true");

  const layout = await page.evaluate(() => {
    const threadStack = document.querySelector(".wb-thread-stack") as HTMLElement | null;
    const composer = document.querySelector(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea") as HTMLTextAreaElement | null;
    if (!threadStack || !composer) return null;
    const stackRect = threadStack.getBoundingClientRect();
    const composerRect = composer.getBoundingClientRect();
    return {
      stackBottom: stackRect.bottom,
      composerTop: composerRect.top,
      composerBottom: composerRect.bottom,
      viewportHeight: window.innerHeight,
    };
  });

  expect(layout).not.toBeNull();
  if (layout) {
    expect(layout.stackBottom).toBeLessThanOrEqual(layout.composerTop + 1);
    expect(layout.composerBottom).toBeLessThanOrEqual(layout.viewportHeight + 1);
  }
});

test("workbench: short bottom-aligned thread stays pinned while composer gains a line", async ({ page, request }, testInfo) => {
  test.setTimeout(120000);
  await page.setViewportSize({ width: 1400, height: 900 });

  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 2,
    messageBytes: { min: 96, max: 128 },
    messagePrefix: "composer-short-thread",
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1, { timeout: 20000 });
  await rows.first().click();

  const composer = page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea");
  await expect(composer).toBeVisible({ timeout: 20000 });

  const scroller = page.locator(scrollSelector).first();
  await expect(scroller).toBeVisible({ timeout: 20000 });
  await expect
    .poll(async () => scroller.evaluate((el) => Math.max(0, el.scrollHeight - el.clientHeight)), {
      timeout: 10000,
    })
    .toBeLessThanOrEqual(2);

  const readShortThreadLayout = async () =>
    page.evaluate((selector) => {
      const scroller = document.querySelector(selector) as HTMLElement | null;
      if (!scroller) return null;
      const scrollerRect = scroller.getBoundingClientRect();
      const rows = Array.from(
        scroller.querySelectorAll<HTMLElement>("[data-pretext-virtualizer-row='1'][data-pretext-virtualizer-item-id]"),
      );
      const lastRow = rows[rows.length - 1] ?? null;
      const lastRowRect = lastRow?.getBoundingClientRect() ?? null;
      return {
        scrollTop: scroller.scrollTop,
        maxScrollTop: Math.max(0, scroller.scrollHeight - scroller.clientHeight),
        lastRowBottomGapPx: lastRowRect ? Math.max(0, scrollerRect.bottom - lastRowRect.bottom) : null,
        rowCount: rows.length,
      };
    }, scrollSelector);

  const initialLayout = await readShortThreadLayout();
  expect(initialLayout).not.toBeNull();
  if (!initialLayout) {
    throw new Error("expected initial short-thread layout");
  }
  expect(initialLayout.rowCount).toBeGreaterThan(0);
  expect(initialLayout.maxScrollTop).toBeLessThanOrEqual(2);
  expect(initialLayout.lastRowBottomGapPx).not.toBeNull();
  expect(Number(initialLayout.lastRowBottomGapPx)).toBeLessThanOrEqual(32);

  await composer.fill("line one\nline two");
  await composer.focus();
  await page.keyboard.press("End");
  await page.keyboard.press("Enter");
  await page.keyboard.type("line three", { delay: 20 });

  await expect
    .poll(async () => {
      const layout = await readShortThreadLayout();
      return layout?.maxScrollTop ?? 9999;
    }, { timeout: 10000 })
    .toBeLessThanOrEqual(2);

  await expect
    .poll(async () => {
      const layout = await readShortThreadLayout();
      return layout?.lastRowBottomGapPx ?? 9999;
    }, { timeout: 10000 })
    .toBeLessThanOrEqual(32);

  const finalLayout = await readShortThreadLayout();
  await testInfo.attach("composer-short-thread-layout.json", {
    body: JSON.stringify(finalLayout, null, 2),
    contentType: "application/json",
  });

  expect(finalLayout).not.toBeNull();
  if (finalLayout) {
    expect(finalLayout.scrollTop).toBe(0);
    expect(finalLayout.maxScrollTop).toBeLessThanOrEqual(2);
    expect(finalLayout.lastRowBottomGapPx).not.toBeNull();
    expect(Number(finalLayout.lastRowBottomGapPx)).toBeLessThanOrEqual(32);
  }
});
