import { afterEach, describe, expect, it, vi } from "vitest";

const storageMock = vi.hoisted(() => ({
  getKv: vi.fn(),
  setKv: vi.fn(),
  deleteKv: vi.fn(),
  getSnapshot: vi.fn(),
  setSnapshot: vi.fn(),
  deleteSnapshot: vi.fn(),
  getHistoryPage: vi.fn(),
  setHistoryPage: vi.fn(),
  deleteHistoryPage: vi.fn(),
  flush: vi.fn(),
}));

vi.mock("./storage", () => ({
  getWebappStorage: () => storageMock,
}));

import {
  clearTaskThoughtsV1,
  decodeWorkspaceActiveSnapshotV1,
  loadSettingsV2,
  loadSessionHistoryPageV1,
  loadSessionHeadV1,
  loadTaskThoughtsV1,
  loadWorkspaceActiveSnapshotV1,
  saveSettingsV2,
  saveSessionHistoryPageV1,
  saveSessionHeadV1,
  saveTaskThoughtsV1,
  settingsKeyV2,
  settingsLegacyKeyV1,
  saveWorkspaceActiveSnapshotV1,
  sessionHistoryPageKeyV2,
  sessionHeadKeyV1,
  taskThoughtsKeyV2,
  workspaceActiveSnapshotKeyV1,
} from "./uiStateStore";
import { createBrowserDaemonTargetScope, createWorkspaceOwnerScope } from "./scopeIdentity";

describe("uiStateStore", () => {
  const ownerScope = createWorkspaceOwnerScope(createBrowserDaemonTargetScope("http://daemon.test"), "ws-1");

  afterEach(() => {
    vi.clearAllMocks();
  });

  it("rejects legacy active snapshot payloads", () => {
    const raw = {
      v: 1,
      workspaceId: "ws-1",
      snapshotRev: 2,
      archivedRev: 1,
      tasks: [
        {
          task: { id: "task-1" },
          primary_session: null,
          primary_session_head: null,
          sessions: [],
          sort_at: "2025-01-01T00:00:00Z",
        },
      ],
      totalCount: 1,
      updatedAtMs: 123,
    };

    expect(decodeWorkspaceActiveSnapshotV1(raw, "ws-1")).toBeNull();
  });

  it("rejects mismatched workspace ids", () => {
    const raw = {
      v: 1,
      workspaceId: "ws-2",
      active: { tasks: [], totalCount: 0 },
      updatedAtMs: 5,
    };
    expect(decodeWorkspaceActiveSnapshotV1(raw, "ws-1")).toBeNull();
  });

  it("stores workspace snapshots via snapshot storage", async () => {
    const stored = {
      v: 1,
      workspaceId: "ws-1",
      snapshotRev: 2,
      archivedRev: 1,
      active: { tasks: [], totalCount: 0 },
      updatedAtMs: 123,
    };
    storageMock.getSnapshot.mockResolvedValue(stored);

    const loaded = await loadWorkspaceActiveSnapshotV1("ws-1");
    expect(storageMock.getSnapshot).toHaveBeenCalledWith(workspaceActiveSnapshotKeyV1("ws-1"));
    expect(storageMock.getKv).not.toHaveBeenCalled();
    expect(loaded?.workspaceId).toBe("ws-1");

    await saveWorkspaceActiveSnapshotV1("ws-1", {
      snapshotRev: 3,
      archivedRev: 1,
      active: { tasks: [], totalCount: 0 },
    });
    expect(storageMock.setSnapshot).toHaveBeenCalledWith(
      workspaceActiveSnapshotKeyV1("ws-1"),
      expect.objectContaining({
        v: 1,
        workspaceId: "ws-1",
        snapshotRev: 3,
        archivedRev: 1,
        active: { tasks: [], totalCount: 0 },
        updatedAtMs: expect.any(Number),
      }),
    );
    expect(storageMock.setKv).not.toHaveBeenCalled();
  });

  it("stores session heads via snapshot storage", async () => {
    const head = {
      session: {
        id: "session-1",
        task_id: "task-1",
        workspace_id: "ws-1",
        worktree_id: "wt-1",
        provider_id: "fake",
        model_id: "fake-model",
        title: "New Task",
        agent_role: "assistant",
        status: "active",
      },
      turns: [],
      messages: [],
      events: [],
      last_event_seq: 0,
      has_more_turns: false,
      has_more_history: false,
      history_cursor: null,
    };
    storageMock.getSnapshot.mockResolvedValue({
      v: 1,
      sessionId: "session-1",
      head,
      updatedAtMs: 5,
    });

    const loaded = await loadSessionHeadV1("session-1");
    expect(storageMock.getSnapshot).toHaveBeenCalledWith(sessionHeadKeyV1("session-1"));
    expect(storageMock.getKv).not.toHaveBeenCalled();
    expect(loaded?.sessionId).toBe("session-1");

    await saveSessionHeadV1("session-1", head);
    expect(storageMock.setSnapshot).toHaveBeenCalledWith(
      sessionHeadKeyV1("session-1"),
      expect.objectContaining({
        v: 1,
        sessionId: "session-1",
        head,
        updatedAtMs: expect.any(Number),
      }),
    );
    expect(storageMock.setKv).not.toHaveBeenCalled();
  });

  it("stores task thoughts via owner-scoped snapshot keys", async () => {
    storageMock.getSnapshot.mockResolvedValue({
      v: 1,
      taskId: "task-1",
      sessions: {},
      updatedAtMs: 5,
    });

    const loaded = await loadTaskThoughtsV1(ownerScope, "task-1");
    expect(storageMock.getSnapshot).toHaveBeenCalledWith(taskThoughtsKeyV2(ownerScope, "task-1"));
    expect(loaded?.taskId).toBe("task-1");

    await saveTaskThoughtsV1(ownerScope, "task-1", { sessions: {} });
    expect(storageMock.setSnapshot).toHaveBeenCalledWith(
      taskThoughtsKeyV2(ownerScope, "task-1"),
      expect.objectContaining({
        v: 1,
        taskId: "task-1",
        sessions: {},
        updatedAtMs: expect.any(Number),
      }),
    );

    await clearTaskThoughtsV1(ownerScope, "task-1");
    expect(storageMock.deleteSnapshot).toHaveBeenCalledWith(taskThoughtsKeyV2(ownerScope, "task-1"));
  });

  it("stores session history pages via owner-scoped history keys", async () => {
    storageMock.getHistoryPage.mockResolvedValue({
      v: 1,
      sessionId: "session-1",
      beforeSeq: 10,
      limit: 20,
      page: { turns: [], messages: [], has_more: false, next_cursor: null },
      updatedAtMs: 5,
    });
    storageMock.getKv.mockResolvedValue({ v: 1, entries: [] });

    const loaded = await loadSessionHistoryPageV1(ownerScope, "session-1", 10, 20);
    expect(storageMock.getHistoryPage).toHaveBeenCalledWith(sessionHistoryPageKeyV2(ownerScope, "session-1", 10, 20));
    expect(loaded?.sessionId).toBe("session-1");

    await saveSessionHistoryPageV1(
      ownerScope,
      "session-1",
      10,
      20,
      { session_id: "session-1", turns: [], messages: [], has_more: true, next_cursor: 5 },
    );
    expect(storageMock.setHistoryPage).toHaveBeenCalledWith(
      sessionHistoryPageKeyV2(ownerScope, "session-1", 10, 20),
      expect.objectContaining({
        v: 1,
        sessionId: "session-1",
        beforeSeq: 10,
        limit: 20,
        updatedAtMs: expect.any(Number),
      }),
    );
  });

  it("migrates legacy cached settings by redacting secret values", async () => {
    storageMock.getKv
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce({
        v: 1,
        updatedAtMs: 123,
        settings: {
          dictation: {
            enabled: true,
            provider: "livekit_inference",
            livekit: {
              base_url: "https://livekit.example",
              api_key: "lk-key",
              api_secret: "lk-secret",
              model: "auto",
              language: "en",
            },
          },
          title_generation: {
            mode: "remote",
            remote: {
              base_url: "https://titles.example",
              api_key: "title-key",
              model: "gpt-test",
              use_json: true,
            },
            local: {
              model_id: "local-model",
              use_json: false,
            },
          },
        },
      });

    const loaded = await loadSettingsV2();

    expect(storageMock.getKv).toHaveBeenNthCalledWith(1, settingsKeyV2());
    expect(storageMock.getKv).toHaveBeenNthCalledWith(2, settingsLegacyKeyV1());
    expect(loaded).toEqual({
      v: 2,
      updatedAtMs: 123,
      settings: {
        dictation: {
          enabled: true,
          provider: "livekit_inference",
          livekit: {
            base_url: "https://livekit.example",
            api_key_set: true,
            api_secret_set: true,
            model: "auto",
            language: "en",
          },
        },
        title_generation: {
          mode: "remote",
          remote: {
            base_url: "https://titles.example",
            api_key_set: true,
            model: "gpt-test",
            use_json: true,
          },
          local: {
            model_id: "local-model",
            use_json: false,
          },
        },
      },
    });
    expect(storageMock.deleteKv).toHaveBeenCalledWith(settingsLegacyKeyV1());
    expect(storageMock.setKv).toHaveBeenCalledWith(
      settingsKeyV2(),
      expect.objectContaining({
        v: 2,
        settings: expect.objectContaining({
          dictation: expect.objectContaining({
            livekit: expect.not.objectContaining({ api_key: expect.anything(), api_secret: expect.anything() }),
          }),
        }),
      }),
    );
    expect(storageMock.flush).toHaveBeenCalled();
  });

  it("stores only redacted public settings in the v2 cache", async () => {
    await saveSettingsV2({
      dictation: {
        enabled: true,
        provider: "livekit_inference",
        livekit: {
          base_url: "https://livekit.example",
          api_key_set: true,
          api_secret_set: true,
          model: "auto",
          language: "en",
        },
      },
      title_generation: {
        mode: "remote",
        remote: {
          base_url: "https://titles.example",
          api_key_set: true,
          model: "gpt-test",
          use_json: true,
        },
        local: {
          model_id: "local-model",
          use_json: false,
        },
      },
    });

    expect(storageMock.setKv).toHaveBeenCalledWith(
      settingsKeyV2(),
      expect.objectContaining({
        v: 2,
        settings: {
          dictation: {
            enabled: true,
            provider: "livekit_inference",
            livekit: {
              base_url: "https://livekit.example",
              api_key_set: true,
              api_secret_set: true,
              model: "auto",
              language: "en",
            },
          },
          title_generation: {
            mode: "remote",
            remote: {
              base_url: "https://titles.example",
              api_key_set: true,
              model: "gpt-test",
              use_json: true,
            },
            local: {
              model_id: "local-model",
              use_json: false,
            },
          },
        },
        updatedAtMs: expect.any(Number),
      }),
    );
    expect(storageMock.deleteKv).toHaveBeenCalledWith(settingsLegacyKeyV1());
    expect(storageMock.flush).toHaveBeenCalled();
  });
});
