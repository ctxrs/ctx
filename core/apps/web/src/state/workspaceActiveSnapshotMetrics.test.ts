import { describe, it, expect, beforeEach, vi } from "vitest";
import * as clientApi from "../api/client";
import { WorkspaceActiveSnapshotStoreImpl } from "./workspaceActiveSnapshotStoreCore";
import { applyWorkerPatch } from "./workspaceActiveSnapshot/workerRuntime";
import type { WorkspaceActiveSnapshotWorkerHost } from "./workspaceActiveSnapshot/workerRuntime";

vi.mock("../api/client", () => ({
  idToString: (id: string | null | undefined) => (typeof id === "string" ? id : ""),
  authToken: vi.fn(() => null),
  getDaemonConnectionReadiness: vi.fn(() => ({
    hasBaseUrl: true,
    hasAuthToken: true,
    isReady: true,
    missing: null,
  })),
  getDaemonClientConfig: vi.fn(() => ({
    baseUrl: "http://localhost:4399",
    wsBaseUrl: "ws://localhost:4399",
    authToken: null,
    runId: null,
  })),
  subscribeDaemonConfig: vi.fn(() => () => {}),
  listWorkspaceArchivedTaskSummaries: vi.fn(async () => ({
    workspace_id: "ws-1",
    archived_rev: 0,
    tasks: [],
    total_archived: 0,
    next_cursor: null,
  })),
  recordClientCounterMetric: vi.fn(),
  recordClientGaugeMetric: vi.fn(),
  recordClientHistogramMetric: vi.fn(),
  recordSemanticTelemetryEvent: vi.fn(),
}));

vi.mock("./uiStateStore", () => ({
  loadWorkspaceActiveSnapshotV1: vi.fn(async () => null),
  saveWorkspaceActiveSnapshotV1: vi.fn(async () => {}),
}));

vi.mock("../utils/desktop", () => ({
  isDesktopApp: vi.fn(() => false),
}));

vi.mock("./diagnosticsChannel", () => ({
  emitUiDiagnostic: vi.fn(),
  normalizeDiagnosticErrorMessage: vi.fn((value: unknown) => String(value ?? "")),
}));

vi.mock("./foregroundFreshnessTelemetry", () => ({
  noteClientReceiveLag: vi.fn(),
  noteQueueAgeSample: vi.fn(),
  noteWorkspaceStreamEventObserved: vi.fn(),
  noteWorkspaceStreamReset: vi.fn(),
}));

describe("WorkspaceActiveSnapshotStore metrics", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("records worker patch flush and apply metrics", async () => {
    const store = new WorkspaceActiveSnapshotStoreImpl("ws-1", {
      disableWorker: true,
      onPatch: () => {},
    });

    applyWorkerPatch(store as unknown as WorkspaceActiveSnapshotWorkerHost, {
      shell: {
        initialized: true,
        activeIds: [],
        archivedIds: [],
        totalActive: 0,
        totalArchived: 0,
        archivedRev: 0,
        hasMoreActive: true,
        hasMoreArchived: false,
        archivedLoaded: false,
        connection: "idle",
        fetchState: { active: "idle", archived: "idle" },
      },
      events: [],
      activeSessionIds: [],
      snapshotRev: 1,
      archivedRev: 0,
      publishSnapshot: false,
      persist: false,
    });

    expect(clientApi.recordClientHistogramMetric).toHaveBeenCalledWith(
      "workspace.active_snapshot.worker_patch_apply_ms",
      "ms",
      expect.any(Number),
      { patch_kind: "diff" },
    );

    vi.mocked(clientApi.recordClientCounterMetric).mockClear();
    vi.mocked(clientApi.recordClientHistogramMetric).mockClear();

    (
      store as unknown as {
        workerPatchPendingEvents: Array<Record<string, unknown>>;
        flushWorkerPatchNow: () => void;
      }
    ).workerPatchPendingEvents = [
      {
        type: "session_head_delta",
        workspace_id: "ws-1",
        snapshot_rev: 1,
      },
    ];
    (
      store as unknown as {
        flushWorkerPatchNow: () => void;
      }
    ).flushWorkerPatchNow();

    expect(clientApi.recordClientCounterMetric).toHaveBeenCalledWith(
      "workspace.active_snapshot.worker_patch_flush_count",
      expect.objectContaining({ patch_kind: "replace" }),
    );
    expect(clientApi.recordClientHistogramMetric).toHaveBeenCalledWith(
      "workspace.active_snapshot.worker_patch_flush_ms",
      "ms",
      expect.any(Number),
      expect.objectContaining({ patch_kind: "replace" }),
    );

    store.destroy();
  });
});
