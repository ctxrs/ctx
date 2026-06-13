import { serializeOwnerScope, type WorkspaceOwnerScope } from "./scopeIdentity";
import { getWebappStorage } from "./storage";

export type UiKvRecord = {
  key: string;
  value: unknown;
  updatedAtMs: number;
};

export type UiStateBatchOp =
  | { kind: "set"; key: string; value: unknown }
  | { kind: "delete"; key: string };

const SESSION_HISTORY_PAGE_LIMIT = 120;
const SESSION_HISTORY_PAGE_TTL_MS = 7 * 24 * 60 * 60 * 1000;
const SESSION_HISTORY_TOUCH_GRACE_MS = 30 * 1000;

const storage = getWebappStorage();

const safeKeyPart = (value: string): string => encodeURIComponent(value);

const ownerScopeKeyPart = (ownerScope: WorkspaceOwnerScope): string => safeKeyPart(serializeOwnerScope(ownerScope));

export async function uiStateBatch(ops: UiStateBatchOp[]): Promise<void> {
  if (ops.length === 0) return;
  for (const op of ops) {
    if (op.kind === "set") {
      await storage.setKv(op.key, op.value);
    } else {
      await storage.deleteKv(op.key);
    }
  }
  await storage.flush();
}

export async function uiStateGet(key: string): Promise<unknown | null> {
  return (await storage.getKv<unknown>(key)) ?? null;
}

export async function uiStateSet(key: string, value: unknown): Promise<void> {
  await storage.setKv(key, value);
}

export async function uiStateDelete(key: string): Promise<void> {
  await storage.deleteKv(key);
}

export type PersistedWorkspaceActiveTaskSummaryV1 = {
  task: import("@ctx/types").Task;
  primary_session: import("@ctx/types").SessionSnapshotSummary | null;
  primary_session_head: import("@ctx/types").SessionHeadSnapshot | null;
  sessions: import("@ctx/types").SessionSnapshotSummary[];
  sort_at?: string | null;
};

export type PersistedWorkspaceActiveSnapshotV1 = {
  v: 1;
  workspaceId: string;
  snapshotRev?: number;
  archivedRev?: number;
  worktreeVcsSnapshots?: import("@ctx/types").WorktreeVcsSnapshot[];
  active: {
    tasks: PersistedWorkspaceActiveTaskSummaryV1[];
    totalCount?: number;
  };
  updatedAtMs: number;
};

export function workspaceActiveSnapshotKeyV1(workspaceId: string) {
  return `wb.active_snapshot.v1.${workspaceId}`;
}

export function decodeWorkspaceActiveSnapshotV1(
  raw: unknown,
  workspaceId: string,
): PersistedWorkspaceActiveSnapshotV1 | null {
  if (!raw || typeof raw !== "object") return null;
  const rec = raw as PersistedWorkspaceActiveSnapshotV1;
  if (rec.v !== 1 || rec.workspaceId !== workspaceId) return null;
  if (!rec.active || !Array.isArray(rec.active.tasks)) return null;
  return rec;
}

export async function loadWorkspaceActiveSnapshotV1(
  workspaceId: string,
): Promise<PersistedWorkspaceActiveSnapshotV1 | null> {
  const raw = await storage.getSnapshot<PersistedWorkspaceActiveSnapshotV1>(
    workspaceActiveSnapshotKeyV1(workspaceId),
  );
  return decodeWorkspaceActiveSnapshotV1(raw, workspaceId);
}

export async function saveWorkspaceActiveSnapshotV1(
  workspaceId: string,
  payload: Omit<PersistedWorkspaceActiveSnapshotV1, "v" | "workspaceId" | "updatedAtMs">,
): Promise<void> {
  await storage.setSnapshot(workspaceActiveSnapshotKeyV1(workspaceId), {
    v: 1,
    workspaceId,
    updatedAtMs: Date.now(),
    ...payload,
  } satisfies PersistedWorkspaceActiveSnapshotV1);
}

export type PersistedSessionHeadV1 = {
  v: 1;
  sessionId: string;
  head: import("@ctx/types").SessionHead;
  updatedAtMs: number;
};

export function sessionHeadKeyV1(sessionId: string) {
  return `wb.session_head.v1.${sessionId}`;
}

export async function loadSessionHeadV1(sessionId: string): Promise<PersistedSessionHeadV1 | null> {
  const raw = await storage.getSnapshot<PersistedSessionHeadV1>(sessionHeadKeyV1(sessionId));
  if (!raw || typeof raw !== "object") return null;
  const rec = raw as PersistedSessionHeadV1;
  if (rec.v !== 1 || rec.sessionId !== sessionId || !rec.head) return null;
  return rec;
}

export async function saveSessionHeadV1(
  sessionId: string,
  head: PersistedSessionHeadV1["head"],
): Promise<void> {
  await storage.setSnapshot(sessionHeadKeyV1(sessionId), {
    v: 1,
    sessionId,
    head,
    updatedAtMs: Date.now(),
  } satisfies PersistedSessionHeadV1);
}

export async function clearSessionHeadV1(sessionId: string): Promise<void> {
  await storage.deleteSnapshot(sessionHeadKeyV1(sessionId));
}

export type PersistedSessionAcpMetaV1 = {
  v: 1;
  sessionId: string;
  models?: unknown;
  modes?: unknown;
  currentModelId?: string;
  commands?: unknown;
  slashCommands?: unknown;
  updatedAtMs: number;
};

export function sessionAcpMetaKeyV1(sessionId: string) {
  return `wb.session_acp_meta.v1.${sessionId}`;
}

export async function loadSessionAcpMetaV1(sessionId: string): Promise<PersistedSessionAcpMetaV1 | null> {
  const raw = await storage.getSnapshot<PersistedSessionAcpMetaV1>(sessionAcpMetaKeyV1(sessionId));
  if (!raw || typeof raw !== "object") return null;
  const rec = raw as PersistedSessionAcpMetaV1;
  if (rec.v !== 1 || rec.sessionId !== sessionId) return null;
  return rec;
}

export async function saveSessionAcpMetaV1(
  sessionId: string,
  payload: Omit<PersistedSessionAcpMetaV1, "v" | "sessionId" | "updatedAtMs">,
): Promise<void> {
  await storage.setSnapshot(sessionAcpMetaKeyV1(sessionId), {
    v: 1,
    sessionId,
    updatedAtMs: Date.now(),
    ...payload,
  } satisfies PersistedSessionAcpMetaV1);
}

export type PersistedThoughtRowV1 = {
  key: string;
  event: import("@ctx/types").SessionEvent;
  updatedAtMs?: number;
};

export type PersistedSessionThoughtsV1 = {
  sessionId: string;
  thoughts: Record<string, PersistedThoughtRowV1>;
};

export type PersistedTaskThoughtsV1 = {
  v: 1;
  taskId: string;
  sessions: Record<string, PersistedSessionThoughtsV1>;
  updatedAtMs: number;
};

export function taskThoughtsKeyV2(ownerScope: WorkspaceOwnerScope, taskId: string) {
  return `wb.task_thoughts.v2.${ownerScopeKeyPart(ownerScope)}.${safeKeyPart(taskId)}`;
}

export async function loadTaskThoughtsV1(
  ownerScope: WorkspaceOwnerScope,
  taskId: string,
): Promise<PersistedTaskThoughtsV1 | null> {
  const raw = await storage.getSnapshot<PersistedTaskThoughtsV1>(taskThoughtsKeyV2(ownerScope, taskId));
  if (!raw || typeof raw !== "object") return null;
  const rec = raw as PersistedTaskThoughtsV1;
  if (rec.v !== 1 || rec.taskId !== taskId || !rec.sessions || typeof rec.sessions !== "object") {
    return null;
  }
  return rec;
}

export async function saveTaskThoughtsV1(
  ownerScope: WorkspaceOwnerScope,
  taskId: string,
  payload: Omit<PersistedTaskThoughtsV1, "v" | "taskId" | "updatedAtMs">,
): Promise<void> {
  await storage.setSnapshot(taskThoughtsKeyV2(ownerScope, taskId), {
    v: 1,
    taskId,
    updatedAtMs: Date.now(),
    ...payload,
  } satisfies PersistedTaskThoughtsV1);
}

export async function clearTaskThoughtsV1(ownerScope: WorkspaceOwnerScope, taskId: string): Promise<void> {
  await storage.deleteSnapshot(taskThoughtsKeyV2(ownerScope, taskId));
}

export type PersistedSessionHistoryPageV1 = {
  v: 1;
  sessionId: string;
  beforeSeq: number;
  limit: number;
  page: import("@ctx/types").SessionHistoryPage;
  updatedAtMs: number;
};

type PersistedSessionHistoryIndexV1 = {
  v: 1;
  entries: Array<{ key: string; updatedAtMs: number }>;
};

function sessionHistoryIndexKeyV1() {
  return "wb.session_history_index.v1";
}

export function sessionHistoryPageKeyV2(
  ownerScope: WorkspaceOwnerScope,
  sessionId: string,
  beforeSeq: number,
  limit: number,
) {
  return "wb.session_history_page.v2."
    + ownerScopeKeyPart(ownerScope)
    + "."
    + safeKeyPart(sessionId)
    + "."
    + beforeSeq
    + "."
    + limit;
}

function decodeSessionHistoryIndexV1(raw: unknown): PersistedSessionHistoryIndexV1 {
  if (!raw || typeof raw !== "object") return { v: 1, entries: [] };
  const rec = raw as PersistedSessionHistoryIndexV1;
  if (rec.v !== 1 || !Array.isArray(rec.entries)) return { v: 1, entries: [] };
  const entries: PersistedSessionHistoryIndexV1["entries"] = [];
  for (const entry of rec.entries) {
    if (!entry || typeof entry.key !== "string") continue;
    if (!Number.isFinite(entry.updatedAtMs)) continue;
    entries.push({ key: entry.key, updatedAtMs: entry.updatedAtMs });
  }
  return { v: 1, entries };
}

function planSessionHistoryEvictions(
  entries: PersistedSessionHistoryIndexV1["entries"],
  nowMs: number,
): { kept: PersistedSessionHistoryIndexV1["entries"]; evictKeys: string[] } {
  const expiresBefore = nowMs - SESSION_HISTORY_PAGE_TTL_MS;
  const evict = new Set<string>();
  const latestByKey = new Map<string, number>();

  for (const entry of entries) {
    if (!entry.key || !Number.isFinite(entry.updatedAtMs)) continue;
    if (entry.updatedAtMs < expiresBefore) {
      evict.add(entry.key);
      continue;
    }
    const existing = latestByKey.get(entry.key);
    if (existing === undefined || existing < entry.updatedAtMs) {
      latestByKey.set(entry.key, entry.updatedAtMs);
    }
  }

  const sorted = Array.from(latestByKey.entries())
    .map(([key, updatedAtMs]) => ({ key, updatedAtMs }))
    .sort((a, b) => b.updatedAtMs - a.updatedAtMs);
  const kept = sorted.slice(0, SESSION_HISTORY_PAGE_LIMIT);
  const keptKeys = new Set(kept.map((entry) => entry.key));

  for (const entry of sorted.slice(SESSION_HISTORY_PAGE_LIMIT)) {
    evict.add(entry.key);
  }

  const evictKeys = Array.from(evict).filter((key) => !keptKeys.has(key));
  return { kept, evictKeys };
}

async function updateSessionHistoryIndexV1(
  key: string,
  nowMs: number,
  opts?: { force?: boolean },
): Promise<void> {
  const index = decodeSessionHistoryIndexV1(
    await storage.getKv<PersistedSessionHistoryIndexV1>(sessionHistoryIndexKeyV1()),
  );
  const existing = index.entries.find((entry) => entry.key === key);
  if (!opts?.force && existing && nowMs - existing.updatedAtMs < SESSION_HISTORY_TOUCH_GRACE_MS) {
    return;
  }
  const entries = index.entries.filter((entry) => entry.key !== key);
  entries.unshift({ key, updatedAtMs: nowMs });
  const { kept, evictKeys } = planSessionHistoryEvictions(entries, nowMs);
  await storage.setKv(sessionHistoryIndexKeyV1(), { v: 1, entries: kept } satisfies PersistedSessionHistoryIndexV1);
  if (evictKeys.length === 0) return;
  await Promise.all(evictKeys.map((evictKey) => storage.deleteHistoryPage(evictKey)));
}

export async function loadSessionHistoryPageV1(
  ownerScope: WorkspaceOwnerScope,
  sessionId: string,
  beforeSeq: number,
  limit: number,
): Promise<PersistedSessionHistoryPageV1 | null> {
  const key = sessionHistoryPageKeyV2(ownerScope, sessionId, beforeSeq, limit);
  const raw = await storage.getHistoryPage<PersistedSessionHistoryPageV1>(key);
  if (!raw || typeof raw !== "object") return null;
  const rec = raw as PersistedSessionHistoryPageV1;
  if (rec.v !== 1 || rec.sessionId !== sessionId) return null;
  void updateSessionHistoryIndexV1(key, Date.now()).catch(() => {});
  return rec;
}

export async function saveSessionHistoryPageV1(
  ownerScope: WorkspaceOwnerScope,
  sessionId: string,
  beforeSeq: number,
  limit: number,
  page: PersistedSessionHistoryPageV1["page"],
): Promise<void> {
  const key = sessionHistoryPageKeyV2(ownerScope, sessionId, beforeSeq, limit);
  const now = Date.now();
  const pageValue: PersistedSessionHistoryPageV1 = {
    v: 1,
    sessionId,
    beforeSeq,
    limit,
    page,
    updatedAtMs: now,
  };

  await storage.setHistoryPage(key, pageValue);
  await updateSessionHistoryIndexV1(key, now, { force: true });
}

export async function clearSessionHistoryPagesV1(sessionId: string): Promise<void> {
  const encodedSessionId = safeKeyPart(sessionId);
  const index = decodeSessionHistoryIndexV1(
    await storage.getKv<PersistedSessionHistoryIndexV1>(sessionHistoryIndexKeyV1()),
  );
  const kept = index.entries.filter(
    (entry) => !entry.key.includes(`.${encodedSessionId}.`),
  );
  const deletedKeys = index.entries
    .filter((entry) => entry.key.includes(`.${encodedSessionId}.`))
    .map((entry) => entry.key);
  await storage.setKv(
    sessionHistoryIndexKeyV1(),
    { v: 1, entries: kept } satisfies PersistedSessionHistoryIndexV1,
  );
  if (deletedKeys.length === 0) return;
  await Promise.all(deletedKeys.map((key) => storage.deleteHistoryPage(key)));
}

const asRecord = (value: unknown): Record<string, unknown> | null => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  return value as Record<string, unknown>;
};

const trim = (value: unknown): string => String(value ?? "").trim();

const readBoolish = (value: unknown): boolean | null => {
  if (typeof value === "boolean") return value;
  if (value === "true") return true;
  if (value === "false") return false;
  return null;
};

function sanitizeSettingsForStorage(
  settings: unknown,
): import("../api/client").PublicSettings | null {
  const rec = asRecord(settings);
  if (!rec) return null;

  const sanitized: Record<string, unknown> = { ...rec };

  const dictation = asRecord(rec.dictation);
  if (dictation) {
    const dictationSanitized: Record<string, unknown> = { ...dictation };
    const livekit = asRecord(dictation.livekit);
    if (livekit) {
      const livekitSanitized: Record<string, unknown> = { ...livekit };
      livekitSanitized.api_key_set =
        readBoolish(livekit.api_key_set) ?? trim(livekit.api_key) !== "";
      livekitSanitized.api_secret_set =
        readBoolish(livekit.api_secret_set) ?? trim(livekit.api_secret) !== "";
      delete livekitSanitized.api_key;
      delete livekitSanitized.api_secret;
      dictationSanitized.livekit = livekitSanitized;
    }
    sanitized.dictation = dictationSanitized;
  }

  const titleGeneration = asRecord(rec.title_generation);
  if (titleGeneration) {
    const titleGenerationSanitized: Record<string, unknown> = { ...titleGeneration };
    const remote = asRecord(titleGeneration.remote);
    if (remote) {
      const remoteSanitized: Record<string, unknown> = { ...remote };
      remoteSanitized.api_key_set =
        readBoolish(remote.api_key_set) ?? trim(remote.api_key) !== "";
      delete remoteSanitized.api_key;
      titleGenerationSanitized.remote = remoteSanitized;
    }
    sanitized.title_generation = titleGenerationSanitized;
  }

  return sanitized as import("../api/client").PublicSettings;
}

export type PersistedSettingsV2 = {
  v: 2;
  settings: import("../api/client").PublicSettings;
  updatedAtMs: number;
};

export function settingsLegacyKeyV1() {
  return "wb.settings.v1";
}

export function settingsKeyV2() {
  return "wb.settings.v2";
}

export async function loadSettingsV2(): Promise<PersistedSettingsV2 | null> {
  const raw = await storage.getKv<PersistedSettingsV2>(settingsKeyV2());
  if (raw && typeof raw === "object") {
    const rec = raw as PersistedSettingsV2;
    if (rec.v === 2 && rec.settings) return rec;
  }

  const legacyRaw = await storage.getKv<{ v: 1; settings: unknown; updatedAtMs: number }>(settingsLegacyKeyV1());
  if (!legacyRaw || typeof legacyRaw !== "object") return null;
  const legacy = legacyRaw as { v: 1; settings: unknown; updatedAtMs: number };
  const sanitized = sanitizeSettingsForStorage(legacy.settings);
  await storage.deleteKv(settingsLegacyKeyV1());
  if (!sanitized) {
    await storage.flush();
    return null;
  }

  const migrated: PersistedSettingsV2 = {
    v: 2,
    settings: sanitized,
    updatedAtMs: legacy.updatedAtMs ?? Date.now(),
  };
  await storage.setKv(settingsKeyV2(), migrated);
  await storage.flush();
  return migrated;
}

export async function saveSettingsV2(settings: PersistedSettingsV2["settings"]): Promise<void> {
  const sanitized = sanitizeSettingsForStorage(settings);
  if (!sanitized) return;
  await storage.setKv(settingsKeyV2(), {
    v: 2,
    settings: sanitized,
    updatedAtMs: Date.now(),
  } satisfies PersistedSettingsV2);
  await storage.deleteKv(settingsLegacyKeyV1());
  await storage.flush();
}

export type SessionViewVerbosity = "terse" | "default" | "verbose";

export type PersistedSessionViewPrefsV1 = {
  v: 1;
  verbosity: SessionViewVerbosity;
  updatedAtMs: number;
};

export function sessionViewPrefsKeyV1() {
  return "wb.session_view_prefs.v1";
}

export async function loadSessionViewPrefsV1(): Promise<PersistedSessionViewPrefsV1 | null> {
  const raw = await storage.getKv<PersistedSessionViewPrefsV1>(sessionViewPrefsKeyV1());
  if (!raw || typeof raw !== "object") return null;
  const rec = raw as PersistedSessionViewPrefsV1;
  if (rec.v !== 1) return null;
  if (!["terse", "default", "verbose"].includes(String(rec.verbosity))) return null;
  return rec;
}

export async function saveSessionViewPrefsV1(verbosity: SessionViewVerbosity): Promise<void> {
  await storage.setKv(sessionViewPrefsKeyV1(), {
    v: 1,
    verbosity,
    updatedAtMs: Date.now(),
  } satisfies PersistedSessionViewPrefsV1);
}
