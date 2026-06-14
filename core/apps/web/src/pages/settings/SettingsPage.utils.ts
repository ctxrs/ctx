import type { ExecutionSettings as ApiExecutionSettings, ProviderUsageSnapshot } from "../../api/client";
import { readBoolish } from "../../utils/boolish";
import { desktopSaveTextFile, isDesktopApp, type DesktopEditorSettings } from "../../utils/desktop";
import { SECTIONS } from "./SettingsPage.constants";
import type { SectionId } from "./SettingsPage.types";

export type WorkspaceExecutionEnvironment = "host" | "sandbox";
export type PromptAutosaveStatus = "idle" | "pending" | "saving" | "saved" | "error";
export type WorktreeBootstrapConfigLike = {
  setup_command?: string | null;
  timeout_sec?: number | null;
  wait_for_completion?: boolean | null;
  cleanup_command?: string | null;
  cleanup_timeout_sec?: number | null;
};
export type WorktreeBootstrapFormState = {
  setup_command: string;
  timeout_sec: string;
  wait_for_completion: boolean;
  cleanup_command: string;
  cleanup_timeout_sec: string;
};

export function isContainerizedEnvironment(environment?: WorkspaceExecutionEnvironment | null): boolean {
  return environment === "sandbox";
}

export function promptAutosaveStatusLabel(status: PromptAutosaveStatus): string {
  switch (status) {
    case "pending":
      return "Pending changes";
    case "saving":
      return "";
    case "saved":
      return "Saved";
    case "error":
      return "Save failed";
    default:
      return "";
  }
}

export function worktreeBootstrapFormFromConfig(
  cfg: WorktreeBootstrapConfigLike | null | undefined,
): WorktreeBootstrapFormState {
  const setupCommand = typeof cfg?.setup_command === "string" ? cfg.setup_command : "";
  const cleanupCommand = typeof cfg?.cleanup_command === "string" ? cfg.cleanup_command : "";
  const timeoutRaw = cfg?.timeout_sec;
  const timeoutSec =
    typeof timeoutRaw === "number" && Number.isFinite(timeoutRaw) && timeoutRaw > 0
      ? String(Math.round(timeoutRaw))
      : "";
  const cleanupTimeoutRaw = cfg?.cleanup_timeout_sec;
  const cleanupTimeoutSec =
    typeof cleanupTimeoutRaw === "number" && Number.isFinite(cleanupTimeoutRaw) && cleanupTimeoutRaw > 0
      ? String(Math.round(cleanupTimeoutRaw))
      : "";
  return {
    setup_command: setupCommand,
    timeout_sec: timeoutSec,
    wait_for_completion: readBoolish(cfg?.wait_for_completion) ?? false,
    cleanup_command: cleanupCommand,
    cleanup_timeout_sec: cleanupTimeoutSec,
  };
}

export function normalizeDesktopEditorSettings(settings: DesktopEditorSettings): DesktopEditorSettings {
  const target = settings.target === "custom" ? "system" : settings.target;
  return {
    target,
    custom_command: null,
    remote_authority: settings.remote_authority?.trim() || null,
  };
}

export function desktopEditorSettingsEqual(
  left: DesktopEditorSettings | null | undefined,
  right: DesktopEditorSettings | null | undefined,
): boolean {
  if (!left || !right) return left === right;
  const normalizedLeft = normalizeDesktopEditorSettings(left);
  const normalizedRight = normalizeDesktopEditorSettings(right);
  return (
    normalizedLeft.target === normalizedRight.target
    && normalizedLeft.custom_command === normalizedRight.custom_command
    && normalizedLeft.remote_authority === normalizedRight.remote_authority
  );
}

export function executionSettingsStableKey(settings: ApiExecutionSettings): string {
  return JSON.stringify({
    mode: settings.mode,
    container: {
      network_mode: settings.container.network_mode,
      allowlist: settings.container.allowlist,
      image: settings.container.image ?? null,
      machine: {
        memory_profile: settings.container.machine.memory_profile,
        custom_memory_mb: settings.container.machine.custom_memory_mb ?? null,
        idle_shutdown_seconds: settings.container.machine.idle_shutdown_seconds,
        host_pressure_swap_threshold_mb: settings.container.machine.host_pressure_swap_threshold_mb,
      },
    },
  });
}

export const saveTextFile = async (name: string, contents: string) => {
  if (isDesktopApp()) {
    await desktopSaveTextFile({ suggested_name: name, contents });
    return;
  }
  const blob = new Blob([contents], { type: "text/plain" });
  const url = URL.createObjectURL(blob);
  try {
    const a = document.createElement("a");
    a.href = url;
    a.download = name;
    a.rel = "noopener";
    a.click();
  } finally {
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
};

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

const readFiniteNumber = (value: unknown): number | null => {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
};

export function sectionFromHash(hash: string): SectionId | null {
  const raw = String(hash || "").replace(/^#/, "").trim();
  if (!raw) return null;
  if (raw === "sandboxing") return "container_network";
  const match = SECTIONS.find((s) => s.id === raw);
  if (!match) return null;
  if (match.id === "dev_tools" && !import.meta.env.DEV) return null;
  return match.id;
}

export function clampPct(n: number): number {
  if (!Number.isFinite(n)) return 0;
  return Math.max(0, Math.min(100, n));
}

export function formatPct(value?: number | null): string {
  if (!Number.isFinite(value)) return "—";
  return `${Math.round(value as number)}%`;
}

export function formatBytes(value?: number | null): string {
  if (!Number.isFinite(value)) return "—";
  const units = ["B", "KB", "MB", "GB", "TB", "PB"];
  let idx = 0;
  let v = value as number;
  while (v >= 1024 && idx < units.length - 1) {
    v /= 1024;
    idx += 1;
  }
  const precision = v >= 100 ? 0 : v >= 10 ? 1 : 2;
  return `${v.toFixed(precision)} ${units[idx]}`;
}

export function formatAge(ms?: number | null): string {
  if (!Number.isFinite(ms)) return "—";
  const totalSeconds = Math.max(0, Math.round((ms as number) / 1000));
  if (totalSeconds < 60) return `${totalSeconds}s`;
  const mins = Math.floor(totalSeconds / 60);
  const secs = totalSeconds % 60;
  return `${mins}m ${secs}s`;
}

export function isLinuxPlatform(): boolean {
  if (typeof navigator === "undefined") return false;
  const platform = navigator.platform?.toLowerCase() ?? "";
  const agent = navigator.userAgent?.toLowerCase() ?? "";
  return platform.includes("linux") || agent.includes("linux");
}

export function codexResetAtMs(window?: unknown): number | null {
  const rec = asRecord(window);
  if (Object.keys(rec).length === 0) return null;
  const resetAt = readFiniteNumber(rec.reset_at);
  if (resetAt !== null) return resetAt * 1000;
  const resetAtCamel = readFiniteNumber(rec.resetAt);
  if (resetAtCamel !== null) return resetAtCamel * 1000;
  const resetAfter = readFiniteNumber(rec.reset_after_seconds);
  if (resetAfter !== null) {
    return Date.now() + resetAfter * 1000;
  }
  const resetAfterCamel = readFiniteNumber(rec.resetAfterSeconds);
  if (resetAfterCamel !== null) {
    return Date.now() + resetAfterCamel * 1000;
  }
  return null;
}

export function codexRemainingPct(window?: unknown): number | null {
  const rec = asRecord(window);
  if (Object.keys(rec).length === 0) return null;
  const remaining = readFiniteNumber(rec.remaining_percent);
  if (remaining !== null) return clampPct(remaining);
  const remainingCamel = readFiniteNumber(rec.remainingPercent);
  if (remainingCamel !== null) return clampPct(remainingCamel);
  const used = readFiniteNumber(rec.used_percent);
  if (used !== null) return clampPct(100 - used);
  const usedCamel = readFiniteNumber(rec.usedPercent);
  if (usedCamel !== null) return clampPct(100 - usedCamel);
  return null;
}

export function formatResetLabel(resetAtMs?: number | null): string {
  if (!Number.isFinite(resetAtMs)) return "Reset time unavailable";
  const date = new Date(resetAtMs as number);
  if (!Number.isFinite(date.getTime())) return "Reset time unavailable";
  const now = new Date();
  const options: Intl.DateTimeFormatOptions = {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  };
  if (date.getFullYear() !== now.getFullYear()) {
    options.year = "numeric";
  }
  return `Resets ${date.toLocaleString(undefined, options)}`;
}

type CodexUsageSummary = {
  planType: string | null;
  primaryRemaining: number | null;
  secondaryRemaining: number | null;
  primaryResetAt: number | null;
  secondaryResetAt: number | null;
  creditsValue: string;
  creditsSub: string;
  updatedLabel: string;
  source: string | null;
  error: string | null;
};

export function summarizeCodexUsage(snapshot?: ProviderUsageSnapshot | null): CodexUsageSummary {
  const payload = asRecord(snapshot?.payload ?? null);
  const planTypeRaw = payload.plan_type ?? payload.planType;
  const planType = typeof planTypeRaw === "string" ? planTypeRaw : null;
  const rateLimit = asRecord(payload.rate_limit ?? payload.rateLimit ?? null);
  const primaryWindow = rateLimit.primary_window ?? rateLimit.primaryWindow ?? null;
  const secondaryWindow = rateLimit.secondary_window ?? rateLimit.secondaryWindow ?? null;
  const credits = asRecord(payload.credits ?? null);
  const primaryRemaining = codexRemainingPct(primaryWindow);
  const secondaryRemaining = codexRemainingPct(secondaryWindow);
  const primaryResetAt = codexResetAtMs(primaryWindow);
  const secondaryResetAt = codexResetAtMs(secondaryWindow);
  const creditsValue = (() => {
    if (Object.keys(credits).length === 0) return "—";
    if (credits.unlimited) return "Unlimited";
    if (credits.balance !== undefined && credits.balance !== null) {
      return String(credits.balance);
    }
    if (credits.has_credits === false) return "None";
    return "—";
  })();
  const creditsSub = (() => {
    if (Object.keys(credits).length === 0) return "Credits unavailable";
    if (credits.unlimited) return "No spend cap";
    if (credits.has_credits === false) return "Credits exhausted";
    return "Credits balance";
  })();
  const updatedLabel = (() => {
    if (!snapshot?.fetched_at) return "";
    const ts = Date.parse(snapshot.fetched_at);
    if (!Number.isFinite(ts)) return "";
    return `Updated ${formatAge(Date.now() - ts)} ago`;
  })();

  return {
    planType,
    primaryRemaining,
    secondaryRemaining,
    primaryResetAt,
    secondaryResetAt,
    creditsValue,
    creditsSub,
    updatedLabel,
    source: snapshot?.source ?? null,
    error: snapshot?.error ?? null,
  };
}

export function formatGiB(mb?: number | null): string {
  if (!Number.isFinite(mb) || !mb) return "";
  const gb = (mb as number) / 1024;
  const precision = gb >= 10 ? 0 : 1;
  return gb.toFixed(precision);
}

export function parseGiB(value: string): number | null {
  const v = Number(value);
  if (!Number.isFinite(v) || v <= 0) return null;
  return Math.round(v * 1024);
}

export function truncateText(value: string, maxLen: number): string {
  const s = String(value ?? "");
  if (s.length <= maxLen) return s;
  return `${s.slice(0, Math.max(0, maxLen - 1))}…`;
}

export function guessAttachmentName(source: string): string {
  let cleaned = String(source ?? "").trim();
  if (!cleaned) return "";
  cleaned = cleaned.replace(/[\\/]+$/, "");
  const slashIdx = Math.max(cleaned.lastIndexOf("/"), cleaned.lastIndexOf(":"));
  let name = slashIdx >= 0 ? cleaned.slice(slashIdx + 1) : cleaned;
  if (name.endsWith(".git")) name = name.slice(0, -4);
  return name;
}
