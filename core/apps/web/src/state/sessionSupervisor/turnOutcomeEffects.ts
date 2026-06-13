import { idToString, type Message, type Session, type SessionEvent, type SessionTurn } from "../../api/client";
import { sendDesktopNotification } from "../../utils/desktopNotifications";
import { isAppInForeground } from "../../utils/windowFocus";
import {
  normalizeTurnFailureKind,
  trackFirstTurnCompleted,
  trackProviderRunCompleted,
  trackTurnCompleted,
} from "../../utils/analytics";
import { markTurnOutcomeTracked } from "../../utils/analytics/turnOutcomeDedup";
import type { AnalyticsSessionKind } from "../../utils/analytics/types";
import { getClientSettingsState } from "../clientSettings";
import type { ExecutionEnvironment } from "@ctx/types";
import { resolveTurnOutcomeNotificationBody, resolveTurnOutcomeNotificationTitle } from "./turnOutcomeNotificationContent";
import type { SessionSupervisorWorkspaceSnapshotState } from "./workspaceInputs";
import { isTerminalTurnStatus } from "./turnLifecycleProjection";

type TerminalTurnStatus = Extract<SessionTurn["status"], "completed" | "failed" | "interrupted">;
type NotifiableTurnStatus = Extract<SessionTurn["status"], "completed" | "failed">;

const isNotifiableTurnStatus = (status: SessionTurn["status"] | undefined): status is NotifiableTurnStatus =>
  status === "completed" || status === "failed";

type TurnOutcomeEffectInput = {
  sessionId: string;
  taskId?: string;
  workspaceId?: string;
  turnId?: string;
  providerId?: string;
  modelId?: string;
  reasoningEffort?: string;
  executionEnvironment?: ExecutionEnvironment;
  sessionKind?: AnalyticsSessionKind;
  startedAt?: string;
  completedAt?: string;
  metrics?: unknown;
  failure?: SessionTurn["failure"];
  title?: string;
  notificationBody?: string;
  notificationTitle?: string;
  previousStatus?: SessionTurn["status"];
  nextStatus?: SessionTurn["status"];
  notify: boolean;
};

type ReplayTurnOutcomeEffectsInput = {
  sessionId: string;
  taskId?: string;
  workspaceId?: string;
  providerId?: string;
  modelId?: string;
  reasoningEffort?: string;
  executionEnvironment?: ExecutionEnvironment;
  sessionKind?: AnalyticsSessionKind;
  notify: boolean;
  session?: Session | null;
  workspaceSnapshotState: SessionSupervisorWorkspaceSnapshotState;
  events: readonly SessionEvent[];
  messages: readonly Message[];
  previousTurns: SessionTurn[];
  nextTurns: SessionTurn[];
};

const parseTimestampMs = (value: string | undefined): number | null => {
  if (!value) return null;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) ? parsed : null;
};

export const shouldTrackTurnOutcome = (
  previousStatus: SessionTurn["status"] | undefined,
  nextStatus: SessionTurn["status"] | undefined,
): nextStatus is TerminalTurnStatus => {
  if (!isTerminalTurnStatus(nextStatus)) return false;
  return nextStatus !== previousStatus;
};

export const shouldNotifyTurnOutcome = (
  previousStatus: SessionTurn["status"] | undefined,
  nextStatus: SessionTurn["status"] | undefined,
): nextStatus is NotifiableTurnStatus => {
  if (!isNotifiableTurnStatus(nextStatus)) return false;
  return nextStatus !== previousStatus;
};

const isNotificationEnabledForStatus = (status: NotifiableTurnStatus): boolean => {
  const state = getClientSettingsState();
  if (!state.loaded) return false;
  const settings = state.settings.desktopNotifications;
  if (status === "completed") return settings.turnCompleted;
  return settings.turnFailed;
};

const notificationTitleForStatus = (status: NotifiableTurnStatus): string =>
  status === "completed" ? "Turn completed" : "Turn failed";

const notificationKindForStatus = (status: NotifiableTurnStatus): "turn_completed" | "turn_failed" =>
  status === "completed" ? "turn_completed" : "turn_failed";

const turnFailureSignal = (
  status: TerminalTurnStatus,
  failure: SessionTurn["failure"] | undefined,
) => {
  if (status === "completed") return undefined;
  return normalizeTurnFailureKind(
    failure?.kind ?? failure?.reason ?? failure?.provider ?? failure?.provider_id,
    status,
  );
};

export const applyTurnOutcomeEffects = ({
  sessionId,
  taskId,
  workspaceId,
  turnId,
  providerId,
  modelId,
  reasoningEffort,
  executionEnvironment,
  sessionKind,
  startedAt,
  completedAt,
  metrics,
  failure,
  notificationBody,
  notificationTitle,
  previousStatus,
  nextStatus,
  notify,
}: TurnOutcomeEffectInput): void => {
  const startedAtMs = parseTimestampMs(startedAt);
  const completedAtMs = parseTimestampMs(completedAt);
  const durationMs = startedAtMs !== null && completedAtMs !== null && completedAtMs >= startedAtMs
    ? completedAtMs - startedAtMs
    : undefined;
  const failureKind = nextStatus && isTerminalTurnStatus(nextStatus)
    ? turnFailureSignal(nextStatus, failure)
    : undefined;
  if (
    shouldTrackTurnOutcome(previousStatus, nextStatus)
      && (!turnId || markTurnOutcomeTracked(sessionId, turnId, nextStatus))
  ) {
    trackTurnCompleted({
      providerId,
      modelId,
      reasoningEffort,
      executionEnvironment,
      status: nextStatus,
      durationMs,
      sessionKind,
      metrics,
      failureKind,
    });
    trackProviderRunCompleted({
      providerId,
      modelId,
      status: nextStatus,
      durationMs,
      sessionKind,
      failureKind,
    });
    trackFirstTurnCompleted({
      sessionId,
      providerId,
      status: nextStatus,
      sessionKind,
    });
  }
  if (!notify) return;
  if (sessionKind !== "primary") return;
  if (!shouldNotifyTurnOutcome(previousStatus, nextStatus)) return;
  if (isAppInForeground()) return;
  if (!workspaceId || !taskId) return;
  if (!isNotificationEnabledForStatus(nextStatus)) return;
  const resolvedNotificationTitle =
    String(notificationTitle ?? "").trim() || notificationTitleForStatus(nextStatus);
  const resolvedNotificationBody = String(notificationBody ?? "").trim() || undefined;
  void sendDesktopNotification({
    kind: notificationKindForStatus(nextStatus),
    title: resolvedNotificationTitle,
    body: resolvedNotificationBody,
    workspaceId,
    taskId,
    sessionId: idToString(sessionId),
  });
};

export const replayTurnOutcomeEffectsFromTurns = ({
  sessionId,
  taskId,
  workspaceId,
  providerId,
  modelId,
  reasoningEffort,
  executionEnvironment,
  sessionKind,
  notify,
  session,
  workspaceSnapshotState,
  events,
  messages,
  previousTurns,
  nextTurns,
}: ReplayTurnOutcomeEffectsInput): void => {
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
    const notificationTitle = resolveTurnOutcomeNotificationTitle({
      session,
      workspaceSnapshotState,
    });
    const notificationBody = resolveTurnOutcomeNotificationBody({
      messages,
      status: turn.status,
      turn,
      turnId,
    });
    applyTurnOutcomeEffects({
      notify,
      sessionId: normalizedSessionId,
      taskId,
      workspaceId,
      turnId,
      providerId,
      modelId,
      reasoningEffort,
      executionEnvironment,
      sessionKind,
      startedAt: turn.started_at,
      completedAt: turn.updated_at,
      metrics: turn.metrics_json,
      failure: turn.failure,
      notificationBody,
      notificationTitle,
      previousStatus: previousStatusesByTurnId.get(turnId),
      nextStatus: turn.status,
    });
  }
};
