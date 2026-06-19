import { type ReactNode, useEffect, useRef } from "react";
import { BrowserRouter, Navigate, Route, Routes } from "react-router-dom";
import { appendDesktopLog } from "./api/client";
import { useDaemonConnection } from "./api/useDaemonConnection";
import DaemonAvailabilityOverlay from "./components/DaemonAvailabilityOverlay";
import { DesktopWindowControls } from "./components/DesktopWindowControls";
import StorageGuardBanner from "./components/StorageGuardBanner";
import LauncherPage from "./pages/LauncherPage";
import WorkbenchPage from "./pages/WorkbenchPage";
import CursorDiffDemoPage from "./pages/CursorDiffDemoPage";
import GeometryHarnessPage from "./pages/GeometryHarnessPage";
import ProvidersPage from "./pages/ProvidersPage";
import DiagnosticsPage from "./pages/DiagnosticsPage";
import SettingsPage from "./pages/SettingsPage";
import WorkspaceSetupPage from "./pages/WorkspaceSetupPage";
import { MobileConnectPage } from "./pages/mobile/MobileConnectPage";
import { MobileHomePage } from "./pages/mobile/MobileHomePage";
import { SessionSupervisorProvider } from "./state/sessionSupervisor";
import { SettingsStoreProvider } from "./state/settingsStore";
import { setUiDiagnosticPersistenceSink, type UiDiagnosticEvent } from "./state/diagnosticsChannel";
import {
  noteDesktopDaemonReady,
  noteDesktopFirstPaint,
  noteDesktopRendererPing,
  noteDesktopRendererTimeout,
  noteDesktopWindowCreated,
} from "./state/foregroundFreshnessTelemetry";
import { DesktopWebviewRecoveryBridge } from "./state/desktopWebviewRecoveryBridge";
import { getDaemonConnectionReadiness } from "./api/client";
import { initAnalytics } from "./utils/analytics";
import { isDesktopApp } from "./utils/desktop";
import { getAppShellKind } from "./utils/runtime";
import {
  AnalyticsSettingsBridge,
  AppForegroundBootstrap,
  ClientSettingsBootstrap,
  DesktopMenuBridge,
  DesktopSettingsListener,
  GlobalUpdateNotice,
} from "./utils/appBridges";
import { refreshUpdateCheck } from "./utils/updateNotice";

const RUNTIME_DIAGNOSTIC_DEDUPE_WINDOW_MS = 15_000;

type WindowWithDesktopStartup = Window & {
  __CTX_DESKTOP_STARTUP__?: {
    windowCreatedAtMs?: unknown;
    windowLabel?: unknown;
    startPath?: unknown;
  };
};

const safeJsonStringify = (value: unknown): string => {
  try {
    return JSON.stringify(value);
  } catch {
    return "[unserializable]";
  }
};

const shouldPersistRuntimeDiagnostic = (event: UiDiagnosticEvent): boolean =>
  event.source === "runtime" && (event.severity === "error" || event.fatal === true);

const shouldPersistUiDiagnostic = (event: UiDiagnosticEvent): boolean => {
  if (shouldPersistRuntimeDiagnostic(event)) return true;
  if (event.source === "foreground_freshness" && event.severity !== "info") return true;
  if (event.source === "desktop_startup") return true;
  return false;
};

const buildRuntimeDiagnosticLogLine = (event: UiDiagnosticEvent): string => {
  const context =
    event.context && Object.keys(event.context).length > 0
      ? ` context=${safeJsonStringify(event.context)}`
      : "";
  return `ui_runtime: code=${event.code} severity=${event.severity} fatal=${event.fatal === true ? "true" : "false"} message=${event.message}${context}`;
};

const buildUiDiagnosticLogLine = (event: UiDiagnosticEvent): string => {
  if (event.source === "runtime") {
    return buildRuntimeDiagnosticLogLine(event);
  }
  const context =
    event.context && Object.keys(event.context).length > 0
      ? ` context=${safeJsonStringify(event.context)}`
      : "";
  return `${event.source}: code=${event.code} severity=${event.severity} message=${event.message}${context}`;
};

function MobileReadyRoute({ children }: { children: ReactNode }) {
  const connection = useDaemonConnection();
  if (!getDaemonConnectionReadiness(connection).isReady) {
    return <Navigate replace to="/mobile/connect" />;
  }
  return <>{children}</>;
}

export default function App() {
  const runtimeLogDedupRef = useRef<Map<string, number>>(new Map());
  const desktopFirstPaintLoggedRef = useRef(false);
  const desktopDaemonReadyLoggedRef = useRef(false);
  const daemonConnection = useDaemonConnection();
  const mobileShell = getAppShellKind() === "mobile";

  useEffect(() => {
    appendDesktopLog("ui: app loaded").catch(() => {});
  }, []);

  useEffect(() => {
    if (!isDesktopApp()) return;
    const startup = (window as WindowWithDesktopStartup).__CTX_DESKTOP_STARTUP__;
    const windowLabel =
      typeof startup?.windowLabel === "string" && startup.windowLabel.trim().length > 0
        ? startup.windowLabel.trim()
        : "unknown";
    const startupPath =
      typeof startup?.startPath === "string" && startup.startPath.trim().length > 0
        ? startup.startPath.trim()
        : window.location.pathname;
    const createdAtMs =
      typeof startup?.windowCreatedAtMs === "number" && Number.isFinite(startup.windowCreatedAtMs)
        ? startup.windowCreatedAtMs
        : null;
    if (createdAtMs !== null) {
      noteDesktopWindowCreated(createdAtMs);
    }
    noteDesktopRendererPing();
    void appendDesktopLog(
      `desktop_startup: renderer_ping label=${safeJsonStringify(windowLabel)} path=${safeJsonStringify(startupPath)}`,
    ).catch(() => {});
    const timeoutId = window.setTimeout(() => {
      if (desktopFirstPaintLoggedRef.current) return;
      noteDesktopRendererTimeout();
      void appendDesktopLog(
        `desktop_startup: renderer_timeout label=${safeJsonStringify(windowLabel)} path=${safeJsonStringify(window.location.pathname)} readyState=${safeJsonStringify(document.readyState)} visibilityState=${safeJsonStringify(document.visibilityState)}`,
        "error",
      ).catch(() => {});
    }, 1000);
    const rafId = window.requestAnimationFrame(() => {
      desktopFirstPaintLoggedRef.current = true;
      noteDesktopFirstPaint();
      void appendDesktopLog(
        `desktop_startup: first_paint label=${safeJsonStringify(windowLabel)} path=${safeJsonStringify(window.location.pathname)}`,
      ).catch(() => {});
    });
    return () => {
      window.clearTimeout(timeoutId);
      window.cancelAnimationFrame(rafId);
    };
  }, []);

  useEffect(() => {
    if (!isDesktopApp()) return;
    if (!getDaemonConnectionReadiness(daemonConnection).isReady) return;
    if (desktopDaemonReadyLoggedRef.current) return;

    desktopDaemonReadyLoggedRef.current = true;
    const startup = (window as WindowWithDesktopStartup).__CTX_DESKTOP_STARTUP__;
    const windowLabel =
      typeof startup?.windowLabel === "string" && startup.windowLabel.trim().length > 0
        ? startup.windowLabel.trim()
        : "unknown";
    noteDesktopDaemonReady();
    void appendDesktopLog(
      `desktop_startup: daemon_ready label=${safeJsonStringify(windowLabel)} path=${safeJsonStringify(window.location.pathname)}`,
    ).catch(() => {});
  }, [daemonConnection]);

  useEffect(() => {
    if (!isDesktopApp()) {
      setUiDiagnosticPersistenceSink(null);
      return;
    }
    setUiDiagnosticPersistenceSink((event) => {
      if (!shouldPersistUiDiagnostic(event)) return;
      const key = `${event.code}|${event.message}`;
      const now = Date.now();
      const prev = runtimeLogDedupRef.current.get(key);
      if (typeof prev === "number" && now - prev < RUNTIME_DIAGNOSTIC_DEDUPE_WINDOW_MS) {
        return;
      }
      runtimeLogDedupRef.current.set(key, now);
      void appendDesktopLog(buildUiDiagnosticLogLine(event), event.severity).catch(() => {});
    });
    return () => {
      setUiDiagnosticPersistenceSink(null);
      runtimeLogDedupRef.current.clear();
    };
  }, []);

  useEffect(() => {
    initAnalytics();
  }, []);

  useEffect(() => {
    refreshUpdateCheck().catch(() => {});
  }, []);

  return (
    <SessionSupervisorProvider>
      <SettingsStoreProvider>
        <BrowserRouter future={{ v7_startTransition: true, v7_relativeSplatPath: true }}>
          <AppForegroundBootstrap />
          <ClientSettingsBootstrap />
          <AnalyticsSettingsBridge />
          <DesktopSettingsListener />
          <DesktopMenuBridge />
          <DesktopWebviewRecoveryBridge />
          <GlobalUpdateNotice />
          <div className="desktop-window-controls-overlay">
            <DesktopWindowControls />
          </div>
          <Routes>
            <Route
              path="/"
              element={
                mobileShell ? (
                  <MobileReadyRoute>
                    <MobileHomePage />
                  </MobileReadyRoute>
                ) : (
                  <LauncherPage />
                )
              }
            />
            <Route path="/index.html" element={<Navigate replace to="/" />} />
            <Route
              path="/mobile/connect"
              element={mobileShell ? <MobileConnectPage /> : <Navigate replace to="/settings" />}
            />
            <Route path="/workspace-setup" element={mobileShell ? <Navigate replace to="/" /> : <WorkspaceSetupPage />} />
            <Route path="/settings" element={mobileShell ? <Navigate replace to="/mobile/connect" /> : <SettingsPage />} />
            <Route path="/providers" element={mobileShell ? <Navigate replace to="/" /> : <ProvidersPage />} />
            <Route path="/diagnostics" element={mobileShell ? <Navigate replace to="/mobile/connect" /> : <DiagnosticsPage />} />
            <Route
              path="/workspaces/:id"
              element={
                mobileShell ? (
                  <MobileReadyRoute>
                    <WorkbenchPage />
                  </MobileReadyRoute>
                ) : (
                  <WorkbenchPage />
                )
              }
            />
            <Route path="/__cursor_diff_demo" element={<CursorDiffDemoPage />} />
            <Route path="/__geometry_harness" element={<GeometryHarnessPage />} />
          </Routes>
          <StorageGuardBanner />
          <DaemonAvailabilityOverlay />
        </BrowserRouter>
      </SettingsStoreProvider>
    </SessionSupervisorProvider>
  );
}
