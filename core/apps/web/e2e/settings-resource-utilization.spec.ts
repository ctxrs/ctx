import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

test.describe.serial("settings: resource utilization", () => {
  let workspaceId = "";

  test.beforeAll(async ({ request }) => {
    const seed = await seedDummyWorkspace(request, {
      tasks: 0,
      sessionsPerTask: 0,
      turnsPerSession: 0,
    });
    workspaceId = seed.workspaceId;
  });

  test("renders the resource utilization section", async ({ page }) => {
    await page.goto(`/settings?ws=${workspaceId}#resource_utilization`, {
      waitUntil: "domcontentloaded",
    });

    await expect(page.locator(".settings-main-title")).toHaveText("Resource Utilization");
    await expect(page.locator(".settings-metrics-grid")).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(".settings-workspace-title")).toBeVisible();

    await page.waitForTimeout(1000);

    await page.screenshot({
      path: test.info().outputPath("resource-utilization.png"),
      fullPage: true,
    });
  });
});
