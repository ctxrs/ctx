import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

type E2EWindow = Window & {
  __ctxE2E?: {
    getWorkspaceSnapshot?: () => {
      archivedRev: number;
      totalArchived: number;
      activeIds: string[];
      archivedIds: string[];
      tasksById: Record<string, { task?: { archived_at?: string | null } }>;
    } | null;
  };
};

test("workbench: unarchived task returns to active list immediately", async ({ page, request }) => {
  const seed = await seedDummyWorkspace(request, {
    tasks: 2,
    sessionsPerTask: 0,
    turnsPerSession: 0,
    throttleMs: 0,
  });

  const archivedTaskId = seed.taskIds[0];
  const archivedTitle = "fixture task 1";
  const activeTitle = "fixture task 2";

  const archiveResp = await request.post(`/api/tasks/${archivedTaskId}/archive`, {});
  expect(archiveResp.ok()).toBeTruthy();

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });

  const archivedToggle = page.getByRole("button", { name: "Archived Tasks" });
  const archivedRow = page.locator(".wb-task-row-archived").filter({ hasText: archivedTitle });
  const activeRow = page.locator(".wb-task-row:not(.wb-task-row-archived)").filter({ hasText: archivedTitle });

  await expect(archivedToggle).toBeVisible({ timeout: 20_000 });
  await expect(
    page.locator(".wb-task-row:not(.wb-task-row-archived)").filter({ hasText: activeTitle }),
  ).toBeVisible({ timeout: 20_000 });
  if ((await archivedToggle.getAttribute("aria-expanded")) !== "true") {
    await archivedToggle.click();
  }

  await expect(archivedRow).toBeVisible({ timeout: 20_000 });
  await expect(activeRow).toHaveCount(0);

  await archivedRow.hover();
  await archivedRow.getByRole("button", { name: "Unarchive" }).click();

  await expect(activeRow).toBeVisible({ timeout: 20_000 });
  await expect(archivedRow).toHaveCount(0);

  await expect
    .poll(
      async () =>
        page.evaluate((taskId) => {
          const snapshot = (window as E2EWindow).__ctxE2E?.getWorkspaceSnapshot?.();
          if (!snapshot) return false;
          return (
            snapshot.archivedRev >= 2 &&
            snapshot.totalArchived === 0 &&
            snapshot.activeIds.includes(taskId) &&
            !snapshot.archivedIds.includes(taskId) &&
            (snapshot.tasksById[taskId]?.task?.archived_at ?? null) === null
          );
        }, archivedTaskId),
      { timeout: 20_000 },
    )
    .toBe(true);

  await expect(activeRow).toBeVisible();
  await expect(archivedRow).toHaveCount(0);
});
