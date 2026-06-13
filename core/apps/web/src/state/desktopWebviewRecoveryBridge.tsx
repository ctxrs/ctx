import { useEffect, useMemo, useState } from "react";
import { X } from "lucide-react";
import { useLocation, useNavigate } from "react-router-dom";
import { desktopRestartApp } from "../utils/desktop";
import {
  desktopWebviewRecoveryConsumeIncidents,
  desktopWebviewRecoveryHeartbeat,
  isDesktopApp,
  type DesktopWebviewRecoveryIncident,
} from "../utils/desktop";
import { emitUiDiagnostic } from "./diagnosticsChannel";
import { trackDesktopWebviewRecoveryObserved } from "../utils/analytics";

const HEARTBEAT_INTERVAL_MS = 4_000;
const INCIDENT_POLL_INTERVAL_MS = 4_000;

const silentNoopSuppressionReasons = new Set<
  NonNullable<DesktopWebviewRecoveryIncident["suppression_reason"]>
>(["window_not_focused", "window_not_visible"]);

const surfaceLabel = (surface: DesktopWebviewRecoveryIncident["window_surface"]): string => {
  switch (surface) {
    case "main":
      return "main window";
    case "workbench":
      return "workspace window";
    case "launcher":
      return "launcher window";
    case "settings":
      return "settings window";
    case "file_preview":
      return "file preview window";
    case "workspace_setup":
      return "workspace setup window";
    default:
      return "desktop window";
  }
};

const buildNotice = (
  incident: DesktopWebviewRecoveryIncident,
): { title: string; subtitle: string; restartRecommended: boolean } => {
  const windowName = surfaceLabel(incident.window_surface);
  if (incident.action === "prompt_restart") {
    return {
      title: `ctx stopped auto-recovering this ${windowName}.`,
      subtitle: "The window failed repeatedly. Restart the app or open Diagnostics for logs.",
      restartRecommended: true,
    };
  }
  if (incident.action === "recreate") {
    return {
      title: `ctx reopened a failed ${windowName}.`,
      subtitle:
        incident.trigger_kind === "native_process_termination"
          ? "The web content process exited, so the window was recreated on its last route."
          : "The renderer stopped responding while the daemon stayed healthy, so the window was recreated.",
      restartRecommended: false,
    };
  }
  if (incident.action === "reload") {
    return {
      title: `ctx recovered a failed ${windowName}.`,
      subtitle:
        incident.trigger_kind === "native_process_termination"
          ? "The web content process exited, so the window was reloaded."
          : "The renderer stopped responding while the daemon stayed healthy, so the window was reloaded.",
      restartRecommended: false,
    };
  }
  if (incident.suppression_reason === "daemon_mismatch") {
    return {
      title: `ctx detected a failed ${windowName} but skipped recovery.`,
      subtitle: "The connected daemon did not match this desktop build, so automatic recovery was suppressed.",
      restartRecommended: true,
    };
  }
  if (incident.suppression_reason === "daemon_down") {
    return {
      title: `ctx detected a failed ${windowName} but skipped recovery.`,
      subtitle: "The connected daemon was unavailable, so automatic recovery was suppressed.",
      restartRecommended: true,
    };
  }
  return {
    title: `ctx detected a failed ${windowName}.`,
    subtitle: "Automatic recovery was skipped by the desktop guardrails. Open Diagnostics for details.",
    restartRecommended: false,
  };
};

const emitIncidentDiagnostic = (incident: DesktopWebviewRecoveryIncident): void => {
  emitUiDiagnostic({
    source: "desktop_recovery",
    code: `desktop_recovery.${incident.trigger_kind}`,
    severity: incident.action === "prompt_restart" ? "error" : "warning",
    message: `${incident.trigger_kind} -> ${incident.action}`,
    context: {
      windowSurface: incident.window_surface,
      action: incident.action,
      daemonHealth: incident.daemon_health,
      suppressionReason: incident.suppression_reason ?? null,
    },
  });
  trackDesktopWebviewRecoveryObserved({
    trigger: incident.trigger_kind,
    action: incident.action,
    surface: incident.window_surface,
    daemonHealth: incident.daemon_health,
    suppressionReason: incident.suppression_reason ?? undefined,
  });
};

const shouldShowIncidentNotice = (incident: DesktopWebviewRecoveryIncident): boolean => {
  if (incident.action !== "noop") return true;
  const suppressionReason = incident.suppression_reason ?? null;
  return suppressionReason === null || !silentNoopSuppressionReasons.has(suppressionReason);
};

export function DesktopWebviewRecoveryBridge() {
  const location = useLocation();
  const navigate = useNavigate();
  const [pendingIncidents, setPendingIncidents] = useState<DesktopWebviewRecoveryIncident[]>([]);

  useEffect(() => {
    if (!isDesktopApp()) return;
    let cancelled = false;
    let consumeInFlight = false;
    const consumeIncidents = () => {
      if (consumeInFlight) return;
      consumeInFlight = true;
      void desktopWebviewRecoveryConsumeIncidents()
        .then((incidents) => {
          if (cancelled || incidents.length === 0) return;
          for (const incident of incidents) {
            emitIncidentDiagnostic(incident);
          }
          const visibleIncidents = incidents.filter(shouldShowIncidentNotice);
          if (visibleIncidents.length > 0) {
            setPendingIncidents((current) => [...current, ...visibleIncidents]);
          }
        })
        .catch(() => {})
        .finally(() => {
          consumeInFlight = false;
        });
    };
    consumeIncidents();
    const intervalId = window.setInterval(consumeIncidents, INCIDENT_POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(intervalId);
    };
  }, []);

  useEffect(() => {
    if (!isDesktopApp()) return;
    const route = `${location.pathname}${location.search}${location.hash}`;
    const sendHeartbeat = () => {
      void desktopWebviewRecoveryHeartbeat({
        route,
        document_visible: document.visibilityState === "visible",
        window_focused: document.hasFocus(),
        startup_ready: document.readyState !== "loading",
      }).catch(() => {});
    };
    sendHeartbeat();
    const intervalId = window.setInterval(sendHeartbeat, HEARTBEAT_INTERVAL_MS);
    window.addEventListener("focus", sendHeartbeat);
    window.addEventListener("pageshow", sendHeartbeat);
    document.addEventListener("visibilitychange", sendHeartbeat);
    return () => {
      window.clearInterval(intervalId);
      window.removeEventListener("focus", sendHeartbeat);
      window.removeEventListener("pageshow", sendHeartbeat);
      document.removeEventListener("visibilitychange", sendHeartbeat);
    };
  }, [location.hash, location.pathname, location.search]);

  const activeIncident = pendingIncidents[0] ?? null;
  const notice = useMemo(
    () => (activeIncident ? buildNotice(activeIncident) : null),
    [activeIncident],
  );

  if (!activeIncident || !notice) {
    return null;
  }

  return (
    <div
      className="wb-snackbar"
      role={notice.restartRecommended ? "alert" : "status"}
      aria-live="polite"
      data-testid="desktop-webview-recovery-snackbar"
    >
      <div className="wb-snackbar-body">
        <div className="wb-snackbar-title">{notice.title}</div>
        <div className="wb-snackbar-subtitle">{notice.subtitle}</div>
      </div>
      <div className="wb-snackbar-actions">
        <button type="button" className="wb-snackbar-btn" onClick={() => navigate("/diagnostics")}>
          Open Diagnostics
        </button>
        {notice.restartRecommended ? (
          <button
            type="button"
            className="wb-snackbar-btn wb-snackbar-btn-secondary"
            onClick={() => {
              void desktopRestartApp().catch(() => {});
            }}
          >
            Restart App
          </button>
        ) : null}
      </div>
      <button
        type="button"
        className="wb-snackbar-close"
        aria-label="Dismiss desktop recovery notice"
        onClick={() => setPendingIncidents((current) => current.slice(1))}
      >
        <X size={14} aria-hidden="true" />
      </button>
    </div>
  );
}
