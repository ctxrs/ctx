import { useCallback, useEffect, useMemo, useRef } from "react";

import type { ProviderOptions, ProviderStatus } from "../../api/client";
import type { DraftHarness } from "../../components/WorkbenchComposer";
import { hasConfiguredHarnessAuth } from "../../utils/providerAuthStatus";
import {
  collectSelectableHarnessProviderIds,
  getHarnessMruStorageKey,
  resolveInitialHarnessSelection,
  shouldFinalizeInitialHarnessSelection,
} from "./harnessSelection";

type UseWorkbenchDraftHarnessSelectionArgs = {
  activeTaskId: string | null;
  workspaceId: string;
  draftHarness: DraftHarness | null;
  setDraftHarness: (updater: (previous: DraftHarness | null) => DraftHarness | null) => void;
  providersById: Record<string, ProviderStatus>;
  providerOptions: Record<string, ProviderOptions | undefined>;
  ensureProviderAuthSummary: (providerId: string) => Promise<unknown>;
  manualDemoHarnessSelection: boolean;
};

export function useWorkbenchDraftHarnessSelection({
  activeTaskId,
  workspaceId,
  draftHarness,
  setDraftHarness,
  providersById,
  providerOptions,
  ensureProviderAuthSummary,
  manualDemoHarnessSelection,
}: UseWorkbenchDraftHarnessSelectionArgs) {
  const prefetchedProviderOptionsRef = useRef<Set<string>>(new Set());
  const initialHarnessSelectionResolvedRef = useRef(false);
  const selectableHarnessProviderIds = useMemo(
    () => collectSelectableHarnessProviderIds(providersById),
    [providersById],
  );

  const setSingleDraftHarness = useCallback((providerId: string) => {
    setDraftHarness((prev) => {
      if (prev?.providerId === providerId) return prev;
      return { providerId, modelId: "" };
    });
  }, [setDraftHarness]);

  useEffect(() => {
    if (activeTaskId) return;
    for (const providerId of selectableHarnessProviderIds) {
      if (providerOptions[providerId]) continue;
      if (prefetchedProviderOptionsRef.current.has(providerId)) continue;
      prefetchedProviderOptionsRef.current.add(providerId);
      ensureProviderAuthSummary(providerId).catch(() => {});
    }
  }, [activeTaskId, ensureProviderAuthSummary, providerOptions, selectableHarnessProviderIds]);

  useEffect(() => {
    prefetchedProviderOptionsRef.current.clear();
    initialHarnessSelectionResolvedRef.current = false;
  }, [workspaceId]);

  useEffect(() => {
    if (activeTaskId) return;
    if (!workspaceId) return;
    if (draftHarness) return;
    if (initialHarnessSelectionResolvedRef.current) return;
    if (selectableHarnessProviderIds.length === 0) return;

    let mruProviderId: string | null = null;
    try {
      mruProviderId = localStorage.getItem(getHarnessMruStorageKey(workspaceId));
    } catch {
      // ignore
    }

    const selectedProviderId = resolveInitialHarnessSelection({
      providerIds: selectableHarnessProviderIds,
      providerOptions,
      mruProviderId,
      disableAutoselect: manualDemoHarnessSelection,
    });
    if (!shouldFinalizeInitialHarnessSelection(selectedProviderId)) return;
    initialHarnessSelectionResolvedRef.current = true;
    setSingleDraftHarness(selectedProviderId);
  }, [
    activeTaskId,
    draftHarness,
    manualDemoHarnessSelection,
    providerOptions,
    selectableHarnessProviderIds,
    setSingleDraftHarness,
    workspaceId,
  ]);

  useEffect(() => {
    if (activeTaskId) return;
    if (!workspaceId) return;
    const selectedDraftProviderId = draftHarness?.providerId ?? null;
    if (!selectedDraftProviderId) return;
    if (!hasConfiguredHarnessAuth(selectedDraftProviderId, providerOptions[selectedDraftProviderId])) return;
    try {
      localStorage.setItem(getHarnessMruStorageKey(workspaceId), selectedDraftProviderId);
    } catch {
      // ignore
    }
  }, [activeTaskId, draftHarness?.providerId, providerOptions, workspaceId]);

  return { setSingleDraftHarness };
}
