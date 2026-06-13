import type { ExecutionEnvironment } from "@ctx/types";
import { getWebappStorage } from "./storage";

export type LauncherExecutionEnvironment = ExecutionEnvironment;

export type LauncherRecentEntry =
  | {
      kind: "local";
      label: string;
      root_path: string;
      execution_environment?: LauncherExecutionEnvironment;
      updated_at_ms: number;
    }
  | {
      kind: "ssh";
      label: string;
      host: string;
      user?: string | null;
      remote_port: number;
      start_remote?: boolean;
      remote_data_dir?: string | null;
      workspace_root_path?: string | null;
      execution_environment?: LauncherExecutionEnvironment;
      updated_at_ms: number;
    };

type PersistedLauncherRecents = {
  v: 1;
  entries: LauncherRecentEntry[];
  updatedAtMs: number;
};

const storage = getWebappStorage();
const STORAGE_KEY = "wb.launcher_recents";
const MAX_RECENTS = 50;

function asRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== "object") return null;
  return value as Record<string, unknown>;
}

function asString(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function asNullableString(value: unknown): string | null {
  if (value === null || value === undefined) return null;
  return typeof value === "string" ? value : null;
}

function asBool(value: unknown): boolean | undefined {
  if (typeof value !== "boolean") return undefined;
  return value;
}

function asFiniteNumber(value: unknown): number | null {
  if (typeof value !== "number" || !Number.isFinite(value)) return null;
  return value;
}

function asExecutionEnvironment(value: unknown): LauncherExecutionEnvironment | undefined {
  if (value === "host" || value === "sandbox") {
    return value;
  }
  return undefined;
}

function parseEntry(raw: unknown): LauncherRecentEntry | null {
  const rec = asRecord(raw);
  if (!rec) return null;
  const kind = asString(rec.kind);
  const label = asString(rec.label);
  const updatedAt = asFiniteNumber(rec.updated_at_ms);
  if (!kind || !label || updatedAt === null) return null;

  if (kind === "local") {
    const rootPath = asString(rec.root_path);
    if (!rootPath) return null;
    return {
      kind: "local",
      label,
      root_path: rootPath,
      execution_environment: asExecutionEnvironment(rec.execution_environment),
      updated_at_ms: updatedAt,
    };
  }

  if (kind === "ssh") {
    const host = asString(rec.host);
    const remotePort = asFiniteNumber(rec.remote_port);
    if (!host || remotePort === null) return null;
    return {
      kind: "ssh",
      label,
      host,
      user: asNullableString(rec.user),
      remote_port: remotePort,
      start_remote: asBool(rec.start_remote),
      remote_data_dir: asNullableString(rec.remote_data_dir),
      workspace_root_path: asNullableString(rec.workspace_root_path),
      execution_environment: asExecutionEnvironment(rec.execution_environment),
      updated_at_ms: updatedAt,
    };
  }

  return null;
}

function entryKey(entry: LauncherRecentEntry): string {
  if (entry.kind === "local") return `local:${entry.root_path}`;
  return `ssh:${entry.user ?? ""}@${entry.host}:${entry.remote_port}:${entry.workspace_root_path ?? ""}:${entry.execution_environment ?? ""}`;
}

function normalizeEntries(entries: LauncherRecentEntry[]): LauncherRecentEntry[] {
  const sorted = [...entries].sort((a, b) => b.updated_at_ms - a.updated_at_ms);
  const seen = new Set<string>();
  const next: LauncherRecentEntry[] = [];
  for (const entry of sorted) {
    const key = entryKey(entry);
    if (seen.has(key)) continue;
    seen.add(key);
    next.push(entry);
    if (next.length >= MAX_RECENTS) break;
  }
  return next;
}

function decodePersisted(raw: unknown): LauncherRecentEntry[] {
  const rec = asRecord(raw);
  if (!rec || rec.v !== 1 || !Array.isArray(rec.entries)) return [];
  const entries: LauncherRecentEntry[] = [];
  for (const item of rec.entries) {
    const parsed = parseEntry(item);
    if (parsed) entries.push(parsed);
  }
  return normalizeEntries(entries);
}

async function writeEntries(entries: LauncherRecentEntry[]): Promise<void> {
  const normalized = normalizeEntries(entries);
  const payload: PersistedLauncherRecents = {
    v: 1,
    entries: normalized,
    updatedAtMs: Date.now(),
  };
  await storage.setKv(STORAGE_KEY, payload);
}

export async function loadLauncherRecents(): Promise<LauncherRecentEntry[]> {
  const raw = await storage.getKv<unknown>(STORAGE_KEY);
  return decodePersisted(raw);
}

export async function upsertLauncherRecent(entry: LauncherRecentEntry): Promise<LauncherRecentEntry[]> {
  const current = await loadLauncherRecents();
  const next = normalizeEntries([entry, ...current]);
  await writeEntries(next);
  return next;
}

export async function getLauncherRecentsCount(): Promise<number> {
  const recents = await loadLauncherRecents();
  return recents.length;
}

export async function clearLauncherRecents(): Promise<void> {
  await storage.deleteKv(STORAGE_KEY);
}

export const launcherRecentsStorageKey = STORAGE_KEY;
