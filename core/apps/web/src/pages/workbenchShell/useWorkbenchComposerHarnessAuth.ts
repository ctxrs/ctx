import { useCallback, useEffect, useRef, useState } from "react";
import type { ProviderOptions } from "../../api/client";
import { hasConfiguredHarnessAuth } from "../../utils/providerAuthStatus";
import type { HarnessAuthenticationController } from "../settings/hooks/useHarnessAuthenticationController";

type ComposerHarnessAuthController = Pick<
  HarnessAuthenticationController,
  "harnessAuthModal" | "openHarnessAuthModal" | "closeHarnessAuthModal"
>;

type UseWorkbenchComposerHarnessAuthArgs = {
  activeTaskId: string | null;
  controller: ComposerHarnessAuthController;
  ensureProviderAuthSummary: (providerId: string, opts?: { force?: boolean }) => Promise<ProviderOptions | undefined>;
  providerOptions: Record<string, ProviderOptions | undefined>;
  setSingleDraftHarness: (providerId: string) => void;
};

type UseWorkbenchComposerHarnessAuthResult = {
  requestHarnessAuthFromComposer: (providerId: string) => void;
};

export function useWorkbenchComposerHarnessAuth({
  activeTaskId,
  controller,
  ensureProviderAuthSummary,
  providerOptions,
  setSingleDraftHarness,
}: UseWorkbenchComposerHarnessAuthArgs): UseWorkbenchComposerHarnessAuthResult {
  const { closeHarnessAuthModal, harnessAuthModal, openHarnessAuthModal } = controller;
  const [pendingHarnessSelectionProviderId, setPendingHarnessSelectionProviderId] = useState<string | null>(null);
  const lastModalProviderIdRef = useRef<string | null>(null);

  const requestHarnessAuthFromComposer = useCallback((providerId: string) => {
    setPendingHarnessSelectionProviderId(providerId);
    openHarnessAuthModal(providerId);
  }, [openHarnessAuthModal]);

  useEffect(() => {
    const currentProviderId = harnessAuthModal?.provider_id ?? null;
    if (currentProviderId) {
      lastModalProviderIdRef.current = currentProviderId;
      return;
    }

    const closedProviderId = lastModalProviderIdRef.current;
    if (!closedProviderId) return;
    lastModalProviderIdRef.current = null;
    if (pendingHarnessSelectionProviderId !== closedProviderId) return;

    setPendingHarnessSelectionProviderId(null);
    void ensureProviderAuthSummary(closedProviderId, { force: true })
      .then((options) => {
        const resolved = options ?? providerOptions[closedProviderId];
        if (!hasConfiguredHarnessAuth(closedProviderId, resolved)) return;
        setSingleDraftHarness(closedProviderId);
      })
      .catch(() => {});
  }, [
    harnessAuthModal?.provider_id,
    ensureProviderAuthSummary,
    pendingHarnessSelectionProviderId,
    providerOptions,
    setSingleDraftHarness,
  ]);

  useEffect(() => {
    if (!activeTaskId) return;
    lastModalProviderIdRef.current = null;
    setPendingHarnessSelectionProviderId(null);
    closeHarnessAuthModal();
  }, [activeTaskId, closeHarnessAuthModal]);

  return { requestHarnessAuthFromComposer };
}
