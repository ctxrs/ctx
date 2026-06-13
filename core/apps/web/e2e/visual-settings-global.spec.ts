import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";
import {
  buildVisualName,
  captureVisual,
  prepareVisualPage,
  visualViewportLabel,
  type VisualTheme,
} from "./utils/visual";
import type { Page } from "playwright/test";

const THEMES = ["dark", "light"] as const satisfies VisualTheme[];

const okHealth = {
  version: "1.0.0",
  daemon_version: "1.0.0",
  pid: 1,
  data_root: "/tmp/ctx",
  daemon_url: "http://127.0.0.1:4399",
  auth_required: false,
  compatibility: {
    desktop_exact_version: "1.0.0",
    mobile_api_min: 1,
    mobile_api_max: 1,
  },
};

const noUpdateCheck = {
  channel: "stable",
  base_url: "https://example.test/functions/v1",
  current_version: "1.0.0",
  latest_version: "1.0.0",
  update_available: false,
};

const availableUpdateCheck = {
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
};

const forcedUpdateCheck = {
  channel: "stable",
  base_url: "https://example.test/functions/v1",
  platform: "linux-x64",
  current_version: "1.0.0",
  latest_version: "1.2.0",
  min_supported_version: "1.1.0",
  in_place_update_supported: true,
  in_place_update_reason: null,
  update_available: true,
  manifest: {
    platforms: {
      "linux-x64": {
        appimage: {
          url_path: "/download/stable/1.2.0/ctx_1.2.0_amd64.AppImage",
          sha256: "forced-abc",
        },
      },
    },
  },
};

const diagnosticsPayload = {
  daemon: okHealth,
  platform: { os: "linux", arch: "x86_64" },
  logs: {
    dir: "/tmp/ctx/logs",
    files: [
      {
        name: "daemon.log",
        bytes: 18243,
        modified_utc: "2026-03-10T12:00:00Z",
      },
      {
        name: "desktop.log",
        bytes: 9041,
        modified_utc: "2026-03-10T12:02:00Z",
      },
    ],
  },
  providers: [
    {
      provider_id: "cursor",
      installed: true,
      health: "warning",
      diagnostics: ["API key expired"],
      details: { active_target: "container" },
    },
  ],
  managed_installs: {
    "ctx-harness": {
      version: "1.0.0",
      status: "ready",
    },
  },
};

const FIXTURE_ISO = "2026-03-10T12:00:00.000Z";

const cursorEndpoint = {
  id: "cursor-endpoint-1",
  provider_id: "cursor",
  name: "Team Key",
  base_url: "https://api.cursor.example/v1",
  api_shape: "openai_responses",
  auth_type: "bearer",
  model_override: "cursor-pro",
  created_at: FIXTURE_ISO,
  updated_at: FIXTURE_ISO,
  last_verification_status: "valid",
  last_verification_at: FIXTURE_ISO,
  last_error: null,
  has_api_key: true,
  model_catalog_status: "ready",
  model_catalog_models: [{ id: "cursor-pro" }],
  model_catalog_source: "remote",
};

const clearUpdaterStorage = async (page: Page) => {
  await page.addInitScript(() => {
    window.localStorage.removeItem("ctx_update_prompt_next_allowed_at_v1");
    window.localStorage.removeItem("ctx_update_prompt_idle_versions_v1");
    window.localStorage.removeItem("ctx_update_restart_required_version_v1");
  });
};

const installJsonRoute = async (
  page: Page,
  url: string,
  body: unknown,
  status = 200,
) => {
  await page.route(url, async (route) => {
    await route.fulfill({
      status,
      contentType: "application/json",
      body: JSON.stringify(body),
    });
  });
};

const installQueuedJsonRoute = async (
  page: Page,
  url: string,
  responses: Array<{ body: unknown; status?: number }>,
) => {
  let index = 0;
  await page.route(url, async (route) => {
    const next = responses[Math.min(index, responses.length - 1)];
    index += 1;
    await route.fulfill({
      status: next.status ?? 200,
      contentType: "application/json",
      body: JSON.stringify(next.body),
    });
  });
};

const installGlobalAppRoutes = async (
  page: Page,
  opts: {
    healthBody?: unknown;
    healthStatus?: number;
    updateResponses?: Array<{ body: unknown; status?: number }>;
  } = {},
) => {
  await installJsonRoute(page, "**/api/health", opts.healthBody ?? okHealth, opts.healthStatus ?? 200);
  await installQueuedJsonRoute(page, "**/api/updates/check**", opts.updateResponses ?? [{ body: noUpdateCheck }]);
  await page.route("**/api/desktop/log", async (route) => {
    await route.fulfill({ status: 204, body: "" });
  });
};

const installDiagnosticsRoutes = async (page: Page) => {
  await installJsonRoute(page, "**/api/diagnostics", diagnosticsPayload);
};

const installHarnessAuthRoutes = async (page: Page) => {
  await page.route("**/api/workspaces/*/providers/bootstrap", async (route) => {
    if (route.request().method() !== "GET") {
      await route.continue();
      return;
    }
    const url = new URL(route.request().url());
    const match = url.pathname.match(/^\/api\/workspaces\/([^/]+)\/providers\/bootstrap$/);
    const workspaceId = match ? decodeURIComponent(match[1]) : "";
    const cursorSource = {
      provider_id: "cursor",
      selected_source_kind: "endpoint",
      selected_endpoint_id: cursorEndpoint.id,
      endpoints: [cursorEndpoint],
    };
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        providers: [
          {
            provider_id: "cursor",
            installed: true,
            health: "ok",
            diagnostics: [],
            details: {
              install_supported: "true",
              install_running: "false",
              install_id: "",
            },
          },
        ],
        provider_options: {
          cursor: {
            provider_id: "cursor",
            workspace_id: workspaceId,
            supports_load: false,
            auth_required: false,
            has_active_auth: true,
            auth_mode: "subscription",
            source: cursorSource,
            probed_at: FIXTURE_ISO,
          },
        },
        provider_harness_config: {
          cursor: cursorSource,
        },
        codex_accounts: { active_account_id: null, accounts: [], logins: [] },
        claude_accounts: { active_account_id: null, accounts: [] },
        gemini_accounts: { active_account_id: null, accounts: [] },
        qwen_accounts: { active_account_id: null, accounts: [] },
        kimi_accounts: { active_account_id: null, accounts: [] },
        mistral_accounts: { active_account_id: null, accounts: [] },
        copilot_accounts: { active_account_id: null, accounts: [], logins: [] },
        cursor_accounts: { active_account_id: null, accounts: [] },
        amp_accounts: { active_account_id: null, accounts: [] },
        auggie_accounts: { active_account_id: null, accounts: [] },
      }),
    });
  });

  await page.route("**/api/providers/cursor/accounts", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ active_account_id: null, accounts: [] }),
    });
  });

  await page.route("**/api/workspaces/*/providers/*/options", async (route) => {
    const url = new URL(route.request().url());
    const match = url.pathname.match(/^\/api\/workspaces\/([^/]+)\/providers\/([^/]+)\/options$/);
    if (!match) {
      await route.continue();
      return;
    }
    const workspaceId = decodeURIComponent(match[1]);
    const providerId = decodeURIComponent(match[2]);
    if (providerId !== "cursor") {
      await route.continue();
      return;
    }
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        provider_id: "cursor",
        workspace_id: workspaceId,
        supports_load: false,
        auth_required: false,
        has_active_auth: true,
        auth_mode: "subscription",
        source: {
          provider_id: "cursor",
          selected_source_kind: "endpoint",
          selected_endpoint_id: cursorEndpoint.id,
          endpoints: [cursorEndpoint],
        },
        probed_at: FIXTURE_ISO,
      }),
    });
  });
};

test.describe.serial("visual: settings and global surfaces", () => {
  let workspaceId = "";

  test.beforeAll(async ({ request }) => {
    const seed = await seedDummyWorkspace(request, {
      tasks: 0,
      sessionsPerTask: 0,
      turnsPerSession: 0,
    });
    workspaceId = seed.workspaceId;
  });

  for (const theme of THEMES) {
    test(`settings general ${theme}`, async ({ page }) => {
      await installGlobalAppRoutes(page);
      await prepareVisualPage(page, {
        theme,
        viewport: "fullpage",
        route: `/settings?ws=${workspaceId}`,
        ready: page.getByText("Theme", { exact: true }),
      });

      await captureVisual(
        page,
        buildVisualName(["settings", "general", theme, visualViewportLabel("fullpage")]),
      );
    });

    test(`updater snackbar and modal ${theme}`, async ({ page }) => {
      await clearUpdaterStorage(page);
      await installGlobalAppRoutes(page, {
        updateResponses: [{ body: availableUpdateCheck }],
      });

      await prepareVisualPage(page, {
        theme,
        viewport: "desktop",
        route: `/workspaces/${workspaceId}`,
        ready: page.getByTestId("update-available-snackbar"),
      });

      const snackbar = page.getByTestId("update-available-snackbar");
      await expect(snackbar).toBeVisible();
      await captureVisual(
        page,
        buildVisualName(["update", "snackbar", theme, visualViewportLabel("desktop")]),
        {
          fullPage: false,
          ready: snackbar,
        },
      );

      await page.getByRole("button", { name: "Learn about update timing" }).click();
      const modal = page.getByRole("dialog", { name: "Update timing info" });
      await expect(modal).toBeVisible();
      await captureVisual(
        page,
        buildVisualName(["update", "modal", theme, visualViewportLabel("desktop")]),
        {
          fullPage: false,
          ready: modal,
        },
      );
    });

    test(`forced update overlay ${theme}`, async ({ page }) => {
      await clearUpdaterStorage(page);
      await installGlobalAppRoutes(page, {
        updateResponses: [{ body: forcedUpdateCheck }],
      });

      await prepareVisualPage(page, {
        theme,
        viewport: "desktop",
        route: `/workspaces/${workspaceId}`,
        ready: page.locator(".wb-update-required-overlay"),
      });

      await captureVisual(
        page,
        buildVisualName(["update", "required-overlay", theme, visualViewportLabel("desktop")]),
        {
          fullPage: false,
          ready: page.locator(".wb-update-required-overlay"),
        },
      );
    });

    test(`daemon overlay ${theme}`, async ({ page }) => {
      await installGlobalAppRoutes(page, {
        healthBody: { error: "daemon down" },
        healthStatus: 503,
      });

      await prepareVisualPage(page, {
        theme,
        viewport: "desktop",
        route: `/workspaces/${workspaceId}`,
        ready: page.locator(".daemon-overlay"),
      });

      await captureVisual(
        page,
        buildVisualName(["daemon", "overlay", theme, visualViewportLabel("desktop")]),
        {
          fullPage: false,
          ready: page.locator(".daemon-overlay"),
        },
      );
    });

    test(`settings harness auth ${theme}`, async ({ page }) => {
      await installGlobalAppRoutes(page);
      await installHarnessAuthRoutes(page);

      await prepareVisualPage(page, {
        theme,
        viewport: "fullpage",
        route: `/settings?ws=${workspaceId}#agent_harnesses`,
        ready: page.locator(".settings-harness-row").filter({ hasText: "Cursor" }).first(),
      });

      await captureVisual(
        page,
        buildVisualName(["settings", "harness-auth", theme, visualViewportLabel("fullpage")]),
      );
    });

    test(`settings harness auth menu ${theme}`, async ({ page }) => {
      await installGlobalAppRoutes(page);
      await installHarnessAuthRoutes(page);

      await prepareVisualPage(page, {
        theme,
        viewport: "fullpage",
        route: `/settings?ws=${workspaceId}#agent_harnesses`,
        ready: page.locator(".settings-harness-row").filter({ hasText: "Cursor" }).first(),
      });

      const cursorRow = page.locator(".settings-harness-row").filter({ hasText: "Cursor" }).first();
      await cursorRow.locator(".settings-harness-auth-menu-trigger").click();
      const deleteAction = page.getByText("Delete", { exact: true }).first();
      await expect(deleteAction).toBeVisible({ timeout: 20_000 });
      await captureVisual(
        page,
        buildVisualName(["settings", "harness-auth-menu", theme, visualViewportLabel("fullpage")]),
        { ready: deleteAction },
      );
    });

    test(`settings harness auth modal ${theme}`, async ({ page }) => {
      await installGlobalAppRoutes(page);
      await installHarnessAuthRoutes(page);

      await prepareVisualPage(page, {
        theme,
        viewport: "fullpage",
        route: `/settings?ws=${workspaceId}#agent_harnesses`,
        ready: page.locator(".settings-harness-row").filter({ hasText: "Cursor" }).first(),
      });

      const cursorRow = page.locator(".settings-harness-row").filter({ hasText: "Cursor" }).first();
      await cursorRow.getByRole("button", { name: "Add auth for Cursor" }).click();
      const modal = page.locator(".settings-harness-modal");
      await expect(modal).toBeVisible({ timeout: 20_000 });
      const apiKeyButton = modal.getByRole("button", { name: "API Key" });
      if ((await apiKeyButton.count()) > 0) {
        await apiKeyButton.click();
      }
      await expect(modal.getByRole("link", { name: "Cursor Integrations" })).toBeVisible({ timeout: 20_000 });
      await captureVisual(
        page,
        buildVisualName(["settings", "harness-auth-modal", theme, visualViewportLabel("fullpage")]),
        { fullPage: false, ready: modal },
      );
    });

    test(`diagnostics page ${theme}`, async ({ page }) => {
      await installGlobalAppRoutes(page, {
        updateResponses: [{ body: noUpdateCheck }, { body: availableUpdateCheck }],
      });
      await installDiagnosticsRoutes(page);

      await prepareVisualPage(page, {
        theme,
        viewport: "fullpage",
        route: "/diagnostics",
        ready: page.getByRole("heading", { name: "Diagnostics" }),
      });

      await page.getByRole("button", { name: "Check updates" }).click();
      await expect(page.getByRole("button", { name: "Download AppImage update" })).toBeVisible();
      await captureVisual(
        page,
        buildVisualName(["diagnostics", theme, visualViewportLabel("fullpage")]),
        {
          ready: page.getByText("Check updates"),
        },
      );
    });
  }
});
