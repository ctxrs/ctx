import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import type { Page } from "playwright/test";

const E2E_AUTH_TOKEN = process.env.CTX_E2E_AUTH_TOKEN ?? "ctx-e2e-auth-token";

const AUTO_APPLY_ON_LAUNCH_STORAGE_KEY = "ctx_update_auto_apply_on_launch_v1";
const PROMPT_SNOOZE_STORAGE_KEY = "ctx_update_prompt_next_allowed_at_v1";
const IDLE_UPDATE_VERSION_STORAGE_KEY = "ctx_update_prompt_idle_versions_v1";
const RESTART_REQUIRED_VERSION_STORAGE_KEY = "ctx_update_restart_required_version_v1";

type DesktopUpdateState = {
  configured: boolean;
  available: boolean;
  restart_required: boolean;
  phase?: "idle" | "staging" | "staged_ready" | "restart_required" | "failed";
  staged?: boolean;
  current_version: string;
  latest_version: string | null;
  target: string;
  endpoint: string;
  message: string | null;
};

type DesktopApplyResponse = {
  applied: boolean;
  needs_restart: boolean;
  up_to_date: boolean;
  latest_version: string | null;
  message: string;
};

type HarnessConfig = {
  authToken: string;
  updateState: DesktopUpdateState;
  applyResponse: DesktopApplyResponse;
};

type HarnessState = {
  updateState: DesktopUpdateState;
  applyResponse: DesktopApplyResponse;
  invokeCalls: string[];
  invokeArgsByCommand: Record<string, unknown[]>;
  menuItemsById: Record<string, { id: string; enabled?: boolean; checked?: boolean; text?: string }>;
  emitMenuAction?: (commandId: string) => void;
};

const createWorkspaceAndOpenWorkbench = async (page: Page, workspaceName: string) => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-updater-desktop-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "README.md"), "desktop updater e2e fixture\n");
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

const installDesktopHarness = async (page: Page, config: HarnessConfig) => {
  await page.addInitScript((initial: HarnessConfig) => {
    type TauriInvoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
    type TauriInternals = {
      invoke?: TauriInvoke;
      transformCallback?: (cb: unknown, once?: boolean) => number;
      unregisterCallback?: (id: number) => void;
      metadata?: Record<string, unknown>;
    } & Record<string, unknown>;

    type TauriWindow = Window & {
      __TAURI__?: { core?: { invoke?: TauriInvoke } };
      __TAURI_INTERNALS__?: TauriInternals;
      __TAURI_EVENT_PLUGIN_INTERNALS__?: {
        unregisterListener?: (event: string, eventId: number) => void;
      };
      __ctxDesktopUpdaterE2E?: HarnessState;
    };

    const w = window as TauriWindow;
    const state: HarnessState = {
      updateState: { ...initial.updateState },
      applyResponse: { ...initial.applyResponse },
      invokeCalls: [],
      invokeArgsByCommand: {},
      menuItemsById: {},
    };
    const callbacks = new Map<number, (payload: unknown) => void>();
    const eventListeners = new Map<number, { event: string; callbackId: number }>();
    let nextCallbackId = 1;
    let nextEventListenerId = 1;

    const currentDaemonBaseUrl = () => window.location.origin;
    const unregisterListener = (eventId: number) => {
      eventListeners.delete(eventId);
    };
    const emitEvent = (event: string, payload: unknown) => {
      for (const [eventId, listener] of eventListeners.entries()) {
        if (listener.event !== event) continue;
        const callback = callbacks.get(listener.callbackId);
        if (!callback) continue;
        callback({
          event,
          id: eventId,
          payload,
        });
      }
    };

    const invoke: TauriInvoke = async (cmd, rawArgs) => {
      const name = String(cmd || "");
      state.invokeCalls.push(name);
      try {
        const recordedArgs = state.invokeArgsByCommand[name] ?? [];
        recordedArgs.push(rawArgs == null ? null : JSON.parse(JSON.stringify(rawArgs)));
        state.invokeArgsByCommand[name] = recordedArgs;
      } catch {
        const recordedArgs = state.invokeArgsByCommand[name] ?? [];
        recordedArgs.push(null);
        state.invokeArgsByCommand[name] = recordedArgs;
      }
      const args =
        rawArgs && typeof rawArgs === "object"
          ? (rawArgs as Record<string, unknown>)
          : {};
      const req =
        args.req && typeof args.req === "object"
          ? (args.req as Record<string, unknown>)
          : {};
      if (name === "plugin:app|version") {
        return state.updateState.current_version;
      }
      if (name === "plugin:event|listen") {
        const event = String(args.event ?? "");
        const callbackId = Number(args.handler ?? 0);
        const eventId = nextEventListenerId++;
        eventListeners.set(eventId, {
          event,
          callbackId,
        });
        return eventId;
      }
      if (name === "plugin:event|unlisten") {
        unregisterListener(Number(args.eventId ?? 0));
        return null;
      }
      if (name === "desktop_get_connection") {
        return {
          kind: "local",
          base_url: currentDaemonBaseUrl(),
          token: initial.authToken,
        };
      }
      if (name === "desktop_get_app_update_state") {
        return {
          ...state.updateState,
        };
      }
      if (name === "desktop_apply_app_update") {
        const response = { ...state.applyResponse };
        if (response.needs_restart) {
          state.updateState = {
            ...state.updateState,
            available: false,
            staged: false,
            restart_required: true,
            phase: "restart_required",
            latest_version: response.latest_version ?? state.updateState.latest_version,
          };
        } else if (response.applied || response.up_to_date) {
          const nextVersion = response.latest_version ?? state.updateState.latest_version ?? state.updateState.current_version;
          state.updateState = {
            ...state.updateState,
            available: false,
            staged: false,
            restart_required: false,
            phase: "idle",
            current_version: nextVersion,
            latest_version: null,
          };
        }
        return response;
      }
      if (name === "desktop_restart_app") {
        return { requested: true, message: "Restart requested." };
      }
      if (name === "desktop_set_menu_state") {
        const items = Array.isArray(req.items) ? req.items : [];
        const nextMenuItemsById: HarnessState["menuItemsById"] = {};
        for (const rawItem of items) {
          if (!rawItem || typeof rawItem !== "object") continue;
          const item = rawItem as Record<string, unknown>;
          const id = String(item.id ?? "").trim();
          if (!id) continue;
          nextMenuItemsById[id] = {
            id,
            ...(typeof item.enabled === "boolean" ? { enabled: item.enabled } : {}),
            ...(typeof item.checked === "boolean" ? { checked: item.checked } : {}),
            ...(typeof item.text === "string" ? { text: item.text } : {}),
          };
        }
        state.menuItemsById = nextMenuItemsById;
        return null;
      }
      if (name === "desktop_daemon_request") {
        const path = String(req.path ?? "/");
        const method = String(req.method ?? "GET").toUpperCase();
        const body = typeof req.body === "string" ? req.body : "";
        const headers = req.headers && typeof req.headers === "object" ? (req.headers as Record<string, unknown>) : {};
        const fetchHeaders: Record<string, string> = {};
        for (const [key, value] of Object.entries(headers)) {
          if (typeof value === "string") {
            fetchHeaders[key] = value;
          }
        }
        if (!fetchHeaders.authorization) {
          fetchHeaders.authorization = `Bearer ${initial.authToken}`;
        }
        if (!fetchHeaders["content-type"] && body) {
          fetchHeaders["content-type"] = "application/json";
        }
        const response = await fetch(path, {
          method,
          headers: fetchHeaders,
          body: method === "GET" || method === "HEAD" ? undefined : body,
        });
        const text = await response.text();
        return {
          status: response.status,
          content_type: response.headers.get("content-type") ?? "application/json",
          body: text,
        };
      }
      return null;
    };

    const existingInternals = w.__TAURI_INTERNALS__ ?? {};
    const metadata =
      existingInternals.metadata && typeof existingInternals.metadata === "object"
        ? existingInternals.metadata
        : {
          currentWindow: { label: "main" },
          currentWebview: { label: "main" },
        };
    w.__TAURI_INTERNALS__ = {
      ...existingInternals,
      metadata,
      transformCallback:
        typeof existingInternals.transformCallback === "function"
          ? existingInternals.transformCallback
          : (callback: unknown) => {
            const callbackId = nextCallbackId++;
            if (typeof callback === "function") {
              callbacks.set(callbackId, callback as (payload: unknown) => void);
            }
            return callbackId;
          },
      unregisterCallback:
        typeof existingInternals.unregisterCallback === "function"
          ? existingInternals.unregisterCallback
          : (callbackId: number) => {
            callbacks.delete(callbackId);
            for (const [eventId, listener] of eventListeners.entries()) {
              if (listener.callbackId === callbackId) {
                eventListeners.delete(eventId);
              }
            }
          },
      invoke,
    };
    const existingEventPluginInternals = w.__TAURI_EVENT_PLUGIN_INTERNALS__ ?? {};
    w.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
      ...existingEventPluginInternals,
      unregisterListener:
        typeof existingEventPluginInternals.unregisterListener === "function"
          ? existingEventPluginInternals.unregisterListener
          : (_event: string, eventId: number) => {
            unregisterListener(eventId);
          },
    };

    const existingTauri = w.__TAURI__ ?? {};
    const existingCore = existingTauri.core ?? {};
    w.__TAURI__ = {
      ...existingTauri,
      core: {
        ...existingCore,
        invoke,
      },
    };
    state.emitMenuAction = (commandId: string) => {
      emitEvent("desktop_menu_action", {
        commandId,
      });
    };
    w.__ctxDesktopUpdaterE2E = state;
  }, config);
};

const desktopCommandCallCount = async (page: Page, command: string): Promise<number> => {
  return await page.evaluate((name: string) => {
    const w = window as Window & { __ctxDesktopUpdaterE2E?: HarnessState };
    const calls = w.__ctxDesktopUpdaterE2E?.invokeCalls ?? [];
    return calls.filter((cmd) => cmd === name).length;
  }, command);
};

const desktopLastCommandArgs = async (page: Page, command: string): Promise<unknown | null> => {
  return await page.evaluate((name: string) => {
    const w = window as Window & { __ctxDesktopUpdaterE2E?: HarnessState };
    const calls = w.__ctxDesktopUpdaterE2E?.invokeArgsByCommand?.[name] ?? [];
    return calls.length > 0 ? calls[calls.length - 1] : null;
  }, command);
};

const desktopMenuItemState = async (
  page: Page,
  commandId: string,
): Promise<{ id: string; enabled?: boolean; checked?: boolean; text?: string } | null> => {
  return await page.evaluate((id: string) => {
    const w = window as Window & { __ctxDesktopUpdaterE2E?: HarnessState };
    return w.__ctxDesktopUpdaterE2E?.menuItemsById?.[id] ?? null;
  }, commandId);
};

const emitDesktopMenuAction = async (page: Page, commandId: string): Promise<void> => {
  await page.evaluate((id: string) => {
    const w = window as Window & { __ctxDesktopUpdaterE2E?: HarnessState };
    const emit = w.__ctxDesktopUpdaterE2E?.emitMenuAction;
    if (typeof emit !== "function") {
      throw new Error("missing desktop menu action emitter");
    }
    emit(id);
  }, commandId);
};

const installUpdatePolicyRoute = async (page: Page, updateAvailable = true) => {
  await page.route("**/api/updates/check**", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        channel: "stable",
        base_url: "https://example.com",
        platform: "macos-arm64",
        current_version: "1.0.0",
        latest_version: "9.9.9",
        update_available: updateAvailable,
        platform_supported: true,
        in_place_update_supported: true,
        in_place_update_reason: null,
      }),
    });
  });
};

test("desktop updater remains silent while staging", async ({ page }) => {
  await page.addInitScript((autoApplyKey: string, snoozeKey: string, idleKey: string, restartKey: string) => {
    localStorage.removeItem("ctx_update_check_v1");
    localStorage.removeItem(snoozeKey);
    localStorage.removeItem(idleKey);
    localStorage.setItem(autoApplyKey, "0");
    sessionStorage.removeItem(restartKey);
  }, AUTO_APPLY_ON_LAUNCH_STORAGE_KEY, PROMPT_SNOOZE_STORAGE_KEY, IDLE_UPDATE_VERSION_STORAGE_KEY, RESTART_REQUIRED_VERSION_STORAGE_KEY);
  await installDesktopHarness(page, {
    authToken: E2E_AUTH_TOKEN,
    updateState: {
      configured: true,
      available: false,
      restart_required: false,
      phase: "staging",
      staged: false,
      current_version: "0.4.7",
      latest_version: "0.4.8",
      target: "macos-arm64",
      endpoint: "https://api.ctx.rs/functions/v1/releases/stable/latest-tauri.json",
      message: "Downloading update in background.",
    },
    applyResponse: {
      applied: true,
      needs_restart: true,
      up_to_date: false,
      latest_version: "0.4.8",
      message: "Update takes ~1 second and preserves data. Active agents will be paused.",
    },
  });
  await installUpdatePolicyRoute(page, true);

  await createWorkspaceAndOpenWorkbench(page, `ws-desktop-banner-${Date.now()}`);
  await expect.poll(async () => desktopCommandCallCount(page, "desktop_get_app_update_state")).toBeGreaterThan(0);
  await expect(page.getByTestId("update-available-snackbar")).toHaveCount(0);
  await expect.poll(async () => {
    const rawArgs = await desktopLastCommandArgs(page, "desktop_set_menu_state");
    if (!rawArgs || typeof rawArgs !== "object") return null;
    const req =
      "req" in rawArgs && rawArgs.req && typeof rawArgs.req === "object"
        ? (rawArgs.req as { items?: unknown[] })
        : null;
    if (!req || !Array.isArray(req.items)) return null;
    const updateItem = req.items.find((item) => {
      return (
        item &&
        typeof item === "object" &&
        "id" in item &&
        item.id === "help.check-for-updates"
      );
    });
    return updateItem ?? null;
  }).toMatchObject({
    id: "help.check-for-updates",
    enabled: false,
    text: "Downloading Update",
  });
  await expect.poll(async () => desktopMenuItemState(page, "help.check-for-updates")).toMatchObject({
    id: "help.check-for-updates",
    enabled: false,
    text: "Downloading Update",
  });
  await expect.poll(async () => desktopCommandCallCount(page, "plugin:event|listen")).toBeGreaterThan(0);

  await emitDesktopMenuAction(page, "help.check-for-updates");
  await expect.poll(async () => desktopCommandCallCount(page, "desktop_check_app_update")).toBe(0);
  await expect.poll(async () => desktopCommandCallCount(page, "desktop_restart_app")).toBe(0);
});

test("desktop Relaunch requests restart in restart-required state", async ({ page }) => {
  await page.addInitScript((autoApplyKey: string, snoozeKey: string, idleKey: string, restartKey: string) => {
    localStorage.removeItem("ctx_update_check_v1");
    localStorage.removeItem(snoozeKey);
    localStorage.removeItem(idleKey);
    localStorage.setItem(autoApplyKey, "0");
    sessionStorage.removeItem(restartKey);
  }, AUTO_APPLY_ON_LAUNCH_STORAGE_KEY, PROMPT_SNOOZE_STORAGE_KEY, IDLE_UPDATE_VERSION_STORAGE_KEY, RESTART_REQUIRED_VERSION_STORAGE_KEY);
  await installDesktopHarness(page, {
    authToken: E2E_AUTH_TOKEN,
    updateState: {
      configured: true,
      available: false,
      restart_required: true,
      phase: "restart_required",
      staged: false,
      current_version: "0.4.7",
      latest_version: "0.4.8",
      target: "macos-arm64",
      endpoint: "https://api.ctx.rs/functions/v1/releases/stable/latest-tauri.json",
      message: null,
    },
    applyResponse: {
      applied: true,
      needs_restart: true,
      up_to_date: false,
      latest_version: "0.4.8",
      message: "Update takes ~1 second and preserves data. Active agents will be paused.",
    },
  });
  await installUpdatePolicyRoute(page, true);

  await createWorkspaceAndOpenWorkbench(page, `ws-desktop-apply-now-${Date.now()}`);
  await expect(page.getByTestId("update-available-snackbar")).toBeVisible({ timeout: 20_000 });
  await page.getByRole("button", { name: "Relaunch" }).dispatchEvent("click");
  await expect.poll(async () => desktopCommandCallCount(page, "desktop_restart_app")).toBe(1);
  await expect.poll(async () => desktopCommandCallCount(page, "desktop_apply_app_update")).toBe(0);
  await expect(page.getByRole("button", { name: "Relaunch" })).toBeVisible({ timeout: 20_000 });
});

test("desktop Help menu requests restart when update is ready", async ({ page }) => {
  await page.addInitScript((autoApplyKey: string, snoozeKey: string, idleKey: string, restartKey: string) => {
    localStorage.removeItem("ctx_update_check_v1");
    localStorage.removeItem(snoozeKey);
    localStorage.removeItem(idleKey);
    localStorage.setItem(autoApplyKey, "0");
    sessionStorage.removeItem(restartKey);
  }, AUTO_APPLY_ON_LAUNCH_STORAGE_KEY, PROMPT_SNOOZE_STORAGE_KEY, IDLE_UPDATE_VERSION_STORAGE_KEY, RESTART_REQUIRED_VERSION_STORAGE_KEY);
  await installDesktopHarness(page, {
    authToken: E2E_AUTH_TOKEN,
    updateState: {
      configured: true,
      available: false,
      restart_required: true,
      phase: "restart_required",
      staged: false,
      current_version: "0.4.7",
      latest_version: "0.4.8",
      target: "macos-arm64",
      endpoint: "https://api.ctx.rs/functions/v1/releases/stable/latest-tauri.json",
      message: null,
    },
    applyResponse: {
      applied: true,
      needs_restart: true,
      up_to_date: false,
      latest_version: "0.4.8",
      message: "Update takes ~1 second and preserves data. Active agents will be paused.",
    },
  });
  await installUpdatePolicyRoute(page, true);

  await createWorkspaceAndOpenWorkbench(page, `ws-desktop-help-restart-${Date.now()}`);
  await expect(page.getByTestId("update-available-snackbar")).toBeVisible({ timeout: 20_000 });
  await expect.poll(async () => desktopMenuItemState(page, "help.check-for-updates")).toMatchObject({
    id: "help.check-for-updates",
    enabled: true,
    text: "Restart to Update",
  });
  await expect.poll(async () => desktopCommandCallCount(page, "plugin:event|listen")).toBeGreaterThan(0);

  await emitDesktopMenuAction(page, "help.check-for-updates");
  await expect.poll(async () => desktopCommandCallCount(page, "desktop_restart_app")).toBe(1);
  await expect.poll(async () => desktopCommandCallCount(page, "desktop_check_app_update")).toBe(0);
  await expect.poll(async () => desktopCommandCallCount(page, "desktop_apply_app_update")).toBe(0);
});

test("desktop Update on Next Idle schedules restart when restart is ready", async ({ page }) => {
  await page.addInitScript((autoApplyKey: string, snoozeKey: string, idleKey: string, restartKey: string) => {
    localStorage.removeItem("ctx_update_check_v1");
    localStorage.removeItem(snoozeKey);
    localStorage.removeItem(idleKey);
    localStorage.setItem(autoApplyKey, "0");
    sessionStorage.removeItem(restartKey);
  }, AUTO_APPLY_ON_LAUNCH_STORAGE_KEY, PROMPT_SNOOZE_STORAGE_KEY, IDLE_UPDATE_VERSION_STORAGE_KEY, RESTART_REQUIRED_VERSION_STORAGE_KEY);
  await installDesktopHarness(page, {
    authToken: E2E_AUTH_TOKEN,
    updateState: {
      configured: true,
      available: false,
      restart_required: true,
      phase: "restart_required",
      staged: false,
      current_version: "0.4.7",
      latest_version: "0.4.8",
      target: "macos-arm64",
      endpoint: "https://api.ctx.rs/functions/v1/releases/stable/latest-tauri.json",
      message: null,
    },
    applyResponse: {
      applied: true,
      needs_restart: true,
      up_to_date: false,
      latest_version: "0.4.8",
      message: "Update takes ~1 second and preserves data. Active agents will be paused.",
    },
  });
  await installUpdatePolicyRoute(page, true);

  await createWorkspaceAndOpenWorkbench(page, `ws-desktop-apply-idle-${Date.now()}`);
  await expect(page.getByTestId("update-available-snackbar")).toBeVisible({ timeout: 20_000 });
  await page.getByRole("button", { name: "Update on Next Idle" }).dispatchEvent("click");
  await expect.poll(async () => desktopCommandCallCount(page, "desktop_restart_app")).toBe(1);
  await expect.poll(async () => desktopCommandCallCount(page, "desktop_apply_app_update")).toBe(0);
  await expect(page.getByRole("button", { name: "Relaunch" })).toBeVisible({ timeout: 20_000 });
});

test("desktop auto-applies when a staged update is ready", async ({ page }) => {
  await page.addInitScript((autoApplyKey: string, snoozeKey: string, idleKey: string, restartKey: string) => {
    localStorage.removeItem("ctx_update_check_v1");
    localStorage.removeItem(snoozeKey);
    localStorage.removeItem(idleKey);
    localStorage.setItem(autoApplyKey, "1");
    sessionStorage.removeItem(restartKey);
  }, AUTO_APPLY_ON_LAUNCH_STORAGE_KEY, PROMPT_SNOOZE_STORAGE_KEY, IDLE_UPDATE_VERSION_STORAGE_KEY, RESTART_REQUIRED_VERSION_STORAGE_KEY);
  await installDesktopHarness(page, {
    authToken: E2E_AUTH_TOKEN,
    updateState: {
      configured: true,
      available: true,
      restart_required: false,
      phase: "staged_ready",
      staged: true,
      current_version: "0.4.7",
      latest_version: "0.4.8",
      target: "macos-arm64",
      endpoint: "https://api.ctx.rs/functions/v1/releases/stable/latest-tauri.json",
      message: null,
    },
    applyResponse: {
      applied: true,
      needs_restart: true,
      up_to_date: false,
      latest_version: "0.4.8",
      message: "Update takes ~1 second and preserves data. Active agents will be paused.",
    },
  });
  await installUpdatePolicyRoute(page, true);

  await createWorkspaceAndOpenWorkbench(page, `ws-desktop-auto-${Date.now()}`);
  await expect.poll(async () => desktopCommandCallCount(page, "desktop_apply_app_update")).toBe(1);
  await expect(page.getByRole("button", { name: "Relaunch" })).toBeVisible({ timeout: 20_000 });
});
