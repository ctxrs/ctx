import { useEffect, useMemo, useState } from "react";
import { X } from "lucide-react";
import type { ProviderStatus } from "../../api/client";
import {
  acknowledgeProviderRuntimeWarnings,
  buildProviderRuntimeWarning,
  clearAcknowledgedProviderRuntimeWarningIds,
  readAcknowledgedProviderRuntimeWarningIds,
  type ProviderRuntimeWarning,
} from "../../utils/providerRuntimeWarnings";

type WorkbenchProviderWarningBannerProps = {
  acknowledgementScopeId: string;
  providersById: Record<string, ProviderStatus>;
  mobileShell?: boolean;
  updateAllBusy?: boolean;
  onUpdateProviders: (providerIds: string[]) => Promise<void> | void;
  onOpenSettings: () => void;
};
export { buildProviderRuntimeWarning as buildWorkbenchProviderWarning };
export type WorkbenchProviderWarning = ProviderRuntimeWarning;

export function WorkbenchProviderWarningBanner({
  acknowledgementScopeId,
  providersById,
  mobileShell = false,
  updateAllBusy = false,
  onUpdateProviders,
  onOpenSettings,
}: WorkbenchProviderWarningBannerProps) {
  const warning = useMemo(
    () => buildProviderRuntimeWarning(providersById),
    [providersById],
  );
  const flaggedProviderIds = useMemo(
    () => warning?.providerIds ?? [],
    [warning],
  );
  const [acknowledgedProviderIds, setAcknowledgedProviderIds] = useState<string[]>(() =>
    readAcknowledgedProviderRuntimeWarningIds(acknowledgementScopeId));
  const acknowledgedProviderIdSet = useMemo(
    () => new Set(acknowledgedProviderIds),
    [acknowledgedProviderIds],
  );

  useEffect(() => {
    setAcknowledgedProviderIds(readAcknowledgedProviderRuntimeWarningIds(acknowledgementScopeId));
  }, [acknowledgementScopeId]);

  useEffect(() => {
    if (warning) return;
    clearAcknowledgedProviderRuntimeWarningIds(acknowledgementScopeId);
    setAcknowledgedProviderIds((current) => (current.length > 0 ? [] : current));
  }, [acknowledgementScopeId, warning]);

  const warningAcknowledged =
    flaggedProviderIds.length > 0
    && flaggedProviderIds.every((providerId) => acknowledgedProviderIdSet.has(providerId));

  if (!warning || warningAcknowledged) return null;

  const acknowledgeWarning = () => {
    const nextAcknowledgedProviderIds = acknowledgeProviderRuntimeWarnings(
      acknowledgementScopeId,
      flaggedProviderIds,
    );
    setAcknowledgedProviderIds(nextAcknowledgedProviderIds);
  };

  const dismiss = () => {
    acknowledgeWarning();
  };

  const handleOpenSettings = () => {
    dismiss();
    onOpenSettings();
  };

  const handleUpdateAll = async () => {
    acknowledgeWarning();
    try {
      await onUpdateProviders(warning.installableProviderIds);
    } catch {
      // The workbench already surfaces install errors independently.
    }
  };

  return (
    <div
      className={`wb-snackbar wb-provider-warning-snackbar${mobileShell ? " wb-provider-warning-snackbar-mobile" : ""}`}
      role="status"
      aria-live="polite"
      aria-labelledby="wb-provider-warning-title"
      data-testid="workbench-provider-warning"
    >
      <div className="wb-provider-warning-header">
        <div className="wb-snackbar-title" id="wb-provider-warning-title">{warning.title}</div>
        <button
          type="button"
          className="wb-snackbar-close"
          onClick={dismiss}
          aria-label="Dismiss"
        >
          <X size={16} aria-hidden="true" />
        </button>
      </div>
      <div className="wb-snackbar-actions wb-provider-warning-actions">
        {warning.installableProviderIds.length > 0 ? (
          <button
            type="button"
            className="wb-snackbar-btn"
            onClick={() => {
              void handleUpdateAll();
            }}
            disabled={updateAllBusy}
          >
            {updateAllBusy ? "Updating…" : "Update All"}
          </button>
        ) : null}
        <button
          type="button"
          className="wb-snackbar-btn wb-snackbar-btn-secondary"
          onClick={handleOpenSettings}
        >
          Open Settings
        </button>
      </div>
    </div>
  );
}
