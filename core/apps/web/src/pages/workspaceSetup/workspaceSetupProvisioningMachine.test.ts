import { describe, expect, it } from "vitest";
import {
  createDesktopLocalDaemonTargetScope,
  createProvisioningScope,
} from "../../state/scopeIdentity";
import {
  beginWorkspaceSetupProvisioningRefresh,
  completeWorkspaceSetupAuthImportRefresh,
  completeWorkspaceSetupHarnessCandidatesRefresh,
  completeWorkspaceSetupTitlingProbeRefresh,
  createInitialWorkspaceSetupProvisioningMachineState,
  failWorkspaceSetupAuthImportRefresh,
  failWorkspaceSetupHarnessCandidatesRefresh,
} from "./workspaceSetupProvisioningMachine";
import { serializeWorkspaceSetupRouteScope, type WorkspaceSetupRouteScope } from "./workflowTypes";
import type { HarnessInstallProviderRow } from "./wizardTypes";
import type { ProviderAuthImportCandidate } from "../../api/client";

const routeScopeFixture = (
  overrides?: Partial<WorkspaceSetupRouteScope>,
): WorkspaceSetupRouteScope => ({
  provisioningScope: createProvisioningScope(createDesktopLocalDaemonTargetScope(), "container"),
  containerSelection: "sandbox",
  ...overrides,
});

const harnessRow = (overrides?: Partial<HarnessInstallProviderRow>): HarnessInstallProviderRow => ({
  providerId: "codex",
  label: "Codex",
  installed: false,
  healthy: false,
  installSupported: true,
  installRunning: false,
  installTarget: "container",
  ...overrides,
});

const authImportCandidate = (
  overrides?: Partial<ProviderAuthImportCandidate>,
): ProviderAuthImportCandidate => ({
  id: "acct-1",
  provider_id: "codex",
  provider_label: "Codex",
  kind: "file",
  path: "/tmp/codex.json",
  signal_strength: "high",
  confidence: "high",
  parse_status: "parsed",
  ...overrides,
});

describe("workspaceSetupProvisioningMachine", () => {
  it("drops stale completions after the route scope changes", () => {
    const scopeA = routeScopeFixture();
    const scopeB = routeScopeFixture({ containerSelection: "host" });

    const startedA = beginWorkspaceSetupProvisioningRefresh(
      createInitialWorkspaceSetupProvisioningMachineState(),
      {
        routeScope: scopeA,
        refreshReason: "ensure_route_plan",
        titlingMode: "unset",
        previousPlan: null,
      },
    );
    const startedB = beginWorkspaceSetupProvisioningRefresh(startedA.state, {
      routeScope: scopeB,
      refreshReason: "ensure_route_plan",
      titlingMode: "unset",
      previousPlan: null,
    });

    const staleAuthRequest = startedA.requests.find((request) => request.resource === "authImport");
    const currentAuthRequest = startedB.requests.find((request) => request.resource === "authImport");
    const currentHarnessRequest = startedB.requests.find((request) => request.resource === "harnessCandidates");
    const currentTitlingRequest = startedB.requests.find((request) => request.resource === "titlingProbe");

    expect(staleAuthRequest).toBeDefined();
    expect(currentAuthRequest).toBeDefined();
    expect(currentHarnessRequest).toBeDefined();
    expect(currentTitlingRequest).toBeDefined();

    const staleCompleted = completeWorkspaceSetupAuthImportRefresh(startedB.state, {
      scope: staleAuthRequest!.scope,
      requestId: staleAuthRequest!.requestId,
      data: [authImportCandidate({ id: "stale" })],
    });
    expect(staleCompleted.authImport.status).toBe("loading");
    expect(staleCompleted.authImport.data).toBeNull();

    const withAuth = completeWorkspaceSetupAuthImportRefresh(staleCompleted, {
      scope: currentAuthRequest!.scope,
      requestId: currentAuthRequest!.requestId,
      data: [],
    });
    const withHarness = completeWorkspaceSetupHarnessCandidatesRefresh(withAuth, {
      scope: currentHarnessRequest!.scope,
      requestId: currentHarnessRequest!.requestId,
      data: [],
    });
    const withTitling = completeWorkspaceSetupTitlingProbeRefresh(withHarness, {
      scope: currentTitlingRequest!.scope,
      requestId: currentTitlingRequest!.requestId,
      data: { required: false },
    });

    expect(withTitling.routePlan).toEqual({
      targetKey: serializeWorkspaceSetupRouteScope(scopeB),
      containerSelection: "host",
      includeHarnessDownloads: false,
      includeAuthImport: false,
      includeTitling: false,
    });
  });

  it("keeps failed harness scans explicit instead of treating them like empty results", () => {
    const scope = routeScopeFixture();
    const started = beginWorkspaceSetupProvisioningRefresh(
      createInitialWorkspaceSetupProvisioningMachineState(),
      {
        routeScope: scope,
        refreshReason: "ensure_route_plan",
        titlingMode: "remote",
        previousPlan: null,
      },
    );

    const authRequest = started.requests.find((request) => request.resource === "authImport")!;
    const harnessRequest = started.requests.find((request) => request.resource === "harnessCandidates")!;
    const titlingRequest = started.requests.find((request) => request.resource === "titlingProbe")!;

    const withAuth = completeWorkspaceSetupAuthImportRefresh(started.state, {
      scope: authRequest.scope,
      requestId: authRequest.requestId,
      data: [authImportCandidate()],
    });
    const withHarnessError = failWorkspaceSetupHarnessCandidatesRefresh(withAuth, {
      scope: harnessRequest.scope,
      requestId: harnessRequest.requestId,
      error: "Harness scan failed.",
    });
    const finalState = completeWorkspaceSetupTitlingProbeRefresh(withHarnessError, {
      scope: titlingRequest.scope,
      requestId: titlingRequest.requestId,
      data: { required: false },
    });

    expect(finalState.routePlan?.includeAuthImport).toBe(true);
    expect(finalState.routePlan?.includeHarnessDownloads).toBe(true);
    expect(finalState.refreshError).toContain("Harness scan failed.");
  });

  it("retries a failed resource with a fresh request id without clearing successful sibling data", () => {
    const scope = routeScopeFixture();
    const started = beginWorkspaceSetupProvisioningRefresh(
      createInitialWorkspaceSetupProvisioningMachineState(),
      {
        routeScope: scope,
        refreshReason: "ensure_route_plan",
        titlingMode: "unset",
        previousPlan: null,
      },
    );

    const authRequest = started.requests.find((request) => request.resource === "authImport")!;
    const harnessRequest = started.requests.find((request) => request.resource === "harnessCandidates")!;
    const titlingRequest = started.requests.find((request) => request.resource === "titlingProbe")!;

    const withAuthError = failWorkspaceSetupAuthImportRefresh(started.state, {
      scope: authRequest.scope,
      requestId: authRequest.requestId,
      error: "Auth scan failed.",
    });
    const withHarness = completeWorkspaceSetupHarnessCandidatesRefresh(withAuthError, {
      scope: harnessRequest.scope,
      requestId: harnessRequest.requestId,
      data: [harnessRow()],
    });
    const settled = completeWorkspaceSetupTitlingProbeRefresh(withHarness, {
      scope: titlingRequest.scope,
      requestId: titlingRequest.requestId,
      data: { required: false },
    });

    expect(settled.routePlan?.includeAuthImport).toBe(true);
    expect(settled.routePlan?.includeHarnessDownloads).toBe(true);

    const retried = beginWorkspaceSetupProvisioningRefresh(settled, {
      routeScope: scope,
      refreshReason: "refresh_auth_import",
      titlingMode: "unset",
      previousPlan: settled.routePlan,
      resources: ["authImport"],
      force: true,
    });
    const retriedAuthRequest = retried.requests[0];

    expect(retried.requests).toHaveLength(1);
    expect(retriedAuthRequest.requestId).toBeGreaterThan(authRequest.requestId);
    expect(retried.state.harnessCandidates.data).toEqual([harnessRow()]);

    const recovered = completeWorkspaceSetupAuthImportRefresh(retried.state, {
      scope: retriedAuthRequest.scope,
      requestId: retriedAuthRequest.requestId,
      data: [],
    });

    expect(recovered.routePlan?.includeAuthImport).toBe(false);
    expect(recovered.routePlan?.includeHarnessDownloads).toBe(true);
    expect(recovered.refreshError).toBeNull();
  });

  it("clears stale auth-import data when a same-scope refresh fails", () => {
    const scope = routeScopeFixture();
    const started = beginWorkspaceSetupProvisioningRefresh(
      createInitialWorkspaceSetupProvisioningMachineState(),
      {
        routeScope: scope,
        refreshReason: "ensure_route_plan",
        titlingMode: "unset",
        previousPlan: null,
      },
    );

    const authRequest = started.requests.find((request) => request.resource === "authImport")!;
    const harnessRequest = started.requests.find((request) => request.resource === "harnessCandidates")!;
    const titlingRequest = started.requests.find((request) => request.resource === "titlingProbe")!;

    const withAuth = completeWorkspaceSetupAuthImportRefresh(started.state, {
      scope: authRequest.scope,
      requestId: authRequest.requestId,
      data: [authImportCandidate()],
    });
    const withHarness = completeWorkspaceSetupHarnessCandidatesRefresh(withAuth, {
      scope: harnessRequest.scope,
      requestId: harnessRequest.requestId,
      data: [],
    });
    const settled = completeWorkspaceSetupTitlingProbeRefresh(withHarness, {
      scope: titlingRequest.scope,
      requestId: titlingRequest.requestId,
      data: { required: false },
    });

    const retried = beginWorkspaceSetupProvisioningRefresh(settled, {
      routeScope: scope,
      refreshReason: "refresh_auth_import",
      titlingMode: "unset",
      previousPlan: settled.routePlan,
      resources: ["authImport"],
      force: true,
    });
    const retriedAuthRequest = retried.requests[0]!;

    expect(retried.state.authImport.status).toBe("loading");
    expect(retried.state.authImport.data).toEqual([authImportCandidate()]);

    const failed = failWorkspaceSetupAuthImportRefresh(retried.state, {
      scope: retriedAuthRequest.scope,
      requestId: retriedAuthRequest.requestId,
      error: "Auth refresh failed.",
    });

    expect(failed.authImport.status).toBe("error");
    expect(failed.authImport.data).toBeNull();
    expect(failed.routePlan?.includeAuthImport).toBe(true);
    expect(failed.refreshError).toContain("Auth refresh failed.");
  });
});
