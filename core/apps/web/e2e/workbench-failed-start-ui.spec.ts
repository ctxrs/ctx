import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";

test("workbench: failed start shows one bounded alert and does not mount a session composer", async ({ page }) => {
  test.setTimeout(120_000);

  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  await createWorkspaceAndOpenWorkbench({
    page,
    request: page.request,
    repo,
    workspaceName: `ws-${Date.now()}`,
  });
  await selectHarnessBySearch(page, "fake", /fake/i);

  let failedOnce = false;
  await page.route("**/api/workspaces/*/tasks", async (route) => {
    if (failedOnce) {
      await route.continue();
      return;
    }
    failedOnce = true;
    await route.fulfill({
      status: 400,
      contentType: "application/json",
      body: JSON.stringify({ error: "model_id must be a concrete model id" }),
    });
  });

  await page.locator("textarea.wb-composer-textarea").first().fill("hi");
  await page.getByRole("button", { name: "Send" }).click();

  const alert = page.getByRole("alert");
  await expect(alert).toContainText("Failed to start", { timeout: 20_000 });
  await expect(alert).toContainText("model_id must be a concrete model id", { timeout: 20_000 });
  await expect(page.getByText("Session not found in workspace snapshot")).toHaveCount(0);
  await expect(page.locator(".wb-session-slot textarea.wb-active-textarea")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Copy prompt" })).toBeVisible();
});
