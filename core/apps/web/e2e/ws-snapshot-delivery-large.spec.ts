import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

const isLargeScale = process.env.CTX_E2E_SCALE === "large";

test("ws: snapshot delivery stays stable for large workspace", async ({ page, request }) => {
  test.skip(!isLargeScale, "CTX_E2E_SCALE=large only");

  const seed = await seedDummyWorkspace(request, {
    tasks: 40,
    sessionsPerTask: 1,
    turnsPerSession: 6,
    throttleMs: 2,
    includeToolSummaries: true,
    toolSummariesPerTurn: 4,
    messageBytes: 2048,
  });

  const warnings: string[] = [];
  page.on("console", (msg) => {
    const text = msg.text();
    if (text.includes("Workspace active snapshot not received")) {
      warnings.push(text);
    }
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const search = page.getByTestId("workbench-task-search");
  await expect(search).toBeVisible({ timeout: 30_000 });
  await search.fill("fixture task 40");
  await expect(page.getByRole("listitem", { name: /fixture task 40/i })).toBeVisible({
    timeout: 30_000,
  });

  await page.waitForTimeout(4_000);
  expect(warnings).toEqual([]);
});
