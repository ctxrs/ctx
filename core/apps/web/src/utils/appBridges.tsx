import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import UpdateNoticeBanner from "../components/UpdateNoticeBanner";
import { getDaemonConnectionReadiness, openLogsFolder } from "../api/client";
import { getClientSettingsState, loadClientSettings, subscribeClientSettings } from "../state/clientSettings";
import { loadLauncherRecents } from "../state/launcherRecentsStore";
import { computeAnalyticsCaptureEnabled } from "./analytics/runtimePolicy";
import {
  consumePendingDownloadAttributionId,
  getPendingDownloadAttributionId,
  setAnalyticsEnabled,
  trackAppOpened,
} from "./analytics";
import {
  desktopListen,
  desktopOpenLauncherInNewWindow,
  desktopOpenWorkspaceSetupInNewWindow,
  desktopSetDockRecentLocalWorkspaces,
  desktopSetMenuState,
  desktopSetWindowTitle,
  isDesktopApp,
  openExternalLink,
} from "./desktop";
import {
  buildDesktopUpdateMenuPatch,
  buildDesktopMenuBaseState,
  DESKTOP_MENU_ACTION_EVENT,
  DESKTOP_UPDATE_MENU_STATE_EVENT,
  isDesktopMenuCommandId,
  isDesktopUpdateMenuState,
  REQUEST_UPDATE_CHECK_EVENT,
  REQUEST_UPDATE_RESTART_EVENT,
  WEB_MENU_COMMAND_EVENT,
  WEB_MENU_STATE_EVENT,
  WEB_MENU_TRACE_EVENT,
  type DesktopMenuActionEventPayload,
  type DesktopMenuItemState,
  type DesktopUpdateMenuState,
  type DesktopUpdateMenuStateDetail,
  type WebMenuCommandDetail,
  type WebMenuStateDetail,
  type WebMenuTraceDetail,
} from "./desktopMenuCommands";
import { preloadHarnessLogos } from "./harnessCatalog";
import {
  WORKBENCH_TASK_IDLE_EVENT,
  writeUpdaterRefreshBroadcast,
  type WorkbenchTaskIdleDetail,
} from "./updaterEvents";
import { initializeAppForegroundTracking } from "./windowFocus";

const isLocalWebAnalyticsOrigin = (): boolean => {
  if (typeof window === "undefined") return false;
  if (isDesktopApp()) return false;
  const hostname = window.location.hostname.trim().toLowerCase();
  return hostname === "localhost" ||
    hostname === "127.0.0.1" ||
    hostname === "::1" ||
    hostname === "[::1]" ||
    hostname.endsWith(".localhost");
};

function settingsTargetForPath(pathname: string): string {
  if (pathname.startsWith("/workspaces/")) {
    const wsId = pathname.split("/")[2];
    if (wsId) {
      return `/settings?ws=${encodeURIComponent(wsId)}`;
    }
  }
  return "/settings";
}

export function DesktopSettingsListener() {
  const navigate = useNavigate();
  const location = useLocation();
  const locationRef = useRef(location);

  useEffect(() => {
    locationRef.current = location;
  }, [location]);

  useEffect(() => {
    if (!isDesktopApp()) return;
    let active = true;
    let unlisten: (() => void) | null = null;
    desktopListen("desktop_open_settings", () => {
      const target = settingsTargetForPath(locationRef.current.pathname);
      navigate(target);
    })
      .then((fn) => {
        if (!active) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch(() => {});
    return () => {
      active = false;
      if (unlisten) unlisten();
    };
  }, [navigate]);

  useEffect(() => {
    if (!isDesktopApp()) return;
    const handler = (event: Event) => {
      const custom = event as CustomEvent<{ target?: unknown }>;
      const detailTarget = custom.detail?.target;
      if (typeof detailTarget === "string" && detailTarget.startsWith("/settings")) {
        navigate(detailTarget);
        return;
      }
      const target = settingsTargetForPath(locationRef.current.pathname);
      navigate(target);
    };
    window.addEventListener("ctx:open-settings", handler as EventListener);
    return () => {
      window.removeEventListener("ctx:open-settings", handler as EventListener);
    };
  }, [navigate]);

  return null;
}

export function ClientSettingsBootstrap() {
  useEffect(() => {
    loadClientSettings().catch((err) => {
      console.warn("client settings bootstrap failed", err);
    });
  }, []);

  return null;
}

export function AppForegroundBootstrap() {
  useEffect(() => {
    initializeAppForegroundTracking();
  }, []);

  return null;
}

export function DesktopMenuBridge() {
  const navigate = useNavigate();
  const location = useLocation();
  const patchRef = useRef<DesktopMenuItemState[]>([]);
  const updateMenuStateRef = useRef<DesktopUpdateMenuState>("check");
  const updateMenuPatchRef = useRef<DesktopMenuItemState>(buildDesktopUpdateMenuPatch("check"));
  const emitMenuTrace = (detail: WebMenuTraceDetail) => {
    window.dispatchEvent(
      new CustomEvent<WebMenuTraceDetail>(WEB_MENU_TRACE_EVENT, {
        detail,
      }),
    );
  };

  const pushMenuState = useRef(() => {});
  pushMenuState.current = () => {
    if (!isDesktopApp()) return;
    const merged = new Map<string, DesktopMenuItemState>();
    for (const item of buildDesktopMenuBaseState(location.pathname)) {
      merged.set(item.id, { ...item });
    }
    for (const patch of patchRef.current) {
      const prev = merged.get(patch.id);
      merged.set(patch.id, { ...(prev ?? { id: patch.id }), ...patch });
    }
    const updatePatch = updateMenuPatchRef.current;
    const prev = merged.get(updatePatch.id);
    merged.set(updatePatch.id, { ...(prev ?? { id: updatePatch.id }), ...updatePatch });
    void desktopSetMenuState(Array.from(merged.values())).catch(() => {});
  };

  useEffect(() => {
    patchRef.current = [];
    pushMenuState.current();
  }, [location.pathname]);

  useEffect(() => {
    if (!isDesktopApp()) return;
    const title = (typeof document !== "undefined" ? document.title : "").trim() || "ctx";
    void desktopSetWindowTitle(title).catch(() => {});
  }, [location.pathname]);

  useEffect(() => {
    if (!isDesktopApp()) return;
    let cancelled = false;
    loadLauncherRecents()
      .then((entries) => {
        if (cancelled) return;
        const localEntries = entries.flatMap((entry) =>
          entry.kind === "local"
            ? [{ label: entry.label, root_path: entry.root_path }]
            : [],
        );
        void desktopSetDockRecentLocalWorkspaces(localEntries).catch(() => {});
      })
      .catch(() => {
        if (cancelled) return;
        void desktopSetDockRecentLocalWorkspaces([]).catch(() => {});
      });
    return () => {
      cancelled = true;
    };
  }, [location.pathname]);

  useEffect(() => {
    if (!isDesktopApp()) return;
    const onMenuState = (event: Event) => {
      const custom = event as CustomEvent<WebMenuStateDetail>;
      const detail = custom.detail;
      if (!detail || !Array.isArray(detail.items)) return;

      if (detail.replace === false) {
        const next = new Map<string, DesktopMenuItemState>();
        for (const item of patchRef.current) next.set(item.id, item);
        for (const item of detail.items) next.set(item.id, item);
        patchRef.current = Array.from(next.values());
      } else {
        patchRef.current = detail.items;
      }
      pushMenuState.current();
    };

    window.addEventListener(WEB_MENU_STATE_EVENT, onMenuState as EventListener);
    return () => {
      window.removeEventListener(WEB_MENU_STATE_EVENT, onMenuState as EventListener);
    };
  }, []);

  useEffect(() => {
    if (!isDesktopApp()) return;
    const onDesktopUpdateMenuState = (event: Event) => {
      const custom = event as CustomEvent<DesktopUpdateMenuStateDetail>;
      const nextState = custom.detail?.state;
      if (!isDesktopUpdateMenuState(nextState)) return;
      updateMenuStateRef.current = nextState;
      updateMenuPatchRef.current = buildDesktopUpdateMenuPatch(nextState);
      pushMenuState.current();
    };

    window.addEventListener(DESKTOP_UPDATE_MENU_STATE_EVENT, onDesktopUpdateMenuState as EventListener);
    return () => {
      window.removeEventListener(DESKTOP_UPDATE_MENU_STATE_EVENT, onDesktopUpdateMenuState as EventListener);
    };
  }, []);

  useEffect(() => {
    if (!isDesktopApp()) return;
    let active = true;
    let unlisten: (() => void) | null = null;
    desktopListen<DesktopMenuActionEventPayload>(DESKTOP_MENU_ACTION_EVENT, (payload) => {
      if (!payload || !isDesktopMenuCommandId(payload.commandId)) return;
      const { commandId } = payload;
      switch (commandId) {
        case "file.new-workspace":
          void desktopOpenWorkspaceSetupInNewWindow().catch(() => {});
          emitMenuTrace({ commandId, layer: "app", status: "handled", note: "open-workspace-setup-window" });
          return;
        case "file.new-window":
          void desktopOpenLauncherInNewWindow().catch(() => {});
          emitMenuTrace({ commandId, layer: "app", status: "handled", note: "open-launcher-window" });
          return;
        case "go.workspace-setup":
          navigate("/workspace-setup");
          emitMenuTrace({ commandId, layer: "app", status: "handled", note: "navigate-workspace-setup" });
          return;
        case "go.launcher":
          navigate("/");
          emitMenuTrace({ commandId, layer: "app", status: "handled", note: "navigate-launcher" });
          return;
        case "go.settings":
          navigate(settingsTargetForPath(location.pathname));
          emitMenuTrace({ commandId, layer: "app", status: "handled", note: "navigate-settings" });
          return;
        case "go.diagnostics":
          navigate("/diagnostics");
          emitMenuTrace({ commandId, layer: "app", status: "handled", note: "navigate-diagnostics" });
          return;
        case "go.agent-harnesses":
          navigate("/settings#agent_harnesses");
          emitMenuTrace({ commandId, layer: "app", status: "handled", note: "navigate-agent-harnesses" });
          return;
        case "help.keyboard-shortcuts":
          navigate(settingsTargetForPath(location.pathname));
          emitMenuTrace({
            commandId,
            layer: "app",
            status: "handled",
            note: "navigate-settings-keyboard-shortcuts",
          });
          return;
        case "help.check-for-updates":
          if (updateMenuStateRef.current === "restart") {
            window.dispatchEvent(new Event(REQUEST_UPDATE_RESTART_EVENT));
            emitMenuTrace({
              commandId,
              layer: "app",
              status: "handled",
              note: "request-update-restart",
            });
            return;
          }
          if (updateMenuStateRef.current === "downloading") {
            emitMenuTrace({
              commandId,
              layer: "app",
              status: "ignored",
              note: "update-download-in-progress",
            });
            return;
          }
          window.dispatchEvent(new Event(REQUEST_UPDATE_CHECK_EVENT));
          writeUpdaterRefreshBroadcast("menu-check-for-updates");
          emitMenuTrace({
            commandId,
            layer: "app",
            status: "handled",
            note: "trigger-silent-update-check",
          });
          return;
        case "help.open-logs-folder":
          void openLogsFolder().catch(() => {});
          emitMenuTrace({ commandId, layer: "app", status: "handled", note: "open-logs-folder" });
          return;
        default:
          break;
      }

      window.dispatchEvent(
        new CustomEvent<WebMenuCommandDetail>(WEB_MENU_COMMAND_EVENT, {
          detail: { commandId },
        }),
      );
      emitMenuTrace({ commandId, layer: "app", status: "forwarded" });
    })
      .then((fn) => {
        if (!active) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch(() => {});
    return () => {
      active = false;
      if (unlisten) unlisten();
    };
  }, [location.pathname, navigate]);

  useEffect(() => {
    if (!isDesktopApp()) return;
    const onMenuCommand = (event: Event) => {
      const custom = event as CustomEvent<WebMenuCommandDetail>;
      const detail = custom.detail;
      if (!detail || !isDesktopMenuCommandId(detail.commandId)) return;
      if (detail.commandId !== "help.report-issue") return;
      void openExternalLink("https://github.com/ctxrs/ctx/issues/new").catch(() => {});
      emitMenuTrace({
        commandId: detail.commandId,
        layer: "app",
        status: "handled",
        note: "open-report-issue-url",
      });
    };
    window.addEventListener(WEB_MENU_COMMAND_EVENT, onMenuCommand as EventListener);
    return () => {
      window.removeEventListener(WEB_MENU_COMMAND_EVENT, onMenuCommand as EventListener);
    };
  }, []);

  return null;
}

export function AnalyticsSettingsBridge() {
  const clientSettingsState = useSyncExternalStore(
    subscribeClientSettings,
    getClientSettingsState,
    getClientSettingsState,
  );
  const appOpenedSentRef = useRef(false);

  useEffect(() => {
    let cancelled = false;
    const enabled = computeAnalyticsCaptureEnabled({
      settingsLoaded: clientSettingsState.loaded,
      telemetryEnabled: clientSettingsState.settings.telemetry.clientEnabled,
      isDev: import.meta.env.DEV,
      isTest: import.meta.env.MODE === "test",
      isCi: __CTX_BUILD_CI__,
      isLocalWebOrigin: isLocalWebAnalyticsOrigin(),
      devCaptureFlag: import.meta.env.VITE_POSTHOG_CAPTURE_IN_DEV,
    });
    setAnalyticsEnabled(enabled);
    if (!appOpenedSentRef.current && enabled) {
      void (async () => {
        let downloadId: string | null = null;
        try {
          downloadId = await getPendingDownloadAttributionId();
        } catch {
          downloadId = null;
        }
        if (cancelled) return;
        if (appOpenedSentRef.current) return;
        appOpenedSentRef.current = true;
        trackAppOpened(downloadId ? { downloadId } : undefined);
        if (downloadId) {
          void consumePendingDownloadAttributionId().catch(() => {});
        }
      })();
    }
    return () => {
      cancelled = true;
    };
  }, [clientSettingsState.loaded, clientSettingsState.settings.telemetry.clientEnabled]);

  return null;
}

export function GlobalUpdateNotice() {
  const location = useLocation();
  const [allTasksIdle, setAllTasksIdle] = useState(false);

  useEffect(() => {
    const onWorkbenchTaskIdle = (event: Event) => {
      const custom = event as CustomEvent<WorkbenchTaskIdleDetail>;
      const detail = custom.detail;
      if (!detail || typeof detail.allTasksIdle !== "boolean") return;
      setAllTasksIdle(detail.allTasksIdle);
    };
    window.addEventListener(WORKBENCH_TASK_IDLE_EVENT, onWorkbenchTaskIdle as EventListener);
    return () => {
      window.removeEventListener(WORKBENCH_TASK_IDLE_EVENT, onWorkbenchTaskIdle as EventListener);
    };
  }, []);

  useEffect(() => {
    if (!location.pathname.startsWith("/workspaces/")) {
      setAllTasksIdle(false);
    }
  }, [location.pathname]);

  useEffect(() => {
    preloadHarnessLogos();
  }, []);

  return <UpdateNoticeBanner allTasksIdle={allTasksIdle} />;
}
