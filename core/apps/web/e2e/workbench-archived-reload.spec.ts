import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

test("workbench: archived tasks load after reload", async ({ page, request }) => {
  await page.route("**/api/updates/check**", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        channel: "stable",
        current_version: "1.0.0",
        latest_version: "1.0.0",
        update_available: false,
      }),
    });
  });

  const seed = await seedDummyWorkspace(request, {
    tasks: 2,
    sessionsPerTask: 0,
    turnsPerSession: 0,
    throttleMs: 0,
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  await expect(page.locator(".wb-main")).toBeVisible({ timeout: 20_000 });

  for (const taskId of seed.taskIds) {
    const resp = await request.post(`/api/tasks/${taskId}/archive`, {});
    expect(resp.ok()).toBeTruthy();
  }

  await page.reload({ waitUntil: "domcontentloaded" });
  await expect(page.locator(".wb-main")).toBeVisible({ timeout: 20_000 });
  const archivedToggle = page.getByRole("button", { name: "Archived Tasks" });
  await archivedToggle.waitFor({ timeout: 10000 });
  const expanded = await archivedToggle.getAttribute("aria-expanded");
  if (expanded === "true") {
    await archivedToggle.click();
  }
  await archivedToggle.click();

  await expect(page.locator(".wb-task-row-archived", { hasText: "fixture task 1" })).toBeVisible({
    timeout: 20000,
  });
});
