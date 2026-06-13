import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";

test("workbench: second message gets a response", async ({ page }) => {
  test.setTimeout(120000);
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceName = `ws-${Date.now()}`;

  await createWorkspaceAndOpenWorkbench({ page, request: page.request, repo, workspaceName });

  // Choose Fake harness so the test doesn't depend on external agents.
  await selectHarnessBySearch(page, "fake", /fake/i);

  await page.locator("textarea.wb-composer-textarea").first().fill("hello 1");
  await page.getByRole("button", { name: "Send" }).click();
  const assistantEntries = page.locator(".wb-session-slot .wb-assistant-entry");
  await expect(assistantEntries.filter({ hasText: "done: hello 1" })).toBeVisible({ timeout: 60000 });

  const sessionComposer = page.locator(".wb-session-slot textarea.wb-active-textarea");
  await expect(sessionComposer).toBeVisible({ timeout: 20000 });
  await sessionComposer.fill("hello 2");
  await expect(page.locator(".wb-session-slot button[aria-label=\"Send\"]")).toBeEnabled({ timeout: 20000 });
  await page.locator(".wb-session-slot button[aria-label=\"Send\"]").click();
  await expect(assistantEntries.filter({ hasText: "done: hello 2" })).toBeVisible({ timeout: 60000 });
});
