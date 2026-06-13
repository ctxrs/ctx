import { test, expect } from "./fixtures";
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

const diagnostics = {
  daemon: okHealth,
  platform: { os: "linux", arch: "x86_64" },
  logs: { dir: "/tmp/ctx/logs", files: [] },
  providers: [],
  managed_installs: {},
};

const installDiagnosticsBaselineRoutes = async (page: Page) => {
  await page.route("**/api/health", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(okHealth),
    });
  });
  await page.route("**/api/desktop/log", async (route) => {
    await route.fulfill({ status: 204, body: "" });
  });
  await page.route("**/api/diagnostics", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(diagnostics),
    });
  });
};

test("diagnostics shows actionable error when update metadata is invalid", async ({ page }) => {
  await installDiagnosticsBaselineRoutes(page);
  await page.route("**/api/updates/check**", async (route) => {
    await route.fulfill({
      status: 502,
      contentType: "application/json",
      body: JSON.stringify({ error: "parsing release manifest JSON" }),
    });
  });

  await page.goto("/diagnostics", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Diagnostics" })).toBeVisible();
  await page.getByRole("button", { name: "Check updates" }).click();

  await expect(page.getByText("parsing release manifest JSON")).toBeVisible();
  await expect(page.getByRole("button", { name: "Check updates" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Refresh" })).toBeVisible();
});

test("diagnostics keeps running on AppImage download failure", async ({ page }) => {
  await installDiagnosticsBaselineRoutes(page);
  await page.route("**/api/updates/check**", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        channel: "stable",
        base_url: "https://example.test/functions/v1",
        platform: "linux-x64",
        current_version: "1.0.0",
        latest_version: "1.0.1",
        platform_supported: true,
        update_available: true,
        manifest: {
          platforms: {
            "linux-x64": {
              appimage: {
                url_path: "/download/stable/1.0.1/ctx_1.0.1_amd64.AppImage",
                sha256: "abc",
              },
            },
          },
        },
      }),
    });
  });
  await page.route("**/api/updates/appimage/download", async (route) => {
    await route.fulfill({
      status: 502,
      contentType: "application/json",
      body: JSON.stringify({ error: "download http error: https://example.test/ctx.AppImage" }),
    });
  });

  await page.goto("/diagnostics", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Check updates" }).click();
  await expect(page.getByRole("button", { name: "Download AppImage update" })).toBeVisible();

  await page.getByRole("button", { name: "Download AppImage update" }).click();
  await expect(page.getByText("download http error: https://example.test/ctx.AppImage")).toBeVisible();
  await expect(page.getByRole("button", { name: "Check updates" })).toBeVisible();
});
