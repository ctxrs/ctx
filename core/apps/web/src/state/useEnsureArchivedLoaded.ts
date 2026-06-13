import { useEffect } from "react";

type EnsureArchivedLoadedParams = {
  archivedCollapsed: boolean;
  archivedLoaded: boolean;
  fetchState: "idle" | "loading" | "error";
  activeInitialized: boolean;
  activeFetchState: "idle" | "loading" | "error";
  prefetchAfterActive?: boolean;
  ensureArchivedLoaded: () => void;
};

export function useEnsureArchivedLoaded({
  archivedCollapsed,
  archivedLoaded,
  fetchState,
  activeInitialized,
  activeFetchState,
  prefetchAfterActive,
  ensureArchivedLoaded,
}: EnsureArchivedLoadedParams) {
  useEffect(() => {
    if (!prefetchAfterActive && archivedCollapsed) return;
    if (archivedLoaded) return;
    if (fetchState === "loading") return;
    if (!activeInitialized) return;
    if (activeFetchState !== "idle") return;
    const timer = window.setTimeout(() => {
      ensureArchivedLoaded();
    }, 300);
    return () => window.clearTimeout(timer);
  }, [archivedCollapsed, archivedLoaded, fetchState, activeInitialized, activeFetchState, ensureArchivedLoaded]);
}
