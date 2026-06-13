import type { InstallTarget, ProviderStatus } from "../../api/client";
import type { HarnessCatalogEntry } from "../../utils/harnessCatalog";
import { providerDetailFlag } from "../../utils/boolish";
import {
  parseInstallTarget,
  providerInstallSizeBytes,
} from "../../utils/providerInstallUi";
import {
  isReadyVisibleHarnessProviderStatus,
  isVisibleHarnessProviderStatus,
} from "../../utils/providerInventory";
import type {
  HarnessInstallProviderRow,
  HarnessInstallRowState,
} from "./wizardTypes";

export type RunningHarnessInstallProviderRow = HarnessInstallProviderRow & {
  installRunning: true;
  installId: string;
};

export function mapHarnessInstallCandidate(
  provider: ProviderStatus,
  harnessByProviderId: ReadonlyMap<string, HarnessCatalogEntry>,
  fallbackInstallTarget: InstallTarget,
): HarnessInstallProviderRow | null {
  if (!isVisibleHarnessProviderStatus(provider)) return null;

  const installSupported = providerDetailFlag(provider.details, "install_supported");
  if (!installSupported) return null;

  const ready = isReadyVisibleHarnessProviderStatus(provider);
  const harness = harnessByProviderId.get(provider.provider_id);
  const installTarget = parseInstallTarget(provider.details?.install_target) ?? fallbackInstallTarget;

  return {
    providerId: provider.provider_id,
    label: harness?.label ?? provider.provider_id,
    installed: ready,
    healthy: ready,
    installSupported,
    installRunning: providerDetailFlag(provider.details, "install_running"),
    blocked: provider.usability.usable === false && provider.usability.status === "blocked",
    installId: provider.details?.install_id,
    installTarget,
    installSizeBytes: providerInstallSizeBytes(provider),
  };
}

export function deriveHarnessInstallSelectedState(
  rows: HarnessInstallProviderRow[],
  previousSelection: Record<string, boolean>,
): Record<string, boolean> {
  return Object.fromEntries(
    rows.map((row) => {
      if (row.installed && row.healthy) {
        return [row.providerId, false];
      }
      if (Object.prototype.hasOwnProperty.call(previousSelection, row.providerId)) {
        return [row.providerId, Boolean(previousSelection[row.providerId])];
      }
      return [row.providerId, row.installSupported];
    }),
  );
}

export function getRunningHarnessInstallProviderRows(
  rows: HarnessInstallProviderRow[],
): RunningHarnessInstallProviderRow[] {
  return rows.filter((row): row is RunningHarnessInstallProviderRow =>
    row.installRunning && typeof row.installId === "string" && row.installId.length > 0
  );
}

export function buildRunningHarnessInstallRowPatch(
  rows: RunningHarnessInstallProviderRow[],
  previousRows: Record<string, HarnessInstallRowState>,
): Record<string, HarnessInstallRowState> {
  return Object.fromEntries(
    rows.map((row) => [
      row.providerId,
      {
        installId: row.installId,
        state: "running" as const,
        pct: previousRows[row.providerId]?.pct ?? null,
        target: row.installTarget,
        errorCode: undefined,
        error: undefined,
      },
    ]),
  );
}
