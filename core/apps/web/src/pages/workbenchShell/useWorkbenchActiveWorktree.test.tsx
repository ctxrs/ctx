// @vitest-environment jsdom

import { renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useWorkbenchActiveWorktree } from "./useWorkbenchActiveWorktree";

const getWorktreeMock = vi.hoisted(() => vi.fn());

vi.mock("../../api/client", () => ({
  getWorktree: (worktreeId: string) => getWorktreeMock(worktreeId),
}));

const buildStore = (root: string | null) => ({
  getWorktreeRoot: vi.fn(() => root),
});

describe("useWorkbenchActiveWorktree", () => {
  afterEach(() => {
    vi.clearAllMocks();
  });

  it("fetches live worktree details when no cached or derived root is available", async () => {
    const workspaceSnapshotStore = buildStore(null);
    getWorktreeMock.mockResolvedValue({
      id: "wt-1",
      workspace_id: "ws-1",
      root_path: "/tmp/ctx-worktrees/ws-1/wt-1",
      base_commit_sha: "base-1",
      created_at: "2026-05-06T00:00:00.000Z",
    });

    const { result } = renderHook(() =>
      useWorkbenchActiveWorktree({
        activeTaskArchived: false,
        activeWorktreeId: "wt-1",
        daemonDataRoot: null,
        workspaceId: "ws-1",
        workspaceSnapshotStore,
      }),
    );

    await waitFor(() => {
      expect(result.current?.root_path).toBe("/tmp/ctx-worktrees/ws-1/wt-1");
    });
    expect(getWorktreeMock).toHaveBeenCalledTimes(1);
    expect(getWorktreeMock).toHaveBeenCalledWith("wt-1");
  });

  it("uses a cached live root without fetching worktree details", async () => {
    const workspaceSnapshotStore = buildStore("/tmp/ctx-worktrees/ws-1/wt-2");

    const { result } = renderHook(() =>
      useWorkbenchActiveWorktree({
        activeTaskArchived: false,
        activeWorktreeId: "wt-2",
        daemonDataRoot: null,
        workspaceId: "ws-1",
        workspaceSnapshotStore,
      }),
    );

    await waitFor(() => {
      expect(result.current?.root_path).toBe("/tmp/ctx-worktrees/ws-1/wt-2");
    });
    expect(getWorktreeMock).not.toHaveBeenCalled();
  });
});
