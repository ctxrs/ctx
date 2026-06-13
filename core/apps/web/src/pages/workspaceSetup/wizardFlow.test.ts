import { describe, expect, it } from "vitest";
import {
  buildWizardStepPath,
  nextAfterAuthImport,
  nextAfterHarnessDownloads,
  nextBoundaryStep,
  resolveWizardCurrentStepKey,
  stepKeyOffset,
  type WizardRoutePlan,
} from "./wizardFlow";
import {
  createInitialWizardFlowState,
  wizardFlowReducer,
} from "./wizardFlowReducer";

const routePlan = (overrides?: Partial<WizardRoutePlan>): WizardRoutePlan => ({
  targetKey: "local",
  containerSelection: "sandbox",
  includeHarnessDownloads: false,
  includeAuthImport: false,
  includeTitling: false,
  ...overrides,
});

describe("wizardFlow", () => {
  it("builds the stable local host path", () => {
    expect(
      buildWizardStepPath({
        containerSelection: "host",
        routePlan: routePlan({ containerSelection: "host" }),
      }),
    ).toEqual([
      "location",
      "container",
      "source",
      "setup",
      "merge-queue",
      "confirm",
    ]);
  });

  it("includes optional post-container steps from the frozen route plan", () => {
    expect(
      buildWizardStepPath({
        containerSelection: "sandbox",
        routePlan: routePlan({
          includeHarnessDownloads: true,
          includeAuthImport: true,
          includeTitling: true,
        }),
      }),
    ).toEqual([
      "location",
      "container",
      "harness-downloads",
      "auth-import",
      "session-titling",
      "source",
      "network",
      "setup",
      "merge-queue",
      "confirm",
    ]);
  });

  it("preserves the current optional step even if the latest route plan no longer includes it", () => {
    expect(
      buildWizardStepPath({
        containerSelection: "sandbox",
        routePlan: routePlan({ includeHarnessDownloads: false }),
        currentStepKey: "harness-downloads",
      }),
    ).toContain("harness-downloads");
  });

  it("resolves the current step without falling back to location when a later optional step disappears", () => {
    const keys = buildWizardStepPath({
      containerSelection: "sandbox",
      routePlan: routePlan({ includeAuthImport: true }),
    });
    expect(resolveWizardCurrentStepKey(keys, "harness-downloads", 2)).toBe("auth-import");
  });

  it("uses the frozen route plan to determine explicit forward routing", () => {
    const plan = routePlan({
      includeHarnessDownloads: true,
      includeAuthImport: true,
      includeTitling: true,
    });
    expect(nextBoundaryStep(plan)).toBe("harness-downloads");
    expect(nextAfterHarnessDownloads(plan)).toBe("auth-import");
    expect(nextAfterAuthImport(plan)).toBe("session-titling");
  });

  it("walks backward and forward over the explicit path", () => {
    const keys = buildWizardStepPath({
      containerSelection: "sandbox",
      routePlan: routePlan({
        includeHarnessDownloads: true,
        includeAuthImport: true,
      }),
    });
    expect(stepKeyOffset(keys, "container", 1)).toBe("harness-downloads");
    expect(stepKeyOffset(keys, "source", -1)).toBe("auth-import");
  });

  it("clears network selection when the flow switches back to host mode", () => {
    const state = wizardFlowReducer(
      {
        ...createInitialWizardFlowState(),
        selections: {
          container: "sandbox",
          network: "allowlist",
        },
      },
      {
        type: "select_option",
        stepKey: "container",
        optionId: "host",
      },
    );

    expect(state.selections.container).toBe("host");
    expect(state.selections.network).toBeUndefined();
  });

  it("invalidates the frozen route plan without resetting the current step", () => {
    const state = wizardFlowReducer(
      {
        ...createInitialWizardFlowState(),
        currentStepKey: "harness-downloads",
        routePlanningBusy: true,
        routePlan: routePlan({
          includeHarnessDownloads: true,
          includeAuthImport: true,
        }),
      },
      { type: "invalidate_route_plan" },
    );

    expect(state.currentStepKey).toBe("harness-downloads");
    expect(state.routePlanningBusy).toBe(false);
    expect(state.routePlan).toBeNull();
  });
});
