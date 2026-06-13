import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import type { Page } from "playwright/test";

const okHealth = {
  version: "1.0.0",
  daemon_version: "1.0.0",
  pid: 1,
  data_root: "/tmp",
  daemon_url: "http://127.0.0.1:4399",
  auth_required: false,
  compatibility: {
    desktop_exact_version: "1.0.0",
    mobile_api_min: 1,
    mobile_api_max: 1,
  },
};

const createWorkspace = async (page: Page, workspaceName: string): Promise<string> => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-update-snackbar-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "README.md"), "update notice screenshot fixture\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });
  const workspaceResp = await page.request.post("/api/workspaces", {
    data: {
      root_path: repo,
      name: workspaceName,
    },
  });
  expect(workspaceResp.ok()).toBeTruthy();
  const workspaceJson = (await workspaceResp.json()) as { id: string };
  expect(typeof workspaceJson.id).toBe("string");
  return workspaceJson.id;
};

test("update snackbar prompt shows when updates are available", async ({ page }, testInfo) => {
  const workspaceId = await createWorkspace(page, `ws-update-snackbar-${Date.now()}`);

  await page.addInitScript(() => {
    localStorage.removeItem("ctx_update_check_v1");
    localStorage.removeItem("ctx_update_prompt_next_allowed_at_v1");
  });
  await page.route("**/api/health", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(okHealth),
    });
  });
  await page.route("**/api/updates/check**", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        channel: "stable",
        base_url: "https://example.com",
        current_version: "1.0.0",
        latest_version: "9.9.9",
        update_available: true,
      }),
    });
  });

  const healthResponse = page.waitForResponse(
    (response) => response.url().includes("/api/health") && response.status() === 200,
    {
      timeout: 20000,
    },
  );
  const updatesResponse = page.waitForResponse(
    (response) => response.url().includes("/api/updates/check") && response.status() === 200,
    { timeout: 20000 },
  );
  await Promise.all([healthResponse, updatesResponse, page.goto(`/workspaces/${workspaceId}`, { waitUntil: "domcontentloaded" })]);
  await expect(page.getByTestId("update-available-snackbar")).toBeVisible({ timeout: 20000 });
  await expect(page.getByText(/Update available:\s*9\.9\.9\./)).toBeVisible({ timeout: 20000 });
  await expect(page.getByRole("button", { name: "Update Now" })).toBeVisible({ timeout: 20000 });
  await expect(page.getByRole("button", { name: "Update on Next Idle" })).toBeVisible({ timeout: 20000 });
  const screenshotPath = testInfo.outputPath("update-available-snackbar.png");
  await page.screenshot({ path: screenshotPath, fullPage: true });
  console.log(`update snackbar screenshot: ${screenshotPath}`);
  await page.getByRole("button", { name: "Learn about update timing" }).click();
  await expect(page.getByRole("dialog", { name: "Update timing info" })).toBeVisible({ timeout: 20000 });
  const modalScreenshotPath = testInfo.outputPath("update-available-info-modal.png");
  await page.screenshot({ path: modalScreenshotPath, fullPage: true });
  console.log(`update info modal screenshot: ${modalScreenshotPath}`);
});

test("daemon availability overlay appears on health failures", async ({ page }) => {
  const workspaceId = await createWorkspace(page, `ws-daemon-unavailable-${Date.now()}`);
  await page.route("**/api/health", async (route) => {
    await route.fulfill({
      status: 503,
      contentType: "application/json",
      body: JSON.stringify({ error: "daemon down" }),
    });
  });

  await page.goto(`/workspaces/${workspaceId}`, { waitUntil: "domcontentloaded" });
  await expect(page.getByText("ctx daemon unavailable")).toBeVisible();
  await expect(
    page.getByText("The daemon is not reachable. Start it, then retry this screen."),
  ).toBeVisible();
});
