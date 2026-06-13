import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

test("workbench: rename task keeps focus and persists", async ({ page, request }) => {
  const seed = await seedDummyWorkspace(request, {
    tasks: 2,
    sessionsPerTask: 1,
    turnsPerSession: 1,
    throttleMs: 0,
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(2);

  const firstTaskId = seed.taskIds[0];
  const firstSessionId = seed.sessionIdsByTask[firstTaskId]?.[0] ?? "";

  const firstRow = rows.nth(0);
  const secondRow = rows.nth(1);

  await firstRow.hover();
  await firstRow.locator(".wb-task-menu-trigger").click();
  await page.locator(".wb-menu.wb-task-menu").getByRole("menuitem", { name: "Rename Task" }).click();

  const renameInput = firstRow.locator("input.wb-task-rename");
  await expect(renameInput).toBeFocused();

  if (firstSessionId) {
    await request.post(`/api/sessions/${firstSessionId}/messages`, {
      data: { content: `background update ${Date.now()}`, delivery: "immediate" },
    });
  }

  const selection = await renameInput.evaluate((el) => ({
    start: el.selectionStart,
    end: el.selectionEnd,
    value: el.value,
  }));
  expect(selection.start).toBe(0);
  expect(selection.end).toBe(selection.value.length);

  await page.waitForTimeout(2000);
  await expect(renameInput).toBeFocused();

  const renamedTitle = `renamed task ${Date.now()}`;
  await renameInput.fill(renamedTitle);
  await renameInput.press("Enter");

  await expect(firstRow.locator(".wb-task-title")).toHaveText(renamedTitle);

  await secondRow.click();
  await firstRow.click();
  await expect(firstRow.locator(".wb-task-title")).toHaveText(renamedTitle);

  await page.reload({ waitUntil: "domcontentloaded" });
  await expect(page.locator(".wb-task-row").nth(0).locator(".wb-task-title")).toHaveText(renamedTitle);

  const renamedTitle2 = `renamed again ${Date.now()}`;
  const refreshedFirstRow = page.locator(".wb-task-row").nth(0);
  await refreshedFirstRow.hover();
  await refreshedFirstRow.locator(".wb-task-menu-trigger").click();
  await page.locator(".wb-menu.wb-task-menu").getByRole("menuitem", { name: "Rename Task" }).click();
  const renameInput2 = refreshedFirstRow.locator("input.wb-task-rename");
  await expect(renameInput2).toBeFocused();
  await renameInput2.fill(renamedTitle2);
  await page.locator(".wb-task-row").nth(1).click();

  await expect(refreshedFirstRow.locator(".wb-task-title")).toHaveText(renamedTitle2);
});
