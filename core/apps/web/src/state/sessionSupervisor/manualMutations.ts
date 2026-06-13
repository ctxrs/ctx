import type { Message, Session, SessionTurn } from "../../api/client";
import type { SessionActivityState } from "@ctx/types";
import {
  reconcileActivityFromTurns,
  reconcileLatestTurnInterruptedFromActivity,
} from "./cachePolicy";
import type { InternalEntry } from "./entryState";

type SessionSupervisorMutationHost = {
  ensureEntry(sessionId: string): InternalEntry;
  mergeMessages(entry: InternalEntry, messages: Message[]): void;
  bumpMessagesRev(entry: InternalEntry): void;
  bumpTurnsRev(entry: InternalEntry): void;
  publish(): void;
  replicaDispatch(cmd: { type: "set_session"; session: Session }): void;
};

export const setSupervisorSession = (
  host: SessionSupervisorMutationHost,
  session: Session,
): void => {
  const sessionId = String(session.id ?? "").trim();
  if (!sessionId) return;
  host.ensureEntry(sessionId);
  host.replicaDispatch({ type: "set_session", session });
};

export const setSupervisorSessionActivity = (
  host: SessionSupervisorMutationHost,
  sessionId: string,
  activity: SessionActivityState | null,
): void => {
  const id = String(sessionId || "").trim();
  if (!id) return;
  const entry = host.ensureEntry(id);
  const nextActivity = activity ?? null;
  const normalizedActivity = reconcileActivityFromTurns(nextActivity, entry.turns);
  if (entry.activity === normalizedActivity) return;
  entry.activity = normalizedActivity;
  if (reconcileLatestTurnInterruptedFromActivity(entry.turns, nextActivity)) {
    host.bumpTurnsRev(entry);
    const normalizedAfterInterrupt = reconcileActivityFromTurns(entry.activity, entry.turns);
    if (normalizedAfterInterrupt !== entry.activity) {
      entry.activity = normalizedAfterInterrupt;
    }
  }
  entry.activity = reconcileActivityFromTurns(entry.activity, entry.turns);
  entry.updatedAtMs = Date.now();
  host.publish();
};

export const setSupervisorMessages = (
  host: SessionSupervisorMutationHost,
  sessionId: string,
  messages: Message[],
  opts?: { replace?: boolean },
): void => {
  const id = String(sessionId || "").trim();
  if (!id) return;
  const entry = host.ensureEntry(id);
  if (opts?.replace) {
    entry.messages = [];
    entry.queue = [];
    host.bumpMessagesRev(entry);
  }
  host.mergeMessages(entry, messages);
  entry.updatedAtMs = Date.now();
  host.publish();
};

export const setSupervisorTurns = (
  host: SessionSupervisorMutationHost,
  sessionId: string,
  turns: SessionTurn[],
  opts?: { replace?: boolean },
): void => {
  const id = String(sessionId || "").trim();
  if (!id) return;
  const entry = host.ensureEntry(id);
  if (opts?.replace) {
    entry.turns = [];
    host.bumpTurnsRev(entry);
    if (turns.length === 0) {
      entry.activity = null;
    }
  }
  if (turns.length > 0) {
    const byId = new Map<string, SessionTurn>();
    for (const turn of entry.turns) {
      const turnId = String(turn.turn_id ?? "").trim();
      if (turnId) byId.set(turnId, turn);
    }
    for (const turn of turns) {
      const turnId = String(turn.turn_id ?? "").trim();
      if (turnId) byId.set(turnId, turn);
    }
    const merged = Array.from(byId.values());
    merged.sort((left, right) => {
      const leftSeq = Number(left.start_seq ?? Number.NaN);
      const rightSeq = Number(right.start_seq ?? Number.NaN);
      if (Number.isFinite(leftSeq) && Number.isFinite(rightSeq) && leftSeq !== rightSeq) {
        return leftSeq - rightSeq;
      }
      if (Number.isFinite(leftSeq) && !Number.isFinite(rightSeq)) return -1;
      if (!Number.isFinite(leftSeq) && Number.isFinite(rightSeq)) return 1;
      const leftStart = String(left.started_at ?? "");
      const rightStart = String(right.started_at ?? "");
      if (leftStart !== rightStart) return leftStart.localeCompare(rightStart);
      return String(left.turn_id ?? "").localeCompare(String(right.turn_id ?? ""));
    });
    entry.turns = merged;
    host.bumpTurnsRev(entry);
    entry.turnsHydrated = true;
  }

  const normalizedActivity = reconcileActivityFromTurns(entry.activity, entry.turns);
  if (normalizedActivity !== entry.activity) {
    entry.activity = normalizedActivity;
  }

  entry.updatedAtMs = Date.now();
  host.publish();
};
