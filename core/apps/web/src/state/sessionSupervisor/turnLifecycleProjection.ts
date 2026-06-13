import type { SessionEvent, SessionTurn } from "../../api/client";
import { readTurnStatusFromPayload } from "./toolStateProjection";

export const isTerminalTurnStatus = (
  status: SessionTurn["status"] | null | undefined,
): status is Extract<SessionTurn["status"], "completed" | "failed" | "interrupted"> =>
  status === "completed" || status === "failed" || status === "interrupted";

export const mergeOrderedTurnStatus = (
  previous: SessionTurn["status"] | null | undefined,
  next: SessionTurn["status"] | null | undefined,
): SessionTurn["status"] => {
  if (previous === "failed" && next === "interrupted") {
    return "interrupted";
  }
  if (isTerminalTurnStatus(previous)) {
    return previous;
  }
  if (!previous) return next ?? "queued";
  return next ?? previous;
};

export const resolveTurnStatusFromLifecycleEvent = (
  previousStatus: SessionTurn["status"] | null | undefined,
  event: SessionEvent,
): SessionTurn["status"] | null => {
  switch (String(event.event_type)) {
    case "turn_queued":
      return mergeOrderedTurnStatus(previousStatus, "queued");
    case "turn_started":
      return mergeOrderedTurnStatus(previousStatus, "running");
    case "turn_finished": {
      const payloadStatus = readTurnStatusFromPayload(event);
      if (payloadStatus) {
        return mergeOrderedTurnStatus(previousStatus, payloadStatus);
      }
      if (previousStatus === "interrupted" || previousStatus === "failed") {
        return previousStatus;
      }
      return mergeOrderedTurnStatus(previousStatus, "completed");
    }
    case "turn_interrupted":
      return mergeOrderedTurnStatus(previousStatus, "interrupted");
    case "done":
      return mergeOrderedTurnStatus(previousStatus, "completed");
    default:
      return null;
  }
};

const asRecord = (value: unknown): Record<string, unknown> =>
  value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};

const readNonEmptyString = (value: unknown): string | undefined => {
  if (typeof value !== "string") return undefined;
  const text = value.trim();
  return text.length > 0 ? text : undefined;
};

export const resolveTurnFailureFromLifecycleEvent = (
  event: SessionEvent,
): SessionTurn["failure"] | null => {
  if (event.event_type !== "turn_finished") return null;
  if (readTurnStatusFromPayload(event) !== "failed") return null;

  const payload = asRecord(event.payload_json);
  const details = payload.details;
  const hasDetails = details !== null && details !== undefined;
  const failure = {
    message:
      readNonEmptyString(payload.message) ??
      readNonEmptyString(payload.error) ??
      readNonEmptyString(payload.reason),
    details: hasDetails ? details : undefined,
    kind: readNonEmptyString(payload.kind),
    reason: readNonEmptyString(payload.reason),
    provider: readNonEmptyString(payload.provider),
    provider_id:
      readNonEmptyString(payload.provider_id) ??
      readNonEmptyString(payload.providerId),
  };

  if (
    !failure.message &&
    !hasDetails &&
    !failure.kind &&
    !failure.reason &&
    !failure.provider &&
    !failure.provider_id
  ) {
    return null;
  }

  return failure;
};
