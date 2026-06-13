import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

test("workbench: background updates land for non-visible active sessions", async ({ page, request }) => {
  const seed = await seedDummyWorkspace(request, {
    tasks: 2,
    sessionsPerTask: 1,
    turnsPerSession: 1,
    throttleMs: 5,
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  const activeSessionView = page.locator(".wb-session-slot[aria-hidden=\"false\"]");
  await expect(rows).toHaveCount(2);

  const taskOne = rows.filter({ hasText: "fixture task 1" }).first();
  const taskTwo = rows.filter({ hasText: "fixture task 2" }).first();

  await taskOne.click();

  const taskId = seed.taskIds[1];
  const sessionId = seed.sessionIdsByTask[taskId][0];
  const msg = `background-${Date.now()}`;
  const resp = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: { content: msg, delivery: "immediate" },
  });
  expect(resp.ok()).toBeTruthy();

  await page.waitForTimeout(300);
  await taskTwo.click();

  await expect(activeSessionView).toContainText(`done: ${msg}`, { timeout: 20000 });
});
