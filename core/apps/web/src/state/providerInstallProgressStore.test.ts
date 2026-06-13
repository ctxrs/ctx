import { beforeEach, describe, expect, it } from "vitest";
import {
  createBrowserDaemonTargetScope,
  createHostOwnerScope,
  createWorkspaceOwnerScope,
} from "./scopeIdentity";
import {
  clearProviderInstallProgress,
  getProviderInstallProgressSnapshot,
  getProviderInstallProgressSnapshotForScope,
  removeProviderInstallProgress,
  removeProviderInstallProgressForScope,
  resolveProviderInstallProgressSession,
  subscribeProviderInstallProgress,
  upsertProviderInstallProgress,
  upsertProviderInstallProgressForScope,
} from "./providerInstallProgressStore";

describe("providerInstallProgressStore", () => {
  beforeEach(() => {
    clearProviderInstallProgress();
  });

  it("does not emit for identical upserts", () => {
    const snapshots: ReturnType<typeof getProviderInstallProgressSnapshot>[] = [];
    const unsubscribe = subscribeProviderInstallProgress((snapshot) => {
      snapshots.push(snapshot);
    });

    upsertProviderInstallProgress("amp", {
      installId: "install-1",
      state: "running",
      pct: 40,
      target: "host",
      errorCode: undefined,
      error: undefined,
    });
    upsertProviderInstallProgress("amp", {
      installId: "install-1",
      state: "running",
      pct: 40,
      target: "host",
      errorCode: undefined,
      error: undefined,
    });

    unsubscribe();

    expect(snapshots).toHaveLength(2);
    expect(snapshots[1].amp?.host?.state).toBe("running");
    expect(snapshots[1].amp?.host?.pct).toBe(40);
  });

  it("emits when install state changes", () => {
    const snapshots: ReturnType<typeof getProviderInstallProgressSnapshot>[] = [];
    const unsubscribe = subscribeProviderInstallProgress((snapshot) => {
      snapshots.push(snapshot);
    });

    upsertProviderInstallProgress("goose", {
      installId: "install-2",
      state: "running",
      pct: 10,
      target: "host",
      errorCode: undefined,
      error: undefined,
    });
    upsertProviderInstallProgress("goose", {
      installId: "install-2",
      state: "succeeded",
      pct: 100,
      target: "host",
      errorCode: undefined,
      error: undefined,
    });

    unsubscribe();

    expect(snapshots).toHaveLength(3);
    expect(snapshots[2].goose?.host?.state).toBe("succeeded");
    expect(snapshots[2].goose?.host?.pct).toBe(100);
  });

  it("keeps concurrent provider installs separated by target", () => {
    upsertProviderInstallProgress("codex", {
      installId: "install-host",
      state: "running",
      pct: 10,
      target: "host",
      errorCode: undefined,
      error: undefined,
    });
    upsertProviderInstallProgress("codex", {
      installId: "install-container",
      state: "running",
      pct: 30,
      target: "container",
      errorCode: undefined,
      error: undefined,
    });

    const snapshot = getProviderInstallProgressSnapshot();
    expect(resolveProviderInstallProgressSession(snapshot, "codex", "host")?.installId).toBe("install-host");
    expect(resolveProviderInstallProgressSession(snapshot, "codex", "container")?.installId).toBe("install-container");

    removeProviderInstallProgress("codex", { target: "host", installId: "install-host" });

    const next = getProviderInstallProgressSnapshot();
    expect(resolveProviderInstallProgressSession(next, "codex", "host")).toBeUndefined();
    expect(resolveProviderInstallProgressSession(next, "codex", "container")?.installId).toBe("install-container");
  });

  it("keeps install progress isolated across owner scopes", () => {
    const daemon = createBrowserDaemonTargetScope("https://daemon-a.example");
    const hostOwner = createHostOwnerScope(daemon);
    const workspaceOwner = createWorkspaceOwnerScope(daemon, "ws-a");

    upsertProviderInstallProgressForScope(hostOwner, "codex", {
      installId: "install-host-owner",
      state: "running",
      pct: 10,
      target: "host",
      errorCode: undefined,
      error: undefined,
    });
    upsertProviderInstallProgressForScope(workspaceOwner, "codex", {
      installId: "install-workspace-owner",
      state: "running",
      pct: 40,
      target: "host",
      errorCode: undefined,
      error: undefined,
    });

    expect(
      resolveProviderInstallProgressSession(getProviderInstallProgressSnapshotForScope(hostOwner), "codex", "host")?.installId,
    ).toBe("install-host-owner");
    expect(
      resolveProviderInstallProgressSession(getProviderInstallProgressSnapshotForScope(workspaceOwner), "codex", "host")?.installId,
    ).toBe("install-workspace-owner");

    removeProviderInstallProgressForScope(hostOwner, "codex", { installId: "install-host-owner" });

    expect(resolveProviderInstallProgressSession(getProviderInstallProgressSnapshotForScope(hostOwner), "codex")).toBeUndefined();
    expect(
      resolveProviderInstallProgressSession(getProviderInstallProgressSnapshotForScope(workspaceOwner), "codex", "host")?.installId,
    ).toBe("install-workspace-owner");
  });
});
