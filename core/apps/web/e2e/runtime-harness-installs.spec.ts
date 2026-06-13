import { test, expect, type Page } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";

type InstallMockState = {
  providerId: string;
  installId: string;
  installed: boolean;
  installRunning: boolean;
  installCalls: number;
  installPolls: number;
};

const nowIso = () => new Date().toISOString();
const E2E_AUTH_TOKEN = "ctx-e2e-auth-token";

const initRepo = (): string => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init -b main", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "README.md"), "runtime harness installs e2e\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });
  return repo;
};

const makeInstallState = (providerId: string): InstallMockState => ({
  providerId,
  installId: `install-${providerId}-e2e`,
  installed: false,
  installRunning: false,
  installCalls: 0,
  installPolls: 0,
});

const providerStatus = (state: InstallMockState) => ({
  provider_id: state.providerId,
  installed: state.installed,
  usability: {
    usable: state.installed,
    reason: state.installed ? null : "Not installed",
  },
  health: state.installed ? "ok" : "error",
  diagnostics: state.installed ? [] : ["Not installed"],
  details: {
    install_supported: "true",
    install_running: state.installRunning ? "true" : "false",
    install_id: state.installRunning ? state.installId : "",
  },
});

const cursorReadyProviderStatus = {
  provider_id: "cursor",
  installed: true,
  usability: {
    usable: true,
    reason: null,
  },
  health: "ok",
  diagnostics: [],
  details: {
    install_supported: "true",
    install_running: "false",
    install_id: "",
  },
};

const bootstrapPayload = (workspaceId: string, state: InstallMockState, includeCursorReady: boolean) => {
  const providers = includeCursorReady ? [providerStatus(state), cursorReadyProviderStatus] : [providerStatus(state)];
  const providerOptionsBase = {
    provider_id: state.providerId,
    workspace_id: workspaceId,
    supports_load: false,
    auth_required: false,
    has_active_auth: state.installed,
    auth_mode: state.installed ? "subscription" : "none",
    probed_at: nowIso(),
  };
  const providerOptions = includeCursorReady
    ? {
        [state.providerId]: providerOptionsBase,
        cursor: {
          provider_id: "cursor",
          workspace_id: workspaceId,
          supports_load: false,
          auth_required: false,
          has_active_auth: true,
          auth_mode: "subscription",
          probed_at: nowIso(),
        },
      }
    : { [state.providerId]: providerOptionsBase };

  const providerHarnessConfig = includeCursorReady
    ? {
        [state.providerId]: {
          provider_id: state.providerId,
          selected_source_kind: "subscription",
          selected_endpoint_id: null,
          endpoints: [],
        },
        cursor: {
          provider_id: "cursor",
          selected_source_kind: "subscription",
          selected_endpoint_id: null,
          endpoints: [],
        },
      }
    : {
        [state.providerId]: {
          provider_id: state.providerId,
          selected_source_kind: "subscription",
          selected_endpoint_id: null,
          endpoints: [],
        },
      };

  return {
    providers,
    provider_options: providerOptions,
    provider_harness_config: providerHarnessConfig,
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
  };
};

const installDesktopHarness = async (page: Page) => {
  await page.addInitScript((token: string) => {
    type TauriInvoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
    type DesktopHttpResponse = { status: number; body: string; content_type?: string | null };
    type TauriInternals = {
      invoke?: TauriInvoke;
      transformCallback?: (cb: unknown, once?: boolean) => number;
      unregisterCallback?: (id: number) => void;
      unregisterListener?: (id: number) => void;
      metadata?: Record<string, unknown>;
    } & Record<string, unknown>;
    type TauriWindow = Window & {
      __TAURI__?: { core?: { invoke?: TauriInvoke } };
      __TAURI_INTERNALS__?: TauriInternals;
    };

    const conn = {
      kind: "local",
      base_url: window.location.origin,
      token,
    };

    const invoke: TauriInvoke = async (cmd, rawArgs) => {
      const name = String(cmd ?? "");
      if (name === "desktop_get_connection" || name === "desktop_connect_local") {
        return conn;
      }
      if (name === "desktop_list_ssh_hosts") {
        return [];
      }
      if (name === "desktop_daemon_request") {
        const args = rawArgs && typeof rawArgs === "object" ? (rawArgs as Record<string, unknown>) : {};
        const req = args.req && typeof args.req === "object" ? (args.req as Record<string, unknown>) : {};
        const method = String(req.method ?? "GET");
        const path = String(req.path ?? "/");
        const body = req.body === undefined || req.body === null ? undefined : String(req.body);
        const headersList = Array.isArray(req.headers) ? (req.headers as Array<[string, string]>) : [];
        const headers = new Headers();
        for (const [k, v] of headersList) {
          if (typeof k === "string" && typeof v === "string") headers.set(k, v);
        }
        if (!headers.has("authorization")) headers.set("authorization", `Bearer ${token}`);
        const target = path.startsWith("http://") || path.startsWith("https://")
          ? path
          : `${window.location.origin}${path}`;
        const res = await fetch(target, {
          method,
          headers,
          body,
        });
        const text = await res.text();
        const out: DesktopHttpResponse = {
          status: res.status,
          body: text,
          content_type: res.headers.get("content-type"),
        };
        return out;
      }
      if (name === "plugin:app|version") {
        return "0.0.0-e2e";
      }
      return null;
    };

    const w = window as TauriWindow;
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
      unregisterListener:
        typeof existingInternals.unregisterListener === "function"
          ? existingInternals.unregisterListener
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
  }, E2E_AUTH_TOKEN);
};

const wireInstallLifecycleRoutes = async (
  page: Page,
  state: InstallMockState,
  opts?: { includeCursorReady?: boolean; includeBootstrap?: boolean },
) => {
  const includeCursorReady = opts?.includeCursorReady ?? false;
  const includeBootstrap = opts?.includeBootstrap ?? false;
  const nextInstallInfo = async () => {
    state.installPolls += 1;
    if (state.installPolls <= 1) {
      return {
        install_id: state.installId,
        provider_id: state.providerId,
        state: "running" as const,
        started_at: nowIso(),
        last_event: {
          install_id: state.installId,
          provider_id: state.providerId,
          at: nowIso(),
          stage: "download",
          message: "Downloading",
          level: "info" as const,
          bytes: 50,
          total_bytes: 100,
        },
      };
    }

    state.installRunning = false;
    state.installed = true;
    if (state.installPolls > 2) {
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    return {
      install_id: state.installId,
      provider_id: state.providerId,
      state: "succeeded" as const,
      started_at: nowIso(),
      finished_at: nowIso(),
      last_event: {
        install_id: state.installId,
        provider_id: state.providerId,
        at: nowIso(),
        stage: "refresh",
        message: "Completed",
        level: "success" as const,
        bytes: 100,
        total_bytes: 100,
      },
    };
  };

  await page.route(/\/api\/providers(?:\?.*)?$/, async (route) => {
    if (route.request().method() !== "GET") {
      await route.continue();
      return;
    }
    const providers = includeCursorReady ? [providerStatus(state), cursorReadyProviderStatus] : [providerStatus(state)];
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(providers),
    });
  });

  if (includeBootstrap) {
    await page.route(/\/api\/workspaces\/[^/]+\/providers\/bootstrap(?:\?.*)?$/, async (route) => {
      if (route.request().method() !== "GET") {
        await route.continue();
        return;
      }
      const url = new URL(route.request().url());
      const match = url.pathname.match(/^\/api\/workspaces\/([^/]+)\/providers\/bootstrap$/);
      const workspaceId = match ? decodeURIComponent(match[1]) : "";
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify(bootstrapPayload(workspaceId, state, includeCursorReady)),
      });
    });
  }

  await page.route(/\/api\/providers\/[^/]+\/install(?:\?.*)?$/, async (route) => {
    if (route.request().method() !== "POST") {
      await route.continue();
      return;
    }
    const url = new URL(route.request().url());
    const match = url.pathname.match(/^\/api\/providers\/([^/]+)\/install$/);
    const providerId = match ? decodeURIComponent(match[1]) : "";
    if (providerId !== state.providerId) {
      await route.continue();
      return;
    }
    state.installCalls += 1;
    state.installRunning = true;
    state.installPolls = 0;
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        provider_id: state.providerId,
        install_id: state.installId,
      }),
    });
  });

  await page.route(/\/api\/providers\/install\/[^/]+(?:\?.*)?$/, async (route) => {
    if (route.request().method() !== "GET") {
      await route.continue();
      return;
    }
    const url = new URL(route.request().url());
    const match = url.pathname.match(/^\/api\/providers\/install\/([^/]+)$/);
    const installId = match ? decodeURIComponent(match[1]) : "";
    if (installId !== state.installId) {
      await route.continue();
      return;
    }

    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(await nextInstallInfo()),
    });
  });

  await page.route(/\/api\/providers\/install\/statuses(?:\?.*)?$/, async (route) => {
    if (route.request().method() !== "POST") {
      await route.continue();
      return;
    }
    const postData = route.request().postData() ?? "{}";
    const parsed = JSON.parse(postData) as { install_ids?: unknown };
    const installIds = Array.isArray(parsed.install_ids)
      ? parsed.install_ids.filter((value): value is string => typeof value === "string")
      : [];
    if (!installIds.includes(state.installId)) {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          installs: installIds.map((installId) => ({ install_id: installId, info: null })),
        }),
      });
      return;
    }
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        installs: await Promise.all(
          installIds.map(async (installId) => ({
            install_id: installId,
            info: installId === state.installId ? await nextInstallInfo() : null,
          })),
        ),
      }),
    });
  });
};

const moveWizardToHarnessDownloads = async (page: Page) => {
  const wizard = page.getByTestId("workspace-setup");

  for (let i = 0; i < 12; i += 1) {
    const key = await wizard.getAttribute("data-step-key");
    if (key === "harness-downloads") return;

    if (key === "location") {
      await page.getByTestId("wizard-option-location-local").click();
      await expect.poll(async () => wizard.getAttribute("data-step-key")).not.toBe("location");
      continue;
    }

    if (key === "auth-import") {
      await page.getByRole("button", { name: "Skip for now" }).click();
      continue;
    }

    if (key === "session-titling") {
      await page.getByTestId("wizard-titling-skip").click();
      continue;
    }

    if (key === "container") {
      await page.getByTestId("wizard-option-container-host").click();
      await expect.poll(async () => wizard.getAttribute("data-step-key")).not.toBe("container");
      continue;
    }

    if (key === "source") {
      // Harness candidates are resolved asynchronously; briefly bouncing back gives
      // the wizard a chance to insert the downloads step once provider scan completes.
      await page.getByTestId("wizard-back").click();
      continue;
    }

    const next = page.getByTestId("wizard-next");
    await expect(next).toBeEnabled();
    await next.click();
  }

  throw new Error("failed to reach harness-downloads step");
};

test("wizard: harness downloads step installs selected provider and advances", async ({ page }) => {
  await installDesktopHarness(page);

  const state = makeInstallState("codex");
  await wireInstallLifecycleRoutes(page, state);

  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "New Workspace" }).click();
  await expect(page.getByTestId("workspace-setup")).toBeVisible({ timeout: 20_000 });

  await moveWizardToHarnessDownloads(page);

  const checkbox = page.getByTestId("wizard-harness-checkbox-codex");
  await expect(checkbox).toBeVisible();
  await expect(checkbox).toBeChecked();
  await expect(page.getByTestId("wizard-next")).toBeEnabled({ timeout: 20_000 });
  await page.getByTestId("wizard-next").click();

  await expect.poll(() => state.installCalls).toBe(1);
  await expect.poll(async () => page.getByTestId("workspace-setup").getAttribute("data-step-key")).toBe("auth-import");
});

test("workbench composer: install action in harness menu reaches installed state", async ({ page, request }) => {
  const state = makeInstallState("codex");
  await wireInstallLifecycleRoutes(page, state, { includeCursorReady: true, includeBootstrap: true });

  const repo = initRepo();
  await createWorkspaceAndOpenWorkbench({
    page,
    request,
    repo,
    workspaceName: `ws-${Date.now()}`,
  });

  const harnessButton = page
    .locator(".wb-new-composer-stack .wb-switcher-harness, .wb-new-composer-stack button[title='Harness'], button[title='Agents']")
    .first();
  await expect(harnessButton).toBeVisible({ timeout: 20_000 });
  await harnessButton.click();

  const menu = page.locator(".wb-harness-menu");
  await expect(menu).toBeVisible({ timeout: 10_000 });

  const codexRow = menu.locator(".wb-harness-row").filter({ hasText: "Codex" }).first();
  const installButton = codexRow.getByRole("button", { name: /^Install$/ });
  await expect(installButton).toBeVisible();
  await installButton.click();

  await expect.poll(() => state.installCalls).toBe(1);
  await expect.poll(() => state.installed).toBe(true);

  await page.reload({ waitUntil: "domcontentloaded" });
  await expect(page.locator(".wb-main")).toBeVisible({ timeout: 20_000 });

  const harnessButtonAfterReload = page
    .locator(".wb-new-composer-stack .wb-switcher-harness, .wb-new-composer-stack button[title='Harness'], button[title='Agents']")
    .first();
  await expect(harnessButtonAfterReload).toBeVisible({ timeout: 20_000 });
  await harnessButtonAfterReload.click();
  const menuAfterReload = page.locator(".wb-harness-menu");
  await expect(menuAfterReload).toBeVisible({ timeout: 10_000 });
  const codexRowAfterReload = menuAfterReload.locator(".wb-harness-row").filter({ hasText: "Codex" }).first();
  await expect(codexRowAfterReload.getByRole("button", { name: /^Install$/ })).toHaveCount(0);
  await expect(codexRowAfterReload.locator(".wb-harness-row-main")).toBeEnabled();
});

test("settings harness authentication: install button becomes add-auth after completion", async ({ page, request }) => {
  const state = makeInstallState("codex");
  await wireInstallLifecycleRoutes(page, state, { includeBootstrap: true });

  const repo = initRepo();
  const workspaceId = await createWorkspaceAndOpenWorkbench({
    page,
    request,
    repo,
    workspaceName: `ws-${Date.now()}`,
  });

  await page.goto(`/settings?ws=${workspaceId}#agent_harnesses`, { waitUntil: "domcontentloaded" });
  await expect(page.locator(".settings-main-title")).toHaveText("Harness Authentication");

  const codexRow = page.locator(".settings-harness-row").filter({ hasText: "Codex" }).first();
  await expect(codexRow).toBeVisible();

  const installButton = codexRow.getByRole("button", { name: /^Install$/ });
  await expect(installButton).toBeVisible();
  await installButton.click();

  await expect.poll(() => state.installCalls).toBe(1);
  await expect.poll(() => state.installed).toBe(true);

  await page.reload({ waitUntil: "domcontentloaded" });
  await page.goto(`/settings?ws=${workspaceId}#agent_harnesses`, { waitUntil: "domcontentloaded" });
  await expect(page.locator(".settings-main-title")).toHaveText("Harness Authentication");
  const codexRowAfterReload = page.locator(".settings-harness-row").filter({ hasText: "Codex" }).first();
  await expect(codexRowAfterReload).toBeVisible({ timeout: 15_000 });
  await expect(codexRowAfterReload.getByRole("button", { name: /Add auth for Codex/i })).toBeVisible({ timeout: 15_000 });
});
