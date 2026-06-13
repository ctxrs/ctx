import type { SessionTitlingMode } from "./WorkspaceSetupPage.logic";
import {
  nextAfterAuthImport,
  nextAfterHarnessDownloads,
  nextBoundaryStep,
  type WizardRoutePlan,
  type WizardStepKey,
} from "./wizardFlow";

export type WorkspaceSetupMachineSnapshot = {
  stepKey: WizardStepKey;
  routePlan: WizardRoutePlan | null;
  locationSelection: string | null | undefined;
  harnessInstallBusy: boolean;
  harnessInstallError: string | null;
  selectedHarnessReadyToStartCount: number;
  selectedHarnessRunningCount: number;
  selectedHarnessFailedCount: number;
  titlingMode: SessionTitlingMode;
  titlingRemoteValid: boolean;
};

export type WorkspaceSetupMachineCommand =
  | {
      kind: "verify_remote_connection";
    }
  | {
      kind: "ensure_route_plan";
      containerSelectionOverride?: string;
    }
  | {
      kind: "advance_auth_import";
      clearSelections?: boolean;
    }
  | {
      kind: "advance_harness_downloads";
      clearSelections?: boolean;
    }
  | {
      kind: "select_titling_local";
    }
  | {
      kind: "persist_titling";
    }
  | {
      kind: "preflight_source_step";
    };

export type WorkspaceSetupMachineCommandResult =
  | {
      kind: "verify_remote_connection";
      connected: boolean;
    }
  | {
      kind: "ensure_route_plan";
      routePlan: WizardRoutePlan | null;
    }
  | {
      kind: "advance_auth_import";
      routePlan: WizardRoutePlan | null;
    }
  | {
      kind: "advance_harness_downloads";
      routePlan: WizardRoutePlan | null;
    }
  | {
      kind: "select_titling_local";
      started: boolean;
    }
  | {
      kind: "persist_titling";
      persisted: boolean;
    }
  | {
      kind: "preflight_source_step";
      preflightOk: boolean;
    };

type WorkspaceSetupMachineBaseEffect = {
  id: number;
};

export type WorkspaceSetupMachineEffect =
  | (WorkspaceSetupMachineBaseEffect & {
      kind: "run_command";
      command: WorkspaceSetupMachineCommand;
    })
  | (WorkspaceSetupMachineBaseEffect & {
      kind: "go_to_step";
      stepKey: WizardStepKey;
    })
  | (WorkspaceSetupMachineBaseEffect & {
      kind: "go_relative_step";
      delta: number;
    })
  | (WorkspaceSetupMachineBaseEffect & {
      kind: "set_titling_persist_error";
      message: string | null;
    })
  | (WorkspaceSetupMachineBaseEffect & {
      kind: "set_titling_mode";
      mode: SessionTitlingMode;
    })
  | (WorkspaceSetupMachineBaseEffect & {
      kind: "invalidate_titling_persisted";
    })
  | (WorkspaceSetupMachineBaseEffect & {
      kind: "set_route_plan";
      routePlan: WizardRoutePlan | null;
    });

type WorkspaceSetupMachineEffectInput =
  | {
      kind: "run_command";
      command: WorkspaceSetupMachineCommand;
    }
  | {
      kind: "go_to_step";
      stepKey: WizardStepKey;
    }
  | {
      kind: "go_relative_step";
      delta: number;
    }
  | {
      kind: "set_titling_persist_error";
      message: string | null;
    }
  | {
      kind: "set_titling_mode";
      mode: SessionTitlingMode;
    }
  | {
      kind: "invalidate_titling_persisted";
    }
  | {
      kind: "set_route_plan";
      routePlan: WizardRoutePlan | null;
    };

type WorkspaceSetupMachineActiveCommand = {
  effectId: number;
  kind: WorkspaceSetupMachineCommand["kind"];
};

export type WorkspaceSetupMachineState = {
  nextEffectId: number;
  pendingEffects: WorkspaceSetupMachineEffect[];
  activeCommand: WorkspaceSetupMachineActiveCommand | null;
};

export type WorkspaceSetupMachineEvent =
  | {
      type: "option_selected";
      stepKey: string;
      optionId: string;
      snapshot: WorkspaceSetupMachineSnapshot;
    }
  | {
      type: "next_requested";
      snapshot: WorkspaceSetupMachineSnapshot;
    }
  | {
      type: "skip_auth_import_requested";
    }
  | {
      type: "skip_harness_downloads_requested";
      snapshot: WorkspaceSetupMachineSnapshot;
    }
  | {
      type: "select_titling_local_requested";
    }
  | {
      type: "skip_titling_requested";
      snapshot: WorkspaceSetupMachineSnapshot;
    }
  | {
      type: "provisioning_snapshot_changed";
      snapshot: WorkspaceSetupMachineSnapshot;
    }
  | {
      type: "command_completed";
      effectId: number;
      result: WorkspaceSetupMachineCommandResult;
      snapshot: WorkspaceSetupMachineSnapshot;
    }
  | {
      type: "effects_applied";
      effectIds: number[];
    };

export const createInitialWorkspaceSetupMachineState = (): WorkspaceSetupMachineState => ({
  nextEffectId: 1,
  pendingEffects: [],
  activeCommand: null,
});

const removePendingEffects = (
  state: WorkspaceSetupMachineState,
  effectIds: number[],
): WorkspaceSetupMachineState => {
  if (effectIds.length === 0) return state;
  const effectIdSet = new Set(effectIds);
  return {
    ...state,
    pendingEffects: state.pendingEffects.filter((effect) => !effectIdSet.has(effect.id)),
  };
};

const appendEffect = (
  state: WorkspaceSetupMachineState,
  effect: WorkspaceSetupMachineEffectInput,
): WorkspaceSetupMachineState => {
  const nextEffect = {
    ...effect,
    id: state.nextEffectId,
  } as WorkspaceSetupMachineEffect;

  return {
    nextEffectId: state.nextEffectId + 1,
    pendingEffects: [...state.pendingEffects, nextEffect],
    activeCommand: nextEffect.kind === "run_command"
      ? {
          effectId: nextEffect.id,
          kind: nextEffect.command.kind,
        }
      : state.activeCommand,
  };
};

const appendEffects = (
  initialState: WorkspaceSetupMachineState,
  effects: WorkspaceSetupMachineEffectInput[],
): WorkspaceSetupMachineState =>
  effects.reduce((state, effect) => appendEffect(state, effect), initialState);

export const shouldAutoAdvanceHarnessDownloads = (
  snapshot: WorkspaceSetupMachineSnapshot,
): boolean =>
  snapshot.stepKey === "harness-downloads"
  && !snapshot.harnessInstallBusy
  && !snapshot.harnessInstallError
  && snapshot.selectedHarnessReadyToStartCount === 0
  && snapshot.selectedHarnessRunningCount === 0
  && snapshot.selectedHarnessFailedCount > 0;

const clearActiveCommand = (
  state: WorkspaceSetupMachineState,
  effectId: number,
): WorkspaceSetupMachineState => {
  if (state.activeCommand?.effectId !== effectId) {
    return state;
  }
  return {
    ...state,
    activeCommand: null,
  };
};

const command_result_matches_visible_step = (
  result: WorkspaceSetupMachineCommandResult,
  snapshot: WorkspaceSetupMachineSnapshot,
): boolean => {
  switch (result.kind) {
    case "verify_remote_connection":
      return snapshot.stepKey === "location";
    case "ensure_route_plan":
      return snapshot.stepKey === "container";
    case "advance_auth_import":
      return snapshot.stepKey === "auth-import";
    case "advance_harness_downloads":
      return snapshot.stepKey === "harness-downloads";
    case "select_titling_local":
    case "persist_titling":
      return snapshot.stepKey === "session-titling";
    case "preflight_source_step":
      return snapshot.stepKey === "source";
    default: {
      const exhaustiveCheck: never = result;
      return exhaustiveCheck;
    }
  }
};

export const workspaceSetupMachineReducer = (
  state: WorkspaceSetupMachineState,
  event: WorkspaceSetupMachineEvent,
): WorkspaceSetupMachineState => {
  switch (event.type) {
    case "effects_applied":
      return removePendingEffects(state, event.effectIds);
    case "option_selected":
      if (event.stepKey === "location" && event.optionId === "local") {
        return appendEffect(state, {
          kind: "go_to_step",
          stepKey: "container",
        });
      }
      if (event.stepKey === "container") {
        return appendEffect(state, {
          kind: "run_command",
          command: {
            kind: "ensure_route_plan",
            containerSelectionOverride: event.optionId,
          },
        });
      }
      if (event.stepKey === "network" && event.optionId !== "allowlist") {
        return appendEffect(state, {
          kind: "go_relative_step",
          delta: 1,
        });
      }
      return state;
    case "next_requested": {
      const { snapshot } = event;
      switch (snapshot.stepKey) {
        case "location":
          if (snapshot.locationSelection === "remote") {
            return appendEffect(state, {
              kind: "run_command",
              command: { kind: "verify_remote_connection" },
            });
          }
          return appendEffect(state, {
            kind: "go_to_step",
            stepKey: "container",
          });
        case "container":
          return appendEffect(state, {
            kind: "run_command",
            command: { kind: "ensure_route_plan" },
          });
        case "auth-import":
          return appendEffect(state, {
            kind: "run_command",
            command: { kind: "advance_auth_import" },
          });
        case "harness-downloads":
          return appendEffect(state, {
            kind: "run_command",
            command: { kind: "advance_harness_downloads" },
          });
        case "session-titling":
          if (snapshot.titlingMode === "skip") {
            return appendEffects(state, [
              {
                kind: "set_titling_persist_error",
                message: null,
              },
              {
                kind: "set_route_plan",
                routePlan: snapshot.routePlan
                  ? { ...snapshot.routePlan, includeTitling: false }
                  : null,
              },
              {
                kind: "go_relative_step",
                delta: 1,
              },
            ]);
          }
          if (snapshot.titlingMode !== "remote" && snapshot.titlingMode !== "local") {
            return appendEffect(state, {
              kind: "set_titling_persist_error",
              message: "Choose a titling option or skip for now.",
            });
          }
          if (snapshot.titlingMode === "remote" && !snapshot.titlingRemoteValid) {
            return appendEffect(state, {
              kind: "set_titling_persist_error",
              message: "Remote titling needs base URL, API key, and model.",
            });
          }
          return appendEffects(state, [
            {
              kind: "set_titling_persist_error",
              message: null,
            },
            {
              kind: "run_command",
              command: { kind: "persist_titling" },
            },
          ]);
        case "source":
          return appendEffect(state, {
            kind: "run_command",
            command: { kind: "preflight_source_step" },
          });
        default:
          return appendEffect(state, {
            kind: "go_relative_step",
            delta: 1,
          });
      }
    }
    case "skip_auth_import_requested":
      return appendEffect(state, {
        kind: "run_command",
        command: {
          kind: "advance_auth_import",
          clearSelections: true,
        },
      });
    case "skip_harness_downloads_requested":
      return appendEffects(state, [
        {
          kind: "go_to_step",
          stepKey: nextAfterHarnessDownloads(event.snapshot.routePlan),
        },
        {
          kind: "run_command",
          command: {
            kind: "advance_harness_downloads",
            clearSelections: true,
          },
        },
      ]);
    case "select_titling_local_requested":
      return appendEffect(state, {
        kind: "run_command",
        command: { kind: "select_titling_local" },
      });
    case "skip_titling_requested":
      return appendEffects(state, [
        {
          kind: "invalidate_titling_persisted",
        },
        {
          kind: "set_titling_mode",
          mode: "skip",
        },
        {
          kind: "set_route_plan",
          routePlan: event.snapshot.routePlan
            ? { ...event.snapshot.routePlan, includeTitling: false }
            : null,
        },
        {
          kind: "go_relative_step",
          delta: 1,
        },
      ]);
    case "provisioning_snapshot_changed":
      if (!shouldAutoAdvanceHarnessDownloads(event.snapshot)) {
        return state;
      }
      return appendEffect(state, {
        kind: "go_to_step",
        stepKey: nextAfterHarnessDownloads(event.snapshot.routePlan),
      });
    case "command_completed": {
      const nextState = clearActiveCommand(state, event.effectId);
      if (nextState === state) {
        return state;
      }
      if (!command_result_matches_visible_step(event.result, event.snapshot)) {
        return nextState;
      }

      switch (event.result.kind) {
        case "verify_remote_connection":
          return event.result.connected
            ? appendEffect(nextState, { kind: "go_to_step", stepKey: "container" })
            : nextState;
        case "ensure_route_plan":
          return event.result.routePlan
            ? appendEffect(nextState, {
                kind: "go_to_step",
                stepKey: nextBoundaryStep(event.result.routePlan),
              })
            : nextState;
        case "advance_auth_import":
          return event.result.routePlan
            ? appendEffects(nextState, [
                {
                  kind: "set_route_plan",
                  routePlan: event.result.routePlan,
                },
                {
                  kind: "go_to_step",
                  stepKey: nextAfterAuthImport(event.result.routePlan),
                },
              ])
            : nextState;
        case "advance_harness_downloads":
          return event.result.routePlan
            ? appendEffects(nextState, [
                {
                  kind: "set_route_plan",
                  routePlan: event.result.routePlan,
                },
                {
                  kind: "go_to_step",
                  stepKey: nextAfterHarnessDownloads(event.result.routePlan),
                },
              ])
            : nextState;
        case "select_titling_local":
          return event.result.started
            ? appendEffect(nextState, {
                kind: "go_relative_step",
                delta: 1,
              })
            : nextState;
        case "persist_titling":
          return event.result.persisted
            ? appendEffect(nextState, {
                kind: "go_relative_step",
                delta: 1,
              })
            : nextState;
        case "preflight_source_step":
          return event.result.preflightOk
            ? appendEffect(nextState, {
                kind: "go_relative_step",
                delta: 1,
              })
            : nextState;
        default:
          return nextState;
      }
    }
    default:
      return state;
  }
};
