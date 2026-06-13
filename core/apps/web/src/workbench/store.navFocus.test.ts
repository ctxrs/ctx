import type { WorkbenchStore } from "./store";
import { describe, expect, it, vi } from "vitest";

vi.mock("./persistence", async () => {
  const actual = await vi.importActual<typeof import("./persistence")>("./persistence");
  return {
    ...actual,
    saveWorkbenchWindowV1: vi.fn(async () => {}),
  };
});

const getActiveTaskId = (store: WorkbenchStore): string | null => {
  const tab = store.getActiveTab();
  return tab && tab.kind === "task" ? tab.ref.taskId : null;
};

const getActiveTabKind = (store: WorkbenchStore): string | null => {
  const tab = store.getActiveTab();
  return tab ? tab.kind : null;
};

describe("WorkbenchStore navigation tokens", () => {
  it("applies system focus when the token is current", async () => {
    const { WorkbenchStore } = await import("./store");
    const store = new WorkbenchStore("ws-1");
    const token = store.getNavToken();

    const applied = store.focusTask("task-1", "session-1", { navToken: token, source: "system" });

    expect(applied).toBe(true);
    expect(getActiveTaskId(store)).toBe("task-1");
  }, 20000);

  it("defaults to user intent when no source is provided", async () => {
    const { WorkbenchStore } = await import("./store");
    const store = new WorkbenchStore("ws-1");
    const token = store.getNavToken();

    store.focusTask("task-1");

    expect(store.getNavToken()).toBe(token + 1);
  }, 20000);

  it("ignores stale system focus after default user navigation", async () => {
    const { WorkbenchStore } = await import("./store");
    const store = new WorkbenchStore("ws-1");
    const token = store.getNavToken();

    store.focusTask("task-1");
    const applied = store.focusTask("task-2", null, { navToken: token, source: "system" });

    expect(applied).toBe(false);
    expect(getActiveTaskId(store)).toBe("task-1");
  }, 20000);

  it("does not bump tokens for system session updates", async () => {
    const { WorkbenchStore } = await import("./store");
    const store = new WorkbenchStore("ws-1");

    store.focusTask("task-1", "session-1");
    const beforeSystem = store.getNavToken();
    store.setActiveSessionForActiveTask("session-2", { source: "system" });

    expect(store.getNavToken()).toBe(beforeSystem);
    expect(getActiveTabKind(store)).toBe("task");
  }, 20000);
});
