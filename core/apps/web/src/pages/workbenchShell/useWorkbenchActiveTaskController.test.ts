import { describe, expect, it } from "vitest";

import { resolveWorkspaceVcsDetailDemand } from "./useWorkbenchActiveTaskController";
import { resolveMeasuredSessionSwitchId } from "./useWorkbenchSessionSwitchMetrics";

describe("resolveWorkspaceVcsDetailDemand", () => {
  it("requests details only when the Git pane is open and inventory demand is allowed", () => {
    expect(
      resolveWorkspaceVcsDetailDemand({
        diffOpen: true,
        activeWorktreeId: "worktree-1",
        inventoryDemandAllowed: true,
      }),
    ).toEqual(["worktree-1"]);
  });

  it("keeps large or unknown changesets at summary tier", () => {
    expect(
      resolveWorkspaceVcsDetailDemand({
        diffOpen: true,
        activeWorktreeId: "worktree-1",
        inventoryDemandAllowed: false,
      }),
    ).toEqual([]);
  });

  it("does not request details when the Git pane is closed", () => {
    expect(
      resolveWorkspaceVcsDetailDemand({
        diffOpen: false,
        activeWorktreeId: "worktree-1",
        inventoryDemandAllowed: true,
      }),
    ).toEqual([]);
  });
});

describe("resolveMeasuredSessionSwitchId", () => {
  it("drops optimistic placeholder session ids from switch timing", () => {
    expect(resolveMeasuredSessionSwitchId("optimistic-session-1", true)).toBeNull();
  });

  it("keeps non-optimistic session ids for switch timing", () => {
    expect(resolveMeasuredSessionSwitchId("session-1", false)).toBe("session-1");
  });

  it("normalizes empty session ids to null", () => {
    expect(resolveMeasuredSessionSwitchId("   ", false)).toBeNull();
    expect(resolveMeasuredSessionSwitchId(null, false)).toBeNull();
  });
});
