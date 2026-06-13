import { beforeEach, describe, expect, it, vi } from "vitest";

const {
  apiAnyMock,
  trackWorkspaceCreatedMock,
  trackWorkspaceCreateSubmittedMock,
  trackWorkspaceCreateSucceededMock,
  trackWorkspaceCreateFailedMock,
} = vi.hoisted(() => ({
  apiAnyMock: vi.fn(),
  trackWorkspaceCreatedMock: vi.fn(),
  trackWorkspaceCreateSubmittedMock: vi.fn(),
  trackWorkspaceCreateSucceededMock: vi.fn(),
  trackWorkspaceCreateFailedMock: vi.fn(),
}));

vi.mock("./clientBase", async () => {
  const actual = await vi.importActual<typeof import("./clientBase")>("./clientBase");
  return {
    ...actual,
    apiAny: apiAnyMock,
  };
});

vi.mock("../utils/analytics", async () => {
  const actual = await vi.importActual<typeof import("../utils/analytics")>("../utils/analytics");
  return {
    ...actual,
    trackWorkspaceCreated: trackWorkspaceCreatedMock,
    trackWorkspaceCreateSubmitted: trackWorkspaceCreateSubmittedMock,
    trackWorkspaceCreateSucceeded: trackWorkspaceCreateSucceededMock,
    trackWorkspaceCreateFailed: trackWorkspaceCreateFailedMock,
  };
});

import { createWorkspace } from "./clientWorkspaces";

describe("createWorkspace analytics", () => {
  beforeEach(() => {
    apiAnyMock.mockReset();
    trackWorkspaceCreatedMock.mockReset();
    trackWorkspaceCreateSubmittedMock.mockReset();
    trackWorkspaceCreateSucceededMock.mockReset();
    trackWorkspaceCreateFailedMock.mockReset();
  });

  it("tracks local workspace creation by default", async () => {
    apiAnyMock.mockResolvedValue({ id: "ws-1" });

    await createWorkspace("/tmp/repo-a", "Repo A");

    expect(trackWorkspaceCreateSubmittedMock).toHaveBeenCalledTimes(1);
    expect(trackWorkspaceCreateSubmittedMock).toHaveBeenCalledWith({
      workspaceKind: "local",
      source: "unknown",
    });
    expect(trackWorkspaceCreatedMock).toHaveBeenCalledTimes(1);
    expect(trackWorkspaceCreatedMock).toHaveBeenCalledWith("local");
    expect(trackWorkspaceCreateSucceededMock).toHaveBeenCalledTimes(1);
    expect(trackWorkspaceCreateSucceededMock).toHaveBeenCalledWith({
      workspaceKind: "local",
      source: "unknown",
    });
    expect(trackWorkspaceCreateFailedMock).not.toHaveBeenCalled();
  });

  it("tracks remote workspace creation when workspace kind is provided", async () => {
    apiAnyMock.mockResolvedValue({ id: "ws-2" });

    await createWorkspace("/tmp/repo-b", "Repo B", "remote", "wizard");

    expect(trackWorkspaceCreatedMock).toHaveBeenCalledTimes(1);
    expect(trackWorkspaceCreatedMock).toHaveBeenCalledWith("remote");
    expect(trackWorkspaceCreateSubmittedMock).toHaveBeenCalledWith({
      workspaceKind: "remote",
      source: "wizard",
    });
    expect(trackWorkspaceCreateSucceededMock).toHaveBeenCalledWith({
      workspaceKind: "remote",
      source: "wizard",
    });
  });

  it("tracks failed workspace creation with failure kind", async () => {
    apiAnyMock.mockRejectedValueOnce(new Error("request failed"));

    await expect(createWorkspace("/tmp/repo-c", "Repo C", "local", "api")).rejects.toThrow("request failed");

    expect(trackWorkspaceCreateSubmittedMock).toHaveBeenCalledWith({
      workspaceKind: "local",
      source: "api",
    });
    expect(trackWorkspaceCreateFailedMock).toHaveBeenCalledWith({
      workspaceKind: "local",
      source: "api",
      failureKind: "request_error",
    });
    expect(trackWorkspaceCreateSucceededMock).not.toHaveBeenCalled();
  });
});
