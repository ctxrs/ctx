import { describe, expect, it } from "vitest";
import {
  buildWorkspaceSetupCreateIntent,
  parseNetworkAllowlist,
  resolveCreateErrorStepKey,
} from "./createHandoff";

describe("createHandoff", () => {
  it("parses allowlist entries deterministically", () => {
    expect(parseNetworkAllowlist(" api.example.com \n\nopenrouter.ai\n")).toEqual([
      "api.example.com",
      "openrouter.ai",
    ]);
  });

  it("maps common create failures back to the owning step", () => {
    expect(resolveCreateErrorStepKey("Remote host is required (user@host).")).toBe("location");
    expect(resolveCreateErrorStepKey("Selected folder is not a repo.")).toBe("source");
    expect(resolveCreateErrorStepKey("Remote session titling requires base URL, API key, and model.")).toBe("session-titling");
    expect(resolveCreateErrorStepKey("unexpected")).toBeNull();
  });

  it("builds an explicit create intent object without mutation", () => {
    const intent = buildWorkspaceSetupCreateIntent({
      selections: {},
      sourcePath: "/tmp/repo",
      repoUrl: "",
      repoBranch: "",
      workspaceName: "repo",
      networkAllowlist: "",
      useSandboxStaging: false,
      importRepoStatus: "idle",
      importRepoNote: null,
      targetBranch: "main",
      verifyCommand: "",
      mergeQueueSkipped: false,
      pushOnSuccess: false,
      pushRemote: "origin",
      pushBranch: "main",
      setupHook: "",
      titlingStepVisible: false,
      titlingMode: "skip",
      titlingRemoteValid: false,
      titlingPersistError: null,
    });

    expect(intent).toMatchObject({
      sourcePath: "/tmp/repo",
      workspaceName: "repo",
      targetBranch: "main",
    });
  });
});
