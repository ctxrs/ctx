import type { InstallErrorCode, InstallInfo, InstallTarget, ProviderStatus } from "../api/client";

const INSTALL_STAGE_PROGRESS: Record<string, number> = {
  start: 2,
  download: 10,
  node: 15,
  node_download: 18,
  node_extract: 22,
  prepare: 25,
  venv: 35,
  npm_install: 65,
  pip_install: 70,
  extract: 78,
  entrypoint: 80,
  inspect: 90,
  refresh: 95,
  registry: 98,
};

export const clampPct = (value: number): number => Math.max(0, Math.min(100, value));

export const parseInstallTarget = (value: string | undefined): InstallTarget | undefined => {
  if (!value) return undefined;
  if (value === "host" || value === "container" || value === "linux-aarch64" || value === "linux-x86_64") {
    return value;
  }
  return undefined;
};

export const installTargetLabel = (target: InstallTarget | undefined): string => {
  if (!target) return "host";
  if (target === "linux-aarch64") return "linux/arm64";
  if (target === "linux-x86_64") return "linux/x86_64";
  return target;
};

const toNumber = (raw: string | undefined): number | null => {
  if (!raw) return null;
  const parsed = Number(raw);
  if (!Number.isFinite(parsed) || parsed <= 0) return null;
  return parsed;
};

export const providerInstallSizeBytes = (provider: ProviderStatus | undefined): number | null => {
  return toNumber(provider?.details?.install_download_size_bytes);
};

export const formatByteSize = (bytes: number | null | undefined): string | null => {
  if (bytes == null || !Number.isFinite(bytes) || bytes <= 0) return null;
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const decimals = value >= 100 || unit === 0 ? 0 : value >= 10 ? 1 : 2;
  return `${value.toFixed(decimals)} ${units[unit]}`;
};

export const computeInstallPct = (
  info: Pick<InstallInfo, "state" | "last_event" | "progress_pct">,
  previousPct: number | null,
): number | null => {
  if (info.state === "succeeded") return 100;
  if (typeof info.progress_pct === "number" && Number.isFinite(info.progress_pct)) {
    return clampPct(Math.round(info.progress_pct));
  }
  const last = info.last_event;
  if (typeof last?.bytes === "number" && typeof last?.total_bytes === "number" && last.total_bytes > 0) {
    const raw = clampPct(Math.round((last.bytes / last.total_bytes) * 100));
    if (typeof last.stage === "string" && last.stage.includes("download")) {
      return clampPct(Math.round((raw / 100) * 75));
    }
    return raw;
  }
  if (typeof last?.stage === "string") {
    const staged = INSTALL_STAGE_PROGRESS[last.stage];
    if (typeof staged === "number") {
      return previousPct == null ? staged : Math.max(previousPct, staged);
    }
  }
  return previousPct;
};

export const installErrorSummary = (errorCode: InstallErrorCode | undefined, fallback: string | undefined): string => {
  if (errorCode === "cancelled") return "Install cancelled.";
  if (errorCode === "unsupported_target") return "This provider is not available for the selected target.";
  if (errorCode === "invalid_target") return "Invalid install target. Re-scan providers and try again.";
  if (errorCode === "download_failed") return "Download failed. Check connectivity and retry.";
  if (errorCode === "checksum_mismatch") return "Integrity check failed. Retry; if it persists, refresh the provider matrix.";
  if (errorCode === "timeout") return "Install timed out. Retry or check host performance/network.";
  if (errorCode === "command_failed") return "Installer command failed. Inspect logs and retry.";
  if (errorCode === "matrix_mismatch") return "Provider matrix/release metadata mismatch. Refresh matrix and retry.";
  if (errorCode === "health_check_failed") return "Install finished but health verification failed. Retry to re-register.";
  if (errorCode === "registry_write_failed") return "Install succeeded but registry update failed. Retry to repair metadata.";
  if (fallback && fallback.trim().length > 0) return fallback.trim();
  return "Install failed. Retry from this screen.";
};
