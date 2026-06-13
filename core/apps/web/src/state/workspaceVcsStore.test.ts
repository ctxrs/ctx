import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { WorktreeVcsSnapshot } from "@ctx/types";
import { WorkspaceVcsStore } from "./workspaceVcsStore";

const clientMocks = vi.hoisted(() => ({
  recordClientCounterMetric: vi.fn(),
  recordClientHistogramMetric: vi.fn(),
  subscribeDaemonConfig: vi.fn(() => () => {}),
}));

vi.mock("../api/client", () => ({
  getDaemonClientConfig: vi.fn(() => ({
    baseUrl: "http://localhost:4399",
    wsBaseUrl: "ws://localhost:4399",
    authToken: null,
    runId: null,
  })),
  idToString: (id: string | null | undefined): string => (typeof id === "string" ? id : ""),
  recordClientCounterMetric: clientMocks.recordClientCounterMetric,
  recordClientHistogramMetric: clientMocks.recordClientHistogramMetric,
  subscribeDaemonConfig: clientMocks.subscribeDaemonConfig,
}));

class MockWebSocket {
  static OPEN = 1;
  static CONNECTING = 0;
  static CLOSED = 3;
  readyState = MockWebSocket.CONNECTING;
  sent: string[] = [];
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;

  constructor(readonly url: string) {
    mockSockets.push(this);
  }

  send(data: string) {
    this.sent.push(data);
  }

  close() {
    this.readyState = MockWebSocket.CLOSED;
    this.onclose?.();
  }

  open() {
    this.readyState = MockWebSocket.OPEN;
    this.onopen?.();
  }

  emit(data: unknown) {
    this.onmessage?.({ data: JSON.stringify(data) } as MessageEvent);
  }
}

const mockSockets: MockWebSocket[] = [];
const originalWebSocket = globalThis.WebSocket;

const makeSnapshot = (worktreeId: string, rev: number): WorktreeVcsSnapshot => ({
  worktree_id: worktreeId,
  rev,
  emitted_at_ms: Date.now(),
  base_commit_sha: "base",
  head_commit_sha: "head",
  base_resolution: { kind: "merge_base" },
  compute_state: "ready",
  summary: {
    file_count: rev,
    line_additions: rev,
    line_deletions: 0,
    line_count: rev,
  },
  git_status: {
    branch: "main",
    upstream: "origin/main",
    ahead: 0,
    behind: 0,
    detached: false,
    staged: 0,
    unstaged: rev,
    untracked: 0,
    entries: [],
  },
  touched_files: {
    total_count: rev,
    truncated: false,
    items: [],
  },
  touched_files_state: "ready",
  freshness: "fresh",
  available: true,
  unavailable_reason: null,
  schema_version: 2,
});

const makeSummaryOnlySnapshot = (worktreeId: string, rev: number): WorktreeVcsSnapshot => ({
  ...makeSnapshot(worktreeId, rev),
  touched_files: {
    total_count: rev,
    truncated: false,
    items: [],
  },
  touched_files_state: "not_loaded",
});

const makeDetailSnapshot = (worktreeId: string, rev: number, path: string): WorktreeVcsSnapshot => ({
  ...makeSnapshot(worktreeId, rev),
  touched_files: {
    total_count: 1,
    truncated: false,
    items: [
      {
        path,
        orig_path: null,
        index_status: null,
        worktree_status: "M",
      },
    ],
  },
  touched_files_state: "ready",
});

const flushQueuedSnapshots = async (): Promise<void> => {
  await Promise.resolve();
  vi.advanceTimersByTime(20);
  await Promise.resolve();
};

describe("WorkspaceVcsStore", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    clientMocks.recordClientCounterMetric.mockClear();
    clientMocks.recordClientHistogramMetric.mockClear();
    clientMocks.subscribeDaemonConfig.mockClear();
  });

  afterEach(() => {
    globalThis.WebSocket = originalWebSocket;
    mockSockets.length = 0;
    vi.useRealTimers();
  });

  it("subscribes to the dedicated VCS stream and ignores stale snapshot revisions", async () => {
    globalThis.WebSocket = MockWebSocket as unknown as typeof WebSocket;
    const store = new WorkspaceVcsStore("workspace-1");
    store.init();
    await Promise.resolve();
    await Promise.resolve();
    const socket = mockSockets[0];
    expect(socket?.url).toBe("ws://localhost:4399/api/workspaces/workspace-1/vcs/stream");
    socket.open();

    store.setDemand({
      summaryWorktreeIds: ["worktree-2", "worktree-1", "worktree-1"],
      detailWorktreeIds: ["worktree-1"],
    });

    expect(JSON.parse(socket.sent.at(-1) ?? "{}")).toEqual({
      type: "replace_subscription",
      summary_worktree_ids: ["worktree-1", "worktree-2"],
      detail_worktree_ids: ["worktree-1"],
    });

    socket.emit({
      type: "subscribed",
      workspace_id: "workspace-1",
      demand_generation: 1,
      summary_worktree_ids: ["worktree-1", "worktree-2"],
      detail_worktree_ids: ["worktree-1"],
    });
    await Promise.resolve();
    socket.emit({
      type: "summary_snapshot",
      workspace_id: "workspace-1",
      worktree_id: "worktree-1",
      demand_generation: 1,
      snapshot: makeSnapshot("worktree-1", 2),
    });
    await flushQueuedSnapshots();
    expect(store.getWorktreeVcsSnapshot("worktree-1")?.rev).toBe(2);
    socket.emit({
      type: "summary_snapshot",
      workspace_id: "workspace-1",
      worktree_id: "worktree-1",
      demand_generation: 1,
      snapshot: makeSnapshot("worktree-1", 1),
    });
    await flushQueuedSnapshots();
    socket.emit({
      type: "details_snapshot",
      workspace_id: "workspace-1",
      worktree_id: "worktree-1",
      demand_generation: 1,
      snapshot: makeSnapshot("worktree-1", 3),
    });
    await flushQueuedSnapshots();

    expect(store.getWorktreeVcsSnapshot("worktree-1")?.rev).toBe(3);
    store.destroy();
  });

  it("ignores snapshots from stale demand generations and no-longer-demanded tiers", async () => {
    globalThis.WebSocket = MockWebSocket as unknown as typeof WebSocket;
    const store = new WorkspaceVcsStore("workspace-1");
    store.init();
    await Promise.resolve();
    await Promise.resolve();
    const socket = mockSockets[0];
    socket.open();

    store.setDemand({ summaryWorktreeIds: ["worktree-1"] });
    socket.emit({
      type: "subscribed",
      workspace_id: "workspace-1",
      demand_generation: 1,
      summary_worktree_ids: ["worktree-1"],
      detail_worktree_ids: [],
    });
    await Promise.resolve();
    socket.emit({
      type: "summary_snapshot",
      workspace_id: "workspace-1",
      worktree_id: "worktree-1",
      demand_generation: 1,
      snapshot: makeSnapshot("worktree-1", 2),
    });
    await flushQueuedSnapshots();
    expect(store.getWorktreeVcsSnapshot("worktree-1")?.rev).toBe(2);

    store.setDemand({ summaryWorktreeIds: ["worktree-2"] });
    socket.emit({
      type: "summary_snapshot",
      workspace_id: "workspace-1",
      worktree_id: "worktree-2",
      demand_generation: 1,
      snapshot: makeSnapshot("worktree-2", 99),
    });
    await flushQueuedSnapshots();
    expect(store.getWorktreeVcsSnapshot("worktree-2")).toBeNull();

    socket.emit({
      type: "subscribed",
      workspace_id: "workspace-1",
      demand_generation: 2,
      summary_worktree_ids: ["worktree-2"],
      detail_worktree_ids: [],
    });
    await Promise.resolve();
    socket.emit({
      type: "summary_snapshot",
      workspace_id: "workspace-1",
      worktree_id: "worktree-1",
      demand_generation: 1,
      snapshot: makeSnapshot("worktree-1", 99),
    });
    await flushQueuedSnapshots();
    expect(store.getWorktreeVcsSnapshot("worktree-1")?.rev).toBe(2);

    store.setDemand({ summaryWorktreeIds: ["worktree-2"], detailWorktreeIds: ["worktree-2"] });
    socket.emit({
      type: "subscribed",
      workspace_id: "workspace-1",
      demand_generation: 3,
      summary_worktree_ids: ["worktree-2"],
      detail_worktree_ids: ["worktree-2"],
    });
    await Promise.resolve();
    socket.emit({
      type: "summary_snapshot",
      workspace_id: "workspace-1",
      worktree_id: "worktree-2",
      demand_generation: 3,
      snapshot: makeSnapshot("worktree-2", 5),
    });
    await flushQueuedSnapshots();
    expect(store.getWorktreeVcsSnapshot("worktree-2")?.rev).toBe(5);

    socket.emit({
      type: "details_snapshot",
      workspace_id: "workspace-1",
      worktree_id: "worktree-2",
      demand_generation: 3,
      snapshot: makeSnapshot("worktree-2", 6),
    });
    await flushQueuedSnapshots();
    expect(store.getWorktreeVcsSnapshot("worktree-2")?.rev).toBe(6);
    store.destroy();
  });

  it("merges summary and detail tiers so detail demand does not starve coarse VCS status", async () => {
    globalThis.WebSocket = MockWebSocket as unknown as typeof WebSocket;
    const store = new WorkspaceVcsStore("workspace-1");
    store.init();
    await Promise.resolve();
    await Promise.resolve();
    const socket = mockSockets[0];
    socket.open();

    store.setDemand({ summaryWorktreeIds: ["worktree-1"], detailWorktreeIds: ["worktree-1"] });
    socket.emit({
      type: "subscribed",
      workspace_id: "workspace-1",
      demand_generation: 1,
      summary_worktree_ids: ["worktree-1"],
      detail_worktree_ids: ["worktree-1"],
    });
    await Promise.resolve();
    socket.emit({
      type: "summary_snapshot",
      workspace_id: "workspace-1",
      worktree_id: "worktree-1",
      demand_generation: 1,
      snapshot: makeSummaryOnlySnapshot("worktree-1", 10),
    });
    await flushQueuedSnapshots();
    expect(store.getWorktreeVcsSnapshot("worktree-1")?.rev).toBe(10);
    expect(store.getWorktreeVcsSnapshot("worktree-1")?.touched_files.total_count).toBe(10);
    expect(store.getWorktreeVcsSnapshot("worktree-1")?.touched_files.items).toEqual([]);

    socket.emit({
      type: "details_snapshot",
      workspace_id: "workspace-1",
      worktree_id: "worktree-1",
      demand_generation: 1,
      snapshot: makeDetailSnapshot("worktree-1", 9, "src/app.ts"),
    });
    await flushQueuedSnapshots();
    const staleDetailSnapshot = store.getWorktreeVcsSnapshot("worktree-1");
    expect(staleDetailSnapshot?.rev).toBe(10);
    expect(staleDetailSnapshot?.touched_files_state).toBe("stale");
    expect((staleDetailSnapshot?.touched_files.items ?? [])[0]?.path).toBe("src/app.ts");

    socket.emit({
      type: "summary_snapshot",
      workspace_id: "workspace-1",
      worktree_id: "worktree-1",
      demand_generation: 1,
      snapshot: makeSummaryOnlySnapshot("worktree-1", 11),
    });
    await flushQueuedSnapshots();
    const newerSummarySnapshot = store.getWorktreeVcsSnapshot("worktree-1");
    expect(newerSummarySnapshot?.rev).toBe(11);
    expect(newerSummarySnapshot?.touched_files_state).toBe("stale");
    expect((newerSummarySnapshot?.touched_files.items ?? [])[0]?.path).toBe("src/app.ts");
    store.destroy();
  });

  it("can pin detail demand for a specific worktree before UI-derived demand catches up", async () => {
    globalThis.WebSocket = MockWebSocket as unknown as typeof WebSocket;
    const store = new WorkspaceVcsStore("workspace-1");
    store.init();
    await Promise.resolve();
    await Promise.resolve();
    const socket = mockSockets[0];
    socket.open();

    store.ensureDetailsDemand(["worktree-1"]);

    expect(JSON.parse(socket.sent.at(-1) ?? "{}")).toEqual({
      type: "replace_subscription",
      summary_worktree_ids: ["worktree-1"],
      detail_worktree_ids: ["worktree-1"],
    });

    socket.emit({
      type: "details_snapshot",
      workspace_id: "workspace-1",
      worktree_id: "worktree-1",
      demand_generation: 1,
      snapshot: makeDetailSnapshot("worktree-1", 1, "vcs-soak-tracked.txt"),
    });
    await flushQueuedSnapshots();
    expect(store.getWorktreeVcsSnapshot("worktree-1")).toBeNull();

    socket.emit({
      type: "subscribed",
      workspace_id: "workspace-1",
      demand_generation: 1,
      summary_worktree_ids: ["worktree-1"],
      detail_worktree_ids: ["worktree-1"],
    });
    await Promise.resolve();
    socket.emit({
      type: "details_snapshot",
      workspace_id: "workspace-1",
      worktree_id: "worktree-1",
      demand_generation: 1,
      snapshot: makeDetailSnapshot("worktree-1", 1, "vcs-soak-tracked.txt"),
    });
    await flushQueuedSnapshots();

    expect(store.getWorktreeVcsSnapshot("worktree-1")?.touched_files.items?.[0]?.path ?? null).toBe(
      "vcs-soak-tracked.txt",
    );
    store.destroy();
  });

  it("coalesces queued snapshots by worktree and tier before applying telemetry", async () => {
    globalThis.WebSocket = MockWebSocket as unknown as typeof WebSocket;
    const store = new WorkspaceVcsStore("workspace-1");
    store.init();
    await Promise.resolve();
    await Promise.resolve();
    const socket = mockSockets[0];
    socket.open();

    store.setDemand({ summaryWorktreeIds: ["worktree-1"] });
    socket.emit({
      type: "subscribed",
      workspace_id: "workspace-1",
      demand_generation: 1,
      summary_worktree_ids: ["worktree-1"],
      detail_worktree_ids: [],
    });
    await Promise.resolve();

    for (let rev = 1; rev <= 100; rev += 1) {
      socket.emit({
        type: "summary_snapshot",
        workspace_id: "workspace-1",
        worktree_id: "worktree-1",
        demand_generation: 1,
        snapshot: makeSnapshot("worktree-1", rev),
      });
    }

    await Promise.resolve();
    expect(store.getWorktreeVcsSnapshot("worktree-1")).toBeNull();
    await flushQueuedSnapshots();

    expect(store.getWorktreeVcsSnapshot("worktree-1")?.rev).toBe(100);
    expect(clientMocks.recordClientCounterMetric).toHaveBeenCalledTimes(1);
    expect(clientMocks.recordClientHistogramMetric).toHaveBeenCalledTimes(1);
    store.destroy();
  });
});
