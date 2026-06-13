import { expect, test } from "./fixtures";
import type { APIRequestContext } from "playwright/test";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

const scrollSelector = '.wb-session-slot[aria-hidden="false"] .wb-thread-scroller';
const sessionViewSelector = '.wb-session-slot[aria-hidden="false"] [data-testid="session-view"]';

async function addLongMessages(request: APIRequestContext, sessionId: string) {
  const longText = Array.from({ length: 220 }, (_, index) => `scroll ownership line ${index + 1}`).join("\n");
  for (let index = 0; index < 8; index += 1) {
    const response = await request.post(`/api/sessions/${sessionId}/messages`, {
      data: { content: `${longText}\nblock ${index + 1}`, delivery: "immediate" },
    });
    expect(response.ok()).toBeTruthy();
  }
}

async function readScrollOwnershipState(page: Parameters<typeof test>[0]["page"]) {
  return page.evaluate(({ composerSelector, scrollSelectorValue, sessionViewSelectorValue }) => {
    const composer = document.querySelector(composerSelector) as HTMLElement | null;
    const scroller = document.querySelector(scrollSelectorValue) as HTMLElement | null;
    const sessionView = document.querySelector(sessionViewSelectorValue) as HTMLElement | null;
    const scrollingElement = document.scrollingElement as HTMLElement | null;
    const composerRect = composer?.getBoundingClientRect() ?? null;
    return {
      windowScrollY: window.scrollY,
      documentScrollTop: scrollingElement?.scrollTop ?? 0,
      sessionViewScrollTop: sessionView?.scrollTop ?? 0,
      scrollerScrollTop: scroller?.scrollTop ?? 0,
      composerScrollTop: composer instanceof HTMLTextAreaElement ? composer.scrollTop : 0,
      composerTop: composerRect?.top ?? null,
      composerBottom: composerRect?.bottom ?? null,
    };
  }, {
    composerSelector: '.wb-session-slot[aria-hidden="false"] textarea.wb-active-textarea',
    scrollSelectorValue: scrollSelector,
    sessionViewSelectorValue: sessionViewSelector,
  });
}

function expectOuterScrollOwnersPinned(state: {
  windowScrollY: number;
  documentScrollTop: number;
  sessionViewScrollTop: number;
}) {
  expect(state.windowScrollY).toBe(0);
  expect(state.documentScrollTop).toBe(0);
  expect(state.sessionViewScrollTop).toBe(0);
}

test("workbench: composer wheel ownership never moves both the textarea and transcript for one gesture", async ({
  page,
  request,
}) => {
  test.setTimeout(120000);
  await page.setViewportSize({ width: 1440, height: 960 });

  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 0,
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1, { timeout: 20000 });
  await rows.first().click();

  const composer = page.locator('.wb-session-slot[aria-hidden="false"] textarea.wb-active-textarea');
  await expect(composer).toBeVisible({ timeout: 20000 });

  const sessionId = seed.sessionIdsByTask[seed.taskIds[0]][0];
  await addLongMessages(request, sessionId);
  await expect(page.locator(".wb-session")).toContainText("block 8", { timeout: 20000 });

  const scroller = page.locator(scrollSelector).first();
  await expect(scroller).toBeVisible({ timeout: 20000 });
  await expect
    .poll(async () => scroller.evaluate((element) => element.scrollHeight - element.clientHeight), {
      timeout: 10000,
    })
    .toBeGreaterThan(180);

  const overflowingComposerText = Array.from(
    { length: 40 },
    (_, index) => `composer overflow ${index + 1} ${"x".repeat(48)}`,
  ).join("\n");
  await composer.fill(overflowingComposerText);
  await expect
    .poll(
      async () =>
        composer.evaluate((element) => ({
          scrollHeight: element.scrollHeight,
          clientHeight: element.clientHeight,
        })),
      { timeout: 5000 },
    )
    .toMatchObject({});
  await expect
    .poll(async () => composer.evaluate((element) => element.clientHeight), { timeout: 5000 })
    .toBeGreaterThanOrEqual(200);
  await expect
    .poll(async () => composer.evaluate((element) => element.scrollHeight - element.clientHeight), {
      timeout: 5000,
    })
    .toBeGreaterThan(120);

  await scroller.evaluate((element) => {
    element.scrollTop = 720;
  });
  await composer.evaluate((element) => {
    element.scrollTop = 40;
  });

  const composerOwnedBefore = await readScrollOwnershipState(page);

  await composer.hover();
  await page.mouse.wheel(0, 180);

  await expect
    .poll(async () => composer.evaluate((element) => element.scrollTop), { timeout: 2000 })
    .toBeGreaterThan(composerOwnedBefore.composerScrollTop + 20);
  const composerOwnedAfter = await readScrollOwnershipState(page);
  expect(Math.abs(composerOwnedAfter.scrollerScrollTop - composerOwnedBefore.scrollerScrollTop)).toBeLessThanOrEqual(4);
  expect(Math.abs((composerOwnedAfter.composerTop ?? 0) - (composerOwnedBefore.composerTop ?? 0))).toBeLessThanOrEqual(1);
  expect(Math.abs((composerOwnedAfter.composerBottom ?? 0) - (composerOwnedBefore.composerBottom ?? 0))).toBeLessThanOrEqual(1);
  expectOuterScrollOwnersPinned(composerOwnedAfter);

  await scroller.evaluate((element) => {
    element.scrollTop = 720;
  });
  await composer.evaluate((element) => {
    element.scrollTop = 0;
  });

  const transcriptOwnedBefore = await readScrollOwnershipState(page);

  await composer.hover();
  await page.mouse.wheel(0, -220);

  await expect
    .poll(async () => scroller.evaluate((element) => element.scrollTop), { timeout: 2000 })
    .toBeLessThan(transcriptOwnedBefore.scrollerScrollTop - 20);
  const transcriptOwnedAfter = await readScrollOwnershipState(page);
  expect(transcriptOwnedAfter.composerScrollTop).toBeLessThanOrEqual(transcriptOwnedBefore.composerScrollTop + 1);
  expect(Math.abs((transcriptOwnedAfter.composerTop ?? 0) - (transcriptOwnedBefore.composerTop ?? 0))).toBeLessThanOrEqual(1);
  expect(Math.abs((transcriptOwnedAfter.composerBottom ?? 0) - (transcriptOwnedBefore.composerBottom ?? 0))).toBeLessThanOrEqual(1);
  expectOuterScrollOwnersPinned(transcriptOwnedAfter);

  await scroller.evaluate((element) => {
    element.scrollTop = 0;
  });
  const topBoundaryBefore = await readScrollOwnershipState(page);
  await scroller.hover();
  await page.mouse.wheel(0, -260);
  await page.waitForTimeout(150);
  const topBoundaryAfter = await readScrollOwnershipState(page);
  expect(topBoundaryAfter.scrollerScrollTop).toBeLessThanOrEqual(1);
  expect(Math.abs((topBoundaryAfter.composerTop ?? 0) - (topBoundaryBefore.composerTop ?? 0))).toBeLessThanOrEqual(1);
  expect(Math.abs((topBoundaryAfter.composerBottom ?? 0) - (topBoundaryBefore.composerBottom ?? 0))).toBeLessThanOrEqual(1);
  expectOuterScrollOwnersPinned(topBoundaryAfter);

  await scroller.evaluate((element) => {
    element.scrollTop = Math.max(0, element.scrollHeight - element.clientHeight);
    element.dispatchEvent(new Event("scroll"));
  });
  await page.waitForTimeout(50);
  const bottomBoundaryBefore = await readScrollOwnershipState(page);
  await scroller.hover();
  await page.mouse.wheel(0, 260);
  await page.waitForTimeout(150);
  const bottomBoundaryAfter = await readScrollOwnershipState(page);
  expect(Math.abs((bottomBoundaryAfter.composerTop ?? 0) - (bottomBoundaryBefore.composerTop ?? 0))).toBeLessThanOrEqual(1);
  expect(Math.abs((bottomBoundaryAfter.composerBottom ?? 0) - (bottomBoundaryBefore.composerBottom ?? 0))).toBeLessThanOrEqual(1);
  expectOuterScrollOwnersPinned(bottomBoundaryAfter);
});
