import { describe, expect, it } from "vitest";
import type { WizardRoutePlan } from "./wizardFlow";
import {
  createInitialWorkspaceSetupMachineState,
  workspaceSetupMachineReducer,
  type WorkspaceSetupMachineSnapshot,
} from "./workspaceSetupMachine";

const routePlanFixture = (overrides?: Partial<WizardRoutePlan>): WizardRoutePlan => ({
  targetKey: "local|container",
  containerSelection: "sandbox",
  includeHarnessDownloads: false,
  includeAuthImport: false,
  includeTitling: false,
  ...overrides,
});

const snapshotFixture = (
  overrides?: Partial<WorkspaceSetupMachineSnapshot>,
): WorkspaceSetupMachineSnapshot => ({
  stepKey: "location",
  routePlan: null,
  locationSelection: "local",
  harnessInstallBusy: false,
  harnessInstallError: null,
  selectedHarnessReadyToStartCount: 0,
  selectedHarnessRunningCount: 0,
  selectedHarnessFailedCount: 0,
  titlingMode: "unset",
  titlingRemoteValid: false,
  ...overrides,
});

describe("workspaceSetupMachine", () => {
  it("requests remote verification before advancing from the location step", () => {
    const state = workspaceSetupMachineReducer(
      createInitialWorkspaceSetupMachineState(),
      {
        type: "next_requested",
        snapshot: snapshotFixture({
          stepKey: "location",
          locationSelection: "remote",
        }),
      },
    );

    expect(state.pendingEffects).toEqual([
      {
        id: 1,
        kind: "run_command",
        command: { kind: "verify_remote_connection" },
      },
    ]);

    const completed = workspaceSetupMachineReducer(state, {
      type: "command_completed",
      effectId: 1,
      result: {
        kind: "verify_remote_connection",
        connected: true,
      },
      snapshot: snapshotFixture({
        stepKey: "location",
        locationSelection: "remote",
      }),
    });

    expect(completed.pendingEffects.at(-1)).toEqual({
      id: 2,
      kind: "go_to_step",
      stepKey: "container",
    });
  });

  it("routes container planning through an explicit command and advances to the planned boundary step", () => {
    const planning = workspaceSetupMachineReducer(
      createInitialWorkspaceSetupMachineState(),
      {
        type: "option_selected",
        stepKey: "container",
        optionId: "sandbox",
        snapshot: snapshotFixture({
          stepKey: "container",
        }),
      },
    );

    expect(planning.pendingEffects).toEqual([
      {
        id: 1,
        kind: "run_command",
        command: {
          kind: "ensure_route_plan",
          containerSelectionOverride: "sandbox",
        },
      },
    ]);

    const advanced = workspaceSetupMachineReducer(planning, {
      type: "command_completed",
      effectId: 1,
      result: {
        kind: "ensure_route_plan",
        routePlan: routePlanFixture({
          includeHarnessDownloads: true,
        }),
      },
      snapshot: snapshotFixture({
        stepKey: "container",
      }),
    });

    expect(advanced.pendingEffects.at(-1)).toEqual({
      id: 2,
      kind: "go_to_step",
      stepKey: "harness-downloads",
    });
  });

  it("uses the returned route plan to update flow state after auth import", () => {
    const state = workspaceSetupMachineReducer(
      createInitialWorkspaceSetupMachineState(),
      {
        type: "command_completed",
        effectId: 1,
        result: {
          kind: "advance_auth_import",
          routePlan: routePlanFixture({
            includeAuthImport: true,
            includeTitling: true,
          }),
        },
        snapshot: snapshotFixture({
          stepKey: "auth-import",
          routePlan: routePlanFixture({
            includeAuthImport: true,
            includeTitling: true,
          }),
        }),
      },
    );

    expect(state).toEqual(createInitialWorkspaceSetupMachineState());

    const activeState = {
      ...createInitialWorkspaceSetupMachineState(),
      activeCommand: {
        effectId: 1,
        kind: "advance_auth_import" as const,
      },
    };

    const completed = workspaceSetupMachineReducer(activeState, {
      type: "command_completed",
      effectId: 1,
      result: {
        kind: "advance_auth_import",
        routePlan: routePlanFixture({
          includeAuthImport: true,
          includeTitling: true,
        }),
      },
      snapshot: snapshotFixture({
        stepKey: "auth-import",
        routePlan: routePlanFixture({
          includeAuthImport: true,
          includeTitling: true,
        }),
      }),
    });

    expect(completed.pendingEffects).toEqual([
      {
        id: 1,
        kind: "set_route_plan",
        routePlan: routePlanFixture({
          includeAuthImport: true,
          includeTitling: true,
        }),
      },
      {
        id: 2,
        kind: "go_to_step",
        stepKey: "session-titling",
      },
    ]);
  });

  it("uses the returned route plan to advance after harness downloads complete", () => {
    const completed = workspaceSetupMachineReducer(
      {
        ...createInitialWorkspaceSetupMachineState(),
        activeCommand: {
          effectId: 3,
          kind: "advance_harness_downloads",
        },
        nextEffectId: 4,
      },
      {
        type: "command_completed",
        effectId: 3,
        result: {
          kind: "advance_harness_downloads",
          routePlan: routePlanFixture({
            includeAuthImport: true,
          }),
        },
        snapshot: snapshotFixture({
          stepKey: "harness-downloads",
          routePlan: routePlanFixture({
            includeAuthImport: true,
          }),
        }),
      },
    );

    expect(completed.pendingEffects).toEqual([
      {
        id: 4,
        kind: "set_route_plan",
        routePlan: routePlanFixture({
          includeAuthImport: true,
        }),
      },
      {
        id: 5,
        kind: "go_to_step",
        stepKey: "auth-import",
      },
    ]);
  });

  it("routes harness downloads through provisioning even when a stale snapshot reports nothing startable", () => {
    const state = workspaceSetupMachineReducer(
      createInitialWorkspaceSetupMachineState(),
      {
        type: "next_requested",
        snapshot: snapshotFixture({
          stepKey: "harness-downloads",
          routePlan: routePlanFixture({
            includeAuthImport: true,
          }),
          selectedHarnessReadyToStartCount: 0,
        }),
      },
    );

    expect(state.pendingEffects).toEqual([
      {
        id: 1,
        kind: "run_command",
        command: {
          kind: "advance_harness_downloads",
        },
      },
    ]);
  });

  it("starts harness downloads in the background when skip is requested", () => {
    const state = workspaceSetupMachineReducer(
      createInitialWorkspaceSetupMachineState(),
      {
        type: "skip_harness_downloads_requested",
        snapshot: snapshotFixture({
          stepKey: "harness-downloads",
          routePlan: routePlanFixture({
            includeAuthImport: true,
          }),
        }),
      },
    );

    expect(state.pendingEffects).toEqual([
      {
        id: 1,
        kind: "go_to_step",
        stepKey: "auth-import",
      },
      {
        id: 2,
        kind: "run_command",
        command: {
          kind: "advance_harness_downloads",
          clearSelections: true,
        },
      },
    ]);
  });

  it("ignores late auth-import completions after the user has already navigated back", () => {
    const activeState = {
      ...createInitialWorkspaceSetupMachineState(),
      activeCommand: {
        effectId: 4,
        kind: "advance_auth_import" as const,
      },
      nextEffectId: 5,
    };

    const completed = workspaceSetupMachineReducer(activeState, {
      type: "command_completed",
      effectId: 4,
      result: {
        kind: "advance_auth_import",
        routePlan: routePlanFixture({
          includeAuthImport: true,
          includeTitling: true,
        }),
      },
      snapshot: snapshotFixture({
        stepKey: "container",
      }),
    });

    expect(completed.pendingEffects).toEqual([]);
    expect(completed.activeCommand).toBeNull();
  });

  it("ignores late container planning completions after the user has left the container step", () => {
    const activeState = {
      ...createInitialWorkspaceSetupMachineState(),
      activeCommand: {
        effectId: 2,
        kind: "ensure_route_plan" as const,
      },
      nextEffectId: 3,
    };

    const completed = workspaceSetupMachineReducer(activeState, {
      type: "command_completed",
      effectId: 2,
      result: {
        kind: "ensure_route_plan",
        routePlan: routePlanFixture({
          includeAuthImport: true,
        }),
      },
      snapshot: snapshotFixture({
        stepKey: "location",
      }),
    });

    expect(completed.pendingEffects).toEqual([]);
    expect(completed.activeCommand).toBeNull();
  });

  it("surfaces titling validation errors before attempting persistence", () => {
    const state = workspaceSetupMachineReducer(
      createInitialWorkspaceSetupMachineState(),
      {
        type: "next_requested",
        snapshot: snapshotFixture({
          stepKey: "session-titling",
          routePlan: routePlanFixture({
            includeTitling: true,
          }),
          titlingMode: "remote",
          titlingRemoteValid: false,
        }),
      },
    );

    expect(state.pendingEffects).toEqual([
      {
        id: 1,
        kind: "set_titling_persist_error",
        message: "Remote titling needs base URL, API key, and model.",
      },
    ]);
  });

  it("auto-advances once selected harness installs fail after background progress settles", () => {
    const state = workspaceSetupMachineReducer(
      createInitialWorkspaceSetupMachineState(),
      {
        type: "provisioning_snapshot_changed",
        snapshot: snapshotFixture({
          stepKey: "harness-downloads",
          routePlan: routePlanFixture({
            includeTitling: true,
          }),
          selectedHarnessReadyToStartCount: 0,
          selectedHarnessRunningCount: 0,
          selectedHarnessFailedCount: 1,
        }),
      },
    );

    expect(state.pendingEffects).toEqual([
      {
        id: 1,
        kind: "go_to_step",
        stepKey: "session-titling",
      },
    ]);
  });
});
