import { type GitStatusSummary, type ProviderOptions } from "../../api/client";
import { randomUuid } from "../../utils/randomUuid";
import { desktopSaveTextFile, isDesktopApp } from "../../utils/desktop";
import type { AnchorRect, OptimisticTaskSummary } from "./WorkbenchPage.types";
import type { WorkspaceActiveSnapshotItem } from "../../state/workspaceActiveSnapshotStore";

export function deriveTaskTitle(_prompt: string): string {
  return "New Task";
}

export const ARCHIVE_CONFIRM_STORAGE_KEY = "wb.archiveConfirmDismissed";
export const WORKSPACE_TABS_STORAGE_KEY = "wb.workspaceTabs.v1";
export const UI_WINDOW_ID_STORAGE_KEY = "contextUiWindowId.v1";

export type WorkspaceTabsState = {
  openWorkspaceIds: string[];
};

type SaveFilePickerOptions = {
  suggestedName?: string;
  types?: Array<{ description: string; accept: Record<string, string[]> }>;
};

type WritableFileLike = {
  write: (data: string) => Promise<void>;
  close: () => Promise<void>;
};

type FileHandleLike = {
  createWritable: () => Promise<WritableFileLike>;
};

type WindowWithSaveFilePicker = Window & {
  showSaveFilePicker?: (opts: SaveFilePickerOptions) => Promise<FileHandleLike>;
};

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

export const getOrCreateUiWindowId = (): string => {
  try {
    const existing = sessionStorage.getItem(UI_WINDOW_ID_STORAGE_KEY);
    if (existing && existing.trim()) return existing;
    const created = randomUuid();
    sessionStorage.setItem(UI_WINDOW_ID_STORAGE_KEY, created);
    return created;
  } catch {
    return randomUuid();
  }
};

export const loadWorkspaceTabsState = (storageKey: string): WorkspaceTabsState | null => {
  try {
    const raw = localStorage.getItem(storageKey);
    if (!raw) return null;
    const parsed = asRecord(JSON.parse(raw));
    const openWorkspaceIds = Array.isArray(parsed.openWorkspaceIds)
      ? parsed.openWorkspaceIds.map((v) => String(v)).filter((v: string) => v.trim())
      : [];
    return { openWorkspaceIds };
  } catch {
    return null;
  }
};

export const saveWorkspaceTabsState = (storageKey: string, state: WorkspaceTabsState) => {
  try {
    localStorage.setItem(storageKey, JSON.stringify(state));
  } catch {
    // ignore
  }
};

export const ensureWorkspaceIdInTabs = (state: WorkspaceTabsState, workspaceId: string): WorkspaceTabsState => {
  if (!workspaceId) return state;
  if (state.openWorkspaceIds.includes(workspaceId)) return state;
  return { openWorkspaceIds: [...state.openWorkspaceIds, workspaceId] };
};

export const normalizeGitStatusSummary = (
  value: GitStatusSummary | string | null | undefined,
): GitStatusSummary | null => {
  if (!value) return null;
  if (typeof value === "string") return { raw: value };
  return value;
};

export const formatGitStatusEntry = (entry: NonNullable<GitStatusSummary["entries"]>[number]): string => {
  const indexStatus = entry.index_status?.[0] ?? " ";
  const worktreeStatus = entry.worktree_status?.[0] ?? " ";
  const prefix = `${indexStatus}${worktreeStatus}`;
  const path = entry.orig_path ? `${entry.orig_path} -> ${entry.path}` : entry.path;
  return `${prefix} ${path}`.trimEnd();
};

export const formatGitStatusPath = (entry: NonNullable<GitStatusSummary["entries"]>[number]): string =>
  entry.orig_path ? `${entry.orig_path} -> ${entry.path}` : entry.path;

export const gitStatusCodeClass = (code: string): string => {
  switch (code) {
    case "A":
    case "C":
      return "wb-git-status-code-added";
    case "M":
    case "T":
      return "wb-git-status-code-modified";
    case "D":
      return "wb-git-status-code-deleted";
    case "R":
      return "wb-git-status-code-renamed";
    case "?":
      return "wb-git-status-code-untracked";
    case "U":
      return "wb-git-status-code-conflict";
    case "!":
      return "wb-git-status-code-ignored";
    default:
      return "wb-git-status-code-neutral";
  }
};

export const normalizeGitStatusCode = (value?: string): string => {
  const code = value?.[0] ?? " ";
  return code || " ";
};

export const gitStatusCodeLabel = (code: string): string => (code === " " ? "" : code);

export const readGitStatusSummaryLine = (summary: GitStatusSummary | null): string => {
  if (!summary) return "";
  const raw = summary.raw ?? summary.summary ?? summary.status ?? "";
  const summaryLine =
    summary.summary_line ?? summary.summaryLine ?? (raw ? String(raw).split("\n")[0].trim() : "");
  return summaryLine || "";
};

export const joinDaemonPath = (root: string, ...parts: string[]) => {
  const sep = root.includes("\\") ? "\\" : "/";
  const cleanedRoot = root.replace(/[\/]+$/, "");
  const cleanedParts = parts.map((part) => String(part).replace(/^[\/]+|[\/]+$/g, ""));
  return [cleanedRoot, ...cleanedParts].filter(Boolean).join(sep);
};

export const deriveManagedWorktreeRoot = (dataRoot: string | null, workspaceId: string, worktreeId: string): string => {
  if (!dataRoot) return "";
  return joinDaemonPath(dataRoot, "worktrees", workspaceId, worktreeId);
};

const selectedEndpointModelOverride = (opts?: ProviderOptions): string => {
  const source = opts?.source;
  if (!source || source.selected_source_kind !== "endpoint") return "";
  const endpointId = String(source.selected_endpoint_id ?? "").trim();
  if (!endpointId) return "";
  const endpoint = source.endpoints.find((candidate) => candidate.id === endpointId);
  return String(endpoint?.model_override ?? "").trim();
};

export function modelIdsFromOptions(opts?: ProviderOptions): string[] {
  const raw = opts?.models;
  const rec = asRecord(raw);
  const preferred = String(opts?.preferred_model_id ?? "").trim();
  const current = String(rec.currentModelId ?? rec.current_model_id ?? "").trim();
  const sourceOverride = selectedEndpointModelOverride(opts);
  const list = rec.availableModels ?? rec.available_models ?? rec.models ?? raw;
  const ids = (Array.isArray(list) ? list : [])
    .map((m) => {
      const model = asRecord(m);
      return String(model.modelId ?? model.model_id ?? model.id ?? model.name ?? "").trim();
    })
    .filter((s: string) => s.length > 0);
  const preferredAvailable =
    preferred.length > 0 && (current === preferred || ids.includes(preferred));
  return Array.from(
    new Set([preferredAvailable ? preferred : "", current, sourceOverride, ...ids].filter((id) => id.length > 0)),
  );
}

export function appendSegment(base: string, addition: string): string {
  const trimmed = addition.trim();
  if (!trimmed) return base;
  if (!base) return trimmed;
  const needsSpace = /\S$/.test(base) && !/^[,.;!?]/.test(trimmed);
  return `${base}${needsSpace ? " " : ""}${trimmed}`;
}

export function parseMs(value: string | null | undefined): number | null {
  if (!value) return null;
  const ms = Date.parse(value);
  return Number.isFinite(ms) ? ms : null;
}

export function formatWorktreePath(raw: string): string {
  const path = String(raw ?? "").trim();
  return path;
}

export function formatWorktreeLabel(raw: string): string {
  const path = String(raw ?? "").trim().replace(/[\\/]+$/, "");
  if (!path) return "";
  const parts = path.split(/[\\/]/).filter(Boolean);
  const base = parts[parts.length - 1] ?? "";
  if (!base) return "";
  const uuidMatch = base.match(/^([0-9a-f]{8})-[0-9a-f-]{27,}$/i);
  if (uuidMatch) return uuidMatch[1];
  if (base.length <= 16) return base;
  return `${base.slice(0, 16)}...`;
}

export function formatWorktreeChipLabel({
  worktreePath,
  worktreeId,
  executionEnvironment,
}: {
  worktreePath?: string | null;
  worktreeId?: string | null;
  executionEnvironment?: string | null;
}): string {
  const pathLabel = formatWorktreeLabel(worktreePath ?? "");
  if (pathLabel) return pathLabel;
  const idLabel = formatWorktreeLabel(worktreeId ?? "");
  if (idLabel) return idLabel;
  const env = String(executionEnvironment ?? "").trim();
  if (env === "sandbox") return "Sandbox worktree";
  if (env === "host") return "Session worktree";
  return "";
}

export function lastAssistantMessageMs(messages: { role: string; created_at: string }[]): number | null {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m?.role === "assistant") return parseMs(m.created_at);
  }
  return null;
}

export function lastRoleMessageMs(messages: { role: string; created_at: string }[], role: string): number | null {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m?.role === role) return parseMs(m.created_at);
  }
  return null;
}

const SPINNER_DURATION_MS = 800;
const SPINNER_ANCHOR_MS = typeof performance !== "undefined" ? performance.now() : Date.now();

export function spinnerDelayForNow(): number {
  const now = typeof performance !== "undefined" ? performance.now() : Date.now();
  return -((now - SPINNER_ANCHOR_MS) % SPINNER_DURATION_MS);
}

export function normalizeAnchorRect(rect: DOMRect | AnchorRect | null | undefined): AnchorRect {
  if (rect) {
    return {
      left: rect.left,
      right: rect.right,
      top: rect.top,
      bottom: rect.bottom,
      width: rect.width,
      height: rect.height,
    };
  }
  const viewportW = typeof window === "undefined" ? 1200 : window.innerWidth;
  const viewportH = typeof window === "undefined" ? 800 : window.innerHeight;
  return {
    left: viewportW / 2,
    right: viewportW / 2,
    top: viewportH / 2,
    bottom: viewportH / 2,
    width: 0,
    height: 0,
  };
}

export function clampNum(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

export const isOptimisticTask = (summary: WorkspaceActiveSnapshotItem): summary is OptimisticTaskSummary => {
  return typeof (summary as OptimisticTaskSummary).localStatus === "string";
};

export function sanitizeFileName(name: string): string {
  const raw = String(name ?? "").trim() || "conversation";
  const noBadChars = raw.replace(/[<>:"/\\|?*\u0000-\u001F]/g, "");
  const collapsed = noBadChars.replace(/\s+/g, "-").replace(/-+/g, "-").replace(/^-+|-+$/g, "");
  return (collapsed || "conversation").slice(0, 80);
}

export async function saveMarkdownExport(suggestedName: string, contents: string): Promise<void> {
  const name = suggestedName.toLowerCase().endsWith(".md") ? suggestedName : `${suggestedName}.md`;
  if (isDesktopApp()) {
    await desktopSaveTextFile({ suggested_name: name, contents });
    return;
  }

  const picker = (window as WindowWithSaveFilePicker).showSaveFilePicker;
  if (typeof picker === "function") {
    const handle = await picker({
      suggestedName: name,
      types: [{ description: "Markdown", accept: { "text/markdown": [".md"] } }],
    });
    const writable = await handle.createWritable();
    await writable.write(contents);
    await writable.close();
    return;
  }

  const blob = new Blob([contents], { type: "text/markdown" });
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
}
