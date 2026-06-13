import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";

test("workbench: task switching never desyncs selection (no URL state)", async ({ page }) => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceName = `ws-${Date.now()}`;

  const workspaceId = await createWorkspaceAndOpenWorkbench({ page, request: page.request, repo, workspaceName });

  // Choose Fake harness so the test doesn't depend on external agents.
  await selectHarnessBySearch(page, "fake", /fake/i);
  await expect(page.locator('button[title="Agents"] .wb-switcher-label').first()).toHaveText(/fake/i, {
    timeout: 20000,
  });

  const activeThread = page.locator(".wb-session-slot .wb-thread-scroller");

  const msg1 = `task one marker ${Date.now()}`;
  await page.locator("textarea.wb-composer-textarea").first().fill(msg1);
  await page.getByRole("button", { name: "Send" }).click();
  await expect(activeThread).toContainText(msg1, { timeout: 20000 });
  const url1 = new URL(page.url());
  expect(url1.searchParams.get("task")).toBeNull();
  expect(url1.searchParams.get("track")).toBeNull();
  expect(url1.searchParams.get("session")).toBeNull();

  // New Task must clear selection and stay cleared (no snap-back).
  await page.getByRole("button", { name: "New Task" }).click();
  await expect(page.locator("textarea.wb-composer-textarea").first()).toBeVisible({ timeout: 20000 });
  await page.waitForTimeout(500);
  const urlAfterNew = new URL(page.url());
  expect(urlAfterNew.searchParams.get("task")).toBeNull();
  expect(urlAfterNew.searchParams.get("track")).toBeNull();
  expect(urlAfterNew.searchParams.get("session")).toBeNull();

  const msg2 = `task two marker ${Date.now()}`;
  await page.locator("textarea.wb-composer-textarea").first().fill(msg2);
  await page.getByRole("button", { name: "Send" }).click();
  await expect(activeThread).toContainText(msg2, { timeout: 20000 });
  const url2 = new URL(page.url());
  expect(url2.searchParams.get("task")).toBeNull();
  expect(url2.searchParams.get("track")).toBeNull();
  expect(url2.searchParams.get("session")).toBeNull();

  // Switching tasks must keep sidebar + conversation pane aligned.
  const taskRows = page.locator(".wb-task-row");
  await expect(taskRows).toHaveCount(2, { timeout: 20000 });
  const newestTaskRow = taskRows.nth(0);
  const olderTaskRow = taskRows.nth(1);

  await olderTaskRow.click();
  await expect(activeThread).toContainText(msg1, { timeout: 20000 });

  await newestTaskRow.click();
  await expect(activeThread).toContainText(msg2, { timeout: 20000 });

  // Ensure both tasks are durably visible in the daemon snapshot before refresh.
  await expect
    .poll(
      async () => {
        const resp = await page.request.get(`/api/workspaces/${workspaceId}/active_snapshot`);
        if (!resp.ok()) return 0;
        const snapshot = await resp.json();
        const active = snapshot?.active;
        const total = Number(active?.total_count ?? NaN);
        if (Number.isFinite(total)) return total;
        return Array.isArray(active?.tasks) ? active.tasks.length : 0;
      },
      { timeout: 30000 },
    )
    .toBeGreaterThanOrEqual(2);

  // Refresh should restore the same selection from IndexedDB (window-scoped).
  await page.reload();
  await expect(page.locator(".wb-task-row")).toHaveCount(2, { timeout: 30000 });
  await expect(activeThread).toContainText(msg2, { timeout: 30000 });
  const urlAfterReload = new URL(page.url());
  expect(urlAfterReload.searchParams.get("task")).toBeNull();
  expect(urlAfterReload.searchParams.get("track")).toBeNull();
  expect(urlAfterReload.searchParams.get("session")).toBeNull();
});
