import { useCallback, useEffect, useState } from "react";
import { getSettings } from "../../api/client";
import { useSettingsSnapshot, useSettingsStore } from "../../state/settingsStore";
import { errorMessage } from "../../utils/errorMessage";

type ProviderGuardNotice = {
  kind: string;
  stage?: string | null;
  provider?: string | null;
  pid?: number | null;
  killAtMs?: number | null;
  limitHighMb?: number | null;
  limitMaxMb?: number | null;
  memoryMb?: number | null;
  systemUsedMb?: number | null;
  systemTotalMb?: number | null;
  message?: string | null;
} | null;

type UseSessionProviderGuardArgs = {
  providerGuardNotice: ProviderGuardNotice;
  sessionProviderId: string | null | undefined;
};

export function useSessionProviderGuard({
  providerGuardNotice,
  sessionProviderId,
}: UseSessionProviderGuardArgs) {
  const settingsStore = useSettingsStore();
  const settingsSnapshot = useSettingsSnapshot();
  const [providerGuardActionError, setProviderGuardActionError] = useState<string | null>(null);
  const [providerGuardActionBusy, setProviderGuardActionBusy] = useState(false);

  const providerGuardNoticeKey = providerGuardNotice
    ? `${providerGuardNotice.kind}:${providerGuardNotice.stage}:${providerGuardNotice.pid ?? ""}:${providerGuardNotice.killAtMs ?? ""}`
    : "";

  useEffect(() => {
    setProviderGuardActionError(null);
  }, [providerGuardNoticeKey]);

  const applyProviderGuardSettings = useCallback(
    async (opts: {
      enabled?: boolean;
      mode?: "auto" | "custom";
      memoryHighMb?: number | null;
      memoryMaxMb?: number | null;
    }) => {
      setProviderGuardActionError(null);
      setProviderGuardActionBusy(true);
      try {
        const current = settingsSnapshot.settings ?? (await getSettings());
        const guard = current.provider_guard ?? { enabled: true, mode: "auto" };
        const nextGuard = {
          enabled: opts.enabled ?? guard.enabled ?? true,
          mode: opts.mode ?? guard.mode ?? "auto",
          memory_high_mb: opts.memoryHighMb ?? guard.memory_high_mb ?? null,
          memory_max_mb: opts.memoryMaxMb ?? guard.memory_max_mb ?? null,
          interval_ms: guard.interval_ms ?? null,
          grace_period_ms: guard.grace_period_ms ?? null,
        };
        await settingsStore.update({ provider_guard: nextGuard });
      } catch (error: unknown) {
        setProviderGuardActionError(errorMessage(error));
      } finally {
        setProviderGuardActionBusy(false);
      }
    },
    [settingsSnapshot.settings, settingsStore],
  );

  const raiseProviderGuardLimit = useCallback(async () => {
    const totalMb = providerGuardNotice?.systemTotalMb;
    if (!totalMb || !Number.isFinite(totalMb)) {
      setProviderGuardActionError("System memory total is unavailable.");
      return;
    }
    const maxMb = Math.max(1024, Math.floor(totalMb * 0.9));
    let highMb = Math.floor(totalMb * 0.85);
    if (highMb > maxMb) highMb = maxMb;
    await applyProviderGuardSettings({
      enabled: true,
      mode: "custom",
      memoryHighMb: highMb,
      memoryMaxMb: maxMb,
    });
  }, [applyProviderGuardSettings, providerGuardNotice?.systemTotalMb]);

  const disableProviderGuard = useCallback(async () => {
    await applyProviderGuardSettings({ enabled: false });
  }, [applyProviderGuardSettings]);

  const providerGuardMemoryLimitMb =
    providerGuardNotice?.stage === "high"
      ? providerGuardNotice?.limitHighMb
      : providerGuardNotice?.limitMaxMb;
  const providerGuardHeading =
    providerGuardNotice?.kind === "provider_guard_kill"
      ? "Provider guard kill"
      : providerGuardNotice?.stage === "max"
        ? "Provider memory limit"
        : "Provider memory warning";
  const providerGuardMessage =
    providerGuardNotice?.message ??
    (providerGuardNotice?.kind === "provider_guard_kill"
      ? "Provider process killed after exceeding memory limits."
      : "Provider memory is above the guard threshold.");
  const providerGuardLimitLabel =
    providerGuardNotice?.stage === "high"
      ? "high limit"
      : providerGuardNotice?.stage === "max"
        ? "max limit"
        : "limit";
  const providerGuardProviderLabel =
    providerGuardNotice?.provider ?? sessionProviderId ?? undefined;
  const providerGuardPidLabel =
    providerGuardNotice?.pid != null
      ? `PID ${Math.round(providerGuardNotice.pid)}`
      : null;
  const canRaiseProviderGuard =
    providerGuardNotice?.systemTotalMb != null &&
    Number.isFinite(providerGuardNotice.systemTotalMb);

  return {
    providerGuardActionError,
    providerGuardActionBusy,
    providerGuardMemoryLimitMb,
    providerGuardHeading,
    providerGuardMessage,
    providerGuardLimitLabel,
    providerGuardProviderLabel,
    providerGuardPidLabel,
    canRaiseProviderGuard,
    raiseProviderGuardLimit,
    disableProviderGuard,
  };
}
