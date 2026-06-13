import type { SessionTitlingMode } from "./WorkspaceSetupPage.logic";
import type { WizardRoutePlan, WizardStepKey } from "./wizardFlow";
import type {
  WorkspaceSetupMachineCommand,
  WorkspaceSetupMachineCommandResult,
  WorkspaceSetupMachineEffect,
} from "./workspaceSetupMachine";

export type WorkspaceSetupMachineCommandHandlers = {
  verifyRemoteConnection: () => Promise<boolean>;
  ensureRoutePlanForSelection: (containerSelectionOverride?: string) => Promise<WizardRoutePlan | null>;
  advanceFromAuthImportStep: (options?: { clearSelections?: boolean }) => Promise<WizardRoutePlan | null>;
  advanceFromHarnessDownloadsStep: (options?: { clearSelections?: boolean }) => Promise<WizardRoutePlan | null>;
  onSelectTitlingLocal: () => boolean;
  ensureTitlingPersistedForCurrentTarget: () => Promise<boolean>;
  preflightSourceStep: () => Promise<boolean>;
};

export type WorkspaceSetupMachineEffectHandlers = {
  goToStepKey: (stepKey: WizardStepKey) => void;
  goRelativeStep: (delta: number) => void;
  setTitlingPersistError: (message: string | null) => void;
  setTitlingMode: (mode: SessionTitlingMode) => void;
  invalidateTitlingPersisted: () => void;
  setRoutePlan: (routePlan: WizardRoutePlan | null) => void;
};

export const executeWorkspaceSetupMachineCommand = async (
  command: WorkspaceSetupMachineCommand,
  handlers: WorkspaceSetupMachineCommandHandlers,
): Promise<WorkspaceSetupMachineCommandResult> => {
  switch (command.kind) {
    case "verify_remote_connection":
      return {
        kind: command.kind,
        connected: await handlers.verifyRemoteConnection(),
      };
    case "ensure_route_plan":
      return {
        kind: command.kind,
        routePlan: await handlers.ensureRoutePlanForSelection(command.containerSelectionOverride),
      };
    case "advance_auth_import":
      return {
        kind: command.kind,
        routePlan: await handlers.advanceFromAuthImportStep({
          clearSelections: command.clearSelections,
        }),
      };
    case "advance_harness_downloads":
      return {
        kind: command.kind,
        routePlan: await handlers.advanceFromHarnessDownloadsStep({
          clearSelections: command.clearSelections,
        }),
      };
    case "select_titling_local":
      return {
        kind: command.kind,
        started: handlers.onSelectTitlingLocal(),
      };
    case "persist_titling":
      return {
        kind: command.kind,
        persisted: await handlers.ensureTitlingPersistedForCurrentTarget(),
      };
    case "preflight_source_step":
      return {
        kind: command.kind,
        preflightOk: await handlers.preflightSourceStep(),
      };
    default: {
      const exhaustiveCheck: never = command;
      return exhaustiveCheck;
    }
  }
};

export const applyWorkspaceSetupMachineEffect = (
  effect: Exclude<WorkspaceSetupMachineEffect, { kind: "run_command" }>,
  handlers: WorkspaceSetupMachineEffectHandlers,
): void => {
  switch (effect.kind) {
    case "go_to_step":
      handlers.goToStepKey(effect.stepKey);
      return;
    case "go_relative_step":
      handlers.goRelativeStep(effect.delta);
      return;
    case "set_titling_persist_error":
      handlers.setTitlingPersistError(effect.message);
      return;
    case "set_titling_mode":
      handlers.setTitlingMode(effect.mode);
      return;
    case "invalidate_titling_persisted":
      handlers.invalidateTitlingPersisted();
      return;
    case "set_route_plan":
      handlers.setRoutePlan(effect.routePlan);
      return;
    default: {
      const exhaustiveCheck: never = effect;
      return exhaustiveCheck;
    }
  }
};
