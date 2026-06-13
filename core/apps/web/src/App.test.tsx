import { StrictMode, type ReactNode } from "react";
import { act, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, test, vi } from "vitest";
import App from "./App";
import { appendDesktopLog } from "./api/client";
import {
  desktopRestartApp,
  desktopListen,
  desktopOpenLauncherInNewWindow,
  desktopOpenWorkspaceSetupInNewWindow,
  desktopSetDockRecentLocalWorkspaces,
  desktopSetMenuState,
  desktopWebviewRecoveryConsumeIncidents,
  desktopWebviewRecoveryHeartbeat,
  isDesktopApp,
  openExternalLink,
} from "./utils/desktop";
import {
  DESKTOP_UPDATE_MENU_STATE_EVENT,
  DESKTOP_MENU_COMMAND_IDS,
  REQUEST_UPDATE_CHECK_EVENT,
  REQUEST_UPDATE_RESTART_EVENT,
  WEB_MENU_COMMAND_EVENT,
  type DesktopUpdateMenuStateDetail,
  type DesktopMenuCommandId,
} from "./utils/desktopMenuCommands";
import {
  consumePendingDownloadAttributionId,
  getPendingDownloadAttributionId,
  trackAppOpened,
  trackDesktopWebviewRecoveryObserved,
} from "./utils/analytics";
import { emitUiDiagnostic, resetUiDiagnosticsForTests } from "./state/diagnosticsChannel";
import { refreshUpdateCheck } from "./utils/updateNotice";

const desktopHandlers = new Map<string, (payload?: unknown) => void>();
const clientSettingsStore = vi.hoisted(() => ({
  state: {
    loaded: true,
    settings: {
      v: 3 as const,
      desktopNotifications: {
        turnCompleted: true,
        turnFailed: true,
        badgeUnreadCount: true,
      },
      telemetry: {
        clientEnabled: true,
      },
    },
  },
  getClientSettingsState: vi.fn(),
  subscribeClientSettings: vi.fn(),
  loadClientSettings: vi.fn(),
}));

vi.mock("./api/client", () => ({
  appendDesktopLog: vi.fn(async () => {}),
  getDaemonConnectionReadiness: vi.fn(() => ({ isReady: false })),
  recordClientCounterMetric: vi.fn(),
  recordClientGaugeMetric: vi.fn(),
  recordClientHistogramMetric: vi.fn(),
}));

vi.mock("./components/DaemonAvailabilityOverlay", () => ({
  __esModule: true,
  default: () => null,
}));

vi.mock("./components/UpdateNoticeBanner", () => ({
  __esModule: true,
  default: () => null,
}));

vi.mock("./components/StorageGuardBanner", () => ({
  __esModule: true,
  default: () => null,
}));

vi.mock("./pages/LauncherPage", () => ({
  __esModule: true,
  default: () => <div>New Workspace</div>,
}));

vi.mock("./pages/WorkbenchPage", () => ({
  __esModule: true,
  default: () => <div>Workbench Screen</div>,
}));

vi.mock("./pages/CursorDiffDemoPage", () => ({
  __esModule: true,
  default: () => <div>Diff Demo Screen</div>,
}));

vi.mock("./pages/GeometryHarnessPage", () => ({
  __esModule: true,
  default: () => <div>Geometry Harness Screen</div>,
}));

vi.mock("./pages/ProvidersPage", () => ({
  __esModule: true,
  default: () => <div>Providers Screen</div>,
}));

vi.mock("./pages/DiagnosticsPage", () => ({
  __esModule: true,
  default: () => <div>Diagnostics Screen</div>,
}));

vi.mock("./pages/SettingsPage", () => ({
  __esModule: true,
  default: () => <div>Settings Screen</div>,
}));

vi.mock("./pages/WorkspaceSetupPage", () => ({
  __esModule: true,
  default: () => <div>Workspace Setup Screen</div>,
}));

vi.mock("./state/sessionSupervisor", () => ({
  SessionSupervisorProvider: ({ children }: { children: ReactNode }) => <>{children}</>,
}));

vi.mock("./state/settingsStore", () => ({
  SettingsStoreProvider: ({ children }: { children: ReactNode }) => <>{children}</>,
}));

vi.mock("./state/clientSettings", () => ({
  getClientSettingsState: clientSettingsStore.getClientSettingsState,
  subscribeClientSettings: clientSettingsStore.subscribeClientSettings,
  loadClientSettings: clientSettingsStore.loadClientSettings,
}));

vi.mock("./utils/harnessCatalog", () => ({
  preloadHarnessLogos: vi.fn(() => {}),
}));

vi.mock("./utils/updateNotice", () => ({
  refreshUpdateCheck: vi.fn(async () => {}),
}));

vi.mock("./utils/analytics", () => ({
  initAnalytics: vi.fn(() => {}),
  setAnalyticsEnabled: vi.fn(() => {}),
  trackAppOpened: vi.fn(() => {}),
  trackDesktopWebviewRecoveryObserved: vi.fn(() => {}),
  getPendingDownloadAttributionId: vi.fn(async () => null),
  consumePendingDownloadAttributionId: vi.fn(async () => null),
}));

vi.mock("./utils/desktop", () => ({
  isDesktopApp: vi.fn(() => false),
  desktopListen: vi.fn(async <T,>(event: string, handler: (payload: T) => void) => {
    desktopHandlers.set(event, (payload?: unknown) => handler(payload as T));
    return () => {
      desktopHandlers.delete(event);
    };
  }),
  desktopSetMenuState: vi.fn(async () => {}),
  desktopSetDockRecentLocalWorkspaces: vi.fn(async () => {}),
  desktopSetWindowTitle: vi.fn(async () => {}),
  desktopCheckAppUpdate: vi.fn(async () => ({
    configured: true,
    available: false,
    restart_required: false,
    current_version: "0.0.0",
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
  })),
  desktopOpenLauncherInNewWindow: vi.fn(async () => {}),
  desktopOpenWorkspaceSetupInNewWindow: vi.fn(async () => {}),
  desktopWebviewRecoveryHeartbeat: vi.fn(async () => {}),
  desktopWebviewRecoveryConsumeIncidents: vi.fn(async () => []),
  desktopRestartApp: vi.fn(async () => ({ requested: true, message: "restart requested" })),
  openExternalLink: vi.fn(async () => true),
}));

vi.mock("./state/launcherRecentsStore", () => ({
  loadLauncherRecents: vi.fn(async () => []),
}));

vi.mock("./state/uiStateStore", () => ({
  uiStateGet: vi.fn(async () => null),
  uiStateBatch: vi.fn(async () => {}),
  loadSettingsV2: vi.fn(async () => null),
  saveSettingsV2: vi.fn(async () => {}),
}));

beforeEach(() => {
  vi.clearAllMocks();
  vi.stubEnv("VITE_POSTHOG_CAPTURE_IN_DEV", "1");
  resetUiDiagnosticsForTests();
  desktopHandlers.clear();
  clientSettingsStore.state.loaded = true;
  clientSettingsStore.state.settings.telemetry.clientEnabled = true;
  clientSettingsStore.getClientSettingsState.mockImplementation(() => clientSettingsStore.state);
  clientSettingsStore.subscribeClientSettings.mockImplementation(() => () => {});
  clientSettingsStore.loadClientSettings.mockResolvedValue(clientSettingsStore.state);
  vi.mocked(isDesktopApp).mockReturnValue(false);
  vi.mocked(desktopSetDockRecentLocalWorkspaces).mockResolvedValue();
  vi.mocked(desktopWebviewRecoveryConsumeIncidents).mockResolvedValue([]);
  vi.mocked(desktopWebviewRecoveryHeartbeat).mockResolvedValue();
  vi.mocked(desktopRestartApp).mockResolvedValue({ requested: true, message: "restart requested" });
  vi.mocked(getPendingDownloadAttributionId).mockResolvedValue(null);
  vi.mocked(consumePendingDownloadAttributionId).mockResolvedValue(null);
  window.history.pushState({}, "", "/");
  const globalWithFetch = globalThis as typeof globalThis & { fetch: typeof fetch };
  globalWithFetch.fetch = vi.fn(async () => {
    return new Response(JSON.stringify([]), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  });
});

afterEach(() => {
  vi.unstubAllEnvs();
});

test("app_opened includes pending download attribution id when present", async () => {
  vi.mocked(getPendingDownloadAttributionId).mockResolvedValue("dl-123");

  render(<App />);

  await waitFor(() => {
    expect(trackAppOpened).toHaveBeenCalledWith({ downloadId: "dl-123" });
  });
  await waitFor(() => {
    expect(consumePendingDownloadAttributionId).toHaveBeenCalledTimes(1);
  });
});

test("app_opened still emits once under StrictMode effect replay", async () => {
  vi.mocked(getPendingDownloadAttributionId).mockResolvedValue("dl-strict");

  render(
    <StrictMode>
      <App />
    </StrictMode>,
  );

  await waitFor(() => {
    expect(trackAppOpened).toHaveBeenCalledTimes(1);
    expect(trackAppOpened).toHaveBeenCalledWith({ downloadId: "dl-strict" });
  });
  await waitFor(() => {
    expect(consumePendingDownloadAttributionId).toHaveBeenCalledTimes(1);
  });
});

test("renders app shell", async () => {
  render(<App />);
  // App root route is the launcher.
  expect(await screen.findByText("New Workspace")).toBeInTheDocument();
});

test("redirects /index.html to the launcher route", async () => {
  window.history.pushState({}, "", "/index.html");

  render(<App />);

  expect(await screen.findByText("New Workspace")).toBeInTheDocument();
  await waitFor(() => {
    expect(window.location.pathname).toBe("/");
  });
});

test("routes geometry harness path to the standalone harness page", async () => {
  window.history.pushState({}, "", "/__geometry_harness");
  render(<App />);
  expect(await screen.findByText("Geometry Harness Screen")).toBeInTheDocument();
});

test("desktop recovery bridge sends renderer heartbeats", async () => {
  vi.mocked(isDesktopApp).mockReturnValue(true);

  render(<App />);

  await waitFor(() => {
    expect(desktopWebviewRecoveryConsumeIncidents).toHaveBeenCalledTimes(1);
    expect(desktopWebviewRecoveryHeartbeat).toHaveBeenCalledWith(expect.objectContaining({
      route: "/",
      document_visible: true,
    }));
  });
});

test("desktop recovery bridge surfaces persisted recovery incidents", async () => {
  vi.mocked(isDesktopApp).mockReturnValue(true);
  vi.mocked(desktopWebviewRecoveryConsumeIncidents).mockResolvedValue([
    {
      incident_id: "inc-1",
      window_label: "main",
      window_surface: "main",
      route: "/",
      trigger_kind: "native_process_termination",
      action: "prompt_restart",
      daemon_health: "unknown",
      suppression_reason: null,
      created_at_ms: 42,
    },
  ]);

  render(<App />);

  expect(
    await screen.findByText("ctx stopped auto-recovering this main window."),
  ).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Restart App" })).toBeInTheDocument();
});

test("desktop recovery bridge polls for incidents that do not reload the renderer", async () => {
  vi.useFakeTimers();
  try {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(desktopWebviewRecoveryConsumeIncidents)
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([
        {
          incident_id: "inc-2",
          window_label: "main",
          window_surface: "main",
          route: "/",
          trigger_kind: "heartbeat_timeout",
          action: "noop",
          daemon_health: "down",
          suppression_reason: "daemon_down",
          created_at_ms: 84,
        },
      ]);

    render(<App />);

    await act(async () => {
      await Promise.resolve();
    });
    expect(desktopWebviewRecoveryConsumeIncidents).toHaveBeenCalledTimes(1);

    await act(async () => {
      vi.advanceTimersByTime(4_000);
      await Promise.resolve();
    });

    expect(
      screen.getByText("ctx detected a failed main window but skipped recovery."),
    ).toBeInTheDocument();
    expect(desktopWebviewRecoveryConsumeIncidents).toHaveBeenCalledTimes(2);
  } finally {
    vi.useRealTimers();
  }
});

test.each(["window_not_focused", "window_not_visible"] as const)(
  "desktop recovery bridge keeps benign %s no-op incidents out of the user notice",
  async (suppressionReason) => {
    vi.mocked(isDesktopApp).mockReturnValue(true);
    vi.mocked(desktopWebviewRecoveryConsumeIncidents).mockResolvedValue([
      {
        incident_id: `inc-${suppressionReason}`,
        window_label: "workspace-setup:ws-1",
        window_surface: "workbench",
        route: "/workspaces/ws-1",
        trigger_kind: "heartbeat_timeout",
        action: "noop",
        daemon_health: "ok",
        suppression_reason: suppressionReason,
        created_at_ms: 126,
      },
    ]);

    render(<App />);

    await waitFor(() => {
      expect(trackDesktopWebviewRecoveryObserved).toHaveBeenCalledWith(
        expect.objectContaining({
          action: "noop",
          daemonHealth: "ok",
          suppressionReason,
          surface: "workbench",
        }),
      );
    });
    expect(screen.queryByTestId("desktop-webview-recovery-snackbar")).not.toBeInTheDocument();
    expect(screen.queryByText(/ctx detected a failed workspace window/i)).not.toBeInTheDocument();
  },
);

test("persists runtime diagnostics to desktop log", async () => {
  vi.mocked(isDesktopApp).mockReturnValue(true);

  render(<App />);
  emitUiDiagnostic({
    source: "runtime",
    code: "runtime.error",
    message: "boom",
    severity: "error",
    context: { filename: "main.tsx", lineno: 42 },
  });

  await waitFor(() => {
    expect(vi.mocked(appendDesktopLog)).toHaveBeenCalledWith(
      expect.stringContaining("ui_runtime: code=runtime.error"),
      "error",
    );
  });
});

test("desktop settings event uses SPA navigation to preserve workspace context", async () => {
  vi.mocked(isDesktopApp).mockReturnValue(true);
  window.history.pushState({}, "", "/workspaces/ws-123");

  render(<App />);
  expect(await screen.findByText("Workbench Screen")).toBeInTheDocument();

  await waitFor(() => {
    expect(vi.mocked(desktopListen)).toHaveBeenCalledWith("desktop_open_settings", expect.any(Function));
  });

  const handler = desktopHandlers.get("desktop_open_settings");
  if (!handler) {
    throw new Error("desktop_open_settings handler was not registered");
  }
  act(() => {
    handler();
  });

  await waitFor(() => {
    expect(window.location.pathname).toBe("/settings");
    expect(new URLSearchParams(window.location.search).get("ws")).toBe("ws-123");
  });
  expect(await screen.findByText("Settings Screen")).toBeInTheDocument();
});

test("desktop titlebar DOM event navigates to the provided settings target", async () => {
  vi.mocked(isDesktopApp).mockReturnValue(true);
  window.history.pushState({}, "", "/workspaces/ws-789");

  render(<App />);
  expect(await screen.findByText("Workbench Screen")).toBeInTheDocument();

  act(() => {
    window.dispatchEvent(
      new CustomEvent("ctx:open-settings", {
        detail: { target: "/settings?ws=ws-789" },
      }),
    );
  });

  await waitFor(() => {
    expect(window.location.pathname).toBe("/settings");
    expect(new URLSearchParams(window.location.search).get("ws")).toBe("ws-789");
  });
  expect(await screen.findByText("Settings Screen")).toBeInTheDocument();
});

test("desktop menu action routes to workspace settings and updates menu state", async () => {
  vi.mocked(isDesktopApp).mockReturnValue(true);
  window.history.pushState({}, "", "/workspaces/ws-321");

  render(<App />);
  expect(await screen.findByText("Workbench Screen")).toBeInTheDocument();

  await waitFor(() => {
    expect(vi.mocked(desktopListen)).toHaveBeenCalledWith("desktop_menu_action", expect.any(Function));
    expect(vi.mocked(desktopSetMenuState)).toHaveBeenCalled();
  });

  const handler = desktopHandlers.get("desktop_menu_action");
  if (!handler) {
    throw new Error("desktop_menu_action handler was not registered");
  }
  act(() => {
    handler({ commandId: "go.settings" });
  });

  await waitFor(() => {
    expect(window.location.pathname).toBe("/settings");
    expect(new URLSearchParams(window.location.search).get("ws")).toBe("ws-321");
  });
  expect(await screen.findByText("Settings Screen")).toBeInTheDocument();
});

test("desktop menu action ignores payloads without commandId", async () => {
  vi.mocked(isDesktopApp).mockReturnValue(true);
  window.history.pushState({}, "", "/workspaces/ws-987");

  render(<App />);
  expect(await screen.findByText("Workbench Screen")).toBeInTheDocument();

  const handler = await waitFor(() => {
    const value = desktopHandlers.get("desktop_menu_action");
    if (!value) {
      throw new Error("desktop_menu_action handler not ready");
    }
    return value;
  });

  act(() => {
    handler({ command_id: "go.settings" });
  });

  expect(window.location.pathname).toBe("/workspaces/ws-987");
});

test("desktop menu new workspace opens setup in a new window", async () => {
  vi.mocked(isDesktopApp).mockReturnValue(true);
  window.history.pushState({}, "", "/workspaces/ws-987");

  render(<App />);
  expect(await screen.findByText("Workbench Screen")).toBeInTheDocument();

  const handler = await waitFor(() => {
    const value = desktopHandlers.get("desktop_menu_action");
    if (!value) {
      throw new Error("desktop_menu_action handler not ready");
    }
    return value;
  });

  act(() => {
    handler({ commandId: "file.new-workspace" });
  });

  await waitFor(() => {
    expect(vi.mocked(desktopOpenWorkspaceSetupInNewWindow)).toHaveBeenCalledTimes(1);
  });
  expect(window.location.pathname).toBe("/workspaces/ws-987");
});

test("desktop menu new window opens launcher in a new window", async () => {
  vi.mocked(isDesktopApp).mockReturnValue(true);
  window.history.pushState({}, "", "/workspaces/ws-741");

  render(<App />);
  expect(await screen.findByText("Workbench Screen")).toBeInTheDocument();

  const handler = await waitFor(() => {
    const value = desktopHandlers.get("desktop_menu_action");
    if (!value) {
      throw new Error("desktop_menu_action handler not ready");
    }
    return value;
  });

  act(() => {
    handler({ commandId: "file.new-window" });
  });

  await waitFor(() => {
    expect(vi.mocked(desktopOpenLauncherInNewWindow)).toHaveBeenCalledTimes(1);
  });
  expect(window.location.pathname).toBe("/workspaces/ws-741");
});

test("desktop menu action forwards workbench-scoped commands to the web menu bus", async () => {
  vi.mocked(isDesktopApp).mockReturnValue(true);
  window.history.pushState({}, "", "/workspaces/ws-456");

  render(<App />);
  expect(await screen.findByText("Workbench Screen")).toBeInTheDocument();

  const handler = await waitFor(() => {
    const value = desktopHandlers.get("desktop_menu_action");
    if (!value) {
      throw new Error("desktop_menu_action handler not ready");
    }
    return value;
  });

  const appHandledCommands = new Set<DesktopMenuCommandId>([
    "file.new-workspace",
    "file.new-window",
    "go.workspace-setup",
    "go.launcher",
    "go.settings",
    "go.diagnostics",
    "go.agent-harnesses",
    "help.keyboard-shortcuts",
    "help.check-for-updates",
    "help.open-logs-folder",
  ]);
  const forwardedCommands = DESKTOP_MENU_COMMAND_IDS.filter((commandId) => !appHandledCommands.has(commandId));

  const seen: DesktopMenuCommandId[] = [];
  const onWebMenuCommand = (event: Event) => {
    const custom = event as CustomEvent<{ commandId: DesktopMenuCommandId }>;
    seen.push(custom.detail.commandId);
  };
  window.addEventListener(WEB_MENU_COMMAND_EVENT, onWebMenuCommand as EventListener);

  try {
    act(() => {
      for (const commandId of forwardedCommands) {
        handler({ commandId });
      }
    });
  } finally {
    window.removeEventListener(WEB_MENU_COMMAND_EVENT, onWebMenuCommand as EventListener);
  }

  expect(seen).toEqual(forwardedCommands);
});

test("desktop menu report issue opens external tracker link", async () => {
  vi.mocked(isDesktopApp).mockReturnValue(true);
  window.history.pushState({}, "", "/workspaces/ws-654");

  render(<App />);
  expect(await screen.findByText("Workbench Screen")).toBeInTheDocument();

  const handler = await waitFor(() => {
    const value = desktopHandlers.get("desktop_menu_action");
    if (!value) {
      throw new Error("desktop_menu_action handler not ready");
    }
    return value;
  });

  act(() => {
    handler({ commandId: "help.report-issue" });
  });

  await waitFor(() => {
    expect(vi.mocked(openExternalLink)).toHaveBeenCalledWith(
      "https://github.com/ctxrs/ctx/issues/new",
    );
  });
});

test("desktop menu check-for-updates requests global update check without route navigation", async () => {
  vi.mocked(isDesktopApp).mockReturnValue(true);
  window.history.pushState({}, "", "/workspaces/ws-654");

  render(<App />);
  expect(await screen.findByText("Workbench Screen")).toBeInTheDocument();

  const handler = await waitFor(() => {
    const value = desktopHandlers.get("desktop_menu_action");
    if (!value) {
      throw new Error("desktop_menu_action handler not ready");
    }
    return value;
  });

  const checkEvent = vi.fn();
  window.addEventListener(REQUEST_UPDATE_CHECK_EVENT, checkEvent as EventListener);
  vi.mocked(refreshUpdateCheck).mockClear();

  act(() => {
    handler({ commandId: "help.check-for-updates" });
  });

  await waitFor(() => {
    expect(checkEvent).toHaveBeenCalledTimes(1);
  });
  expect(vi.mocked(refreshUpdateCheck)).not.toHaveBeenCalled();
  expect(window.location.pathname).toBe("/workspaces/ws-654");
  window.removeEventListener(REQUEST_UPDATE_CHECK_EVENT, checkEvent as EventListener);
});

test("desktop menu switches check-for-updates item to downloading while update is staging", async () => {
  vi.mocked(isDesktopApp).mockReturnValue(true);
  window.history.pushState({}, "", "/workspaces/ws-654");

  render(<App />);
  expect(await screen.findByText("Workbench Screen")).toBeInTheDocument();

  act(() => {
    window.dispatchEvent(
      new CustomEvent<DesktopUpdateMenuStateDetail>(DESKTOP_UPDATE_MENU_STATE_EVENT, {
        detail: { state: "downloading" },
      }),
    );
  });

  await waitFor(() => {
    expect(vi.mocked(desktopSetMenuState)).toHaveBeenCalledWith(
      expect.arrayContaining([
        expect.objectContaining({
          id: "help.check-for-updates",
          enabled: false,
          text: "Downloading Update",
        }),
      ]),
    );
  });
});

test("desktop menu check-for-updates requests restart when update is ready", async () => {
  vi.mocked(isDesktopApp).mockReturnValue(true);
  window.history.pushState({}, "", "/workspaces/ws-654");

  render(<App />);
  expect(await screen.findByText("Workbench Screen")).toBeInTheDocument();

  const handler = await waitFor(() => {
    const value = desktopHandlers.get("desktop_menu_action");
    if (!value) {
      throw new Error("desktop_menu_action handler not ready");
    }
    return value;
  });

  const restartEvent = vi.fn();
  window.addEventListener(REQUEST_UPDATE_RESTART_EVENT, restartEvent as EventListener);

  act(() => {
    window.dispatchEvent(
      new CustomEvent<DesktopUpdateMenuStateDetail>(DESKTOP_UPDATE_MENU_STATE_EVENT, {
        detail: { state: "restart" },
      }),
    );
  });

  await waitFor(() => {
    expect(vi.mocked(desktopSetMenuState)).toHaveBeenCalledWith(
      expect.arrayContaining([
        expect.objectContaining({
          id: "help.check-for-updates",
          enabled: true,
          text: "Restart to Update",
        }),
      ]),
    );
  });
  vi.mocked(refreshUpdateCheck).mockClear();

  act(() => {
    handler({ commandId: "help.check-for-updates" });
  });

  await waitFor(() => {
    expect(restartEvent).toHaveBeenCalledTimes(1);
  });
  expect(vi.mocked(refreshUpdateCheck)).not.toHaveBeenCalled();
  window.removeEventListener(REQUEST_UPDATE_RESTART_EVENT, restartEvent as EventListener);
});
