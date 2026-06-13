import type {
  SessionHead,
  SessionHeadSnapshot,
  SessionSnapshotSummary,
  WorkspaceActiveSnapshotEvent,
} from "@ctx/types";
import {
  idToString,
  recordClientCounterMetric,
  recordClientHistogramMetric,
} from "../../api/client";
import { getSessionHead } from "../../api/clientSessions";
import {
  type AuthoritativePrefetchCompletion,
  SessionHeadBootstrapCache,
} from "../../state/sessionHeadBootstrapCache";
import { HEAD_LIMIT, WARM_SESSION_BUDGET } from "../../state/sessionSupervisor/config";
import { loadSessionHeadV1 } from "../../state/uiStateStore";
import {
  isSessionHeadCompatibleWithSummary,
  isSessionSummaryWorking,
  shouldReplaceSessionHead,
} from "../../state/workspaceActiveSnapshot/summaryHelpers";
import {
  type WorkspaceActiveSnapshotEventSource,
  type WorkspaceActiveSnapshotState,
} from "../../state/workspaceActiveSnapshotStore";

type SessionHeadStoreReader = Pick<WorkspaceActiveSnapshotEventSource, "getSessionHeadSnapshot"> & {
  getSessionHeadsSnapshot?: () => Record<string, SessionHeadSnapshot>;
};

export const SESSION_HEAD_PREFETCH_TARGET_LIMIT = Math.max(1, Math.min(WARM_SESSION_BUDGET, 8));
export const SESSION_HEAD_PREFETCH_CONCURRENCY = 2;

type PrefetchControlOptions = {
  maxTargets?: number;
  concurrency?: number;
  shouldContinue?: () => boolean;
  getSnapshot?: () => WorkspaceActiveSnapshotState;
  shouldRetainSessionId?: (sessionId: string) => boolean;
  force?: boolean;
  reason?: SessionHeadPrefetchReason;
};

export type SessionHeadPrefetchReason =
  | "workspace_sync"
  | "summary_repair"
  | "warm_prefetch"
  | "foreground_force"
  | "explicit";

type AuthoritativePrefetchOutcome =
  | "skip_current"
  | "skip_bootstrap_current"
  | "skip_working"
  | "wait_in_flight"
  | "fetch_started"
  | "fetch_success"
  | "fetch_stale"
  | "fetch_missing"
  | "fetch_failed"
  | "canceled"
  | "not_retained"
  | "throttled";

const noteAuthoritativePrefetchOutcome = (
  reason: SessionHeadPrefetchReason,
  outcome: AuthoritativePrefetchOutcome,
  forced: boolean,
): void => {
  recordClientCounterMetric("workbench.session_head_authoritative_prefetch_count", {
    reason,
    outcome,
    forced: forced ? "true" : "false",
  });
};

const noteAuthoritativePrefetchDuration = (
  reason: SessionHeadPrefetchReason,
  outcome: Extract<AuthoritativePrefetchOutcome, "fetch_success" | "fetch_stale" | "fetch_missing" | "fetch_failed" | "canceled" | "not_retained">,
  forced: boolean,
  durationMs: number,
): void => {
  recordClientHistogramMetric("workbench.session_head_authoritative_prefetch_ms", "ms", durationMs, {
    reason,
    outcome,
    forced: forced ? "true" : "false",
  });
};

const nowMs = (): number => {
  if (typeof performance !== "undefined" && typeof performance.now === "function") {
    return (performance.timeOrigin ?? Date.now()) + performance.now();
  }
  return Date.now();
};

export type SessionHeadPrefetchTargetPlan = {
  targetSessionIds: string[];
  foregroundSessionIds: string[];
  warmSessionIds: string[];
};

const uniqueSessionIds = (sessionIds: readonly string[]): string[] => {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const candidate of sessionIds) {
    const sessionId = idToString(candidate);
    if (!sessionId || seen.has(sessionId)) continue;
    seen.add(sessionId);
    out.push(sessionId);
  }
  return out;
};

export const planSessionHeadPrefetchTargets = ({
  foregroundSessionIds = [],
  warmSessionIds = [],
  maxTargets = SESSION_HEAD_PREFETCH_TARGET_LIMIT,
}: {
  foregroundSessionIds?: readonly string[];
  warmSessionIds?: readonly string[];
  maxTargets?: number;
}): SessionHeadPrefetchTargetPlan => {
  const limit = Math.max(1, Math.floor(maxTargets));
  const foreground = uniqueSessionIds(foregroundSessionIds);
  const warm = uniqueSessionIds(warmSessionIds).filter((sessionId) => !foreground.includes(sessionId));
  const targetSessionIds = [...foreground, ...warm].slice(0, limit);
  return {
    targetSessionIds,
    foregroundSessionIds: foreground.filter((sessionId) => targetSessionIds.includes(sessionId)),
    warmSessionIds: warm.filter((sessionId) => targetSessionIds.includes(sessionId)),
  };
};

const collectPrefetchTargetSessionIds = (
  snapshot: WorkspaceActiveSnapshotState,
  sessionIds?: readonly string[],
  maxTargets = SESSION_HEAD_PREFETCH_TARGET_LIMIT,
): string[] => {
  const candidates = sessionIds ?? collectWorkspaceSessionHeadIds(snapshot);
  return planSessionHeadPrefetchTargets({ warmSessionIds: candidates, maxTargets }).targetSessionIds;
};

const runWithConcurrencyLimit = async <T>(
  items: readonly T[],
  concurrency: number,
  worker: (item: T) => Promise<void>,
): Promise<void> => {
  const workerCount = Math.max(1, Math.min(Math.floor(concurrency), items.length));
  let nextIndex = 0;
  await Promise.all(
    Array.from({ length: workerCount }, async () => {
      while (nextIndex < items.length) {
        const item = items[nextIndex];
        nextIndex += 1;
        await worker(item);
      }
    }),
  );
};

const getPrimarySessionIdForTask = (
  snapshot: WorkspaceActiveSnapshotState,
  taskId: string,
): string | null => {
  const item = snapshot.tasksById[taskId];
  if (!item) return null;

  return (
    item.primarySessionId ||
    idToString(item.task.primary_session_id ?? "") ||
    idToString(item.primarySessionHead?.session?.id ?? "")
  );
};

export const collectWorkspaceSessionHeadIds = (
  snapshot: WorkspaceActiveSnapshotState,
): string[] => {
  const ids = new Set<string>();
  for (const taskId of snapshot.activeIds) {
    const item = snapshot.tasksById[taskId];
    const primaryId = getPrimarySessionIdForTask(snapshot, taskId);
    if (primaryId) ids.add(primaryId);
    for (const summary of item?.sessions ?? []) {
      const sessionId = idToString(summary.session?.id ?? "");
      if (sessionId) ids.add(sessionId);
    }
  }
  return Array.from(ids);
};

const collectTargetSessionIds = (
  snapshot: WorkspaceActiveSnapshotState,
  sessionIds?: readonly string[],
): string[] => {
  return Array.from(new Set((sessionIds ?? collectWorkspaceSessionHeadIds(snapshot)).filter(Boolean)));
};

const findSessionSummary = (
  snapshot: WorkspaceActiveSnapshotState,
  sessionId: string,
): SessionSnapshotSummary | null => {
  const normalizedSessionId = idToString(sessionId);
  if (!normalizedSessionId) return null;
  for (const taskId of snapshot.activeIds) {
    const item = snapshot.tasksById[taskId];
    const summary =
      item?.sessions.find((candidate) => idToString(candidate.session?.id ?? "") === normalizedSessionId) ??
      null;
    if (summary) return summary;
  }
  return null;
};

const buildPrefetchVersionKey = (
  summary: SessionSnapshotSummary | null,
  sessionId: string,
): string => {
  const lastEventSeq =
    typeof summary?.last_event_seq === "number" && Number.isFinite(summary.last_event_seq)
      ? summary.last_event_seq
      : "none";
  const projectionRev =
    typeof summary?.projection_rev === "number" && Number.isFinite(summary.projection_rev)
      ? summary.projection_rev
      : "none";
  const stateRev =
    typeof summary?.state_rev === "number" && Number.isFinite(summary.state_rev)
      ? summary.state_rev
      : "none";
  return `${sessionId}:${lastEventSeq}:${projectionRev}:${stateRev}`;
};

export const buildWorkspaceSyncPrefetchVersionKey = (
  snapshot: WorkspaceActiveSnapshotState,
  sessionIds: readonly string[],
): string => {
  return uniqueSessionIds(sessionIds)
    .map((sessionId) => buildPrefetchVersionKey(findSessionSummary(snapshot, sessionId), sessionId))
    .join("\u001f");
};

export const collectAuthoritativePrefetchReadySessionIds = (
  snapshot: WorkspaceActiveSnapshotState,
  sessionIds: readonly string[],
): string[] => {
  return uniqueSessionIds(sessionIds).filter((sessionId) => {
    const summary = findSessionSummary(snapshot, sessionId);
    return !isSessionSummaryWorking(summary);
  });
};

export const noteWorkspaceSyncPrefetchSuppressed = (
  reason: "unchanged_session_versions" | "working_sessions",
): void => {
  recordClientCounterMetric("workbench.workspace_sync_prefetch_suppressed_count", {
    reason,
  });
};

export const collectSessionHeadsForSupervisor = (
  snapshot: WorkspaceActiveSnapshotState,
  store: SessionHeadStoreReader,
  bootstrapCache: SessionHeadBootstrapCache,
  sessionIds?: readonly string[],
): Record<string, SessionHeadSnapshot> => {
  const out: Record<string, SessionHeadSnapshot> = {};
  const targetSessionIds = collectTargetSessionIds(snapshot, sessionIds);
  const batchHeads = store.getSessionHeadsSnapshot?.() ?? {};
  const bootstrapHeads = bootstrapCache.snapshot();

  for (const sessionId of targetSessionIds) {
    const summary = findSessionSummary(snapshot, sessionId);
    const batchHead = batchHeads[sessionId];
    if (batchHead && isSessionHeadCompatibleWithSummary(summary, batchHead)) {
      out[sessionId] = batchHead;
    }
    const head = store.getSessionHeadSnapshot(sessionId);
    if (head && isSessionHeadCompatibleWithSummary(summary, head) && shouldReplaceSessionHead(out[sessionId], head)) {
      out[sessionId] = head;
    }
  }

  for (const [sessionId, head] of Object.entries(bootstrapHeads)) {
    if (!targetSessionIds.includes(sessionId)) continue;
    const summary = findSessionSummary(snapshot, sessionId);
    if (isSessionHeadCompatibleWithSummary(summary, head) && shouldReplaceSessionHead(out[sessionId], head)) {
      out[sessionId] = head;
    }
  }

  return out;
};

const persistedHeadToSnapshot = (head: SessionHead): SessionHeadSnapshot => {
  const headWithOptionalStateRev = head as SessionHead & { state_rev?: number };
  return {
    ...head,
    state_rev:
      typeof headWithOptionalStateRev.state_rev === "number"
        ? headWithOptionalStateRev.state_rev
        : undefined,
    has_more_history: head.has_more_turns,
    history_cursor: head.has_more_turns ? null : null,
  };
};

export const primePersistedSessionHeads = async (
  snapshot: WorkspaceActiveSnapshotState,
  store: SessionHeadStoreReader,
  bootstrapCache: SessionHeadBootstrapCache,
  sessionIds?: readonly string[],
  opts?: PrefetchControlOptions,
): Promise<boolean> => {
  const batchHeads = store.getSessionHeadsSnapshot?.() ?? {};
  let changed = false;
  const targetSessionIds = collectPrefetchTargetSessionIds(snapshot, sessionIds, opts?.maxTargets);

  await runWithConcurrencyLimit(
    targetSessionIds,
    opts?.concurrency ?? SESSION_HEAD_PREFETCH_CONCURRENCY,
    async (sessionId) => {
      while (true) {
        if (opts?.shouldContinue && !opts.shouldContinue()) return;
        if (opts?.shouldRetainSessionId && !opts.shouldRetainSessionId(sessionId)) return;
        const lease = bootstrapCache.beginPersistedPrefetch(sessionId);
        if (lease.state === "skip") return;
        if (lease.state === "wait") {
          await lease.promise;
          continue;
        }
        let completed = false;
        try {
          const persisted = await loadSessionHeadV1(sessionId).catch(() => null);
          if (opts?.shouldContinue && !opts.shouldContinue()) return;
          if (opts?.shouldRetainSessionId && !opts.shouldRetainSessionId(sessionId)) return;
          completed = true;
          if (!persisted?.head) return;
          const persistedHead = persistedHeadToSnapshot(persisted.head);
          const directHead = batchHeads[sessionId] ?? store.getSessionHeadSnapshot(sessionId);
          if (directHead && !shouldReplaceSessionHead(directHead, persistedHead)) {
            return;
          }
          if (bootstrapCache.upsert(persistedHead)) {
            changed = true;
          }
          return;
        } finally {
          if (!completed) {
            lease.finish(false);
          } else {
            lease.finish(true);
          }
        }
      }
    },
  );

  return changed;
};

export const primeAuthoritativeSessionHeads = async (
  snapshot: WorkspaceActiveSnapshotState,
  store: SessionHeadStoreReader,
  bootstrapCache: SessionHeadBootstrapCache,
  sessionIds?: readonly string[],
  opts?: PrefetchControlOptions & {
    onHead?: (sessionId: string, head: SessionHeadSnapshot) => void;
  },
): Promise<boolean> => {
  const batchHeads = store.getSessionHeadsSnapshot?.() ?? {};
  let changed = false;
  const targetSessionIds = collectPrefetchTargetSessionIds(snapshot, sessionIds, opts?.maxTargets);
  const reason = opts?.reason ?? "explicit";
  const forced = Boolean(opts?.force);

  await runWithConcurrencyLimit(
    targetSessionIds,
    opts?.concurrency ?? SESSION_HEAD_PREFETCH_CONCURRENCY,
    async (sessionId) => {
      while (true) {
        if (opts?.shouldContinue && !opts.shouldContinue()) return;
        if (opts?.shouldRetainSessionId && !opts.shouldRetainSessionId(sessionId)) return;
        const summary = findSessionSummary(snapshot, sessionId);
        if (!opts?.force && isSessionSummaryWorking(summary)) {
          noteAuthoritativePrefetchOutcome(reason, "skip_working", forced);
          return;
        }
        const directHead = batchHeads[sessionId] ?? store.getSessionHeadSnapshot(sessionId);
        if (!opts?.force && isSessionHeadCompatibleWithSummary(summary, directHead)) {
          noteAuthoritativePrefetchOutcome(reason, "skip_current", forced);
          return;
        }
        const bootstrapHead = bootstrapCache.get(sessionId);
        if (!opts?.force && isSessionHeadCompatibleWithSummary(summary, bootstrapHead)) {
          noteAuthoritativePrefetchOutcome(reason, "skip_bootstrap_current", forced);
          return;
        }
        const versionKey = buildPrefetchVersionKey(summary, sessionId);
        const lease = bootstrapCache.beginAuthoritativePrefetch(sessionId, versionKey, {
          force: opts?.force,
        });
        if (lease.state === "skip") {
          noteAuthoritativePrefetchOutcome(reason, "skip_current", forced);
          return;
        }
        if (lease.state === "throttled") {
          noteAuthoritativePrefetchOutcome(reason, "throttled", forced);
          return;
        }
        if (lease.state === "wait") {
          noteAuthoritativePrefetchOutcome(reason, "wait_in_flight", forced);
          const outcome = await lease.promise;
          if (outcome !== "canceled" && outcome !== "not_retained") {
            return;
          }
          continue;
        }
        let completion: AuthoritativePrefetchCompletion = "canceled";
        const startedAtMs = nowMs();
        noteAuthoritativePrefetchOutcome(reason, "fetch_started", forced);
        try {
          const head = await getSessionHead(sessionId, HEAD_LIMIT, true);
          if (opts?.shouldContinue && !opts.shouldContinue()) {
            completion = "canceled";
            noteAuthoritativePrefetchOutcome(reason, "canceled", forced);
            noteAuthoritativePrefetchDuration(reason, "canceled", forced, nowMs() - startedAtMs);
            return;
          }
          if (opts?.shouldRetainSessionId && !opts.shouldRetainSessionId(sessionId)) {
            completion = "not_retained";
            noteAuthoritativePrefetchOutcome(reason, "not_retained", forced);
            noteAuthoritativePrefetchDuration(reason, "not_retained", forced, nowMs() - startedAtMs);
            return;
          }
          if (!head) {
            completion = "missing";
            noteAuthoritativePrefetchOutcome(reason, "fetch_missing", forced);
            noteAuthoritativePrefetchDuration(reason, "fetch_missing", forced, nowMs() - startedAtMs);
            return;
          }
          const latestSummary = findSessionSummary(opts?.getSnapshot?.() ?? snapshot, sessionId);
          if (!isSessionHeadCompatibleWithSummary(latestSummary, head)) {
            completion = "stale";
            noteAuthoritativePrefetchOutcome(reason, "fetch_stale", forced);
            noteAuthoritativePrefetchDuration(reason, "fetch_stale", forced, nowMs() - startedAtMs);
            return;
          }
          completion = "success";
          noteAuthoritativePrefetchOutcome(reason, "fetch_success", forced);
          noteAuthoritativePrefetchDuration(reason, "fetch_success", forced, nowMs() - startedAtMs);
          const didChange = bootstrapCache.upsert(head);
          if (didChange) {
            changed = true;
          }
          opts?.onHead?.(sessionId, head);
          return;
        } catch {
          completion = "failed";
          noteAuthoritativePrefetchOutcome(reason, "fetch_failed", forced);
          noteAuthoritativePrefetchDuration(reason, "fetch_failed", forced, nowMs() - startedAtMs);
          return;
        } finally {
          lease.finish(completion);
        }
      }
    },
  );

  return changed;
};

export const maybeCacheSessionHeadSeed = (
  cache: SessionHeadBootstrapCache,
  evt: WorkspaceActiveSnapshotEvent,
  retainedSessionIds?: ReadonlySet<string>,
): boolean => {
  if (evt.type !== "session_head_seed") return false;
  const head = (evt as { head?: SessionHeadSnapshot }).head;
  const sessionId = idToString(head?.session?.id ?? "");
  if (!sessionId) return false;
  if (retainedSessionIds && !retainedSessionIds.has(sessionId)) return false;
  return cache.upsert(head);
};
