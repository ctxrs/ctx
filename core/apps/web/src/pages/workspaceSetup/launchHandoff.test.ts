import { afterEach, describe, expect, it, vi } from "vitest";
import type { ExecutionLaunchSnapshot, ExecutionLaunchStreamEvent } from "../../api/client";

const clientMocks = vi.hoisted(() => ({
  buildExecutionLaunchWsUrl: vi.fn(),
  getExecutionLaunchStatus: vi.fn(),
  startWorkspaceSetupLaunchHandoff: vi.fn(),
}));

vi.mock("../../api/client", () => ({
  buildExecutionLaunchWsUrl: clientMocks.buildExecutionLaunchWsUrl,
  getExecutionLaunchStatus: clientMocks.getExecutionLaunchStatus,
  startWorkspaceSetupLaunchHandoff: clientMocks.startWorkspaceSetupLaunchHandoff,
}));

import {
  createLaunchLogBatcher,
  launchErrorFromSnapshot,
  waitForLaunchHandoffTerminal,
} from "./launchHandoff";

const launchSnapshot = (state: ExecutionLaunchSnapshot["state"]): ExecutionLaunchSnapshot => ({
  job_id: "job_123",
  workspace_id: "ws_123",
  kind: "workspace_launch",
  state,
  created_at: "2026-03-09T00:00:00Z",
  started_at: "2026-03-09T00:00:01Z",
  current_phase: "container_start_or_create",
  phases: [],
  logs: [],
});

class FakeWebSocket {
  static instances: FakeWebSocket[] = [];

  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  closed = false;

  constructor(readonly url: string) {
    FakeWebSocket.instances.push(this);
  }

  close() {
    this.closed = true;
  }

  emitMessage(event: ExecutionLaunchStreamEvent) {
    this.onmessage?.({ data: JSON.stringify(event) });
  }

  emitClose() {
    this.onclose?.();
  }
}

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
  FakeWebSocket.instances = [];
});

describe("launchHandoff", () => {
  it("preserves the current launch phase in extracted error messages", () => {
    expect(launchErrorFromSnapshot({
      job_id: "job_123",
      workspace_id: "ws_123",
      kind: "workspace_launch",
      state: "error",
      created_at: "2026-03-09T00:00:00Z",
      started_at: "2026-03-09T00:00:01Z",
      current_phase: "machine_start_or_init",
      error: "failed to start remote daemon",
      phases: [],
      logs: [],
    })).toBe("Machine start/init: failed to start remote daemon");
  });

  it("prefers the detailed current step label for provisioning-style error messages", () => {
    expect(launchErrorFromSnapshot({
      job_id: "job_123",
      workspace_id: "ws_123",
      kind: "workspace_launch",
      state: "error",
      created_at: "2026-03-09T00:00:00Z",
      started_at: "2026-03-09T00:00:01Z",
      current_phase: null,
      current_step_label: "Cloning repository",
      error: "clone exploded",
      phases: [],
      logs: [],
    })).toBe("Cloning repository: clone exploded");
  });

  it("batches launch-log lines before flushing them", () => {
    const appended: number[][] = [];
    const scheduler: { scheduledFlush: (() => void) | null } = { scheduledFlush: null };
    const batcher = createLaunchLogBatcher(
      (lines) => appended.push(lines.map((line) => line.seq)),
      {
        scheduleFlush: (flush) => {
          scheduler.scheduledFlush = flush;
          return 1;
        },
        cancelFlush: () => {
          scheduler.scheduledFlush = null;
        },
      },
    );

    batcher.enqueue({
      seq: 1,
      ts: "2026-03-09T00:00:01Z",
      phase: "machine_check",
      level: "info",
      message: "checking",
    });
    batcher.enqueue({
      seq: 2,
      ts: "2026-03-09T00:00:02Z",
      phase: "machine_check",
      level: "info",
      message: "still checking",
    });

    expect(appended).toEqual([]);
    expect(scheduler.scheduledFlush).not.toBeNull();
    if (!scheduler.scheduledFlush) {
      throw new Error("expected a scheduled flush");
    }
    scheduler.scheduledFlush();

    expect(appended).toEqual([[1, 2]]);
  });

  it("flushes pending launch-log lines immediately when requested", () => {
    const appended: number[][] = [];
    let canceled = false;
    const batcher = createLaunchLogBatcher(
      (lines) => appended.push(lines.map((line) => line.seq)),
      {
        scheduleFlush: () => 7,
        cancelFlush: () => {
          canceled = true;
        },
      },
    );

    batcher.enqueue({
      seq: 3,
      ts: "2026-03-09T00:00:03Z",
      phase: "machine_start_or_init",
      level: "info",
      message: "starting",
    });

    batcher.flush();

    expect(canceled).toBe(true);
    expect(appended).toEqual([[3]]);
  });

  it("reconnects the launch stream when the socket closes before the launch is terminal", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("WebSocket", FakeWebSocket);
    clientMocks.buildExecutionLaunchWsUrl.mockResolvedValue("ws://launch/job_123");
    clientMocks.getExecutionLaunchStatus.mockResolvedValue(launchSnapshot("running"));
    const applySnapshot = vi.fn();
    const appendLines = vi.fn();

    const completion = waitForLaunchHandoffTerminal(
      launchSnapshot("running"),
      { applySnapshot, appendLines },
      { maxReconnects: 1, reconnectDelayMs: 0 },
    );

    await vi.waitFor(() => expect(FakeWebSocket.instances).toHaveLength(1));
    FakeWebSocket.instances[0]?.emitClose();
    await vi.runOnlyPendingTimersAsync();
    await vi.waitFor(() => expect(FakeWebSocket.instances).toHaveLength(2));
    FakeWebSocket.instances[1]?.emitMessage({
      type: "launch_complete",
      snapshot: launchSnapshot("ready"),
    });

    await expect(completion).resolves.toBeUndefined();
    expect(clientMocks.getExecutionLaunchStatus).toHaveBeenCalledWith("job_123");
    expect(applySnapshot).toHaveBeenCalledWith(expect.objectContaining({ state: "running" }));
    expect(applySnapshot).toHaveBeenCalledWith(expect.objectContaining({ state: "ready" }));
  });
});
