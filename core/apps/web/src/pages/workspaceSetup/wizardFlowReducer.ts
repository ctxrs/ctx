import type { WizardRoutePlan, WizardStepKey } from "./wizardFlow";

export type WizardSelections = Record<string, string>;

export type WizardFlowState = {
  currentStepKey: WizardStepKey;
  selections: WizardSelections;
  routePlan: WizardRoutePlan | null;
  routePlanningBusy: boolean;
};

export type WizardFlowAction =
  | {
      type: "select_option";
      stepKey: string;
      optionId: string;
    }
  | {
      type: "clear_selection";
      stepKey: string;
    }
  | {
      type: "set_step";
      stepKey: WizardStepKey;
    }
  | {
      type: "invalidate_route_plan";
    }
  | {
      type: "set_route_plan";
      routePlan: WizardRoutePlan | null;
    }
  | {
      type: "set_route_planning_busy";
      busy: boolean;
    };

export const createInitialWizardFlowState = (): WizardFlowState => ({
  currentStepKey: "location",
  selections: {},
  routePlan: null,
  routePlanningBusy: false,
});

const nextSelectionsForOption = (
  selections: WizardSelections,
  stepKey: string,
  optionId: string,
): WizardSelections => {
  const next = { ...selections, [stepKey]: optionId };
  if (stepKey === "container" && optionId === "host") {
    delete next.network;
  }
  return next;
};

export const wizardFlowReducer = (
  state: WizardFlowState,
  action: WizardFlowAction,
): WizardFlowState => {
  switch (action.type) {
    case "select_option":
      return {
        ...state,
        selections: nextSelectionsForOption(state.selections, action.stepKey, action.optionId),
      };
    case "clear_selection": {
      if (!Object.prototype.hasOwnProperty.call(state.selections, action.stepKey)) {
        return state;
      }
      const nextSelections = { ...state.selections };
      delete nextSelections[action.stepKey];
      return {
        ...state,
        selections: nextSelections,
      };
    }
    case "set_step":
      return {
        ...state,
        currentStepKey: action.stepKey,
      };
    case "invalidate_route_plan":
      return {
        ...state,
        routePlan: null,
        routePlanningBusy: false,
      };
    case "set_route_plan":
      return {
        ...state,
        routePlan: action.routePlan,
      };
    case "set_route_planning_busy":
      return {
        ...state,
        routePlanningBusy: action.busy,
      };
    default:
      return state;
  }
};
