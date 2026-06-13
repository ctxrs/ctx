import type {
  WorkspaceActiveSnapshotClientMessage,
  WorkspaceActiveSnapshotSessionIntent,
  WorkspaceActiveSnapshotSessionReplay,
} from "../../api/client";
import type { SessionSubscriptionCursor } from "../sessionSubscription";
import { shouldRequestWorkspaceSnapshot } from "./transport";

const toWorkspaceReplay = (
  replay: SessionSubscriptionCursor["replay"],
): WorkspaceActiveSnapshotSessionReplay => {
  switch (replay.kind) {
    case "reset":
      return { mode: "reset" };
    case "resume":
      return {
        mode: "resume",
        after_seq: replay.afterSeq,
        ...(typeof replay.afterProjectionRev === "number"
          ? { after_projection_rev: replay.afterProjectionRev }
          : {}),
      };
    default:
      return { mode: "auto" };
  }
};

const toWorkspaceIntent = (
  session: SessionSubscriptionCursor,
  foregroundSessionId: string | null,
): WorkspaceActiveSnapshotSessionIntent => {
  if (foregroundSessionId && session.sessionId === foregroundSessionId) {
    return "replay";
  }
  if (session.replay.kind === "reset") {
    return "replay";
  }
  return session.intent === "head" ? "head" : "replay";
};

const replayKey = (replay: WorkspaceActiveSnapshotSessionReplay): string => {
  switch (replay.mode) {
    case "resume":
      return `resume:${replay.after_seq}:${replay.after_projection_rev ?? 0}`;
    default:
      return replay.mode;
  }
};

export function buildWorkspaceActiveSubscribeMessage(
  reason: string,
  foregroundSessionId: string | null,
  subscribedSessions: SessionSubscriptionCursor[],
): {
  message: WorkspaceActiveSnapshotClientMessage;
  requestSnapshot: boolean;
  canonicalKey: string;
} {
  const requestSnapshot = shouldRequestWorkspaceSnapshot(reason);
  const message: WorkspaceActiveSnapshotClientMessage = {
    type: "subscribe",
    scope: "active",
    include_active_heads: requestSnapshot,
  };
  if (foregroundSessionId) {
    message.foreground_session_id = foregroundSessionId;
  }
  if (subscribedSessions.length > 0) {
    message.sessions = subscribedSessions.map((session) => ({
      session_id: session.sessionId,
      intent: toWorkspaceIntent(session, foregroundSessionId),
      replay: toWorkspaceReplay(session.replay),
    }));
  }
  const sessionKey = (message.sessions ?? [])
    .map((session) => {
      const intent = session.intent ?? "replay";
      const replay = intent === "head" ? "head" : replayKey(session.replay);
      return `${session.session_id}:${intent}:${replay}`;
    })
    .join("|");
  const canonicalKey = [
    "scope=active",
    `foreground=${foregroundSessionId ?? ""}`,
    `sessions=${sessionKey}`,
  ].join(";");
  return { message, requestSnapshot, canonicalKey };
}
