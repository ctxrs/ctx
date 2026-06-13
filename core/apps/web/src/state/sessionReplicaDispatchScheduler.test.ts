import type { SessionHeadDelta, WorkspaceActiveSnapshotEvent } from "@ctx/types";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { SessionReplicaDispatchScheduler } from "./sessionReplicaDispatchScheduler";
import type { SessionReplicaCommand } from "./sessionReplicaProtocol";

const makeDeltaEvent = (sessionId: string, seq: number): WorkspaceActiveSnapshotEvent => ({
  type: "session_head_delta",
  workspace_id: "ws-1",
  snapshot_rev: seq,
  delta: {
    session_id: sessionId,
    last_event_seq: seq,
    projection_rev: seq,
    state_rev: seq,
  } as SessionHeadDelta,
});

const makeWorkspaceCommand = (
  sessionId: string,
  seq: number,
  lane: "foreground" | "workspace",
): SessionReplicaCommand => ({
  type: "workspace_event",
  event: makeDeltaEvent(sessionId, seq),
  lane,
  receivedAtMs: seq,
  streamSource: "live",
});

const postedSessionIds = (commands: readonly SessionReplicaCommand[]): string[] =>
  commands.map((cmd) => {
    if (cmd.type !== "workspace_event") return cmd.type;
    if (cmd.event.type !== "session_head_delta") return cmd.event.type;
    return String(cmd.event.delta.session_id);
  });

describe("SessionReplicaDispatchScheduler", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  it("keeps foreground workspace events ahead of queued background work", () => {
    const posted: SessionReplicaCommand[] = [];
    const scheduler = new SessionReplicaDispatchScheduler((cmd) => posted.push(cmd), {
      backgroundBatchSize: 2,
      backgroundDrainDelayMs: 0,
    });

    scheduler.dispatch(makeWorkspaceCommand("background-1", 1, "workspace"));
    scheduler.dispatch(makeWorkspaceCommand("background-2", 2, "workspace"));
    scheduler.dispatch(makeWorkspaceCommand("background-3", 3, "workspace"));
    scheduler.dispatch(makeWorkspaceCommand("foreground", 4, "foreground"));

    expect(postedSessionIds(posted)).toEqual(["foreground"]);

    vi.runOnlyPendingTimers();
    expect(postedSessionIds(posted)).toEqual([
      "foreground",
      "background-1",
      "background-2",
    ]);

    vi.runOnlyPendingTimers();
    expect(postedSessionIds(posted)).toEqual([
      "foreground",
      "background-1",
      "background-2",
      "background-3",
    ]);
  });

  it("preserves same-session ordering before foreground preemption", () => {
    const posted: SessionReplicaCommand[] = [];
    const scheduler = new SessionReplicaDispatchScheduler((cmd) => posted.push(cmd), {
      backgroundBatchSize: 10,
      backgroundDrainDelayMs: 0,
    });

    scheduler.dispatch(makeWorkspaceCommand("background", 1, "workspace"));
    scheduler.dispatch(makeWorkspaceCommand("foreground", 2, "workspace"));
    scheduler.dispatch(makeWorkspaceCommand("other", 3, "workspace"));
    scheduler.dispatch(makeWorkspaceCommand("foreground", 4, "foreground"));

    expect(postedSessionIds(posted)).toEqual(["foreground", "foreground"]);

    vi.runOnlyPendingTimers();
    expect(postedSessionIds(posted)).toEqual([
      "foreground",
      "foreground",
      "background",
      "other",
    ]);
  });

  it("drops queued background events for sessions that close before draining", () => {
    const posted: SessionReplicaCommand[] = [];
    const scheduler = new SessionReplicaDispatchScheduler((cmd) => posted.push(cmd));

    scheduler.dispatch(makeWorkspaceCommand("closing", 1, "workspace"));
    scheduler.dispatch(makeWorkspaceCommand("retained", 2, "workspace"));
    scheduler.dispatch({ type: "close_session", sessionId: "closing" });

    expect(postedSessionIds(posted)).toEqual(["close_session"]);

    vi.runOnlyPendingTimers();
    expect(postedSessionIds(posted)).toEqual(["close_session", "retained"]);
  });

  it("paces default background batches so foreground events cannot be buried behind worker backlog", () => {
    const posted: SessionReplicaCommand[] = [];
    const scheduler = new SessionReplicaDispatchScheduler((cmd) => posted.push(cmd));

    for (let seq = 1; seq <= 12; seq += 1) {
      scheduler.dispatch(makeWorkspaceCommand(`background-${seq}`, seq, "workspace"));
    }

    expect(postedSessionIds(posted)).toEqual([]);

    vi.advanceTimersByTime(49);
    expect(postedSessionIds(posted)).toEqual([]);

    vi.advanceTimersByTime(1);
    expect(postedSessionIds(posted)).toEqual(["background-1"]);

    scheduler.dispatch(makeWorkspaceCommand("foreground", 99, "foreground"));
    expect(postedSessionIds(posted)).toEqual(["background-1", "foreground"]);

    vi.advanceTimersByTime(999);
    expect(postedSessionIds(posted)).toEqual(["background-1", "foreground"]);

    vi.advanceTimersByTime(1);
    expect(postedSessionIds(posted)).toEqual(["background-1", "foreground", "background-2"]);
  });

  it("keeps extending the background drain quiet window while foreground events arrive", () => {
    const posted: SessionReplicaCommand[] = [];
    const scheduler = new SessionReplicaDispatchScheduler((cmd) => posted.push(cmd), {
      foregroundQuietDelayMs: 50,
    });

    scheduler.dispatch(makeWorkspaceCommand("background-1", 1, "workspace"));
    scheduler.dispatch(makeWorkspaceCommand("background-2", 2, "workspace"));
    scheduler.dispatch(makeWorkspaceCommand("foreground", 3, "foreground"));
    vi.advanceTimersByTime(49);
    scheduler.dispatch(makeWorkspaceCommand("foreground", 4, "foreground"));
    vi.advanceTimersByTime(49);

    expect(postedSessionIds(posted)).toEqual(["foreground", "foreground"]);

    vi.advanceTimersByTime(1);
    expect(postedSessionIds(posted)).toEqual(["foreground", "foreground", "background-1"]);
  });

  it("invokes browser timer functions with the global receiver", () => {
    const posted: SessionReplicaCommand[] = [];
    const brandedSetTimeout = function (
      this: typeof globalThis,
      ...args: Parameters<typeof globalThis.setTimeout>
    ): ReturnType<typeof globalThis.setTimeout> {
      if (this !== globalThis) {
        throw new TypeError("Illegal invocation");
      }
      return globalThis.setTimeout(...args);
    } as typeof globalThis.setTimeout;
    const brandedClearTimeout = function (
      this: typeof globalThis,
      ...args: Parameters<typeof globalThis.clearTimeout>
    ): ReturnType<typeof globalThis.clearTimeout> {
      if (this !== globalThis) {
        throw new TypeError("Illegal invocation");
      }
      return globalThis.clearTimeout(...args);
    } as typeof globalThis.clearTimeout;
    const scheduler = new SessionReplicaDispatchScheduler((cmd) => posted.push(cmd), {
      backgroundBatchSize: 1,
      backgroundDrainDelayMs: 0,
      setTimeoutFn: brandedSetTimeout,
      clearTimeoutFn: brandedClearTimeout,
    });

    scheduler.dispatch(makeWorkspaceCommand("background", 1, "workspace"));

    vi.runOnlyPendingTimers();
    expect(postedSessionIds(posted)).toEqual(["background"]);

    scheduler.dispatch(makeWorkspaceCommand("destroyed-before-drain", 2, "workspace"));
    expect(() => scheduler.destroy()).not.toThrow();
  });
});
