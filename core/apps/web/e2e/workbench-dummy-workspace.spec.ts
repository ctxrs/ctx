import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

test("workbench: seed heavy dummy workspace fixture", async ({ page, request }) => {
  test.skip(
    !process.env.DUMMY_WORKSPACE_SEED,
    "Set DUMMY_WORKSPACE_SEED=1 to generate the heavy dummy workspace fixture.",
  );

  const turnsPerSession = Number(process.env.DUMMY_WORKSPACE_TURNS ?? "100");
  const seed = await seedDummyWorkspace(request, {
    tasks: 3,
    sessionsPerTask: { min: 4, max: 8 },
    turnsPerSession,
    throttleMs: 5,
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  await expect(page.locator(".wb-task-row")).toHaveCount(3, { timeout: 20000 });
});
