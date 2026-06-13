import { expect, test } from "./fixtures";
import type { APIRequestContext, Locator, Page, Route } from "@playwright/test";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

const scrollSelector = ".wb-thread-scroller";

async function postMessage(request: APIRequestContext, sessionId: string, content: string) {
  const response = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: { content, delivery: "immediate" },
  });
  expect(response.ok()).toBeTruthy();
}

async function seedReferenceLikeHistory(request: APIRequestContext, sessionId: string) {
  const giantA = [
    "<transcript>",
    "# Reference",
    "",
    ...Array.from({ length: 2050 }, (_, index) => `reference line ${index + 1}`),
    "",
    "</transcript>",
  ].join("\n");
  const giantB = Array.from({ length: 1160 }, (_, index) => `history line ${index + 1}`).join("\n");
  await postMessage(request, sessionId, giantA);
  await postMessage(request, sessionId, giantB);

  const longText = Array.from({ length: 220 }, (_, index) => `scroll line ${index + 1}`).join("\n");
  for (let index = 0; index < 96; index += 1) {
    await postMessage(request, sessionId, `${longText}\nreference scroll ${index + 1}`);
  }
}

async function scrollUpUntilHistoryRequest(
  page: Page,
  scroller: Locator,
  wasHistoryIntercepted: () => boolean,
) {
  for (let index = 0; index < 60; index += 1) {
    await scroller.evaluate((element) => {
      element.scrollTop = Math.max(0, element.scrollTop - Math.max(element.clientHeight * 2, 1200));
      element.dispatchEvent(new Event("scroll"));
    });
    await page.waitForTimeout(70);
    if (wasHistoryIntercepted()) {
      return;
    }
  }
  throw new Error("expected upward scroll to trigger a history request");
}

async function readScrollerSnapshot(scroller: Locator) {
  return scroller.evaluate((element) => ({
    top: Math.round(element.scrollTop),
    maxTop: Math.round(Math.max(0, element.scrollHeight - element.clientHeight)),
    scrollHeight: Math.round(element.scrollHeight),
    renderedFirstIndex: element.getAttribute("data-pretext-virtualizer-rendered-first-index"),
    renderedLastIndex: element.getAttribute("data-pretext-virtualizer-rendered-last-index"),
  }));
}

async function openSeededReferenceSession(page: Page, request: APIRequestContext) {
  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 10,
    messageBytes: { min: 180, max: 260 },
    messagePrefix: "reference-freeze",
  });

  const taskId = seed.taskIds[0];
  const sessionId = seed.sessionIdsByTask[taskId]?.[0] ?? "";
  expect(sessionId).toBeTruthy();
  await seedReferenceLikeHistory(request, sessionId);

  await page.goto(`/workspaces/${seed.workspaceId}?desktop_ui=1`, { waitUntil: "domcontentloaded" });
  await expect(page.locator(".wb-task-row")).toHaveCount(1, { timeout: 20000 });
  await page.locator(".wb-task-row").first().click();
  const scroller = page.locator(scrollSelector).first();
  await expect(scroller).toBeVisible({ timeout: 30000 });
  await expect(page.locator("textarea.wb-active-textarea")).toBeVisible({ timeout: 30000 });
  return { seed, sessionId, scroller };
}

test("workbench: older history with giant user messages stays responsive", async ({ page, request }) => {
  test.setTimeout(180000);
  await page.setViewportSize({ width: 1440, height: 960 });

  const { sessionId, scroller } = await openSeededReferenceSession(page, request);

  const historyUrlPattern = `**/api/sessions/${sessionId}/history**`;
  let historyIntercepted = false;
  let crashDetected = false;
  page.on("crash", () => {
    crashDetected = true;
  });
  const routeHandler = async (route: Route) => {
    historyIntercepted = true;
    await new Promise((resolve) => setTimeout(resolve, 250));
    await route.continue();
  };
  await page.route(historyUrlPattern, routeHandler);

  const historyResponse = page.waitForResponse(
    (response) => response.url().includes(`/api/sessions/${sessionId}/history`),
    { timeout: 20000 },
  );

  const before = await readScrollerSnapshot(scroller);
  await scrollUpUntilHistoryRequest(page, scroller, () => historyIntercepted);
  await historyResponse;
  await page.waitForTimeout(500);

  await expect
    .poll(
      async () => {
        const snapshot = await readScrollerSnapshot(scroller);
        if (!snapshot.renderedFirstIndex || !snapshot.renderedLastIndex) {
          return -1;
        }
        return snapshot.scrollHeight;
      },
      {
        timeout: 15000,
      },
    )
    .toBeGreaterThanOrEqual(before.scrollHeight);

  await scroller.evaluate((element) => {
    element.scrollTop = Math.max(0, element.scrollTop - 600);
    element.dispatchEvent(new Event("scroll"));
  });

  await expect
    .poll(async () => (await readScrollerSnapshot(scroller)).top, {
      timeout: 10000,
    })
    .toBeGreaterThanOrEqual(0);
  const finalSnapshot = await readScrollerSnapshot(scroller);

  expect(crashDetected).toBe(false);
  expect(finalSnapshot.scrollHeight).toBeGreaterThanOrEqual(before.scrollHeight);

  await page.unroute(historyUrlPattern, routeHandler);
});

test("workbench: expanding a giant user reference header stays responsive", async ({ page, request }) => {
  test.setTimeout(180000);
  await page.setViewportSize({ width: 1440, height: 960 });

  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 0,
    messagePrefix: "reference-expand",
  });
  const taskId = seed.taskIds[0];
  const sessionId = seed.sessionIdsByTask[taskId]?.[0] ?? "";
  expect(sessionId).toBeTruthy();

  const giantReference = [
    "# Reference",
    "",
    ...Array.from({ length: 2200 }, (_, index) => `reference line ${index + 1}`),
  ].join("\n");
  await postMessage(request, sessionId, giantReference);

  await page.goto(`/workspaces/${seed.workspaceId}?desktop_ui=1`, { waitUntil: "domcontentloaded" });
  await expect(page.locator(".wb-task-row")).toHaveCount(1, { timeout: 20000 });
  await page.locator(".wb-task-row").first().click();
  const scroller = page.locator(scrollSelector).first();
  await expect(scroller).toBeVisible({ timeout: 30000 });
  await expect(page.locator("textarea.wb-active-textarea")).toBeVisible({ timeout: 30000 });

  let crashDetected = false;
  page.on("crash", () => {
    crashDetected = true;
  });

  const header = page.locator(".wb-turn-header").first();
  await expect(header).toBeVisible({ timeout: 20000 });
  await expect(header).toHaveAttribute("aria-expanded", "false");

  const before = await readScrollerSnapshot(scroller);
  await header.click();

  await expect(header).toHaveAttribute("aria-expanded", "true", { timeout: 10000 });
  await expect
    .poll(async () => (await readScrollerSnapshot(scroller)).scrollHeight, {
      timeout: 10000,
    })
    .toBeGreaterThanOrEqual(before.scrollHeight);
  expect(crashDetected).toBe(false);
});
