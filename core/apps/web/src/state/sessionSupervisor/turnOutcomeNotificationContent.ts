import { idToString, type Message, type Session, type SessionTurn } from "../../api/client";
import { markdownToPlainText } from "../../utils/markdownPlainText";
import type { SessionSupervisorWorkspaceSnapshotState } from "./workspaceInputs";
import { readPayloadString } from "./eventNormalization";

const NOTIFICATION_BODY_MAX_CHARS = 140;
const FALLBACK_NOTIFICATION_TITLE = "Task update";

const collapseNotificationWhitespace = (value: string): string =>
  value.replace(/\s+/g, " ").trim();

const truncateNotificationText = (value: string, maxChars: number): string => {
  if (value.length <= maxChars) return value;
  const limit = Math.max(1, maxChars - 1);
  let truncated = value.slice(0, limit).trimEnd();
  const lastSpace = truncated.lastIndexOf(" ");
  if (lastSpace >= Math.floor(limit * 0.6)) {
    truncated = truncated.slice(0, lastSpace).trimEnd();
  }
  return `${truncated}\u2026`;
};

export function buildTurnOutcomeNotificationBodyPreview(content: string): string | undefined {
  const plainText = collapseNotificationWhitespace(markdownToPlainText(content));
  if (!plainText) return undefined;
  return truncateNotificationText(plainText, NOTIFICATION_BODY_MAX_CHARS);
}

export function resolveTurnOutcomeNotificationTitle({
  session,
  workspaceSnapshotState,
}: {
  session: Session | null | undefined;
  workspaceSnapshotState: SessionSupervisorWorkspaceSnapshotState;
}): string | undefined {
  const taskId = idToString(session?.task_id ?? "");
  if (taskId) {
    const taskTitle = String(workspaceSnapshotState?.tasksById?.[taskId]?.task.title ?? "").trim();
    if (taskTitle) return taskTitle;
  }
  return FALLBACK_NOTIFICATION_TITLE;
}

export function resolveTurnOutcomeNotificationBody({
  messages,
  status,
  turn,
  turnId,
}: {
  messages: readonly Message[];
  status: SessionTurn["status"] | null | undefined;
  turn?: SessionTurn | null;
  turnId?: string | null;
}): string | undefined {
  const normalizedTurnId = idToString(turnId ?? "");
  if (!normalizedTurnId) return undefined;
  if (status !== "completed" && status !== "failed") return undefined;
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (!message || message.role !== "assistant") continue;
    if (message.delivery === "queued") continue;
    if (idToString(message.turn_id ?? "") !== normalizedTurnId) continue;
    const preview = buildTurnOutcomeNotificationBodyPreview(message.content);
    if (preview) return preview;
  }
  if (status !== "failed") return undefined;
  const failureMessage =
    readPayloadString(turn?.failure, ["message", "details", "error", "kind"]) ?? null;
  if (failureMessage) return buildTurnOutcomeNotificationBodyPreview(failureMessage);
  return undefined;
}
