import type {
  Session,
  SessionHeadSnapshot,
  SessionSnapshotSummary,
  SessionSummary,
  SessionTurnStatus,
  Task,
  WorkspaceActiveSnapshotSessionSummaryDelta,
} from "@ctx/types";
import { idToString } from "../../api/client";
import { compactActiveSessionHeadSnapshot } from "../sessionHeadState";
import { asRecord, hasOwnProperty, readString } from "./projection";

type SessionSummaryVersion = {
  lastEventSeq: number | null;
  projectionRev: number | null;
  stateRev: number | null;
};

const readVersionNumber = (value: number | null | undefined): number | null =>
  typeof value === "number" && Number.isFinite(value) ? value : null;

const WORKING_TURN_STATUSES = new Set<SessionTurnStatus>(["queued", "starting", "running"]);

const isWorkingTurnStatus = (status: SessionTurnStatus | null | undefined): boolean =>
  status ? WORKING_TURN_STATUSES.has(status) : false;

export const isSessionSummaryWorking = (
  summary: SessionSnapshotSummary | null | undefined,
): boolean => {
  const activity = summary?.activity ?? null;
  if (!activity) return false;
  if (activity.is_working) return true;
  return isWorkingTurnStatus(activity.last_turn_status);
};

const isSessionHeadActivityCompatible = (
  summary: SessionSnapshotSummary,
  head: SessionHeadSnapshot,
): boolean => {
  const summaryActivity = summary.activity ?? null;
  if (!summaryActivity) return true;
  const latestHeadTurnStatus = (head.turns ?? []).at(-1)?.status ?? null;
  const headActivity = head.activity ?? null;
  const headStatus = headActivity?.last_turn_status ?? latestHeadTurnStatus;
  const headWorking =
    typeof headActivity?.is_working === "boolean"
      ? headActivity.is_working
      : isWorkingTurnStatus(latestHeadTurnStatus);
  if (!summaryActivity.is_working && headWorking) {
    return false;
  }
  const summaryStatus = summaryActivity.last_turn_status ?? null;
  if (summaryStatus && headStatus && summaryStatus !== headStatus) {
    return false;
  }
  return true;
};

const hasSessionSummaryVersion = (value: SessionSummaryVersion): boolean =>
  value.lastEventSeq !== null || value.projectionRev !== null || value.stateRev !== null;

const compareSessionSummaryVersion = (
  left: SessionSummaryVersion,
  right: SessionSummaryVersion,
): number => {
  const fields: Array<keyof SessionSummaryVersion> = ["lastEventSeq", "projectionRev", "stateRev"];
  for (const field of fields) {
    const leftValue = left[field];
    const rightValue = right[field];
    if (leftValue === null || rightValue === null || leftValue === rightValue) continue;
    return leftValue - rightValue;
  }
  return 0;
};

export function taskSortAt(task: Task, fallback?: string | null): string {
  if (task.archived_at) return task.archived_at;
  if (task.created_at) return task.created_at;
  if (fallback) return fallback;
  return task.updated_at ?? "";
}

export function pickArchivedSessionId(task: Task, sessions: Session[]): string | null {
  const primaryId = idToString(task.primary_session_id ?? "");
  if (primaryId && (sessions.length === 0 || sessions.some((session) => idToString(session.id) === primaryId))) {
    return primaryId;
  }
  const nonSubagents = sessions.filter((session) => session.relationship !== "sub_agent");
  const pool = nonSubagents.length ? nonSubagents : sessions;
  const selected = pool[0];
  return selected ? idToString(selected.id) : null;
}

export function pickArchivedSessionIdFromSummaries(
  task: Task,
  sessions: SessionSummary[],
): string | null {
  const primaryId = idToString(task.primary_session_id ?? "");
  if (primaryId && (sessions.length === 0 || sessions.some((session) => idToString(session.id) === primaryId))) {
    return primaryId;
  }
  const nonSubagents = sessions.filter((session) => session.relationship !== "sub_agent");
  const pool = nonSubagents.length ? nonSubagents : sessions;
  const selected = pool[0];
  return selected ? idToString(selected.id) : null;
}

export function normalizeSessionSummary(summary: SessionSnapshotSummary): SessionSnapshotSummary {
  return {
    session: { ...summary.session },
    last_message_at: summary.last_message_at ?? null,
    last_message_preview: summary.last_message_preview ?? null,
    last_event_seq: summary.last_event_seq ?? null,
    projection_rev: summary.projection_rev ?? undefined,
    state_rev: summary.state_rev ?? undefined,
    activity: summary.activity ?? { is_working: false, last_turn_status: null },
    unread: summary.unread,
  };
}

export function mergeSessionSummaryDelta(
  current: SessionSnapshotSummary,
  delta: WorkspaceActiveSnapshotSessionSummaryDelta,
): SessionSnapshotSummary | null {
  const nextSummary: SessionSnapshotSummary = { ...current };
  let changed = false;
  const incomingLastEventSeq =
    typeof delta.last_event_seq === "number" ? delta.last_event_seq : null;
  const incomingProjectionRev = readVersionNumber(delta.projection_rev);
  const incomingStateRev = readVersionNumber(delta.state_rev);

  if (hasOwnProperty(delta, "last_message_at")) {
    const incoming = delta.last_message_at;
    if (typeof incoming === "string" && incoming) {
      const currentValue = nextSummary.last_message_at;
      const incMs = Date.parse(incoming);
      const curMs = currentValue ? Date.parse(currentValue) : Number.NaN;
      const shouldUpdate =
        !currentValue ||
        (Number.isFinite(incMs) && Number.isFinite(curMs) ? incMs > curMs : incoming > currentValue);
      if (shouldUpdate && nextSummary.last_message_at !== incoming) {
        nextSummary.last_message_at = incoming;
        changed = true;
      }
    }
  }
  if (hasOwnProperty(delta, "last_message_preview")) {
    const incoming = delta.last_message_preview;
    if (typeof incoming === "string") {
      const nextValue = incoming.length ? incoming : null;
      if (nextSummary.last_message_preview !== nextValue) {
        nextSummary.last_message_preview = nextValue;
        changed = true;
      }
    } else if (incoming === null && nextSummary.last_message_preview !== null) {
      nextSummary.last_message_preview = null;
      changed = true;
    }
  }
  if (hasOwnProperty(delta, "last_event_seq")) {
    const incoming = delta.last_event_seq;
    if (typeof incoming === "number") {
      const nextValue = Math.max(nextSummary.last_event_seq ?? incoming, incoming);
      if (nextSummary.last_event_seq !== nextValue) {
        nextSummary.last_event_seq = nextValue;
        changed = true;
      }
    }
  }
  if (typeof delta.projection_rev === "number") {
    const nextCurrentProjectionRev = nextSummary.projection_rev ?? 0;
    if (delta.projection_rev > nextCurrentProjectionRev) {
      nextSummary.projection_rev = delta.projection_rev;
      changed = true;
    }
  }
  if (typeof delta.state_rev === "number") {
    const nextCurrentStateRev = nextSummary.state_rev ?? 0;
    if (delta.state_rev > nextCurrentStateRev) {
      nextSummary.state_rev = delta.state_rev;
      changed = true;
    }
  }
  if (hasOwnProperty(delta, "activity")) {
    const currentVersion: SessionSummaryVersion = {
      lastEventSeq: readVersionNumber(current.last_event_seq ?? null),
      projectionRev: readVersionNumber(current.projection_rev),
      stateRev: readVersionNumber(current.state_rev),
    };
    const incomingVersion: SessionSummaryVersion = {
      lastEventSeq: incomingLastEventSeq,
      projectionRev: incomingProjectionRev,
      stateRev: incomingStateRev,
    };
    const canUpdateActivity =
      !hasSessionSummaryVersion(currentVersion) ||
      (hasSessionSummaryVersion(incomingVersion) &&
        compareSessionSummaryVersion(incomingVersion, currentVersion) >= 0);
    const nextActivity = delta.activity ?? { is_working: false, last_turn_status: null };
    if (canUpdateActivity) {
      const currentActivity = nextSummary.activity ?? { is_working: false, last_turn_status: null };
      if (
        currentActivity.is_working !== nextActivity.is_working ||
        (currentActivity.last_turn_status ?? null) !== (nextActivity.last_turn_status ?? null)
      ) {
        nextSummary.activity = nextActivity;
        changed = true;
      }
    }
  }

  return changed ? nextSummary : null;
}

export function sessionToSummary(session: Session): SessionSnapshotSummary {
  return normalizeSessionSummary({
    session,
    last_message_at: null,
    last_message_preview: null,
    last_event_seq: null,
    projection_rev: undefined,
    state_rev: undefined,
    activity: { is_working: false, last_turn_status: null },
    unread: undefined,
  });
}

export function shouldReplaceSessionHead(
  prev: SessionHeadSnapshot | null | undefined,
  next: SessionHeadSnapshot,
): boolean {
  if (!prev) return true;
  const prevSeq = typeof prev.last_event_seq === "number" ? prev.last_event_seq : -1;
  const nextSeq = typeof next.last_event_seq === "number" ? next.last_event_seq : -1;
  if (prevSeq < 0 && nextSeq >= 0) return true;
  if (prevSeq >= 0 && nextSeq < 0) return false;
  if (prevSeq >= 0 && nextSeq >= 0) {
    if (nextSeq > prevSeq) return true;
    if (nextSeq < prevSeq) return false;
  }
  const prevProjectionRev = typeof prev.projection_rev === "number" ? prev.projection_rev : -1;
  const nextProjectionRev = typeof next.projection_rev === "number" ? next.projection_rev : -1;
  if (prevProjectionRev >= 0 || nextProjectionRev >= 0) {
    if (nextProjectionRev > prevProjectionRev) return true;
    if (nextProjectionRev < prevProjectionRev) return false;
  }
  const prevStateRev = typeof prev.state_rev === "number" ? prev.state_rev : -1;
  const nextStateRev = typeof next.state_rev === "number" ? next.state_rev : -1;
  if (prevStateRev >= 0 || nextStateRev >= 0) {
    if (nextStateRev > prevStateRev) return true;
    if (nextStateRev < prevStateRev) return false;
  }
  return true;
}

export function isSessionHeadCompatibleWithSummary(
  summary: SessionSnapshotSummary | null | undefined,
  head: SessionHeadSnapshot | null | undefined,
): boolean {
  if (!summary) return true;
  if (!head) return false;

  const summarySessionId = idToString(summary.session?.id ?? "");
  const headSessionId = idToString(head.session?.id ?? "");
  if (summarySessionId && headSessionId && summarySessionId !== headSessionId) {
    return false;
  }

  const summaryLastEventSeq =
    typeof summary.last_event_seq === "number" && summary.last_event_seq >= 0
      ? summary.last_event_seq
      : null;
  const headLastEventSeq =
    typeof head.last_event_seq === "number" && head.last_event_seq >= 0
      ? head.last_event_seq
      : null;

  const summaryProjectionRev =
    typeof summary.projection_rev === "number" ? summary.projection_rev : null;
  const headProjectionRev =
    typeof head.projection_rev === "number" ? head.projection_rev : null;
  const summaryStateRev = typeof summary.state_rev === "number" ? summary.state_rev : null;
  const headStateRev = typeof head.state_rev === "number" ? head.state_rev : null;
  if (
    summaryProjectionRev !== null &&
    headProjectionRev !== null &&
    headProjectionRev < summaryProjectionRev
  ) {
    // Keep a usable head when only the projection cursor advanced but the durable event cursor matches.
    if (summaryLastEventSeq === null || headLastEventSeq === null || headLastEventSeq < summaryLastEventSeq) {
      return false;
    }
  }
  if (
    summaryLastEventSeq !== null &&
    (headLastEventSeq === null || headLastEventSeq < summaryLastEventSeq)
  ) {
    return false;
  }

  const headIsNewerThanSummary =
    compareSessionSummaryVersion(
      {
        lastEventSeq: headLastEventSeq,
        projectionRev: headProjectionRev,
        stateRev: headStateRev,
      },
      {
        lastEventSeq: summaryLastEventSeq,
        projectionRev: summaryProjectionRev,
        stateRev: summaryStateRev,
      },
    ) > 0;
  if (!headIsNewerThanSummary && !isSessionHeadActivityCompatible(summary, head)) {
    return false;
  }

  return true;
}

export function readPrimarySessionHead(summary: unknown): SessionHeadSnapshot | null {
  if (!summary || typeof summary !== "object") return null;
  const rec = summary as Record<string, unknown>;
  const head = rec.primary_session_head ?? rec.primarySessionHead ?? null;
  if (!head || typeof head !== "object") return null;
  return compactActiveSessionHeadSnapshot(head as SessionHeadSnapshot);
}

export function readPrimarySessionId(summary: unknown): string | null {
  const rec = asRecord(summary);
  if (Object.keys(rec).length === 0) return null;
  const fromPrimary = idToString(readString(asRecord(asRecord(rec.primary_session).session).id) ?? "");
  if (fromPrimary) return fromPrimary;
  const fromHead = idToString(
    readString(asRecord(asRecord(rec.primary_session_head).session).id) ??
      readString(asRecord(asRecord(rec.primarySessionHead).session).id) ??
      "",
  );
  if (fromHead) return fromHead;
  return null;
}
