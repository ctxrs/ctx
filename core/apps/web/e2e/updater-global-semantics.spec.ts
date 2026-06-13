import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import type { Page } from "playwright/test";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";

const E2E_AUTH_TOKEN = process.env.CTX_E2E_AUTH_TOKEN ?? "ctx-e2e-auth-token";

// Contract reference:
// core/apps/web/e2e/contracts/updater_global_semantics_contract.md
const AUTO_APPLY_ON_LAUNCH_STORAGE_KEY = "ctx_update_auto_apply_on_launch_v1";
const PROMPT_SNOOZE_STORAGE_KEY = "ctx_update_prompt_next_allowed_at_v1";
const IDLE_UPDATE_VERSION_STORAGE_KEY = "ctx_update_prompt_idle_versions_v1";
const RESTART_REQUIRED_VERSION_STORAGE_KEY = "ctx_update_restart_required_version_v1";
const REQUEST_UPDATE_CHECK_EVENT = "ctx:request-update-check";
const WORKBENCH_TASK_IDLE_EVENT = "ctx:workbench-task-idle";
const UPDATER_REFRESH_BROADCAST_STORAGE_KEY = "ctx_update_refresh_token_v1";

type DesktopUpdateState = {
  configured: boolean;
  available: boolean;
  staged: boolean;
  restart_required: boolean;
  phase: "idle" | "staging" | "staged_ready" | "restart_required" | "failed";
  current_version: string;
  latest_version: string | null;
  target: string;
  endpoint: string;
  message: string | null;
  last_error: string | null;
};

type DesktopApplyResponse = {
  applied: boolean;
  needs_restart: boolean;
  up_to_date: boolean;
  latest_version: string | null;
  message: string;
};

type SharedHarnessState = {
  updateState: DesktopUpdateState;
  applyResponse: DesktopApplyResponse;
  restartShouldFail: boolean;
  restartErrorMessage: string;
};

type HarnessSnapshot = {
  at: number;
  state: DesktopUpdateState;
};

type HarnessWindowState = {
  sharedStorageKey: string;
  invokeCalls: string[];
  snapshots: HarnessSnapshot[];
};

type DesktopHarnessConfig = {
  authToken: string;
  sharedStorageKey: string;
  sharedState: SharedHarnessState;
};

const createTempRepo = (prefix: string): string => {
  const repo = mkdtempSync(path.join(tmpdir(), prefix));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "README.md"), "updater global semantics fixture\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });
  return repo;
};

const clearUpdateStorageKeys = async (page: Page): Promise<void> => {
  await page.addInitScript(
    ({
      autoApplyKey,
      snoozeKey,
      idleKey,
      restartKey,
      refreshKey,
    }: {
      autoApplyKey: string;
      snoozeKey: string;
      idleKey: string;
      restartKey: string;
      refreshKey: string;
    }) => {
      try {
        localStorage.removeItem("ctx_update_check_v1");
        localStorage.removeItem(snoozeKey);
        localStorage.removeItem(idleKey);
        localStorage.removeItem(refreshKey);
        localStorage.setItem(autoApplyKey, "0");
        sessionStorage.removeItem(restartKey);
      } catch {
        // Ignore storage failures in pre-navigation contexts.
      }
    },
    {
      autoApplyKey: AUTO_APPLY_ON_LAUNCH_STORAGE_KEY,
      snoozeKey: PROMPT_SNOOZE_STORAGE_KEY,
      idleKey: IDLE_UPDATE_VERSION_STORAGE_KEY,
      restartKey: RESTART_REQUIRED_VERSION_STORAGE_KEY,
      refreshKey: UPDATER_REFRESH_BROADCAST_STORAGE_KEY,
    },
  );
};

const installUpdatePolicyRoute = async (page: Page): Promise<void> => {
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
        update_available: true,
        platform_supported: true,
        in_place_update_supported: true,
        in_place_update_reason: null,
      }),
    });
  });
};

const installDesktopHarness = async (page: Page, config: DesktopHarnessConfig): Promise<void> => {
  await page.addInitScript((initial: DesktopHarnessConfig) => {
    type TauriInvoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
    type TauriInternals = {
      invoke?: TauriInvoke;
      transformCallback?: (cb: unknown, once?: boolean) => number;
      unregisterCallback?: (id: number) => void;
      metadata?: Record<string, unknown>;
    } & Record<string, unknown>;
    type HarnessWindow = Window & {
      __TAURI__?: { core?: { invoke?: TauriInvoke } };
      __TAURI_INTERNALS__?: TauriInternals;
      __ctxDesktopUpdaterSemanticsE2E?: HarnessWindowState;
    };

    const w = window as HarnessWindow;

    const readState = (): SharedHarnessState => {
      try {
        const raw = localStorage.getItem(initial.sharedStorageKey);
        if (raw) {
          const parsed = JSON.parse(raw) as SharedHarnessState;
          if (parsed && parsed.updateState && parsed.applyResponse) return parsed;
        }
      } catch {
        // Ignore parse failures and rewrite with initial.
      }
      localStorage.setItem(initial.sharedStorageKey, JSON.stringify(initial.sharedState));
      return initial.sharedState;
    };

    const writeState = (next: SharedHarnessState): void => {
      localStorage.setItem(initial.sharedStorageKey, JSON.stringify(next));
    };

    readState();

    const perWindow: HarnessWindowState = {
      sharedStorageKey: initial.sharedStorageKey,
      invokeCalls: [],
      snapshots: [],
    };

    const currentDaemonTarget = () => {
      const parsed = new URL(window.location.origin);
      const port = parsed.port
        ? Number(parsed.port)
        : parsed.protocol === "https:"
          ? 443
          : 80;
      return {
        base_url: parsed.origin,
        port,
      };
    };

    const invoke: TauriInvoke = async (cmd, rawArgs) => {
      const name = String(cmd || "");
      perWindow.invokeCalls.push(name);
      if (name === "plugin:app|version") {
        return readState().updateState.current_version;
      }
      if (name === "desktop_get_connection") {
        const target = currentDaemonTarget();
        return {
          kind: "local",
          base_url: target.base_url,
          token: initial.authToken,
        };
      }
      if (name === "desktop_check_app_update") {
        const state = readState();
        return {
          current_version: state.updateState.current_version,
          latest_version: state.updateState.latest_version,
          available: state.updateState.available,
          restart_required: state.updateState.restart_required,
          configured: state.updateState.configured,
          target: state.updateState.target,
          endpoint: state.updateState.endpoint,
          message: state.updateState.message,
          phase: state.updateState.phase,
          staged: state.updateState.staged,
          last_error: state.updateState.last_error,
        };
      }
      if (name === "desktop_get_app_update_state") {
        const state = readState();
        const snapshot = { ...state.updateState };
        perWindow.snapshots.push({
          at: Date.now(),
          state: snapshot,
        });
        return snapshot;
      }
      if (name === "desktop_apply_app_update") {
        const state = readState();
        const response = { ...state.applyResponse };
        if (response.needs_restart) {
          state.updateState = {
            ...state.updateState,
            available: false,
            staged: false,
            restart_required: true,
            phase: "restart_required",
            latest_version: response.latest_version ?? state.updateState.latest_version,
            message: response.message,
          };
          writeState(state);
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
            message: response.message,
            last_error: null,
          };
          writeState(state);
        }
        return response;
      }
      if (name === "desktop_restart_app") {
        const state = readState();
        if (state.restartShouldFail) {
          throw new Error(state.restartErrorMessage || "Restart failed.");
        }
        return { requested: true, message: "Restart requested." };
      }
      if (name === "desktop_daemon_request") {
        const args = rawArgs && typeof rawArgs === "object" ? (rawArgs as Record<string, unknown>) : {};
        const req = args.req && typeof args.req === "object" ? (args.req as Record<string, unknown>) : {};
        const reqPath = String(req.path ?? "/");
        const method = String(req.method ?? "GET").toUpperCase();
        const body = typeof req.body === "string" ? req.body : "";
        const headers = req.headers && typeof req.headers === "object" ? (req.headers as Record<string, unknown>) : {};
        const fetchHeaders: Record<string, string> = {};
        for (const [key, value] of Object.entries(headers)) {
          if (typeof value === "string") fetchHeaders[key] = value;
        }
        if (!fetchHeaders.authorization) {
          fetchHeaders.authorization = `Bearer ${initial.authToken}`;
        }
        if (!fetchHeaders["content-type"] && body) {
          fetchHeaders["content-type"] = "application/json";
        }
        const response = await fetch(reqPath, {
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
          : () => 1,
      unregisterCallback:
        typeof existingInternals.unregisterCallback === "function"
          ? existingInternals.unregisterCallback
          : () => {},
      invoke,
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
    w.__ctxDesktopUpdaterSemanticsE2E = perWindow;
  }, config);
};

const desktopCallCount = async (page: Page, command: string): Promise<number> => {
  return await page.evaluate((name: string) => {
    const w = window as Window & { __ctxDesktopUpdaterSemanticsE2E?: HarnessWindowState };
    const calls = w.__ctxDesktopUpdaterSemanticsE2E?.invokeCalls ?? [];
    return calls.filter((cmd) => cmd === name).length;
  }, command);
};

const readHarnessDiagnostics = async (page: Page) => {
  return await page.evaluate(() => {
    const w = window as Window & { __ctxDesktopUpdaterSemanticsE2E?: HarnessWindowState };
    const state = w.__ctxDesktopUpdaterSemanticsE2E;
    if (!state) return { invokeCalls: [], snapshots: [], sharedStorageKey: "" };
    return {
      invokeCalls: state.invokeCalls,
      snapshots: state.snapshots,
      sharedStorageKey: state.sharedStorageKey,
    };
  });
};

const mutateSharedHarnessState = async (
  page: Page,
  patch: Partial<SharedHarnessState> & { updateState?: Partial<DesktopUpdateState> },
): Promise<void> => {
  await page.evaluate((nextPatch: Partial<SharedHarnessState> & { updateState?: Partial<DesktopUpdateState> }) => {
    const w = window as Window & { __ctxDesktopUpdaterSemanticsE2E?: HarnessWindowState };
    const key = w.__ctxDesktopUpdaterSemanticsE2E?.sharedStorageKey;
    if (!key) return;
    const raw = localStorage.getItem(key);
    if (!raw) return;
    const parsed = JSON.parse(raw) as SharedHarnessState;
    const merged: SharedHarnessState = {
      ...parsed,
      ...nextPatch,
      updateState: {
        ...parsed.updateState,
        ...(nextPatch.updateState ?? {}),
      },
    };
    localStorage.setItem(key, JSON.stringify(merged));
  }, patch);
};

const broadcastUpdaterRefresh = async (page: Page, reason: string): Promise<void> => {
  await page.evaluate(
    ({ key, why }: { key: string; why: string }) => {
      localStorage.setItem(key, JSON.stringify({ at: Date.now(), reason: why }));
    },
    { key: UPDATER_REFRESH_BROADCAST_STORAGE_KEY, why: reason },
  );
};

const dispatchIdleEvent = async (page: Page, allTasksIdle: boolean): Promise<void> => {
  await page.evaluate(
    ({ eventName, idle }: { eventName: string; idle: boolean }) => {
      window.dispatchEvent(
        new CustomEvent(eventName, {
          detail: { allTasksIdle: idle },
        }),
      );
    },
    { eventName: WORKBENCH_TASK_IDLE_EVENT, idle: allTasksIdle },
  );
};

const dispatchManualCheckEvent = async (page: Page): Promise<void> => {
  await page.evaluate((eventName: string) => {
    window.dispatchEvent(new Event(eventName));
  }, REQUEST_UPDATE_CHECK_EVENT);
};

const refreshUpdaterAcrossWindows = async (page: Page, reason: string): Promise<void> => {
  await broadcastUpdaterRefresh(page, reason);
  await dispatchManualCheckEvent(page);
};

const waitForIdleVersionScheduled = async (page: Page, version: string): Promise<void> => {
  await expect
    .poll(
      async () =>
        await page.evaluate((idleKey: string) => {
          return localStorage.getItem(idleKey);
        }, IDLE_UPDATE_VERSION_STORAGE_KEY),
      { timeout: 5_000 },
    )
    .toContain(version);
};

const waitForRestartRequiredVersionState = async (page: Page, version: string): Promise<void> => {
  await expect
    .poll(
      async () =>
        await page.evaluate((restartKey: string) => {
          return sessionStorage.getItem(restartKey);
        }, RESTART_REQUIRED_VERSION_STORAGE_KEY),
      { timeout: 5_000 },
    )
    .toBe(version);
};

test("global updater checks stay app-scoped across launcher, wizard, and workbench", async ({ page }, testInfo) => {
  const sharedStorageKey = `ctx_updater_global_semantics_${Date.now()}`;
  await clearUpdateStorageKeys(page);
  await installDesktopHarness(page, {
    authToken: E2E_AUTH_TOKEN,
    sharedStorageKey,
    sharedState: {
      updateState: {
        configured: true,
        available: true,
        staged: false,
        restart_required: false,
        phase: "staging",
        current_version: "0.5.0",
        latest_version: "0.5.1",
        target: "macos-arm64",
        endpoint: "https://api.ctx.rs/functions/v1/releases/stable/latest-tauri.json",
        message: null,
        last_error: null,
      },
      applyResponse: {
        applied: true,
        needs_restart: true,
        up_to_date: false,
        latest_version: "0.5.1",
        message: "Update takes ~1 second and preserves data. Active agents will be paused.",
      },
      restartShouldFail: false,
      restartErrorMessage: "Failed to restart app.",
    },
  });
  await installUpdatePolicyRoute(page);

  await page.goto("/", { waitUntil: "domcontentloaded" });
  await expect.poll(async () => desktopCallCount(page, "desktop_get_app_update_state")).toBeGreaterThan(0);
  await expect(page.getByTestId("update-available-snackbar")).toHaveCount(0);

  const previousCount = await desktopCallCount(page, "desktop_get_app_update_state");
  await dispatchManualCheckEvent(page);
  await expect.poll(async () => desktopCallCount(page, "desktop_get_app_update_state")).toBeGreaterThan(previousCount);

  await page.goto("/workspace-setup", { waitUntil: "domcontentloaded" });
  await expect(page).toHaveURL(/\/workspace-setup$/);
  await expect.poll(async () => desktopCallCount(page, "desktop_get_app_update_state")).toBeGreaterThan(0);
  await expect(page.getByTestId("update-available-snackbar")).toHaveCount(0);

  const repo = createTempRepo("ctx-e2e-updater-global-semantics-");
  const workspaceId = await createWorkspaceAndOpenWorkbench({
    page,
    request: page.request,
    repo,
    workspaceName: `ws-global-semantics-${Date.now()}`,
  });
  await expect(page).toHaveURL(new RegExp(`/workspaces/${workspaceId}(\\?.*)?$`));
  await expect.poll(async () => desktopCallCount(page, "desktop_get_app_update_state")).toBeGreaterThan(0);
  await expect(page.getByTestId("update-available-snackbar")).toHaveCount(0);

  await mutateSharedHarnessState(page, {
    updateState: {
      available: true,
      staged: true,
      phase: "staged_ready",
    },
  });
  await dispatchManualCheckEvent(page);
  await expect.poll(async () => {
    const diagnostics = await readHarnessDiagnostics(page);
    const phases = diagnostics.snapshots.map((entry) => entry.state.phase);
    return phases.includes("staged_ready");
  }).toBeTruthy();
  await expect.poll(async () => desktopCallCount(page, "desktop_apply_app_update")).toBeGreaterThan(0);
  await expect(page.getByTestId("update-available-snackbar")).toBeVisible({ timeout: 20_000 });

  const diagnostics = await readHarnessDiagnostics(page);
  const outputPath = testInfo.outputPath("updater-global-visibility-diagnostics.json");
  writeFileSync(
    outputPath,
    `${JSON.stringify(
      {
        route: await page.url(),
        invoke_calls: diagnostics.invokeCalls,
        snapshot_count: diagnostics.snapshots.length,
        snapshots: diagnostics.snapshots,
      },
      null,
      2,
    )}\n`,
    "utf8",
  );
});

test("restart-required state converges across windows and idle scheduling stays consistent", async ({ page }, testInfo) => {
  const sharedStorageKey = `ctx_updater_multi_window_${Date.now()}`;
  await clearUpdateStorageKeys(page);
  await installDesktopHarness(page, {
    authToken: E2E_AUTH_TOKEN,
    sharedStorageKey,
    sharedState: {
      updateState: {
        configured: true,
        available: false,
        staged: false,
        restart_required: false,
        phase: "idle",
        current_version: "0.5.0",
        latest_version: null,
        target: "macos-arm64",
        endpoint: "https://api.ctx.rs/functions/v1/releases/stable/latest-tauri.json",
        message: null,
        last_error: null,
      },
      applyResponse: {
        applied: true,
        needs_restart: true,
        up_to_date: false,
        latest_version: "0.5.1",
        message: "Update takes ~1 second and preserves data. Active agents will be paused.",
      },
      restartShouldFail: false,
      restartErrorMessage: "Failed to restart app.",
    },
  });
  await installUpdatePolicyRoute(page);

  const repo = createTempRepo("ctx-e2e-updater-multi-window-");
  const workspaceId = await createWorkspaceAndOpenWorkbench({
    page,
    request: page.request,
    repo,
    workspaceName: `ws-multi-window-${Date.now()}`,
  });

  const page2 = await page.context().newPage();
  await clearUpdateStorageKeys(page2);
  await installDesktopHarness(page2, {
    authToken: E2E_AUTH_TOKEN,
    sharedStorageKey,
    sharedState: {
      updateState: {
        configured: true,
        available: false,
        staged: false,
        restart_required: false,
        phase: "idle",
        current_version: "0.5.0",
        latest_version: null,
        target: "macos-arm64",
        endpoint: "https://api.ctx.rs/functions/v1/releases/stable/latest-tauri.json",
        message: null,
        last_error: null,
      },
      applyResponse: {
        applied: true,
        needs_restart: true,
        up_to_date: false,
        latest_version: "0.5.1",
        message: "Update takes ~1 second and preserves data. Active agents will be paused.",
      },
      restartShouldFail: false,
      restartErrorMessage: "Failed to restart app.",
    },
  });
  await installUpdatePolicyRoute(page2);
  await page2.goto(`/workspaces/${workspaceId}`, { waitUntil: "domcontentloaded" });
  await expect(page2.locator(".wb-main")).toBeVisible({ timeout: 20_000 });
  await expect.poll(async () => desktopCallCount(page2, "desktop_get_app_update_state")).toBeGreaterThan(0);

  await mutateSharedHarnessState(page, {
    updateState: {
      available: false,
      staged: false,
      restart_required: true,
      phase: "restart_required",
      latest_version: "0.5.1",
      message: "Update takes ~1 second and preserves data. Active agents will be paused.",
      last_error: null,
    },
  });
  await refreshUpdaterAcrossWindows(page, "set-restart-required");

  await expect(page.getByTestId("update-available-snackbar")).toBeVisible({ timeout: 20_000 });
  await expect(page2.getByTestId("update-available-snackbar")).toBeVisible({ timeout: 20_000 });
  await expect(page.getByText(/Ready to relaunch:\s*0\.5\.1\./)).toBeVisible({ timeout: 20_000 });
  await expect(page2.getByText(/Ready to relaunch:\s*0\.5\.1\./)).toBeVisible({ timeout: 20_000 });

  await dispatchIdleEvent(page, false);
  await dispatchIdleEvent(page2, false);
  const restartBaselinePage1 = await desktopCallCount(page, "desktop_restart_app");
  const restartBaselinePage2 = await desktopCallCount(page2, "desktop_restart_app");
  const restartBaselineCombined = restartBaselinePage1 + restartBaselinePage2;
  await page.getByRole("button", { name: "Update on Next Idle" }).dispatchEvent("click");
  await expect
    .poll(async () => desktopCallCount(page, "desktop_restart_app"), { timeout: 3_000 })
    .toBe(restartBaselinePage1);
  await expect
    .poll(async () => desktopCallCount(page2, "desktop_restart_app"), { timeout: 3_000 })
    .toBe(restartBaselinePage2);
  await waitForIdleVersionScheduled(page2, "0.5.1");

  await dispatchIdleEvent(page, true);
  await dispatchIdleEvent(page2, true);
  await expect
    .poll(
      async () =>
        (await desktopCallCount(page, "desktop_restart_app")) + (await desktopCallCount(page2, "desktop_restart_app")),
      { timeout: 5_000 },
    )
    .toBeGreaterThan(restartBaselineCombined);
  await expect.poll(async () => desktopCallCount(page, "desktop_apply_app_update")).toBe(0);
  await expect.poll(async () => desktopCallCount(page2, "desktop_apply_app_update")).toBe(0);

  const outputPath = testInfo.outputPath("updater-multi-window-diagnostics.json");
  writeFileSync(
    outputPath,
    `${JSON.stringify(
      {
        page1: await readHarnessDiagnostics(page),
        page2: await readHarnessDiagnostics(page2),
      },
      null,
      2,
    )}\n`,
    "utf8",
  );

  await page2.close();
});

test("Update on Next Idle waits while active and can recover from restart failure", async ({ page }, testInfo) => {
  const sharedStorageKey = `ctx_updater_idle_recovery_${Date.now()}`;
  await clearUpdateStorageKeys(page);
  await installDesktopHarness(page, {
    authToken: E2E_AUTH_TOKEN,
    sharedStorageKey,
    sharedState: {
      updateState: {
        configured: true,
        available: false,
        staged: false,
        restart_required: true,
        phase: "restart_required",
        current_version: "0.5.0",
        latest_version: "0.5.1",
        target: "macos-arm64",
        endpoint: "https://api.ctx.rs/functions/v1/releases/stable/latest-tauri.json",
        message: "Update takes ~1 second and preserves data. Active agents will be paused.",
        last_error: null,
      },
      applyResponse: {
        applied: true,
        needs_restart: true,
        up_to_date: false,
        latest_version: "0.5.1",
        message: "Update takes ~1 second and preserves data. Active agents will be paused.",
      },
      restartShouldFail: true,
      restartErrorMessage: "Restart test failure.",
    },
  });
  await installUpdatePolicyRoute(page);

  const repo = createTempRepo("ctx-e2e-updater-idle-recovery-");
  await createWorkspaceAndOpenWorkbench({
    page,
    request: page.request,
    repo,
    workspaceName: `ws-idle-recovery-${Date.now()}`,
  });

  await expect(page.getByTestId("update-available-snackbar")).toBeVisible({ timeout: 20_000 });
  await waitForRestartRequiredVersionState(page, "0.5.1");
  await dispatchIdleEvent(page, false);
  const restartBaseline = await desktopCallCount(page, "desktop_restart_app");
  await page.getByRole("button", { name: "Update on Next Idle" }).dispatchEvent("click");
  await waitForIdleVersionScheduled(page, "0.5.1");
  await dispatchIdleEvent(page, false);
  await expect
    .poll(async () => desktopCallCount(page, "desktop_restart_app"), { timeout: 3_000 })
    .toBe(restartBaseline);

  await dispatchIdleEvent(page, true);
  await expect.poll(async () => desktopCallCount(page, "desktop_restart_app")).toBe(1);
  await expect(page.getByText("Restart test failure.")).toBeVisible({ timeout: 20_000 });

  await mutateSharedHarnessState(page, { restartShouldFail: false });
  await dispatchIdleEvent(page, false);
  await waitForRestartRequiredVersionState(page, "0.5.1");
  const restartRecoveryBaseline = await desktopCallCount(page, "desktop_restart_app");
  await expect
    .poll(async () => desktopCallCount(page, "desktop_restart_app"), { timeout: 1_000 })
    .toBe(restartRecoveryBaseline);
  await page.getByRole("button", { name: "Update on Next Idle" }).dispatchEvent("click");
  await expect
    .poll(async () => desktopCallCount(page, "desktop_restart_app"), { timeout: 3_000 })
    .toBe(restartRecoveryBaseline);
  await dispatchIdleEvent(page, true);
  await expect.poll(async () => desktopCallCount(page, "desktop_restart_app")).toBe(restartRecoveryBaseline + 1);

  const outputPath = testInfo.outputPath("updater-idle-recovery-diagnostics.json");
  writeFileSync(
    outputPath,
    `${JSON.stringify(
      {
        diagnostics: await readHarnessDiagnostics(page),
      },
      null,
      2,
    )}\n`,
    "utf8",
  );
});
