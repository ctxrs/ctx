import type { WorkspaceActiveSnapshotEvent } from "@ctx/types";
import type { SessionReplicaCommand } from "./sessionReplicaProtocol";

const DEFAULT_BACKGROUND_BATCH_SIZE = 1;
const DEFAULT_BACKGROUND_DRAIN_DELAY_MS = 50;
const DEFAULT_FOREGROUND_QUIET_DELAY_MS = 1000;

type TimerHandle = ReturnType<typeof globalThis.setTimeout>;

type SchedulerOptions = {
  backgroundBatchSize?: number;
  backgroundDrainDelayMs?: number;
  foregroundQuietDelayMs?: number;
  setTimeoutFn?: typeof globalThis.setTimeout;
  clearTimeoutFn?: typeof globalThis.clearTimeout;
};

type WorkspaceEventCommand = Extract<SessionReplicaCommand, { type: "workspace_event" }>;

const bindSetTimeout = (fn: typeof globalThis.setTimeout): typeof globalThis.setTimeout =>
  ((...args: Parameters<typeof globalThis.setTimeout>) =>
    fn.call(globalThis, ...args)) as typeof globalThis.setTimeout;

const bindClearTimeout = (fn: typeof globalThis.clearTimeout): typeof globalThis.clearTimeout =>
  ((...args: Parameters<typeof globalThis.clearTimeout>) =>
    fn.call(globalThis, ...args)) as typeof globalThis.clearTimeout;

const normalizeId = (value: unknown): string => (typeof value === "string" ? value.trim() : "");

export const sessionIdForReplicaWorkspaceEvent = (
  event: WorkspaceActiveSnapshotEvent,
): string => {
  switch (event.type) {
    case "session_head_delta":
      return normalizeId(event.delta.session_id);
    case "session_head_seed":
      return normalizeId(event.head.session.id);
    case "session_gap":
      return normalizeId(event.session_id);
    default:
      return "";
  }
};

const sessionIdForCommand = (cmd: SessionReplicaCommand): string => {
  switch (cmd.type) {
    case "workspace_event":
      return sessionIdForReplicaWorkspaceEvent(cmd.event);
    case "open_session":
    case "close_session":
    case "drop_session":
    case "refresh_session":
    case "hydrate_session_head":
    case "seed_head":
      return normalizeId(cmd.sessionId);
    case "set_session":
      return normalizeId(cmd.session.id);
    default:
      return "";
  }
};

export class SessionReplicaDispatchScheduler {
  private readonly backgroundBatchSize: number;
  private readonly backgroundDrainDelayMs: number;
  private readonly foregroundQuietDelayMs: number;
  private readonly setTimeoutFn: typeof globalThis.setTimeout;
  private readonly clearTimeoutFn: typeof globalThis.clearTimeout;
  private backgroundQueue: WorkspaceEventCommand[] = [];
  private backgroundTimer: TimerHandle | null = null;
  private destroyed = false;

  constructor(
    private readonly post: (cmd: SessionReplicaCommand) => void,
    opts?: SchedulerOptions,
  ) {
    this.backgroundBatchSize = Math.max(1, Math.floor(opts?.backgroundBatchSize ?? DEFAULT_BACKGROUND_BATCH_SIZE));
    this.backgroundDrainDelayMs = Math.max(
      0,
      Math.floor(opts?.backgroundDrainDelayMs ?? DEFAULT_BACKGROUND_DRAIN_DELAY_MS),
    );
    this.foregroundQuietDelayMs = Math.max(
      0,
      Math.floor(opts?.foregroundQuietDelayMs ?? DEFAULT_FOREGROUND_QUIET_DELAY_MS),
    );
    this.setTimeoutFn = bindSetTimeout(opts?.setTimeoutFn ?? globalThis.setTimeout);
    this.clearTimeoutFn = bindClearTimeout(opts?.clearTimeoutFn ?? globalThis.clearTimeout);
  }

  dispatch(cmd: SessionReplicaCommand): void {
    if (this.destroyed) return;
    if (cmd.type !== "workspace_event") {
      this.dispatchControlCommand(cmd);
      return;
    }
    if (cmd.lane === "foreground") {
      this.flushQueuedSession(sessionIdForReplicaWorkspaceEvent(cmd.event));
      this.post(cmd);
      this.deferBackgroundDrainAfterForeground();
      return;
    }
    this.backgroundQueue.push(cmd);
    this.scheduleBackgroundDrain();
  }

  destroy(): void {
    this.destroyed = true;
    this.backgroundQueue = [];
    if (this.backgroundTimer) {
      this.clearTimeoutFn(this.backgroundTimer);
      this.backgroundTimer = null;
    }
  }

  private dispatchControlCommand(cmd: SessionReplicaCommand): void {
    const sessionId = sessionIdForCommand(cmd);
    if (cmd.type === "close_session" || cmd.type === "drop_session") {
      this.backgroundQueue = this.backgroundQueue.filter(
        (queued) => sessionIdForReplicaWorkspaceEvent(queued.event) !== sessionId,
      );
      this.post(cmd);
      return;
    }
    if (sessionId) {
      this.flushQueuedSession(sessionId);
    }
    this.post(cmd);
  }

  private flushQueuedSession(sessionId: string): void {
    if (!sessionId || this.backgroundQueue.length === 0) return;
    const remaining: WorkspaceEventCommand[] = [];
    for (const queued of this.backgroundQueue) {
      if (sessionIdForReplicaWorkspaceEvent(queued.event) === sessionId) {
        this.post(queued);
      } else {
        remaining.push(queued);
      }
    }
    this.backgroundQueue = remaining;
  }

  private scheduleBackgroundDrain(): void {
    if (this.backgroundTimer || this.destroyed) return;
    this.backgroundTimer = this.setTimeoutFn(() => {
      this.backgroundTimer = null;
      this.drainBackgroundBatch();
    }, this.backgroundDrainDelayMs);
  }

  private deferBackgroundDrainAfterForeground(): void {
    if (this.destroyed || this.backgroundQueue.length === 0) return;
    if (this.backgroundTimer) {
      this.clearTimeoutFn(this.backgroundTimer);
    }
    this.backgroundTimer = this.setTimeoutFn(() => {
      this.backgroundTimer = null;
      this.drainBackgroundBatch();
    }, this.foregroundQuietDelayMs);
  }

  private drainBackgroundBatch(): void {
    if (this.destroyed) return;
    const batch = this.backgroundQueue.splice(0, this.backgroundBatchSize);
    for (const cmd of batch) {
      this.post(cmd);
    }
    if (this.backgroundQueue.length > 0) {
      this.scheduleBackgroundDrain();
    }
  }
}
