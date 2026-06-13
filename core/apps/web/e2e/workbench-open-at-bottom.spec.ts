import { test, expect } from "./fixtures";
import type { APIRequestContext } from "@playwright/test";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

const scrollSelector = ".wb-session-slot[aria-hidden=\"false\"] .wb-thread-scroller";

async function addLongMessages(request: APIRequestContext, sessionId: string) {
  const longText = Array.from({ length: 120 }, (_, i) => `fixture line ${i + 1}`).join("\n");
  for (let i = 0; i < 6; i++) {
    await request.post(`/api/sessions/${sessionId}/messages`, {
      data: { content: `${longText}\nblock ${i + 1}`, delivery: "immediate" },
    });
    await new Promise((r) => setTimeout(r, 25));
  }
  await new Promise((r) => setTimeout(r, 400));
}

async function expectAtBottom(page: import("playwright/test").Page) {
  const scroller = page.locator(scrollSelector).first();
  await expect
    .poll(async () => scroller.evaluate((el) => el.scrollHeight - (el.scrollTop + el.clientHeight)), {
      timeout: 10_000,
    })
    .toBeLessThanOrEqual(16);
}

test("workbench: opening a different session reopens at bottom", async ({ page, request }) => {
  const seed = await seedDummyWorkspace(request, {
    tasks: 2,
    sessionsPerTask: 1,
    turnsPerSession: 12,
    throttleMs: 5,
  });

  const [taskA, taskB] = seed.taskIds;
  const sessionA = seed.sessionIdsByTask[taskA]?.[0];
  const sessionB = seed.sessionIdsByTask[taskB]?.[0];
  expect(sessionA).toBeTruthy();
  expect(sessionB).toBeTruthy();

  await addLongMessages(request, sessionA);
  await addLongMessages(request, sessionB);

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(2);

  const taskRowA = rows.filter({ hasText: "fixture task 1" }).first();
  const taskRowB = rows.filter({ hasText: "fixture task 2" }).first();

  await taskRowA.click();
  await page.waitForSelector(scrollSelector);

  const scroller = page.locator(scrollSelector).first();
  await expect
    .poll(async () => scroller.evaluate((el) => el.scrollHeight), { timeout: 10_000 })
    .toBeGreaterThan(0);
  await expect
    .poll(async () => scroller.evaluate((el) => el.scrollHeight - el.clientHeight), { timeout: 10_000 })
    .toBeGreaterThan(400);
  await expectAtBottom(page);

  await scroller.evaluate((el) => {
    const maxTop = Math.max(0, el.scrollHeight - el.clientHeight);
    el.scrollTop = Math.max(0, maxTop - 500);
  });
  await page.waitForTimeout(300);

  const scrolledAwayTop = await scroller.evaluate((el) => el.scrollTop);
  expect(scrolledAwayTop).toBeGreaterThan(100);
  await expect
    .poll(async () => scroller.evaluate((el) => el.scrollHeight - (el.scrollTop + el.clientHeight)), {
      timeout: 10_000,
    })
    .toBeGreaterThan(200);

  await taskRowB.click();
  await expectAtBottom(page);
});
