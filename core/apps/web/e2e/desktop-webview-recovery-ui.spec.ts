import { test, expect } from "./fixtures";
import type { Page } from "playwright/test";

type RecoveryIncident = {
  incident_id: string;
  window_label: string;
  window_surface: "main";
  route: string;
  trigger_kind: "native_process_termination" | "heartbeat_timeout";
  action: "reload" | "recreate" | "prompt_restart" | "noop";
  daemon_health: "unknown" | "ok" | "down" | "mismatch";
  suppression_reason?: "daemon_down" | "daemon_mismatch" | null;
  created_at_ms: number;
};

type RecoveryHarnessState = {
  invokeCalls: string[];
  pendingIncidents: RecoveryIncident[];
  restartCallCount: number;
};

const installApiRoutes = async (page: Page) => {
  await page.route("**/api/**", async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname === "/api/telemetry/client") {
      await route.fulfill({ status: 204, body: "" });
      return;
    }
    if (url.pathname === "/api/health") {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          version: "0.59.0",
          daemon_version: "0.59.0",
          pid: 1,
          data_root: "/tmp/ctx-e2e",
          daemon_url: url.origin,
          auth_required: false,
          compatibility: {
            desktop_exact_version: "0.59.0",
            mobile_api_min: 1,
            mobile_api_max: 1,
          },
        }),
      });
      return;
    }
    if (url.pathname === "/api/updates/check") {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          channel: "stable",
          latest_version: "0.59.0",
          current_version: "0.59.0",
          update_available: false,
        }),
      });
      return;
    }
    if (url.pathname === "/api/workspaces" || url.pathname === "/api/providers") {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: "[]",
      });
      return;
    }
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: "{}",
    });
  });
};

const installDesktopRecoveryHarness = async (page: Page, incidents: RecoveryIncident[]) => {
  await page.addInitScript((initialIncidents: RecoveryIncident[]) => {
    type TauriInvoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
    type TauriWindow = Window & {
      __TAURI__?: { core?: { invoke?: TauriInvoke } };
      __TAURI_INTERNALS__?: {
        invoke?: TauriInvoke;
        transformCallback?: (cb: unknown, once?: boolean) => number;
        unregisterCallback?: (id: number) => void;
        metadata?: Record<string, unknown>;
      };
      __ctxDesktopRecoveryE2E?: RecoveryHarnessState;
    };

    const w = window as TauriWindow;
    const state: RecoveryHarnessState = {
      invokeCalls: [],
      pendingIncidents: [...initialIncidents],
      restartCallCount: 0,
    };

    const invoke: TauriInvoke = async (cmd) => {
      const name = String(cmd || "");
      state.invokeCalls.push(name);

      if (name === "plugin:event|listen") return 1;
      if (name === "plugin:event|unlisten") return null;
      if (name === "plugin:app|version") return "0.59.0";
      if (name === "desktop_get_connection") {
        return {
          kind: "local",
          base_url: window.location.origin,
          token: "ctx-e2e-auth-token",
        };
      }
      if (name === "desktop_check_app_update" || name === "desktop_get_app_update_state") {
        return {
          configured: true,
          available: false,
          restart_required: false,
          current_version: "0.59.0",
          latest_version: null,
          target: "macos-arm64",
          endpoint: "https://api.ctx.rs/functions/v1/releases/stable/latest-tauri.json",
          message: null,
          phase: "idle",
          staged: false,
          last_error: null,
          checked_at: null,
          downloaded_at: null,
          last_attempt_id: null,
          last_attempt_started_at: null,
        };
      }
      if (
        name === "desktop_set_menu_state"
        || name === "desktop_set_dock_recent_local_workspaces"
        || name === "desktop_set_window_title"
        || name === "desktop_storage_consume_notice"
      ) {
        return null;
      }
      if (name === "desktop_webview_recovery_consume_incidents") {
        const next = state.pendingIncidents.slice();
        state.pendingIncidents = [];
        return next;
      }
      if (name === "desktop_webview_recovery_heartbeat") return null;
      if (name === "desktop_restart_app") {
        state.restartCallCount += 1;
        return { requested: true, message: "restart requested" };
      }

      return null;
    };

    w.__TAURI_INTERNALS__ = {
      invoke,
      transformCallback: () => 1,
      unregisterCallback: () => {},
      metadata: {
        currentWindow: { label: "main" },
        currentWebview: { label: "main" },
      },
    };
    w.__TAURI__ = { core: { invoke } };
    w.__ctxDesktopRecoveryE2E = state;
  }, incidents);
};

const commandCallCount = async (page: Page, command: string): Promise<number> => {
  return await page.evaluate((name: string) => {
    const w = window as Window & { __ctxDesktopRecoveryE2E?: RecoveryHarnessState };
    return (w.__ctxDesktopRecoveryE2E?.invokeCalls ?? []).filter((cmd) => cmd === name).length;
  }, command);
};

test("desktop recovery notice opens diagnostics after a recovered incident", async ({ page }) => {
  await installApiRoutes(page);
  await installDesktopRecoveryHarness(page, [
    {
      incident_id: "reload-1",
      window_label: "main",
      window_surface: "main",
      route: "/settings",
      trigger_kind: "native_process_termination",
      action: "reload",
      daemon_health: "ok",
      created_at_ms: Date.now(),
    },
  ]);

  await page.goto("/settings", { waitUntil: "domcontentloaded" });

  await expect(page.getByTestId("desktop-webview-recovery-snackbar")).toBeVisible();
  await expect(page.getByText("ctx recovered a failed main window.")).toBeVisible();
  await expect
    .poll(async () => commandCallCount(page, "desktop_webview_recovery_heartbeat"))
    .toBeGreaterThan(0);

  await page.getByRole("button", { name: "Open Diagnostics" }).click();
  await expect(page).toHaveURL(/\/diagnostics$/);
});

test("desktop recovery notice requests restart after repeated failures", async ({ page }) => {
  await installApiRoutes(page);
  await installDesktopRecoveryHarness(page, [
    {
      incident_id: "restart-1",
      window_label: "main",
      window_surface: "main",
      route: "/settings",
      trigger_kind: "heartbeat_timeout",
      action: "prompt_restart",
      daemon_health: "ok",
      created_at_ms: Date.now(),
    },
  ]);

  await page.goto("/settings", { waitUntil: "domcontentloaded" });

  await expect(page.getByTestId("desktop-webview-recovery-snackbar")).toBeVisible();
  await expect(page.getByRole("button", { name: "Restart App" })).toBeVisible();

  await page.getByRole("button", { name: "Restart App" }).click();
  await expect.poll(async () => commandCallCount(page, "desktop_restart_app")).toBe(1);
});
