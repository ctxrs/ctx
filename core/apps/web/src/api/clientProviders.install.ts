import { apiAny, authToken } from "./clientBase";
import { setBrowserStreamQueryToken } from "./browserStreamAuth";
import { getDaemonHttpUrl } from "./daemonConnection";

export type InstallEventLevel = "info" | "warning" | "error" | "success";
export type InstallTarget = "host" | "container" | "linux-aarch64" | "linux-x86_64";
export type InstallErrorCode =
  | "invalid_target"
  | "unsupported_target"
  | "download_failed"
  | "checksum_mismatch"
  | "command_failed"
  | "timeout"
  | "matrix_mismatch"
  | "health_check_failed"
  | "registry_write_failed"
  | "cancelled"
  | "unknown";

export type InstallProgressEvent = {
  install_id: string;
  provider_id: string;
  target?: InstallTarget;
  at: string;
  stage: string;
  message: string;
  level: InstallEventLevel;
  bytes?: number;
  total_bytes?: number;
  attempt?: number;
  error_code?: InstallErrorCode;
};

export type InstallInfo = {
  install_id: string;
  provider_id: string;
  target?: InstallTarget;
  state: "running" | "succeeded" | "failed" | "cancelled";
  progress_pct?: number | null;
  started_at: string;
  finished_at?: string;
  error?: string;
  error_code?: InstallErrorCode;
  last_event?: InstallProgressEvent;
};

export type InstallStartResponse = {
  provider_id: string;
  install_id: string;
  target: InstallTarget;
};

export type InstallInfoBatchItem = {
  install_id: string;
  info: InstallInfo | null;
};

export type InstallInfoBatchResponse = {
  installs: InstallInfoBatchItem[];
};

export const withInstallTargetParam = (path: string, target?: InstallTarget): string => {
  if (!target) return path;
  const separator = path.includes("?") ? "&" : "?";
  return `${path}${separator}target=${encodeURIComponent(target)}`;
};

export const installProvider = (providerId: string, target?: InstallTarget) =>
  apiAny<InstallStartResponse>(withInstallTargetParam(`/api/providers/${providerId}/install`, target), { method: "POST" });

export const installAllProviders = (target?: InstallTarget) =>
  apiAny<InstallStartResponse[]>(withInstallTargetParam(`/api/providers/install_all`, target), { method: "POST" });

export const getInstall = (installId: string) =>
  apiAny<InstallInfo>(`/api/providers/install/${installId}`);

export const getInstallStatuses = (installIds: string[]) =>
  apiAny<InstallInfoBatchResponse>(`/api/providers/install/statuses`, {
    method: "POST",
    body: JSON.stringify({ install_ids: installIds }),
  });

export const cancelInstall = (installId: string) =>
  apiAny<InstallInfo>(`/api/providers/install/${installId}/cancel`, { method: "POST" });

export const listInstallEvents = (installId: string) =>
  apiAny<InstallProgressEvent[]>(`/api/providers/install/${installId}/events`);

export const installStreamUrl = async (installId: string): Promise<string> => {
  const base = getDaemonHttpUrl(`/api/providers/install/${installId}/stream`);
  const query = new URLSearchParams();
  await setBrowserStreamQueryToken(query, authToken(), {
    kind: "provider_install",
    installId,
  });
  const serialized = query.toString();
  return serialized ? `${base}?${serialized}` : base;
};

export type DevRestartProvidersMode = "immediate" | "drain";

export type DevRestartProvidersResult = {
  provider_id: string;
  status: string;
  message?: string;
};

export type DevRestartProvidersResponse = {
  mode: DevRestartProvidersMode;
  results: DevRestartProvidersResult[];
};

export const devRestartProviders = (mode: DevRestartProvidersMode) =>
  apiAny<DevRestartProvidersResponse>(`/api/dev/providers/restart`, {
    method: "POST",
    body: JSON.stringify({ mode }),
  });
