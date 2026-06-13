import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import type { Page } from "playwright/test";

const AUTO_APPLY_ON_LAUNCH_STORAGE_KEY = "ctx_update_auto_apply_on_launch_v1";
const PROMPT_SNOOZE_STORAGE_KEY = "ctx_update_prompt_next_allowed_at_v1";
const LINUX_PLATFORM = "linux-x64";
const LINUX_APPIMAGE_MANIFEST = {
  platforms: {
    [LINUX_PLATFORM]: {
      appimage: {
        url_path: "/ctx.AppImage",
        sha256: "abc123",
      },
    },
  },
};

const createWorkspaceAndOpenWorkbench = async (page: Page, workspaceName: string) => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-updater-auto-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "README.md"), "updater auto apply e2e fixture\n");
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
  await page.goto(`/workspaces/${workspaceJson.id}`, { waitUntil: "domcontentloaded" });
};

test("browser path does not auto-apply on app open", async ({ page }) => {
  let applyCalls = 0;
  await page.addInitScript((autoApplyKey: string, promptSnoozeKey: string) => {
    try {
      localStorage.removeItem("ctx_update_check_v1");
      localStorage.removeItem(promptSnoozeKey);
      localStorage.removeItem("ctx_update_prompt_idle_versions_v1");
      localStorage.setItem(autoApplyKey, "1");
    } catch {
      // ignore storage access failures in pre-navigation contexts
    }
  }, AUTO_APPLY_ON_LAUNCH_STORAGE_KEY, PROMPT_SNOOZE_STORAGE_KEY);
  await page.route("**/api/updates/check**", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        channel: "stable",
        base_url: "https://example.com",
        platform: LINUX_PLATFORM,
        in_place_update_supported: true,
        in_place_update_reason: null,
        manifest: LINUX_APPIMAGE_MANIFEST,
        current_version: "1.0.0",
        latest_version: "1.1.0",
        update_available: true,
      }),
    });
  });
  await page.route("**/api/updates/appimage/download", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        downloaded_path: "/tmp/ctx-appimage.new",
        can_apply_in_place: true,
      }),
    });
  });
  await page.route("**/api/updates/appimage/apply", async (route) => {
    applyCalls += 1;
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        applied: true,
        restarted: true,
        target_path: "/tmp/ctx-appimage",
        message: "Applied update",
      }),
    });
  });

  await createWorkspaceAndOpenWorkbench(page, `ws-auto-open-${Date.now()}`);
  await expect.poll(() => applyCalls, { timeout: 10_000 }).toBe(0);
  await expect(page.getByTestId("update-available-snackbar")).toBeVisible({ timeout: 20_000 });
});

test("notice appears and Update Now applies successfully", async ({ page }) => {
  let applyCalls = 0;
  await page.addInitScript((autoApplyKey: string, promptSnoozeKey: string) => {
    try {
      localStorage.removeItem("ctx_update_check_v1");
      localStorage.removeItem(promptSnoozeKey);
      localStorage.removeItem("ctx_update_prompt_idle_versions_v1");
      localStorage.setItem(autoApplyKey, "0");
    } catch {
      // ignore storage access failures in pre-navigation contexts
    }
  }, AUTO_APPLY_ON_LAUNCH_STORAGE_KEY, PROMPT_SNOOZE_STORAGE_KEY);
  await page.route("**/api/updates/check**", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        channel: "stable",
        base_url: "https://example.com",
        platform: LINUX_PLATFORM,
        in_place_update_supported: true,
        in_place_update_reason: null,
        manifest: LINUX_APPIMAGE_MANIFEST,
        current_version: "1.0.0",
        latest_version: "1.2.0",
        update_available: true,
      }),
    });
  });
  await page.route("**/api/updates/appimage/download", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        downloaded_path: "/tmp/ctx-appimage.new",
        can_apply_in_place: true,
      }),
    });
  });
  await page.route("**/api/updates/appimage/apply", async (route) => {
    applyCalls += 1;
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        applied: true,
        restarted: true,
        target_path: "/tmp/ctx-appimage",
        message: "Applied update",
      }),
    });
  });

  await createWorkspaceAndOpenWorkbench(page, `ws-update-now-${Date.now()}`);
  await expect(page.getByTestId("update-available-snackbar")).toBeVisible({ timeout: 20_000 });
  await page.getByRole("button", { name: "Update Now" }).click();
  await expect.poll(() => applyCalls, { timeout: 20_000 }).toBe(1);
  await expect(page.getByTestId("update-available-snackbar")).toBeVisible({ timeout: 20_000 });
  await expect(page.getByRole("button", { name: "Relaunch" })).toBeDisabled();
});

test("Update on Next Idle applies successfully", async ({ page }) => {
  let applyCalls = 0;
  await page.addInitScript((autoApplyKey: string, promptSnoozeKey: string) => {
    try {
      localStorage.removeItem("ctx_update_check_v1");
      localStorage.removeItem(promptSnoozeKey);
      localStorage.removeItem("ctx_update_prompt_idle_versions_v1");
      localStorage.setItem(autoApplyKey, "0");
    } catch {
      // ignore storage access failures in pre-navigation contexts
    }
  }, AUTO_APPLY_ON_LAUNCH_STORAGE_KEY, PROMPT_SNOOZE_STORAGE_KEY);
  await page.route("**/api/updates/check**", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        channel: "stable",
        base_url: "https://example.com",
        platform: LINUX_PLATFORM,
        in_place_update_supported: true,
        in_place_update_reason: null,
        manifest: LINUX_APPIMAGE_MANIFEST,
        current_version: "1.0.0",
        latest_version: "1.3.0",
        update_available: true,
      }),
    });
  });
  await page.route("**/api/updates/appimage/download", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        downloaded_path: "/tmp/ctx-appimage.new",
        can_apply_in_place: true,
      }),
    });
  });
  await page.route("**/api/updates/appimage/apply", async (route) => {
    applyCalls += 1;
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        applied: true,
        restarted: true,
        target_path: "/tmp/ctx-appimage",
        message: "Applied update",
      }),
    });
  });

  await createWorkspaceAndOpenWorkbench(page, `ws-update-idle-${Date.now()}`);
  await expect(page.getByTestId("update-available-snackbar")).toBeVisible({ timeout: 20_000 });
  await page.getByRole("button", { name: "Update on Next Idle" }).click();
  await expect.poll(() => applyCalls, { timeout: 20_000 }).toBe(1);
  await expect(page.getByTestId("update-available-snackbar")).toBeVisible({ timeout: 20_000 });
  await expect(page.getByRole("button", { name: "Relaunch" })).toBeDisabled();
});

test("required update blocks workbench and only allows immediate update", async ({ page }) => {
  let applyCalls = 0;
  await page.addInitScript((autoApplyKey: string, promptSnoozeKey: string) => {
    try {
      localStorage.removeItem("ctx_update_check_v1");
      localStorage.removeItem(promptSnoozeKey);
      localStorage.removeItem("ctx_update_prompt_idle_versions_v1");
      localStorage.setItem(autoApplyKey, "0");
    } catch {
      // ignore storage access failures in pre-navigation contexts
    }
  }, AUTO_APPLY_ON_LAUNCH_STORAGE_KEY, PROMPT_SNOOZE_STORAGE_KEY);
  await page.route("**/api/updates/check**", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        channel: "stable",
        base_url: "https://example.com",
        platform: LINUX_PLATFORM,
        in_place_update_supported: true,
        in_place_update_reason: null,
        manifest: LINUX_APPIMAGE_MANIFEST,
        current_version: "1.0.0",
        latest_version: "1.2.0",
        min_supported_version: "1.1.0",
        update_available: true,
      }),
    });
  });
  await page.route("**/api/updates/appimage/download", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        downloaded_path: "/tmp/ctx-appimage.new",
        can_apply_in_place: true,
      }),
    });
  });
  await page.route("**/api/updates/appimage/apply", async (route) => {
    applyCalls += 1;
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        applied: true,
        target_path: "/tmp/ctx-appimage",
        message: "Applied update",
      }),
    });
  });

  await createWorkspaceAndOpenWorkbench(page, `ws-required-update-${Date.now()}`);
  await expect(page.getByRole("dialog", { name: "Update required" })).toBeVisible({ timeout: 20_000 });
  await expect(page.getByRole("button", { name: "Update Now" })).toBeVisible({ timeout: 20_000 });
  await expect(page.getByRole("button", { name: "Update on Next Idle" })).toHaveCount(0);
  await page.getByRole("button", { name: "Update Now" }).click();
  await expect.poll(() => applyCalls, { timeout: 20_000 }).toBe(1);
  await expect(page.getByRole("dialog", { name: "Update required" })).toBeVisible({ timeout: 20_000 });
  await expect(page.getByRole("button", { name: "Relaunch" })).toBeDisabled();
});
