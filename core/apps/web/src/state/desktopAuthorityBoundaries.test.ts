import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const getDaemonClientConfigMock = vi.hoisted(() => vi.fn(() => ({
  baseUrl: "http://127.0.0.1:4399",
  wsBaseUrl: "ws://127.0.0.1:4399",
  authToken: "desktop-browser-secret",
  runId: null,
})));
const subscribeDaemonConfigMock = vi.hoisted(() => vi.fn(() => () => {}));
const workerCtorMock = vi.hoisted(() => vi.fn());

vi.mock("../utils/desktop", () => ({
  isDesktopApp: () => true,
}));

vi.mock("../api/client", () => ({
  getDaemonClientConfig: getDaemonClientConfigMock,
  subscribeDaemonConfig: subscribeDaemonConfigMock,
  getSessionHead: vi.fn(),
  getSessionSnapshot: vi.fn(),
  getSessionState: vi.fn(),
  listWorkspaceArchivedTaskSummaries: vi.fn(),
  recordClientCounterMetric: vi.fn(),
  recordClientHistogramMetric: vi.fn(),
}));

class UnexpectedWorker {
  onmessage: ((event: MessageEvent) => void) | null = null;

  constructor() {
    workerCtorMock();
  }

  postMessage(): void {}

  terminate(): void {}
}

describe("desktop renderer authority boundaries", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.stubGlobal("Worker", UnexpectedWorker);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("keeps session replica processing on the main thread in desktop mode", async () => {
    const { SessionReplicaBridge } = await import("./sessionReplicaBridge");

    const bridge = new SessionReplicaBridge(vi.fn(), {
      eventBufferLimit: 32,
      headLimit: 16,
    });

    expect(workerCtorMock).not.toHaveBeenCalled();
    expect((bridge as unknown as { worker: Worker | null }).worker).toBeNull();
    bridge.destroy();
  });

  it("defaults workspace active snapshot stores to the non-worker path in desktop mode", async () => {
    const { WorkspaceActiveSnapshotStoreImpl } = await import("./workspaceActiveSnapshotStoreCore");

    const store = new WorkspaceActiveSnapshotStoreImpl("ws-desktop");

    expect((store as unknown as { disableWorker: boolean }).disableWorker).toBe(true);
    expect(workerCtorMock).not.toHaveBeenCalled();
    store.destroy();
  });
});
