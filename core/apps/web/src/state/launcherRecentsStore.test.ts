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
  clearLauncherRecents,
  getLauncherRecentsCount,
  launcherRecentsStorageKey,
  loadLauncherRecents,
  upsertLauncherRecent,
} from "./launcherRecentsStore";

describe("launcherRecentsStore", () => {
  afterEach(() => {
    vi.clearAllMocks();
  });

  it("returns an empty list for missing or invalid payloads", async () => {
    storageMock.getKv.mockResolvedValueOnce(null);
    await expect(loadLauncherRecents()).resolves.toEqual([]);

    storageMock.getKv.mockResolvedValueOnce({ v: 1, entries: "bad" });
    await expect(loadLauncherRecents()).resolves.toEqual([]);
  });

  it("loads valid entries and drops malformed items", async () => {
    storageMock.getKv.mockResolvedValue({
      v: 1,
      entries: [
        { kind: "local", label: "repo-a", root_path: "/tmp/repo-a", updated_at_ms: 200 },
        { kind: "ssh", label: "devbox", host: "devbox.example", remote_port: 4399, updated_at_ms: 100 },
        { kind: "ssh", label: "bad", host: "devbox.example", updated_at_ms: 100 },
      ],
      updatedAtMs: 1,
    });

    await expect(loadLauncherRecents()).resolves.toEqual([
      {
        kind: "local",
        label: "repo-a",
        root_path: "/tmp/repo-a",
        execution_environment: undefined,
        updated_at_ms: 200,
      },
      {
        kind: "ssh",
        label: "devbox",
        host: "devbox.example",
        user: null,
        remote_port: 4399,
        start_remote: undefined,
        remote_data_dir: null,
        workspace_root_path: null,
        execution_environment: undefined,
        updated_at_ms: 100,
      },
    ]);
  });

  it("upserts by identity, keeps newest first, and caps at 50 entries", async () => {
    const existing = Array.from({ length: 55 }, (_, idx) => ({
      kind: "local" as const,
      label: `repo-${idx}`,
      root_path: `/tmp/repo-${idx}`,
      updated_at_ms: idx,
    }));
    storageMock.getKv.mockResolvedValue({
      v: 1,
      entries: existing,
      updatedAtMs: 1,
    });

    const updated = await upsertLauncherRecent({
      kind: "local",
      label: "repo-10-renamed",
      root_path: "/tmp/repo-10",
      updated_at_ms: 999,
    });

    expect(updated.length).toBe(50);
    expect(updated[0]).toEqual({
      kind: "local",
      label: "repo-10-renamed",
      root_path: "/tmp/repo-10",
      updated_at_ms: 999,
    });

    expect(storageMock.setKv).toHaveBeenCalledWith(
      launcherRecentsStorageKey,
      expect.objectContaining({
        v: 1,
        entries: updated,
        updatedAtMs: expect.any(Number),
      }),
    );
  });

  it("supports count and clear helpers", async () => {
    storageMock.getKv.mockResolvedValue({
      v: 1,
      entries: [{ kind: "local", label: "repo-a", root_path: "/tmp/repo-a", updated_at_ms: 10 }],
      updatedAtMs: 1,
    });
    await expect(getLauncherRecentsCount()).resolves.toBe(1);

    await clearLauncherRecents();
    expect(storageMock.deleteKv).toHaveBeenCalledWith(launcherRecentsStorageKey);
  });
});
