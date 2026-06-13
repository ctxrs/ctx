import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";

test("workbench: harness menu in prod mode hides inline auth actions", async ({ page, request }) => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init -b main", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "README.md"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  await createWorkspaceAndOpenWorkbench({
    page,
    request,
    repo,
    workspaceName: `ws-${Date.now()}`,
  });

  await page.locator('button[title="Agents"]').first().click();
  const menu = page.locator(".wb-harness-menu");
  await expect(menu).toBeVisible({ timeout: 10_000 });
  await expect(menu.locator(".wb-harness-row").first()).toBeVisible();
  await expect(menu.locator(".wb-harness-row .wb-harness-row-main:enabled").first()).toBeVisible();
  await expect(menu.getByRole("button", { name: "Install all" })).toBeVisible();
  await expect(menu.getByRole("button", { name: "Verify" })).toHaveCount(0);
  await expect(menu.getByRole("button", { name: "Authenticate" })).toHaveCount(0);
});
