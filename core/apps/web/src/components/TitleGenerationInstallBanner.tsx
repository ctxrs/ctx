import { useCallback, useEffect, useRef, useState } from "react";
import { X } from "lucide-react";
import { getTitleGenerationLocalStatus } from "../api/client";
import {
  observeInstall,
  subscribeInstallProgress,
  type InstallProgressEntry,
} from "../state/installProgressMonitor";

const TITLE_GEN_DISMISSED_INSTALLS_KEY = "wb.title_generation.dismissed_install_ids.v1";
const TITLE_GENERATION_LOCAL_PROVIDER_ID = "title_generation_local";

type TitleGenInstallBannerState = {
  installId: string;
  status: "running" | "failed";
  pct: number | null;
  stage: string | null;
  message: string | null;
  error: string | null;
};

const loadDismissedInstallIds = (): Set<string> => {
  try {
    const raw = localStorage.getItem(TITLE_GEN_DISMISSED_INSTALLS_KEY);
    if (!raw) return new Set<string>();
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return new Set<string>();
    const ids = parsed.filter((value): value is string => typeof value === "string" && value.trim().length > 0);
    return new Set(ids);
  } catch {
    return new Set<string>();
  }
};

const saveDismissedInstallIds = (ids: Set<string>) => {
  try {
    localStorage.setItem(TITLE_GEN_DISMISSED_INSTALLS_KEY, JSON.stringify(Array.from(ids)));
  } catch {
    // Best effort only.
  }
};

const toBannerState = (entry: InstallProgressEntry): TitleGenInstallBannerState | null => {
  const stage = typeof entry.lastEvent?.stage === "string" ? entry.lastEvent.stage : null;
  const message = typeof entry.lastEvent?.message === "string" ? entry.lastEvent.message : null;
  if (entry.state === "running") {
    return {
      installId: entry.installId,
      status: "running",
      pct: entry.pct,
      stage,
      message,
      error: null,
    };
  }
  if (entry.state === "failed") {
    return {
      installId: entry.installId,
      status: "failed",
      pct: entry.pct,
      stage,
      message,
      error: entry.error ?? "Local title model install failed.",
    };
  }
  return null;
};

const findTrackedEntry = (
  currentInstallId: string | null,
  entries: InstallProgressEntry[],
  dismissedInstallIds: Set<string>,
): InstallProgressEntry | null => {
  const visibleEntries = entries.filter((entry) => !dismissedInstallIds.has(entry.installId));
  const runningEntry = visibleEntries.find(
    (entry) => entry.providerId === TITLE_GENERATION_LOCAL_PROVIDER_ID && entry.state === "running",
  );
  if (runningEntry) return runningEntry;
  if (currentInstallId) {
    const currentEntry = visibleEntries.find((entry) => entry.installId === currentInstallId);
    if (currentEntry) return currentEntry;
  }
  return visibleEntries.find((entry) => entry.providerId === TITLE_GENERATION_LOCAL_PROVIDER_ID) ?? null;
};

export function TitleGenerationInstallBanner() {
  const [state, setState] = useState<TitleGenInstallBannerState | null>(null);
  const activeInstallIdRef = useRef<string | null>(null);
  const dismissedInstallIdsRef = useRef<Set<string>>(new Set<string>());
  const bootstrapObserverRef = useRef<(() => void) | null>(null);

  const attachInstall = useCallback((installId: string) => {
    const normalizedInstallId = installId.trim();
    if (!normalizedInstallId) return;
    if (dismissedInstallIdsRef.current.has(normalizedInstallId)) return;
    if (activeInstallIdRef.current === normalizedInstallId && bootstrapObserverRef.current) {
      return;
    }
    bootstrapObserverRef.current?.();
    activeInstallIdRef.current = normalizedInstallId;
    bootstrapObserverRef.current = observeInstall(normalizedInstallId, {
      loadHistory: true,
      initialState: {
        state: "running",
      },
    });
  }, []);

  useEffect(() => {
    dismissedInstallIdsRef.current = loadDismissedInstallIds();
    let cancelled = false;
    void getTitleGenerationLocalStatus()
      .then((status) => {
        if (cancelled) return;
        if (status.install_running && typeof status.install_id === "string" && status.install_id.trim()) {
          attachInstall(status.install_id);
        }
      })
      .catch(() => {});
    const unsubscribe = subscribeInstallProgress((snapshot) => {
      const tracked = findTrackedEntry(
        activeInstallIdRef.current,
        Object.values(snapshot),
        dismissedInstallIdsRef.current,
      );
      if (!tracked) {
        activeInstallIdRef.current = null;
        setState(null);
        return;
      }
      activeInstallIdRef.current = tracked.installId;
      setState(toBannerState(tracked));
      if (tracked.state === "succeeded" || tracked.state === "cancelled") {
        bootstrapObserverRef.current?.();
        bootstrapObserverRef.current = null;
      }
    });
    return () => {
      cancelled = true;
      unsubscribe();
      bootstrapObserverRef.current?.();
      bootstrapObserverRef.current = null;
      activeInstallIdRef.current = null;
    };
  }, [attachInstall]);

  const dismiss = useCallback(() => {
    if (state?.installId) {
      dismissedInstallIdsRef.current.add(state.installId);
      saveDismissedInstallIds(dismissedInstallIdsRef.current);
    }
    if (activeInstallIdRef.current === state?.installId) {
      activeInstallIdRef.current = null;
      bootstrapObserverRef.current?.();
      bootstrapObserverRef.current = null;
    }
    setState(null);
  }, [state]);

  if (!state) return null;

  const progressLabel = state.pct == null ? "Downloading…" : `Downloading… ${state.pct}%`;
  const title = state.status === "failed"
    ? "Local session titling install failed."
    : "Session titling model download in progress.";
  const subtitle = state.status === "failed"
    ? state.error ?? "Review daemon logs for more details."
    : (state.message ?? state.stage ?? "The model will be ready automatically when download completes.");

  return (
    <div className="wb-snackbar wb-snackbar-titlegen" role="status" aria-live="polite">
      <div className="wb-snackbar-body">
        <div className="wb-snackbar-title">{title}</div>
        <div className="wb-snackbar-subtitle">{subtitle}</div>
        {state.status === "running" ? (
          <div className="wb-snackbar-progress-block">
            <div className="wb-snackbar-progress-label">{progressLabel}</div>
            <div
              className="wb-snackbar-progress"
              role="progressbar"
              aria-label="Session titling install progress"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={state.pct ?? undefined}
            >
              <div
                className={`wb-snackbar-progress-fill${state.pct == null ? " is-indeterminate" : ""}`}
                style={state.pct == null ? undefined : { width: `${state.pct}%` }}
              />
            </div>
          </div>
        ) : null}
      </div>
      <button type="button" className="wb-snackbar-close" onClick={dismiss} aria-label="Dismiss">
        <X size={14} aria-hidden="true" />
      </button>
    </div>
  );
}
