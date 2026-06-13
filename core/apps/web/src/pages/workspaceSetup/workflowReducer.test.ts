import { describe, expect, it } from "vitest";
import { createDesktopSshDaemonTargetScope } from "../../state/scopeIdentity";
import {
  createInitialWorkflowDraftState,
  makeTargetDraftFieldSetter,
  makeDraftFieldSetter,
  workspaceSetupWorkflowReducer,
} from "./workflowReducer";
import { deriveWorkspaceSetupEffectiveTarget } from "./workflowTypes";

describe("workflowReducer", () => {
  it("updates a draft field through the shared setter helper", () => {
    const actions: Parameters<typeof workspaceSetupWorkflowReducer>[1][] = [];
    const dispatch = (action: Parameters<typeof workspaceSetupWorkflowReducer>[1]) => {
      actions.push(action);
    };
    const setSourcePath = makeDraftFieldSetter(dispatch, "sourcePath");
    setSourcePath("/tmp/repo");

    const next = actions.reduce(
      (state, action) => workspaceSetupWorkflowReducer(state, action),
      createInitialWorkflowDraftState(),
    );
    expect(next.sourcePath).toBe("/tmp/repo");
  });

  it("supports functional updates for draft fields", () => {
    const actions: Parameters<typeof workspaceSetupWorkflowReducer>[1][] = [];
    const dispatch = (action: Parameters<typeof workspaceSetupWorkflowReducer>[1]) => {
      actions.push(action);
    };
    const setPushOnSuccess = makeDraftFieldSetter(dispatch, "pushOnSuccess");
    setPushOnSuccess((prev) => !prev);

    const next = actions.reduce(
      (state, action) => workspaceSetupWorkflowReducer(state, action),
      createInitialWorkflowDraftState(),
    );
    expect(next.pushOnSuccess).toBe(true);
  });

  it("supports functional updates for nested target draft fields", () => {
    const actions: Parameters<typeof workspaceSetupWorkflowReducer>[1][] = [];
    const dispatch = (action: Parameters<typeof workspaceSetupWorkflowReducer>[1]) => {
      actions.push(action);
    };
    const setRemoteHostInput = makeTargetDraftFieldSetter(dispatch, "remoteHostInput");
    setRemoteHostInput(() => "devbox.example");

    const next = actions.reduce(
      (state, action) => workspaceSetupWorkflowReducer(state, action),
      createInitialWorkflowDraftState(),
    );
    expect(next.targetDraft.remoteHostInput).toBe("devbox.example");
  });

  it("derives the effective remote target from the workflow-owned target draft", () => {
    const state = createInitialWorkflowDraftState();
    const next = workspaceSetupWorkflowReducer(state, {
      type: "update",
      updater: (current) => ({
        ...current,
        targetDraft: {
          ...current.targetDraft,
          remoteHostInput: "alice@devbox.example",
          remotePortInput: "4411",
          remoteDataDirInput: "/srv/ctx",
        },
      }),
    });

    expect(deriveWorkspaceSetupEffectiveTarget("remote", next.targetDraft)).toEqual({
      kind: "remote",
      targetKey: "ssh:alice@devbox.example:4411:/srv/ctx",
      daemonScope: createDesktopSshDaemonTargetScope({
        host: "devbox.example",
        user: "alice",
        port: 4411,
        dataDir: "/srv/ctx",
      }),
      hostInput: "alice@devbox.example",
      host: "devbox.example",
      user: "alice",
      portInput: "4411",
      port: 4411,
      dataDirInput: "/srv/ctx",
      dataDir: "/srv/ctx",
    });
  });
});
