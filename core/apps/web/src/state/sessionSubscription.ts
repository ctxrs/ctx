export type SessionSubscriptionCursor = {
  sessionId: string;
  intent?: SessionSubscriptionIntent;
  replay: SessionSubscriptionReplay;
};

export type SessionSubscriptionIntent = "head" | "replay";

export type SessionSubscriptionReplay =
  | {
      kind: "auto";
    }
  | {
      kind: "reset";
    }
  | {
      kind: "resume";
      afterSeq: number;
      afterProjectionRev?: number;
    };

const normalizeSessionId = (value: unknown): string => {
  if (typeof value !== "string") return "";
  return value.trim();
};

const AUTO_REPLAY: SessionSubscriptionReplay = { kind: "auto" };
const RESET_REPLAY: SessionSubscriptionReplay = { kind: "reset" };

const normalizeIntent = (value: SessionSubscriptionIntent | null | undefined): SessionSubscriptionIntent =>
  value === "head" ? "head" : "replay";

const normalizeReplay = (
  value: SessionSubscriptionReplay | null | undefined,
): SessionSubscriptionReplay => {
  switch (value?.kind) {
    case "reset":
      return RESET_REPLAY;
    case "resume":
      return typeof value.afterSeq === "number" && Number.isFinite(value.afterSeq)
        ? {
            kind: "resume",
            afterSeq: value.afterSeq,
            ...(typeof value.afterProjectionRev === "number" && Number.isFinite(value.afterProjectionRev)
              ? { afterProjectionRev: value.afterProjectionRev }
              : {}),
          }
        : AUTO_REPLAY;
    default:
      return AUTO_REPLAY;
  }
};

const compareResumeReplay = (
  left: Extract<SessionSubscriptionReplay, { kind: "resume" }>,
  right: Extract<SessionSubscriptionReplay, { kind: "resume" }>,
): number => {
  if (left.afterSeq !== right.afterSeq) {
    return left.afterSeq - right.afterSeq;
  }
  return (left.afterProjectionRev ?? 0) - (right.afterProjectionRev ?? 0);
};

const mergeReplay = (
  left: SessionSubscriptionReplay,
  right: SessionSubscriptionReplay,
): SessionSubscriptionReplay => {
  if (left.kind === "reset" || right.kind === "reset") {
    return RESET_REPLAY;
  }
  if (left.kind === "resume" && right.kind === "resume") {
    return compareResumeReplay(left, right) >= 0 ? left : right;
  }
  if (left.kind === "resume") {
    return left;
  }
  if (right.kind === "resume") {
    return right;
  }
  return AUTO_REPLAY;
};

const mergeIntent = (
  left: SessionSubscriptionIntent,
  right: SessionSubscriptionIntent,
): SessionSubscriptionIntent => (left === "replay" || right === "replay" ? "replay" : "head");

const sameReplay = (
  left: SessionSubscriptionReplay | null | undefined,
  right: SessionSubscriptionReplay | null | undefined,
): boolean => {
  if (left?.kind !== right?.kind) return false;
  if (left?.kind === "resume" && right?.kind === "resume") {
    return (
      left.afterSeq === right.afterSeq &&
      (left.afterProjectionRev ?? 0) === (right.afterProjectionRev ?? 0)
    );
  }
  return true;
};

export function normalizeSessionSubscriptionCursors(
  sessions: SessionSubscriptionCursor[],
): SessionSubscriptionCursor[] {
  const ordered = new Map<string, SessionSubscriptionCursor>();
  for (const session of sessions) {
    const sessionId = normalizeSessionId(session.sessionId);
    if (!sessionId) continue;
    const replay = normalizeReplay(session.replay);
    const intent = replay.kind === "reset" ? "replay" : normalizeIntent(session.intent);
    const previous = ordered.get(sessionId);
    if (!previous) {
      ordered.set(sessionId, { sessionId, replay, intent });
      continue;
    }
    const mergedReplay = mergeReplay(previous.replay, replay);
    ordered.set(sessionId, {
      sessionId,
      replay: mergedReplay,
      intent:
        mergedReplay.kind === "reset"
          ? "replay"
          : mergeIntent(normalizeIntent(previous.intent), intent),
    });
  }
  return Array.from(ordered.values());
}

export function sameSessionSubscriptionCursorIds(
  left: SessionSubscriptionCursor[],
  right: SessionSubscriptionCursor[],
): boolean {
  if (left.length !== right.length) return false;
  for (let i = 0; i < left.length; i += 1) {
    if (left[i]?.sessionId !== right[i]?.sessionId) return false;
  }
  return true;
}

export function sameSessionSubscriptionCursors(
  left: SessionSubscriptionCursor[],
  right: SessionSubscriptionCursor[],
): boolean {
  if (left.length !== right.length) return false;
  for (let i = 0; i < left.length; i += 1) {
    if (left[i]?.sessionId !== right[i]?.sessionId) return false;
    if (normalizeIntent(left[i]?.intent) !== normalizeIntent(right[i]?.intent)) return false;
    if (!sameReplay(left[i]?.replay, right[i]?.replay)) return false;
  }
  return true;
}
