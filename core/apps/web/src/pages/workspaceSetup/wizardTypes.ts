import type { ReactNode } from "react";
import type { InstallInfo, InstallTarget } from "../../api/client";
import type { WizardStepKey } from "./wizardFlow";

export type WizardOption = {
  id: string;
  title: string;
  desc: string;
  badge?: string;
  advanced?: boolean;
};

export type WizardStep = {
  key: WizardStepKey;
  title: string;
  note: string;
  options?: WizardOption[];
  body?: ReactNode;
  info?: string;
};

export type ImportInitDialogState = {
  path: string;
};

export type LocalInstallState = {
  installId: string;
  state: InstallInfo["state"];
  pct: number | null;
  errorCode?: InstallInfo["error_code"];
  error?: string;
};

export type HarnessInstallProviderRow = {
  providerId: string;
  label: string;
  installed: boolean;
  healthy: boolean;
  installSupported: boolean;
  installRunning: boolean;
  blocked?: boolean;
  installId?: string;
  installTarget?: InstallTarget;
  installSizeBytes?: number | null;
};

export type HarnessInstallRowState = {
  installId: string;
  state: InstallInfo["state"];
  pct: number | null;
  target?: InstallTarget;
  errorCode?: InstallInfo["error_code"];
  error?: string;
};

export type HarnessInstallCandidateStatus =
  | "installed"
  | "running"
  | "ready_to_start"
  | "succeeded"
  | "failed"
  | "cancelled";

export type RemoteStatus = "idle" | "connecting" | "connected" | "error";

export type SshSuggestion = {
  host: string;
  user?: string | null;
};

export function resolveHarnessInstallCandidateStatus(
  candidate: HarnessInstallProviderRow,
  installUi?: HarnessInstallRowState,
): HarnessInstallCandidateStatus {
  if (candidate.installed && candidate.healthy) return "installed";
  if (installUi?.state === "failed") return "failed";
  if (installUi?.state === "cancelled") return "cancelled";
  if (installUi?.state === "succeeded") return "succeeded";
  if (installUi?.state === "running" || candidate.installRunning) return "running";
  return "ready_to_start";
}

export const looksLikeSshAuthFailure = (message: string): boolean => {
  const lowered = message.toLowerCase();
  return lowered.includes("permission denied")
    || lowered.includes("publickey")
    || lowered.includes("authentication failed")
    || lowered.includes("too many authentication failures");
};

export const messageFromError = (error: unknown): string =>
  error instanceof Error && error.message ? error.message : String(error);

export function lastPathSegment(path: string): string {
  const normalized = String(path || "").trim().replace(/\/+$/, "");
  if (!normalized) return "";
  const idx = normalized.lastIndexOf("/");
  return idx >= 0 ? normalized.slice(idx + 1) : normalized;
}
