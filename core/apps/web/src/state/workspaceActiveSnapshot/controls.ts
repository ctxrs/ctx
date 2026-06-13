import type { WorkspaceActiveSnapshotEvent } from "@ctx/types";
import { getDaemonClientConfig, recordClientCounterMetric } from "../../api/client";
import type {
  WorkspaceActiveSnapshotCommand,
  WorkspaceActiveSnapshotPatch,
} from "../workspaceActiveSnapshotProtocol";
import type { SessionSubscriptionCursor } from "../sessionSubscription";
import {
  isForegroundPrioritySessionEvent,
  prioritizeForegroundPriorityEvents,
  resolveForegroundPrioritySessionIds,
} from "./foregroundPriority";
import {
  normalizeSessionSubscriptionCursors,
  sameSessionSubscriptionCursorIds,
  sameSessionSubscriptionCursors,
  type SessionSubscriptionReplay,
} from "../sessionSubscription";
import { buildWorkspaceActiveSubscribeMessage } from "./subscriptions";
import type { WorkspaceActiveSnapshotStoreState } from "./storeState";

export type WorkspaceActiveSnapshotControlHost = {
  e2eEnabled: boolean;
  e2eDropStreamMessages: boolean;
  worker: Worker | null;
  state: WorkspaceActiveSnapshotStoreState;
  ws: WebSocket | null;
  wsBaseUrlOverride: string | null;
  authTokenOverride: string | null;
  canonicalStreamUrl: string | null;
  lastSubscriptionKey: string | null;
  workspaceId: string;
  subscribedSessions: SessionSubscriptionCursor[];
  foregroundSessionId: string | null;
  eventListeners: Set<(event: WorkspaceActiveSnapshotEvent) => void>;
  workerPatchEmitter: ((patch: WorkspaceActiveSnapshotPatch) => void) | null;
  workerPatchPendingEvents: WorkspaceActiveSnapshotEvent[];
  postWorkerCommand(cmd: WorkspaceActiveSnapshotCommand): void;
  publish(): void;
  enqueueStreamMessage(data: unknown): void;
  scheduleSnapshotWarning(reason: string): void;
  scheduleWorkerPatchFlush(): void;
  flushWorkerPatchNow(prioritySessionIds?: readonly string[]): void;
};

const isImmediateWorkerPatchEvent = (
  host: WorkspaceActiveSnapshotControlHost,
  evt: WorkspaceActiveSnapshotEvent,
): boolean => {
  if (!isForegroundPrioritySessionEvent(host.foregroundSessionId, host.subscribedSessions, evt)) {
    return false;
  }

  switch (evt.type) {
    case "session_gap":
    case "session_head_seed":
    case "session_summary":
    case "session_summary_delta":
      return true;
    case "session_head_delta": {
      if (evt.delta.message) return true;
      const eventType = String(evt.delta.event?.event_type ?? "");
      if (!eventType) return false;
      return (
        eventType !== "assistant_chunk" &&
        eventType !== "thought_chunk" &&
        eventType !== "context_window_update"
      );
    }
    default:
      return false;
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

const replayControlChanged = (
  previous: SessionSubscriptionReplay,
  next: SessionSubscriptionReplay,
): boolean => {
  if (next.kind === "auto") {
    return false;
  }
  if (next.kind === "reset") {
    return previous.kind !== "reset";
  }
  if (previous.kind !== next.kind) {
    return true;
  }
  if (previous.kind !== "resume" || next.kind !== "resume") {
    return false;
  }
  return compareResumeReplay(next, previous) < 0;
};

const shouldFlushLiveSubscriptionUpdate = (
  previous: SessionSubscriptionCursor[],
  next: SessionSubscriptionCursor[],
): boolean => {
  if (!sameSessionSubscriptionCursorIds(previous, next)) {
    return true;
  }
  for (let index = 0; index < previous.length; index += 1) {
    const prior = previous[index];
    const current = next[index];
    if (!prior || !current) return true;
    if (replayControlChanged(prior.replay, current.replay)) {
      return true;
    }
  }
  return false;
};

const subscriptionCountBucket = (count: number): string => {
  if (count <= 0) return "0";
  if (count <= 5) return "1_5";
  if (count <= 20) return "6_20";
  if (count <= 50) return "21_50";
  return "51_plus";
};

const recordSubscriptionFlushMetric = (
  action: "sent" | "skipped",
  reason: string,
  requestSnapshot: boolean,
  sessions: SessionSubscriptionCursor[],
  foregroundSessionId: string | null,
): void => {
  let headCount = 0;
  let replayCount = 0;
  for (const session of sessions) {
    const intent =
      foregroundSessionId && session.sessionId === foregroundSessionId
        ? "replay"
        : session.replay.kind === "reset"
          ? "replay"
          : session.intent === "head"
            ? "head"
            : "replay";
    if (intent === "head") {
      headCount += 1;
    } else {
      replayCount += 1;
    }
  }
  recordClientCounterMetric("workbench.workspace_subscription_flush_count", {
    action,
    reason,
    request_snapshot: requestSnapshot ? "true" : "false",
    session_count: subscriptionCountBucket(sessions.length),
    head_count: subscriptionCountBucket(headCount),
    replay_count: subscriptionCountBucket(replayCount),
  });
};

export function unwrapEvent(value: unknown): unknown {
  if (!value || typeof value !== "object") return value;
  const rec = value as { type?: string; event?: unknown };
  if (rec.type === "event" && rec.event && typeof rec.event === "object") {
    return rec.event;
  }
  return value;
}

export function notifyEventListeners(
  host: WorkspaceActiveSnapshotControlHost,
  evt: WorkspaceActiveSnapshotEvent,
) {
  for (const listener of host.eventListeners) {
    listener(evt);
  }
  if (host.workerPatchEmitter) {
    host.workerPatchPendingEvents.push(evt);
    if (isImmediateWorkerPatchEvent(host, evt)) {
      host.workerPatchPendingEvents = prioritizeForegroundPriorityEvents(
        host.foregroundSessionId,
        host.subscribedSessions,
        host.workerPatchPendingEvents,
      );
      host.flushWorkerPatchNow(
        resolveForegroundPrioritySessionIds(host.foregroundSessionId, host.subscribedSessions),
      );
      return;
    }
    host.scheduleWorkerPatchFlush();
  }
}

export function setE2EEnabled(
  host: WorkspaceActiveSnapshotControlHost,
  enabled: boolean,
) {
  host.e2eEnabled = enabled;
  if (!enabled) {
    host.e2eDropStreamMessages = false;
  }
  if (host.worker) {
    host.postWorkerCommand({ type: "e2e_set_enabled", enabled });
  }
}

export function closeActiveSnapshotStream(host: WorkspaceActiveSnapshotControlHost) {
  if (!host.e2eEnabled) return;
  if (host.worker) {
    host.postWorkerCommand({ type: "e2e_close_stream" });
    if (host.state.setConnection("disconnected")) {
      host.publish();
    }
    return;
  }
  try {
    host.ws?.close();
  } catch {
    // ignore
  }
  if (host.state.setConnection("disconnected")) {
    host.publish();
  }
}

export function setDropActiveSnapshotMessages(
  host: WorkspaceActiveSnapshotControlHost,
  drop: boolean,
) {
  if (!host.e2eEnabled) return;
  if (host.worker) {
    host.postWorkerCommand({ type: "e2e_set_drop_messages", drop });
    return;
  }
  host.e2eDropStreamMessages = drop;
}

export function injectActiveSnapshotStreamMessage(
  host: WorkspaceActiveSnapshotControlHost,
  data: unknown,
): boolean {
  if (!host.e2eEnabled) return false;
  if (host.worker) {
    host.postWorkerCommand({ type: "e2e_inject_stream_message", data });
    return true;
  }
  host.enqueueStreamMessage(data);
  return true;
}

export function getCanonicalStreamUrl(
  host: WorkspaceActiveSnapshotControlHost,
): string | null {
  if (!host.e2eEnabled) return null;
  if (host.canonicalStreamUrl) return host.canonicalStreamUrl;
  const daemonConfig = getDaemonClientConfig();
  const wsBaseUrl = host.wsBaseUrlOverride ?? daemonConfig.wsBaseUrl ?? null;
  const token = host.authTokenOverride ?? daemonConfig.authToken;
  if (!wsBaseUrl) return null;
  if (token) return null;
  return `${wsBaseUrl.replace(/\/+$/, "")}/api/workspaces/${host.workspaceId}/active_snapshot/stream`;
}

const syncRetainedLiveSessionIds = (host: WorkspaceActiveSnapshotControlHost) => {
  const retained = new Set<string>();
  for (const session of host.subscribedSessions) {
    const sessionId = session.sessionId.trim();
    if (sessionId) {
      retained.add(sessionId);
    }
  }
  const foregroundSessionId =
    typeof host.foregroundSessionId === "string" ? host.foregroundSessionId.trim() : "";
  if (foregroundSessionId) {
    retained.add(foregroundSessionId);
  }
  if (host.state.setRetainedLiveSessionIds(Array.from(retained))) {
    host.publish();
  }
};

export function setSubscribedSessions(
  host: WorkspaceActiveSnapshotControlHost,
  sessions: SessionSubscriptionCursor[],
) {
  const deduped = normalizeSessionSubscriptionCursors(sessions);
  if (sameSessionSubscriptionCursors(deduped, host.subscribedSessions)) return;
  const previous = host.subscribedSessions;
  const idsChanged = !sameSessionSubscriptionCursorIds(deduped, previous);
  host.subscribedSessions = deduped;
  syncRetainedLiveSessionIds(host);
  if (host.worker) {
    host.postWorkerCommand({ type: "set_subscribed_sessions", sessions: deduped });
    return;
  }
  if (!shouldFlushLiveSubscriptionUpdate(previous, deduped)) {
    return;
  }
  flushSubscriptions(host, idsChanged ? "session_ids" : "session_cursors");
}

export function setForegroundSessionId(
  host: WorkspaceActiveSnapshotControlHost,
  sessionId: string | null,
) {
  const normalized = typeof sessionId === "string" ? sessionId.trim() : "";
  const next = normalized ? normalized : null;
  if (next === host.foregroundSessionId) return;
  host.foregroundSessionId = next;
  syncRetainedLiveSessionIds(host);
  if (host.worker) {
    host.postWorkerCommand({ type: "set_foreground_session_id", sessionId: next });
    return;
  }
  flushSubscriptions(host, "foreground_session");
}

export function flushSubscriptions(
  host: WorkspaceActiveSnapshotControlHost,
  reason = "subscribe",
) {
  const ws = host.ws;
  if (!ws || ws.readyState !== WebSocket.OPEN) return;
  const built = buildWorkspaceActiveSubscribeMessage(
    reason,
    host.foregroundSessionId,
    host.subscribedSessions,
  );
  const forceSend =
    reason === "ws_open" || reason === "reset_required" || reason === "snapshot_rev_reset";
  if (!forceSend && host.subscribedSessions.length === 0 && !host.foregroundSessionId) {
    recordSubscriptionFlushMetric(
      "skipped",
      reason,
      built.requestSnapshot,
      host.subscribedSessions,
      host.foregroundSessionId,
    );
    return;
  }
  if (!forceSend && built.canonicalKey === host.lastSubscriptionKey) {
    recordSubscriptionFlushMetric(
      "skipped",
      reason,
      built.requestSnapshot,
      host.subscribedSessions,
      host.foregroundSessionId,
    );
    return;
  }
  host.lastSubscriptionKey = built.canonicalKey;
  const { message, requestSnapshot } = built;
  recordSubscriptionFlushMetric(
    "sent",
    reason,
    requestSnapshot,
    host.subscribedSessions,
    host.foregroundSessionId,
  );
  if (requestSnapshot) {
    host.scheduleSnapshotWarning(reason);
  }
  try {
    ws.send(JSON.stringify(message));
  } catch {
    // ignore send errors
  }
}
