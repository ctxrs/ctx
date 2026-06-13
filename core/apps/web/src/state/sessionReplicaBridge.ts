import {
  getSessionHead,
  getSessionSnapshot,
  getSessionState,
  getDaemonClientConfig,
  subscribeDaemonConfig,
} from "../api/client";
import { isDesktopApp } from "../utils/desktop";
import { SessionReplicaCore } from "./sessionReplicaCore";
import type {
  SessionReplicaCommand,
  SessionReplicaConfig,
  SessionReplicaFreshnessEvent,
  SessionReplicaPatch,
  SessionReplicaWorkerMessage,
} from "./sessionReplicaProtocol";
import {
  noteFinalDeltaReceived,
  noteGapRecoveryFinished,
  noteGapRecoveryStarted,
  noteGapRepairMismatch,
  noteProjectionOrSeqRegression,
  noteSessionReplicaApplyDuration,
  noteSessionReplicaEventAge,
  noteSessionReplicaApplyLag,
  noteStaleHeadDeltaDropped,
} from "./foregroundFreshnessTelemetry";
import { SessionReplicaDispatchScheduler } from "./sessionReplicaDispatchScheduler";

const shouldUseWorker = (): boolean => {
  if (typeof Worker === "undefined") return false;
  if (isDesktopApp()) return false;
  const metaEnv =
    typeof import.meta !== "undefined" ? (import.meta as { env?: { MODE?: string } }).env : undefined;
  if (metaEnv?.MODE === "test") return false;
  return true;
};

const nowMs = (): number => {
  if (typeof performance !== "undefined" && typeof performance.now === "function") {
    return (performance.timeOrigin ?? Date.now()) + performance.now();
  }
  return Date.now();
};

const patchOpLabel = (patches: readonly SessionReplicaPatch[]): string => {
  const first = patches[0]?.op;
  if (!first) return "none";
  return patches.every((patch) => patch.op === first) ? first : "mixed";
};

export class SessionReplicaBridge {
  private worker: Worker | null = null;
  private core: SessionReplicaCore | null = null;
  private workerScheduler: SessionReplicaDispatchScheduler | null = null;
  private configUnsubscribe: (() => void) | null = null;

  constructor(
    private onPatches: (patches: SessionReplicaPatch[]) => void,
    private config: SessionReplicaConfig,
  ) {
    if (shouldUseWorker()) {
      this.worker = new Worker(new URL("../workers/sessionReplica.worker.ts", import.meta.url), { type: "module" });
      this.workerScheduler = new SessionReplicaDispatchScheduler((cmd) => {
        this.worker?.postMessage(cmd);
      });
      this.worker.onmessage = (event: MessageEvent<SessionReplicaWorkerMessage>) => {
        const msg = event.data;
        if (msg?.type === "patches") {
          this.applyPatches(msg.patches);
          return;
        }
        if (msg?.type === "freshness_event") {
          handleSessionReplicaFreshnessEvent(msg.event);
        }
      };
    } else {
      this.core = new SessionReplicaCore({
        api: {
          getSessionHead,
          getSessionSnapshot,
          getSessionState,
        },
        emit: (patches) => this.applyPatches(patches),
        emitFreshness: handleSessionReplicaFreshnessEvent,
      });
    }

    const daemonConfig = getDaemonClientConfig();
    this.dispatch({
      type: "init",
      config: this.config,
      baseUrl: daemonConfig.baseUrl,
      authToken: daemonConfig.authToken,
      runId: daemonConfig.runId,
    });
    this.configUnsubscribe = subscribeDaemonConfig((next) => {
      this.dispatch({
        type: "update_auth",
        baseUrl: next.baseUrl,
        authToken: next.authToken,
        runId: next.runId,
      });
    });
  }

  dispatch(cmd: SessionReplicaCommand) {
    if (this.workerScheduler) {
      this.workerScheduler.dispatch(cmd);
      return;
    }
    this.core?.handleCommand(cmd);
  }

  destroy() {
    if (this.worker) {
      this.workerScheduler?.destroy();
      this.workerScheduler = null;
      this.worker.terminate();
      this.worker = null;
    }
    if (this.configUnsubscribe) {
      this.configUnsubscribe();
      this.configUnsubscribe = null;
    }
    this.core = null;
  }

  private applyPatches(patches: SessionReplicaPatch[]): void {
    const startedAtMs = nowMs();
    this.onPatches(patches);
    noteSessionReplicaApplyDuration(Math.max(0, nowMs() - startedAtMs), {
      patch_count: patches.length,
      op: patchOpLabel(patches),
    });
  }
}

export const handleSessionReplicaFreshnessEvent = (event: SessionReplicaFreshnessEvent): void => {
  switch (event.type) {
    case "final_delta_received":
      noteFinalDeltaReceived({
        sessionId: event.sessionId,
        turnId: event.turnId,
        emittedAtMs: event.emittedAtMs,
        lastEventSeq: event.lastEventSeq,
      });
      return;
    case "replica_delta_applied": {
      const appliedAtMs = nowMs();
      const freshnessContext = {
        session_id: event.sessionId,
        last_event_seq: event.lastEventSeq,
        event_type: event.eventType,
        ...(event.streamSource ? { stream_source: event.streamSource } : {}),
      };
      if (typeof event.emittedAtMs === "number") {
        noteSessionReplicaEventAge(Math.max(0, appliedAtMs - event.emittedAtMs), freshnessContext);
      }
      if (typeof event.receivedAtMs === "number") {
        noteSessionReplicaApplyLag(Math.max(0, appliedAtMs - event.receivedAtMs), {
          ...freshnessContext,
          lag_source: "received_at",
        });
      }
      return;
    }
    case "gap_recovery_started":
      noteGapRecoveryStarted(event.sessionId, event.reason, event.lane);
      return;
    case "gap_recovery_finished":
      noteGapRecoveryFinished(event.sessionId);
      return;
    case "gap_repair_mismatch":
      noteGapRepairMismatch(
        event.sessionId,
        event.baselineLastEventSeq,
        event.repairedLastEventSeq,
      );
      return;
    case "projection_or_seq_regression":
      noteProjectionOrSeqRegression(
        event.sessionId,
        event.dimension,
        event.incoming,
        event.existing,
      );
      return;
    case "stale_head_delta_dropped":
      noteStaleHeadDeltaDropped(
        event.sessionId,
        event.dimension,
        event.incoming,
        event.existing,
      );
      return;
  }
};
