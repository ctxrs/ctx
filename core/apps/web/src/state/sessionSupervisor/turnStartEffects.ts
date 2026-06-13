import { idToString, type SessionTurn } from "../../api/client";
import { trackTurnStarted } from "../../utils/analytics";
import { markTurnStartedTracked } from "../../utils/analytics/turnStartDedup";
import type { AnalyticsSessionKind } from "../../utils/analytics/types";
import type { ExecutionEnvironment } from "@ctx/types";

type TurnStartEffectInput = {
  sessionId: string;
  turnId?: string;
  providerId?: string;
  modelId?: string;
  reasoningEffort?: string;
  executionEnvironment?: ExecutionEnvironment;
  sessionKind?: AnalyticsSessionKind;
  previousStatus?: SessionTurn["status"];
  nextStatus?: SessionTurn["status"];
};

type ReplayTurnStartEffectsInput = {
  sessionId: string;
  providerId?: string;
  modelId?: string;
  reasoningEffort?: string;
  executionEnvironment?: ExecutionEnvironment;
  sessionKind?: AnalyticsSessionKind;
  previousTurns: SessionTurn[];
  nextTurns: SessionTurn[];
};

const hasStarted = (status: SessionTurn["status"] | undefined): boolean =>
  status === "running"
  || status === "completed"
  || status === "failed"
  || status === "interrupted";

export const shouldTrackTurnStart = (
  _previousStatus: SessionTurn["status"] | undefined,
  nextStatus: SessionTurn["status"] | undefined,
): boolean => hasStarted(nextStatus);

export const applyTurnStartEffects = ({
  sessionId,
  turnId,
  providerId,
  modelId,
  reasoningEffort,
  executionEnvironment,
  sessionKind,
  previousStatus,
  nextStatus,
}: TurnStartEffectInput): void => {
  if (!shouldTrackTurnStart(previousStatus, nextStatus)) return;
  if (turnId && !markTurnStartedTracked(sessionId, turnId)) return;
  trackTurnStarted({
    providerId,
    modelId,
    reasoningEffort,
    executionEnvironment,
    sessionKind,
  });
};

export const replayTurnStartEffectsFromTurns = ({
  sessionId,
  providerId,
  modelId,
  reasoningEffort,
  executionEnvironment,
  sessionKind,
  previousTurns,
  nextTurns,
}: ReplayTurnStartEffectsInput): void => {
  const normalizedSessionId = sessionId.trim();
  if (!normalizedSessionId || nextTurns.length === 0) return;

  const previousStatusesByTurnId = new Map<string, SessionTurn["status"]>();
  for (const turn of previousTurns) {
    const turnId = idToString(turn.turn_id);
    if (!turnId) continue;
    previousStatusesByTurnId.set(turnId, turn.status);
  }

  for (const turn of nextTurns) {
    const turnId = idToString(turn.turn_id);
    if (!turnId) continue;
    applyTurnStartEffects({
      sessionId: normalizedSessionId,
      turnId,
      providerId,
      modelId,
      reasoningEffort,
      executionEnvironment,
      sessionKind,
      previousStatus: previousStatusesByTurnId.get(turnId),
      nextStatus: turn.status,
    });
  }
};
