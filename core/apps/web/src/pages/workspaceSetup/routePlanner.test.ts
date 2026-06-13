import { describe, expect, it } from "vitest";
import {
  createDesktopLocalDaemonTargetScope,
  createProvisioningScope,
} from "../../state/scopeIdentity";
import {
  buildOnboardingAfterConnectResult,
  buildWizardRoutePlan,
  resolveRoutePlanInsertionStep,
} from "./routePlanner";
import {
  serializeWorkspaceSetupRouteScope,
  type WorkspaceSetupProvisioningSnapshot,
  type WorkspaceSetupRouteScope,
} from "./workflowTypes";

const routeScope = (
  overrides?: Partial<WorkspaceSetupRouteScope>,
): WorkspaceSetupRouteScope => ({
  provisioningScope: createProvisioningScope(createDesktopLocalDaemonTargetScope(), "container"),
  containerSelection: "sandbox",
  ...overrides,
});

const snapshot = (
  overrides?: Partial<WorkspaceSetupProvisioningSnapshot>,
): WorkspaceSetupProvisioningSnapshot => ({
  routeScope: routeScope(),
  authImportStatus: "ready",
  authImportCandidateCount: 0,
  harnessCandidatesStatus: "ready",
  missingHarnessCount: 0,
  titlingProbeStatus: "ready",
  titlingRequired: false,
  titlingMode: "unset",
  ...overrides,
});

describe("routePlanner", () => {
  it("builds a route plan from explicit provisioning snapshot state", () => {
    expect(buildWizardRoutePlan(snapshot({
      authImportCandidateCount: 2,
      missingHarnessCount: 1,
      titlingRequired: true,
      titlingMode: "remote",
    }))).toEqual({
      targetKey: serializeWorkspaceSetupRouteScope(routeScope()),
      containerSelection: "sandbox",
      includeHarnessDownloads: true,
      includeAuthImport: true,
      includeTitling: true,
    });
  });

  it("suppresses titling insertion when the user already chose skip", () => {
    expect(buildWizardRoutePlan(snapshot({
      titlingRequired: true,
      titlingMode: "skip",
    })).includeTitling).toBe(false);
  });

  it("prefers harness downloads before auth import and titling for new insertions", () => {
    const plan = buildWizardRoutePlan(snapshot({
      authImportCandidateCount: 1,
      missingHarnessCount: 1,
      titlingRequired: true,
      titlingMode: "remote",
    }));
    expect(resolveRoutePlanInsertionStep(plan, null)).toBe("harness-downloads");
  });

  it("reuses prior onboarding insertions only for the same route key", () => {
    const previousPlan = {
      targetKey: serializeWorkspaceSetupRouteScope(routeScope()),
      containerSelection: "sandbox",
      includeHarnessDownloads: true,
      includeAuthImport: false,
      includeTitling: false,
    };
    const plan = buildWizardRoutePlan(snapshot({
      authImportCandidateCount: 1,
      missingHarnessCount: 1,
    }));

    expect(resolveRoutePlanInsertionStep(plan, previousPlan)).toBe("auth-import");
  });

  it("does not suppress onboarding insertions when the route key changes", () => {
    const previousPlan = {
      targetKey: serializeWorkspaceSetupRouteScope(routeScope()),
      containerSelection: "sandbox",
      includeHarnessDownloads: true,
      includeAuthImport: true,
      includeTitling: true,
    };
    const plan = buildWizardRoutePlan(snapshot({
      routeScope: {
        provisioningScope: createProvisioningScope(createDesktopLocalDaemonTargetScope(), "container"),
        containerSelection: "host",
      },
      authImportCandidateCount: 1,
      missingHarnessCount: 1,
      titlingRequired: true,
      titlingMode: "remote",
    }));

    expect(resolveRoutePlanInsertionStep(plan, previousPlan)).toBe("harness-downloads");
  });

  it("can suppress titling insertion during create-time rechecks", () => {
    const result = buildOnboardingAfterConnectResult(snapshot({
      titlingRequired: true,
      titlingMode: "remote",
    }), null, { allowTitlingInsertion: false });
    expect(result.insertionStep).toBeNull();
    expect(result.routePlan.includeTitling).toBe(true);
  });

  it("keeps onboarding steps visible when a scoped refresh fails", () => {
    const plan = buildWizardRoutePlan(snapshot({
      authImportStatus: "error",
      harnessCandidatesStatus: "error",
      titlingProbeStatus: "error",
      titlingMode: "remote",
    }));

    expect(plan.includeAuthImport).toBe(true);
    expect(plan.includeHarnessDownloads).toBe(true);
    expect(plan.includeTitling).toBe(false);
  });
});
