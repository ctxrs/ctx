import { getDaemonConnection, idToString, type SessionEvent, type SessionTurn } from "../../api/client";
import {
  clearTaskThoughtsV1,
  loadTaskThoughtsV1,
  saveTaskThoughtsV1,
  type PersistedTaskThoughtsV1,
} from "../uiStateStore";
import {
  createWorkspaceOwnerScope,
  serializeOwnerScope,
  type WorkspaceOwnerScope,
} from "../scopeIdentity";
import type { SessionSupervisorWorkspaceSnapshotState } from "./workspaceInputs";

export type SessionSupervisorThoughtCacheEntry = {
  sessionId: string;
  session?: { workspace_id?: string | null; task_id?: string | null };
  thoughtCacheByKey: Record<string, { key: string; event: SessionEvent; updatedAtMs?: number }>;
  thoughtCacheDirty: boolean;
  thoughtCacheLoaded: boolean;
  thoughtCacheLoading: boolean;
  thoughtCacheOwnerTaskKey?: string;
  thoughtCacheLoadToken: number;
  events: SessionEvent[];
  turns: SessionTurn[];
  updatedAtMs: number;
  seqSet: Set<number>;
};

export type SessionSupervisorThoughtCacheHost = {
  taskThoughtCache: Map<string, PersistedTaskThoughtsV1>;
  taskThoughtCacheLoading: Map<string, Promise<PersistedTaskThoughtsV1>>;
  workspaceSnapshotState: SessionSupervisorWorkspaceSnapshotState;
  entries: Map<string, SessionSupervisorThoughtCacheEntry>;
  publish(): void;
  bumpEventsRev(entry: SessionSupervisorThoughtCacheEntry): void;
  bumpTurnsRev(entry: SessionSupervisorThoughtCacheEntry): void;
  overlayThoughtCacheOnEvents(
    entry: SessionSupervisorThoughtCacheEntry,
    events: SessionEvent[],
  ): SessionEvent[];
  resolveWorkspaceOwnerScope(workspaceId: string | null | undefined): WorkspaceOwnerScope | null;
  resolveEntryWorkspaceOwnerScope(entry: SessionSupervisorThoughtCacheEntry): WorkspaceOwnerScope | null;
  persistThoughtCache(entry: SessionSupervisorThoughtCacheEntry): Promise<void>;
};

const taskThoughtOwnerKey = (ownerScope: WorkspaceOwnerScope, taskId: string): string =>
  `${serializeOwnerScope(ownerScope)}\u0000${taskId}`;

const clearTaskThoughtCachesForTask = (
  host: SessionSupervisorThoughtCacheHost,
  taskId: string,
) => {
  for (const key of Array.from(host.taskThoughtCache.keys())) {
    if (key.endsWith(`\u0000${taskId}`)) {
      host.taskThoughtCache.delete(key);
    }
  }
  for (const key of Array.from(host.taskThoughtCacheLoading.keys())) {
    if (key.endsWith(`\u0000${taskId}`)) {
      host.taskThoughtCacheLoading.delete(key);
    }
  }
};

const getTaskThoughtCache = async (
  host: SessionSupervisorThoughtCacheHost,
  ownerScope: WorkspaceOwnerScope,
  taskId: string,
): Promise<PersistedTaskThoughtsV1> => {
  const cacheKey = taskThoughtOwnerKey(ownerScope, taskId);
  const cached = host.taskThoughtCache.get(cacheKey);
  if (cached) return cached;
  const inflight = host.taskThoughtCacheLoading.get(cacheKey);
  if (inflight) return inflight;
  const loader = (async () => {
    const existing = await loadTaskThoughtsV1(ownerScope, taskId);
    return (
      existing ?? {
        v: 1,
        taskId,
        sessions: {},
        updatedAtMs: Date.now(),
      }
    );
  })();
  host.taskThoughtCacheLoading.set(cacheKey, loader);
  try {
    const resolved = await loader;
    host.taskThoughtCache.set(cacheKey, resolved);
    return resolved;
  } finally {
    host.taskThoughtCacheLoading.delete(cacheKey);
  }
};

export function resolveWorkspaceOwnerScope(
  workspaceId: string | null | undefined,
): WorkspaceOwnerScope | null {
  const normalizedWorkspaceId = idToString(workspaceId);
  const daemonTargetScope = getDaemonConnection().targetScope ?? null;
  if (!normalizedWorkspaceId || !daemonTargetScope) return null;
  return createWorkspaceOwnerScope(daemonTargetScope, normalizedWorkspaceId);
}

export function resolveEntryWorkspaceOwnerScope(
  this: SessionSupervisorThoughtCacheHost,
  entry: SessionSupervisorThoughtCacheEntry,
): WorkspaceOwnerScope | null {
  return this.resolveWorkspaceOwnerScope(
    idToString(entry.session?.workspace_id) || this.workspaceSnapshotState?.workspaceId || "",
  );
}

export async function ensureThoughtCache(
  this: SessionSupervisorThoughtCacheHost,
  entry: SessionSupervisorThoughtCacheEntry,
) {
  const taskId = idToString(entry.session?.task_id);
  if (!taskId) return;
  const ownerScope = this.resolveEntryWorkspaceOwnerScope(entry);
  if (!ownerScope) return;
  const cacheKey = taskThoughtOwnerKey(ownerScope, taskId);
  if (entry.thoughtCacheLoaded && entry.thoughtCacheOwnerTaskKey === cacheKey) {
    const overlayed = this.overlayThoughtCacheOnEvents(entry, entry.events);
    if (overlayed !== entry.events) {
      entry.events = overlayed;
      this.bumpEventsRev(entry);
      entry.seqSet = new Set(
        overlayed
          .map((ev) => (typeof ev.seq === "number" ? ev.seq : Number.NaN))
          .filter((seq) => Number.isFinite(seq)) as number[],
      );
      entry.updatedAtMs = Date.now();
      this.publish();
    }
    if (entry.thoughtCacheDirty) {
      void this.persistThoughtCache(entry);
    }
    return;
  }
  if (entry.thoughtCacheLoading) return;
  entry.thoughtCacheLoading = true;
  const token = (entry.thoughtCacheLoadToken += 1);
  try {
    const cache = await getTaskThoughtCache(this, ownerScope, taskId);
    if (entry.thoughtCacheLoadToken !== token) return;
    const sessionCache = cache.sessions?.[entry.sessionId]?.thoughts ?? {};
    entry.thoughtCacheByKey = {
      ...sessionCache,
      ...entry.thoughtCacheByKey,
    };
    entry.thoughtCacheLoaded = true;
    entry.thoughtCacheOwnerTaskKey = cacheKey;
    const overlayed = this.overlayThoughtCacheOnEvents(entry, entry.events);
    if (overlayed !== entry.events) {
      entry.events = overlayed;
      this.bumpEventsRev(entry);
      entry.seqSet = new Set(
        overlayed
          .map((ev) => (typeof ev.seq === "number" ? ev.seq : Number.NaN))
          .filter((seq) => Number.isFinite(seq)) as number[],
      );
      entry.updatedAtMs = Date.now();
    }
    if (entry.thoughtCacheDirty) {
      void this.persistThoughtCache(entry);
    }
    this.publish();
  } finally {
    entry.thoughtCacheLoading = false;
  }
}

export async function persistThoughtCache(
  this: SessionSupervisorThoughtCacheHost,
  entry: SessionSupervisorThoughtCacheEntry,
) {
  if (!entry.thoughtCacheDirty) return;
  const taskId = idToString(entry.session?.task_id);
  if (!taskId) return;
  const ownerScope = this.resolveEntryWorkspaceOwnerScope(entry);
  if (!ownerScope) return;
  const cacheKey = taskThoughtOwnerKey(ownerScope, taskId);
  const cache = await getTaskThoughtCache(this, ownerScope, taskId);
  const existingSession = cache.sessions?.[entry.sessionId];
  const mergedThoughts = {
    ...(existingSession?.thoughts ?? {}),
    ...entry.thoughtCacheByKey,
  };
  cache.sessions = {
    ...cache.sessions,
    [entry.sessionId]: {
      sessionId: entry.sessionId,
      thoughts: mergedThoughts,
    },
  };
  cache.updatedAtMs = Date.now();
  this.taskThoughtCache.set(cacheKey, cache);
  entry.thoughtCacheDirty = false;
  await saveTaskThoughtsV1(ownerScope, taskId, { sessions: cache.sessions });
}

export async function clearTaskThoughts(
  this: SessionSupervisorThoughtCacheHost,
  taskId: string,
) {
  clearTaskThoughtCachesForTask(this, taskId);
  const ownerScope = this.resolveWorkspaceOwnerScope(this.workspaceSnapshotState?.workspaceId ?? "");
  if (ownerScope) {
    await clearTaskThoughtsV1(ownerScope, taskId);
  }
  let changed = false;
  for (const entry of this.entries.values()) {
    if (idToString(entry.session?.task_id) !== taskId) continue;
    if (Object.keys(entry.thoughtCacheByKey).length > 0) {
      entry.thoughtCacheByKey = {};
      entry.thoughtCacheDirty = false;
      entry.thoughtCacheLoaded = true;
      changed = true;
    }
    entry.thoughtCacheOwnerTaskKey = undefined;
    if (entry.turns.length > 0) {
      const nextTurns = entry.turns.map((turn) => {
        const current = String(turn.thought_partial ?? "");
        if (!current.trim()) return turn;
        changed = true;
        return { ...turn, thought_partial: "" };
      });
      entry.turns = nextTurns;
      this.bumpTurnsRev(entry);
    }
    if (entry.events.length > 0) {
      const nextEvents = entry.events.filter((ev) => ev.event_type !== "thought_chunk");
      if (nextEvents.length !== entry.events.length) {
        entry.events = nextEvents;
        this.bumpEventsRev(entry);
        entry.seqSet = new Set(
          nextEvents
            .map((ev) => (typeof ev.seq === "number" ? ev.seq : Number.NaN))
            .filter((seq) => Number.isFinite(seq)) as number[],
        );
        changed = true;
      }
    }
  }
  if (changed) {
    this.publish();
  }
}

export function overlayThoughtCacheOnEvents(
  entry: SessionSupervisorThoughtCacheEntry,
  events: SessionEvent[],
): SessionEvent[] {
  if (!entry.thoughtCacheLoaded) return events;
  const cache = entry.thoughtCacheByKey;
  if (!cache || Object.keys(cache).length === 0) return events;
  const bySeq = new Map<number, SessionEvent>();
  for (const ev of events) {
    if (typeof ev.seq === "number") bySeq.set(ev.seq, ev);
  }
  let changed = false;
  for (const cached of Object.values(cache)) {
    const ev = cached.event;
    if (!ev || typeof ev.seq !== "number") continue;
    if (bySeq.has(ev.seq)) continue;
    bySeq.set(ev.seq, ev);
    changed = true;
  }
  if (!changed) return events;
  return Array.from(bySeq.values()).sort((a, b) => Number(a.seq ?? 0) - Number(b.seq ?? 0));
}

export function overlayThoughtCacheOnTurns(
  entry: SessionSupervisorThoughtCacheEntry,
  turns: SessionTurn[],
): SessionTurn[] {
  if (!entry.thoughtCacheLoaded) return turns;
  const cache = entry.thoughtCacheByKey;
  const keys = cache ? Object.keys(cache) : [];
  if (keys.length === 0) return turns;
  const turnIdsWithThoughts = new Set(keys.map((key) => key.split("|")[0]));
  let changed = false;
  const next = turns.map((turn) => {
    const turnId = idToString(turn.turn_id);
    if (!turnId || !turnIdsWithThoughts.has(turnId)) return turn;
    const current = String(turn.thought_partial ?? "");
    if (!current.trim()) return turn;
    changed = true;
    return { ...turn, thought_partial: "" };
  });
  return changed ? next : turns;
}
