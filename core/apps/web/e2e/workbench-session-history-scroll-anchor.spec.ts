import { test, expect } from "./fixtures";
import type { APIRequestContext, Locator, Page } from "playwright/test";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

const scrollSelector = ".wb-thread-scroller";
const historySeedTurns = 72;
const oldestHistoryMessagePattern = /\bhistory scroll 1\b/;

type ScrollAnchor = { id?: string; offset: number };

async function scrollScrollerUp(
  scroller: Locator,
  deltaPx: number,
): Promise<number> {
  return scroller.evaluate((element, delta) => {
    element.scrollTop = Math.max(0, element.scrollTop - delta);
    element.dispatchEvent(new Event("scroll"));
    return element.scrollTop;
  }, deltaPx);
}

type ScrollerProbe = {
  top: number;
  maxTop: number;
  clientHeight: number;
  scrollHeight: number;
  renderedFirstIndex: string | null;
  renderedLastIndex: string | null;
  pendingProgrammatic: string | null;
  pendingRestore: string | null;
};

type VisibleThreadDebug = {
  projectionRev: number;
  listItemCount: number;
  firstItemId: string | null;
  lastItemId: string | null;
};

type SeedTranscriptTurn = {
  user: string;
  assistant: string;
};

const readEnvTrimmed = (key: string): string | null => {
  const value = process.env[key];
  if (typeof value !== "string") {
    return null;
  }
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
};

const resolveSeedSessionSource = () => {
  const providerId = readEnvTrimmed("CTX_E2E_SESSION_PROVIDER_ID");
  const modelId = readEnvTrimmed("CTX_E2E_SESSION_MODEL_ID");
  if (!providerId || !modelId) {
    return undefined;
  }

  const executionEnvironment =
    readEnvTrimmed("CTX_E2E_SESSION_EXECUTION_ENVIRONMENT") ?? "host";
  return {
    providerId,
    modelId,
    executionEnvironment,
  };
};

const shouldUseDevTranscriptSeed = () => readEnvTrimmed("CTX_E2E_USE_DEV_TRANSCRIPT_SEED") === "1";

async function apiPost<T>(request: APIRequestContext, url: string, data: unknown): Promise<T> {
  const response = await request.post(url, { data });
  expect(response.ok(), `request failed for ${url} (${response.status()})`).toBe(true);
  return (await response.json()) as T;
}

async function waitForSessionSnapshotReady(
  request: APIRequestContext,
  sessionId: string,
  options: { timeoutMs?: number; pollMs?: number } = {},
): Promise<void> {
  const timeoutMs = options.timeoutMs ?? 20_000;
  const pollMs = options.pollMs ?? 250;
  const deadline = Date.now() + timeoutMs;
  let lastStatus = "no response";
  while (Date.now() < deadline) {
    const response = await request.get(`/api/sessions/${sessionId}/snapshot?limit=1`);
    if (response.ok()) {
      return;
    }
    lastStatus = `${response.status()}`;
    await new Promise((resolve) => setTimeout(resolve, pollMs));
  }
  throw new Error(`session ${sessionId} snapshot was not ready before timeout (last status ${lastStatus})`);
}

const buildSeedTranscriptTurns = (
  count: number,
  options: {
    linePrefix: string;
    lineCount: number;
    markerPrefix: string;
    assistantPrefix: string;
  },
): SeedTranscriptTurn[] => {
  const longText = Array.from(
    { length: options.lineCount },
    (_, i) => `${options.linePrefix} ${i + 1}`,
  ).join("\n");
  return Array.from({ length: count }, (_, i) => {
    const index = i + 1;
    return {
      user: `${longText}\n${options.markerPrefix} ${index}`,
      assistant: `${options.assistantPrefix} ${options.markerPrefix} ${index}.`,
    };
  });
};

async function readTopVisibleStressTurn(scroller: Locator): Promise<number | null> {
  return scroller.evaluate((element) => {
    const scrollerRect = element.getBoundingClientRect();
    const items = Array.from(element.querySelectorAll<HTMLElement>('[role="listitem"]'));
    for (const item of items) {
      const rect = item.getBoundingClientRect();
      if (rect.bottom <= scrollerRect.top + 4) continue;
      if (rect.top >= scrollerRect.bottom - 4) continue;
      const match = item.textContent?.match(/fixture turn (\d+)/);
      if (match) {
        const parsed = Number.parseInt(match[1] ?? "", 10);
        return Number.isFinite(parsed) ? parsed : null;
      }
    }
    return null;
  });
}

async function readScrollerProbe(scroller: Locator): Promise<ScrollerProbe> {
  return scroller.evaluate((element) => ({
    top: Math.round(element.scrollTop),
    maxTop: Math.round(Math.max(0, element.scrollHeight - element.clientHeight)),
    clientHeight: Math.round(element.clientHeight),
    scrollHeight: Math.round(element.scrollHeight),
    renderedFirstIndex: element.getAttribute("data-pretext-virtualizer-rendered-first-index"),
    renderedLastIndex: element.getAttribute("data-pretext-virtualizer-rendered-last-index"),
    pendingProgrammatic: element.getAttribute("data-pretext-virtualizer-programmatic-pending"),
    pendingRestore: element.getAttribute("data-pretext-virtualizer-pending-restore"),
  }));
}

async function readVisibleThreadDebug(page: Page): Promise<VisibleThreadDebug> {
  return page.evaluate(() => {
    const debug = (
      window as Window & {
        __ctxE2E?: {
          getVisibleSessionThreadDebug?: () => {
            projectionRev: number;
            listItemIds: string[];
          };
        };
      }
    ).__ctxE2E?.getVisibleSessionThreadDebug?.();
    const listItemIds = Array.isArray(debug?.listItemIds) ? debug.listItemIds : [];
    return {
      projectionRev: typeof debug?.projectionRev === "number" ? debug.projectionRev : -1,
      listItemCount: listItemIds.length,
      firstItemId: listItemIds[0] ?? null,
      lastItemId: listItemIds.at(-1) ?? null,
    };
  });
}

async function forceHistoryRequestAtTop(
  page: Page,
  scroller: Locator,
  wasHistoryIntercepted: () => boolean,
): Promise<void> {
  const samples: string[] = [];
  for (let i = 0; i < 40; i += 1) {
    const before = await readScrollerProbe(scroller);
    const deltaPx = Math.max(before.clientHeight * 2, 1_200);
    const top = await scrollScrollerUp(scroller, deltaPx);
    await page.waitForTimeout(80);
    const after = await readScrollerProbe(scroller);
    samples.push(
      `${before.top}->${after.top} max=${after.maxTop} idx=${after.renderedFirstIndex}-${after.renderedLastIndex} prog=${after.pendingProgrammatic} restore=${after.pendingRestore}`,
    );
    if (wasHistoryIntercepted()) {
      break;
    }
    if (top <= 1) {
      await scroller.evaluate((el) => {
        el.scrollTop = 0;
        el.dispatchEvent(new Event("scroll"));
      });
      await page.waitForTimeout(60);
      if (wasHistoryIntercepted()) {
        break;
      }
    }
    if (i === 19) {
      await scroller.evaluate((el) => {
        el.scrollTop = 0;
        el.dispatchEvent(new Event("scroll"));
      });
      await page.waitForTimeout(80);
      if (wasHistoryIntercepted()) {
        break;
      }
    }
    if (i === 39) {
      throw new Error(
        `expected upward scroll to trigger a history request; samples: ${samples.join(" | ")}`,
      );
    }
  }

  await scroller.evaluate((el) => {
    el.scrollTop = 0;
    el.dispatchEvent(new Event("scroll"));
  });
  await expect
    .poll(() => scroller.evaluate((el) => el.scrollTop), {
      timeout: 5_000,
    })
    .toBeLessThanOrEqual(1);
}

async function addLongMessages(request: APIRequestContext, sessionId: string, count: number) {
  const longText = Array.from({ length: 200 }, (_, i) => `history line ${i + 1}`).join("\n");
  for (let i = 0; i < count; i++) {
    await request.post(`/api/sessions/${sessionId}/messages`, {
      data: { content: `${longText}\nhistory scroll ${i + 1}`, delivery: "immediate" },
    });
  }
}

async function addStressMessages(request: APIRequestContext, sessionId: string, count: number) {
  const longText = Array.from({ length: 240 }, (_, i) => `stress line ${i + 1}`).join("\n");
  for (let i = 0; i < count; i += 1) {
    await request.post(`/api/sessions/${sessionId}/messages`, {
      data: { content: `${longText}\nfixture turn ${i + 1}`, delivery: "immediate" },
    });
  }
}

async function openSeededSession(
  page: Page,
  request: APIRequestContext,
  options: {
    devTranscriptTurns?: SeedTranscriptTurn[];
    initialSessionText?: string;
  } = {},
): Promise<{ workspaceId: string; sessionId: string }> {
  const useDevTranscriptSeed =
    shouldUseDevTranscriptSeed() && (options.devTranscriptTurns?.length ?? 0) > 0;
  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: useDevTranscriptSeed ? 0 : 10,
    messageBytes: { min: 220, max: 320 },
    messagePrefix: "history-anchor",
    sessionSource: resolveSeedSessionSource(),
  });
  const taskId = seed.taskIds[0] ?? "";
  const sessionId = seed.sessionIdsByTask[taskId]?.[0] ?? "";
  expect(sessionId).not.toBe("");

  if (useDevTranscriptSeed) {
    await waitForSessionSnapshotReady(request, sessionId);
    await apiPost(request, `/api/dev/sessions/${sessionId}/seed_transcript`, {
      turns: options.devTranscriptTurns,
    });
  }

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });

  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1, { timeout: 20_000 });
  const focused = await page.evaluate(
    ({ taskId: currentTaskId, sessionId: currentSessionId }) => {
      const bridge = (
        window as Window & {
          __ctxE2E?: {
            focusTask?: (taskId: string, sessionId?: string | null) => boolean;
          };
        }
      ).__ctxE2E;
      return bridge?.focusTask?.(currentTaskId, currentSessionId) ?? false;
    },
    { taskId, sessionId },
  );
  if (!focused) {
    await rows.first().click();
  }
  await expect(page.locator("textarea.wb-active-textarea")).toBeVisible({
    timeout: 20_000,
  });
  await expect(page.locator(".wb-session")).toContainText(
    options.initialSessionText ?? "history-anchor",
    { timeout: 20_000 },
  );

  return { workspaceId: seed.workspaceId, sessionId };
}

test("workbench: preserves scroll position when prepending history", async ({ page, request }) => {
  test.setTimeout(120000);
  await page.setViewportSize({ width: 1200, height: 700 });
  const useDevTranscriptSeed = shouldUseDevTranscriptSeed();
  const historyTurns = buildSeedTranscriptTurns(historySeedTurns, {
    linePrefix: "history line",
    lineCount: 200,
    markerPrefix: "history scroll",
    assistantPrefix: "Recorded",
  });

  let historyRequestResolve: (() => void) | null = null;
  let historyReleaseResolve: (() => void) | null = null;
  let historyPayloadText: string | null = null;
  const historyRequestSeen = new Promise<void>((resolve) => {
    historyRequestResolve = resolve;
  });
  const historyRelease = new Promise<void>((resolve) => {
    historyReleaseResolve = resolve;
  });
  let historyIntercepted = false;
  await page.route(/\/api\/sessions\/[^/]+\/history/, async (route) => {
    if (historyIntercepted) {
      await route.continue();
      return;
    }
    historyIntercepted = true;
    const response = await route.fetch();
    const body = await response.json();
    historyPayloadText = JSON.stringify(body);
    historyRequestResolve?.();
    await historyRelease;
    await route.fulfill({ response, json: body });
  });
  const historyResponse = page.waitForResponse((response) => response.url().includes("/history"));

  const { workspaceId, sessionId } = await openSeededSession(page, request, {
    devTranscriptTurns: historyTurns,
    initialSessionText: useDevTranscriptSeed ? `history scroll ${historySeedTurns}` : "history-anchor",
  });
  if (!useDevTranscriptSeed) {
    await addLongMessages(request, sessionId, historySeedTurns);
    await page.goto(`/workspaces/${workspaceId}`, { waitUntil: "domcontentloaded" });
    const rows = page.locator(".wb-task-row");
    await expect(rows).toHaveCount(1, { timeout: 20000 });
    await rows.first().click();
    await expect(page.locator("textarea.wb-active-textarea")).toBeVisible({
      timeout: 20000,
    });
  }
  await expect(page.locator(".wb-session")).toContainText(`history scroll ${historySeedTurns}`, { timeout: 20000 });
  await expect(page.locator(".wb-session")).not.toContainText(oldestHistoryMessagePattern, {
    timeout: 1000,
  });

  const scroller = page.locator(scrollSelector).first();
  await expect(scroller).toBeVisible({ timeout: 20000 });

  await expect
    .poll(async () => scroller.evaluate((el) => (el.scrollHeight ?? 0) - (el.clientHeight ?? 0)), {
      timeout: 20000,
    })
    .toBeGreaterThan(100);

  await scroller.evaluate((el) => {
    el.scrollTop = Math.max(0, el.scrollHeight - el.clientHeight);
    el.dispatchEvent(new Event("scroll"));
  });
  await page.waitForTimeout(50);

  for (let i = 0; i < 10; i++) {
    await scrollScrollerUp(scroller, 160);
    await page.waitForTimeout(60);
  }

  let preTop = await scroller.evaluate((el) => el.scrollTop);
  if (preTop <= 0) {
    preTop = await scroller.evaluate((el) => {
      const max = Math.max(0, el.scrollHeight - el.clientHeight);
      const next = Math.max(1, Math.floor(max / 2));
      el.scrollTop = next;
      el.dispatchEvent(new Event("scroll"));
      return el.scrollTop;
    });
  }
  expect(preTop).toBeGreaterThan(0);

  const captureAnchor = async (): Promise<ScrollAnchor | null> => {
    return page.evaluate((selector) => {
      const scrollerEl = document.querySelector(selector);
      if (!scrollerEl) return null;
      const scrollerRect = scrollerEl.getBoundingClientRect();
      const items = Array.from(scrollerEl.querySelectorAll('[role="listitem"]'));
      for (const item of items) {
        const rect = item.getBoundingClientRect();
        if (rect.bottom <= scrollerRect.top + 4) continue;
        const anchorEl = item.querySelector("[data-thread-item-id]") as HTMLElement | null;
        const itemId = anchorEl?.getAttribute("data-thread-item-id");
        if (!itemId) continue;
        return { id: itemId, offset: rect.top - scrollerRect.top };
      }
      return null;
    }, scrollSelector);
  };

  let atTop = false;
  for (let i = 0; i < 60; i++) {
    const top = await scrollScrollerUp(scroller, 200);
    await page.waitForTimeout(60);
    if (top <= 1) {
      atTop = true;
      break;
    }
  }
  if (!atTop) {
    await scroller.evaluate((el) => {
      el.scrollTop = 0;
      el.dispatchEvent(new Event("scroll"));
    });
  }
  await historyRequestSeen;
  await page.waitForTimeout(50);
  const anchorBefore = await captureAnchor();
  expect(anchorBefore).not.toBeNull();
  const anchorId = anchorBefore?.id;
  expect(anchorId).toBeTruthy();
  historyReleaseResolve?.();

  await historyResponse;
  expect(historyPayloadText).toContain("history scroll 1");
  await page.waitForTimeout(100);

  const anchorAfter = await page.evaluate(
    ({ selector, itemId }) => {
      const scrollerEl = document.querySelector(selector);
      if (!scrollerEl) return null;
      const scrollerRect = scrollerEl.getBoundingClientRect();
      const anchorEl = scrollerEl.querySelector(`[data-thread-item-id=\"${itemId}\"]`) as HTMLElement | null;
      const item = anchorEl?.closest('[role="listitem"]') as HTMLElement | null;
      if (!item) return null;
      const rect = item.getBoundingClientRect();
      return rect.top - scrollerRect.top;
    },
    { selector: scrollSelector, itemId: anchorId },
  );
  expect(anchorAfter).not.toBeNull();
  expect(Math.abs((anchorAfter as number) - anchorBefore.offset)).toBeLessThanOrEqual(25);

  await expect
    .poll(async () => {
      await scroller.evaluate((element) => {
        element.scrollTop = 0;
        element.dispatchEvent(new Event("scroll"));
      });
      return (await scroller.evaluate((el) => Math.round(el.scrollTop))) <= 1;
    }, {
      timeout: 5_000,
      intervals: [100, 200, 400, 600],
    })
    .toBe(true);
});

test("workbench: downward wheel after reaching the top is not pulled back upward by a delayed history prepend", async ({
  page,
  request,
}) => {
  test.setTimeout(120000);
  await page.setViewportSize({ width: 1200, height: 700 });
  const useDevTranscriptSeed = shouldUseDevTranscriptSeed();
  const historyTurns = buildSeedTranscriptTurns(historySeedTurns, {
    linePrefix: "history line",
    lineCount: 200,
    markerPrefix: "history scroll",
    assistantPrefix: "Recorded",
  });

  let historyRequestResolve: (() => void) | null = null;
  let historyReleaseResolve: (() => void) | null = null;
  const historyRequestSeen = new Promise<void>((resolve) => {
    historyRequestResolve = resolve;
  });
  const historyRelease = new Promise<void>((resolve) => {
    historyReleaseResolve = resolve;
  });
  let historyRequestCount = 0;
  let historyIntercepted = false;
  await page.route(/\/api\/sessions\/[^/]+\/history/, async (route) => {
    historyRequestCount += 1;
    if (historyRequestCount > 1) {
      await route.continue();
      return;
    }
    historyIntercepted = true;
    const response = await route.fetch();
    const body = await response.json();
    historyRequestResolve?.();
    await historyRelease;
    await route.fulfill({ response, json: body });
  });
  const historyResponse = page.waitForResponse((response) => response.url().includes("/history"));

  const { workspaceId, sessionId } = await openSeededSession(page, request, {
    devTranscriptTurns: historyTurns,
    initialSessionText: useDevTranscriptSeed ? `history scroll ${historySeedTurns}` : "history-anchor",
  });
  if (!useDevTranscriptSeed) {
    await addLongMessages(request, sessionId, historySeedTurns);
    await page.goto(`/workspaces/${workspaceId}`, { waitUntil: "domcontentloaded" });
    const rows = page.locator(".wb-task-row");
    await expect(rows).toHaveCount(1, { timeout: 20000 });
    await rows.first().click();
    await expect(page.locator("textarea.wb-active-textarea")).toBeVisible({
      timeout: 20000,
    });
  }
  await expect(page.locator(".wb-session")).toContainText(`history scroll ${historySeedTurns}`, { timeout: 20000 });
  await expect(page.locator(".wb-session")).not.toContainText(oldestHistoryMessagePattern, {
    timeout: 1000,
  });

  const scroller = page.locator(scrollSelector).first();
  await expect(scroller).toBeVisible({ timeout: 20000 });

  await scroller.evaluate((el) => {
    el.scrollTop = Math.max(0, el.scrollHeight - el.clientHeight);
    el.dispatchEvent(new Event("scroll"));
  });
  await page.waitForTimeout(50);

  for (let i = 0; i < 10; i += 1) {
    await scrollScrollerUp(scroller, 160);
    await page.waitForTimeout(60);
  }

  let preTop = await scroller.evaluate((el) => el.scrollTop);
  if (preTop <= 0) {
    preTop = await scroller.evaluate((el) => {
      const max = Math.max(0, el.scrollHeight - el.clientHeight);
      const next = Math.max(1, Math.floor(max / 2));
      el.scrollTop = next;
      el.dispatchEvent(new Event("scroll"));
      return el.scrollTop;
    });
  }
  expect(preTop).toBeGreaterThan(0);

  await forceHistoryRequestAtTop(page, scroller, () => historyIntercepted);
  await historyRequestSeen;

  const topBeforeDownWheel = await scroller.evaluate((el) => el.scrollTop);
  expect(topBeforeDownWheel).toBeLessThanOrEqual(1);

  await scroller.hover();
  await scroller.click({ position: { x: 24, y: 24 } });
  await page.mouse.wheel(0, 260);
  await page.waitForTimeout(120);

  const scrollTopAfterDownWheel = await scroller.evaluate((el) => el.scrollTop);
  expect(
    scrollTopAfterDownWheel,
    "expected downward wheel to move away from the top before history resolves",
  ).toBeGreaterThan(24);

  historyReleaseResolve?.();
  await historyResponse;

  const postReleaseSamples: number[] = [];
  for (let i = 0; i < 10; i += 1) {
    await page.waitForTimeout(80);
    postReleaseSamples.push(await scroller.evaluate((el) => el.scrollTop));
  }

  expect(
    Math.min(...postReleaseSamples),
    `history prepend should not yank scrollTop back toward the top after a downward wheel: ${postReleaseSamples.join(", ")}`,
  ).toBeGreaterThanOrEqual(scrollTopAfterDownWheel - 24);

  await page.waitForTimeout(250);
  expect(historyRequestCount, "downward reversal after the top edge should not trigger an immediate extra history fetch").toBe(1);
});

test("workbench: upward wheel stays monotonic when delayed history prepends land mid-scroll", async ({
  page,
  request,
}) => {
  test.setTimeout(120000);
  await page.setViewportSize({ width: 1200, height: 700 });
  const useDevTranscriptSeed = shouldUseDevTranscriptSeed();
  const stressTurns = buildSeedTranscriptTurns(180, {
    linePrefix: "stress line",
    lineCount: 240,
    markerPrefix: "fixture turn",
    assistantPrefix: "Recorded",
  });

  let historyIntercepted = false;
  let historyReleaseResolve: (() => void) | null = null;
  const historyRelease = new Promise<void>((resolve) => {
    historyReleaseResolve = resolve;
  });
  await page.route(/\/api\/sessions\/[^/]+\/history/, async (route) => {
    if (historyIntercepted) {
      await route.continue();
      return;
    }
    historyIntercepted = true;
    const response = await route.fetch();
    const body = await response.json();
    await historyRelease;
    await route.fulfill({ response, json: body });
  });

  const { workspaceId, sessionId } = await openSeededSession(page, request, {
    devTranscriptTurns: stressTurns,
    initialSessionText: useDevTranscriptSeed ? "fixture turn 180" : "history-anchor",
  });
  if (!useDevTranscriptSeed) {
    await addStressMessages(request, sessionId, 180);
    await page.goto(`/workspaces/${workspaceId}`, { waitUntil: "domcontentloaded" });
    const rows = page.locator(".wb-task-row");
    await expect(rows).toHaveCount(1, { timeout: 20_000 });
    await rows.first().click();
    await expect(page.locator("textarea.wb-active-textarea")).toBeVisible({
      timeout: 20_000,
    });
  }
  await expect(page.locator(".wb-session")).toContainText("fixture turn 180", { timeout: 20_000 });

  const scroller = page.locator(scrollSelector).first();
  await expect(scroller).toBeVisible({ timeout: 20000 });
  await page.waitForTimeout(4000);
  await scroller.evaluate((el) => {
    const maxTop = Math.max(0, el.scrollHeight - el.clientHeight);
    const targetTop = maxTop > 1400 ? 1200 : Math.max(0, Math.floor(maxTop / 2));
    el.scrollTop = targetTop;
    el.dispatchEvent(new Event("scroll"));
  });
  await page.waitForTimeout(200);

  await scroller.hover();

  const samples: Array<{
    step: number;
    top: number;
    maxTop: number;
    historyIntercepted: boolean;
    postInterceptSteps: number;
    topVisibleTurn: number | null;
    projectionRev: number;
    listItemCount: number;
    firstItemId: string | null;
    lastItemId: string | null;
  }> = [];
  let postInterceptSteps = 0;
  let releaseStep: number | null = null;

  for (let step = 1; step <= 80; step += 1) {
    await page.mouse.wheel(0, -200);
    await page.waitForTimeout(100);
    const probe = await readScrollerProbe(scroller);
    const topVisibleTurn = await readTopVisibleStressTurn(scroller);
    const threadDebug = await readVisibleThreadDebug(page);
    if (historyIntercepted) {
      postInterceptSteps += 1;
      if (postInterceptSteps === 6) {
        releaseStep = step;
        historyReleaseResolve?.();
      }
    }
    samples.push({
      step,
      top: probe.top,
      maxTop: probe.maxTop,
      historyIntercepted,
      postInterceptSteps,
      topVisibleTurn,
      projectionRev: threadDebug.projectionRev,
      listItemCount: threadDebug.listItemCount,
      firstItemId: threadDebug.firstItemId,
      lastItemId: threadDebug.lastItemId,
    });

    if (releaseStep != null && postInterceptSteps >= 20) {
      break;
    }
  }

  expect(historyIntercepted, "expected sustained upward wheel scrolling to trigger older-history loading").toBe(true);
  expect(releaseStep, "expected to release the delayed history page after several more upward wheel steps").not.toBeNull();

  const turnProgressSamples = samples
    .filter((sample) => sample.step > (releaseStep ?? 0) && sample.topVisibleTurn != null);
  expect(turnProgressSamples.length, "expected numeric turn labels while wheeling through delayed history").toBeGreaterThan(3);

  expect(
    turnProgressSamples.every((sample, index) => {
      if (index === 0) return true;
      const previousTurn = turnProgressSamples[index - 1]?.topVisibleTurn ?? null;
      const currentTurn = sample.topVisibleTurn ?? null;
      if (previousTurn == null || currentTurn == null) return true;
      return currentTurn <= previousTurn + 2;
    }),
    `expected delayed history prepends to avoid jumping forward to newer turns while wheeling upward; samples=${samples.map((sample) => `${sample.step}:${sample.top}/${sample.maxTop}:${sample.topVisibleTurn ?? "na"}:${sample.historyIntercepted ? "h" : "-"}:${sample.postInterceptSteps}:rev${sample.projectionRev}:n${sample.listItemCount}:${sample.firstItemId ?? "na"}:${sample.lastItemId ?? "na"}`).join(" | ")}`,
  ).toBe(true);
});
