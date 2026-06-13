import type { ProviderAuthImportCandidate } from "../../api/client";
import { buildOnboardingAfterConnectResult } from "./routePlanner";
import type { HarnessInstallProviderRow } from "./wizardTypes";
import type { WizardRoutePlan } from "./wizardFlow";
import type {
  RoutePlanInsertionStep,
  WorkspaceSetupProvisioningSnapshot,
  WorkspaceSetupProvisioningTerminalStatus,
  WorkspaceSetupRouteScope,
} from "./workflowTypes";
import { sameProvisioningScope } from "../../state/scopeIdentity";
import { sameWorkspaceSetupRouteScope } from "./workflowTypes";
import type { SessionTitlingMode } from "./WorkspaceSetupPage.logic";

export type WorkspaceSetupProvisioningRefreshReason =
  | "ensure_route_plan"
  | "ensure_onboarding_after_connect"
  | "refresh_auth_import"
  | "refresh_harness_candidates"
  | "refresh_titling_probe";

export type WorkspaceSetupProvisioningResourceStatus =
  | "idle"
  | "loading"
  | WorkspaceSetupProvisioningTerminalStatus;

type WorkspaceSetupProvisioningResourceState<TData> = {
  scope: WorkspaceSetupRouteScope["provisioningScope"] | null;
  status: WorkspaceSetupProvisioningResourceStatus;
  data: TData | null;
  error: string | null;
  requestId: number | null;
};

type WorkspaceSetupTitlingProbeData = {
  required: boolean;
};

export type WorkspaceSetupProvisioningMachineState = {
  nextRequestId: number;
  provisioningScope: WorkspaceSetupRouteScope["provisioningScope"] | null;
  routeScope: WorkspaceSetupRouteScope | null;
  authImport: WorkspaceSetupProvisioningResourceState<ProviderAuthImportCandidate[]>;
  harnessCandidates: WorkspaceSetupProvisioningResourceState<HarnessInstallProviderRow[]>;
  titlingProbe: WorkspaceSetupProvisioningResourceState<WorkspaceSetupTitlingProbeData>;
  routePlan: WizardRoutePlan | null;
  insertionStep: RoutePlanInsertionStep | null;
  refreshReason: WorkspaceSetupProvisioningRefreshReason | null;
  refreshError: string | null;
  titlingMode: SessionTitlingMode;
  allowTitlingInsertion: boolean;
  previousPlan: WizardRoutePlan | null;
};

export type WorkspaceSetupProvisioningResourceKind =
  | "authImport"
  | "harnessCandidates"
  | "titlingProbe";

export type WorkspaceSetupProvisioningRequest = {
  resource: WorkspaceSetupProvisioningResourceKind;
  requestId: number;
  scope: WorkspaceSetupRouteScope["provisioningScope"];
};

export type BeginWorkspaceSetupProvisioningRefreshArgs = {
  routeScope: WorkspaceSetupRouteScope;
  refreshReason: WorkspaceSetupProvisioningRefreshReason;
  titlingMode: SessionTitlingMode;
  previousPlan: WizardRoutePlan | null;
  allowTitlingInsertion?: boolean;
  resources?: WorkspaceSetupProvisioningResourceKind[];
  force?: boolean;
};

type BeginWorkspaceSetupProvisioningRefreshResult = {
  state: WorkspaceSetupProvisioningMachineState;
  requests: WorkspaceSetupProvisioningRequest[];
};

type CompleteWorkspaceSetupProvisioningRequestArgs<TData> = {
  scope: WorkspaceSetupRouteScope["provisioningScope"];
  requestId: number;
  data: TData;
};

type FailWorkspaceSetupProvisioningRequestArgs = {
  scope: WorkspaceSetupRouteScope["provisioningScope"];
  requestId: number;
  error: string;
};

const createResourceState = <TData>(): WorkspaceSetupProvisioningResourceState<TData> => ({
  scope: null,
  status: "idle",
  data: null,
  error: null,
  requestId: null,
});

const asTerminalStatus = (
  status: WorkspaceSetupProvisioningResourceStatus,
): WorkspaceSetupProvisioningTerminalStatus => {
  if (status === "ready" || status === "error") {
    return status;
  }
  throw new Error(`Expected terminal provisioning status, received ${status}.`);
};

export const createInitialWorkspaceSetupProvisioningMachineState = (): WorkspaceSetupProvisioningMachineState => ({
  nextRequestId: 1,
  provisioningScope: null,
  routeScope: null,
  authImport: createResourceState<ProviderAuthImportCandidate[]>(),
  harnessCandidates: createResourceState<HarnessInstallProviderRow[]>(),
  titlingProbe: createResourceState<WorkspaceSetupTitlingProbeData>(),
  routePlan: null,
  insertionStep: null,
  refreshReason: null,
  refreshError: null,
  titlingMode: "unset",
  allowTitlingInsertion: true,
  previousPlan: null,
});

const scopeMatches = (
  state: WorkspaceSetupProvisioningResourceState<unknown>,
  scope: WorkspaceSetupRouteScope["provisioningScope"],
): boolean => Boolean(state.scope) && sameProvisioningScope(state.scope!, scope);

export const hasReadyWorkspaceSetupProvisioningStateForRouteScope = (
  state: WorkspaceSetupProvisioningMachineState,
  routeScope: WorkspaceSetupRouteScope,
): boolean =>
  Boolean(state.routeScope)
  && sameWorkspaceSetupRouteScope(state.routeScope!, routeScope)
  && [state.authImport, state.harnessCandidates, state.titlingProbe].every((resource) =>
    resource.status === "ready" && scopeMatches(resource, routeScope.provisioningScope)
  );

const shouldRefreshResource = (
  state: WorkspaceSetupProvisioningResourceState<unknown>,
  scope: WorkspaceSetupRouteScope["provisioningScope"],
  force: boolean,
): boolean => {
  if (force) return true;
  if (!scopeMatches(state, scope)) return true;
  return state.status !== "ready";
};

const startResourceRefresh = <TData>(
  state: WorkspaceSetupProvisioningMachineState,
  resourceState: WorkspaceSetupProvisioningResourceState<TData>,
  scope: WorkspaceSetupRouteScope["provisioningScope"],
): {
  nextRequestId: number;
  resourceState: WorkspaceSetupProvisioningResourceState<TData>;
  request: WorkspaceSetupProvisioningRequest;
} => {
  const requestId = state.nextRequestId;
  return {
    nextRequestId: requestId + 1,
    resourceState: {
      scope,
      status: "loading",
      data: scopeMatches(resourceState, scope) ? resourceState.data : null,
      error: null,
      requestId,
    },
    request: {
      resource: "authImport",
      requestId,
      scope,
    },
  };
};

const updateRefreshSummary = (
  state: WorkspaceSetupProvisioningMachineState,
): WorkspaceSetupProvisioningMachineState => {
  const routeScope = state.routeScope;
  if (!routeScope) {
    return {
      ...state,
      routePlan: null,
      insertionStep: null,
      refreshError: null,
    };
  }

  const currentScope = routeScope.provisioningScope;
  const resources = [state.authImport, state.harnessCandidates, state.titlingProbe];
  if (resources.some((resource) => !scopeMatches(resource, currentScope))) {
    return {
      ...state,
      routePlan: state.routePlan && sameWorkspaceSetupRouteScope(
        routeScope,
        {
          provisioningScope: currentScope,
          containerSelection: state.routePlan.containerSelection,
        },
      )
        ? state.routePlan
        : null,
      insertionStep: null,
      refreshError: null,
    };
  }

  if (resources.some((resource) => resource.status === "idle" || resource.status === "loading")) {
    return {
      ...state,
      routePlan: state.routePlan,
      insertionStep: state.insertionStep,
      refreshError: null,
    };
  }

  const snapshot: WorkspaceSetupProvisioningSnapshot = {
    routeScope,
    authImportStatus: asTerminalStatus(state.authImport.status),
    authImportCandidateCount: state.authImport.data?.length ?? 0,
    harnessCandidatesStatus: asTerminalStatus(state.harnessCandidates.status),
    missingHarnessCount: (state.harnessCandidates.data ?? []).filter(
      (candidate) => candidate.installSupported && !(candidate.installed && candidate.healthy),
    ).length,
    titlingProbeStatus: asTerminalStatus(state.titlingProbe.status),
    titlingRequired: state.titlingProbe.data?.required === true,
    titlingMode: state.titlingMode,
  };

  const refreshErrorParts = [
    state.authImport.status === "error" ? state.authImport.error : null,
    state.harnessCandidates.status === "error" ? state.harnessCandidates.error : null,
    state.titlingProbe.status === "error" ? state.titlingProbe.error : null,
  ].filter((value): value is string => Boolean(value));

  const onboardingResult = buildOnboardingAfterConnectResult(
    snapshot,
    state.previousPlan,
    { allowTitlingInsertion: state.allowTitlingInsertion },
  );

  return {
    ...state,
    routePlan: onboardingResult.routePlan,
    insertionStep: onboardingResult.insertionStep,
    refreshError: refreshErrorParts.length > 0 ? refreshErrorParts.join(" ") : null,
  };
};

const beginResourceRefresh = (
  state: WorkspaceSetupProvisioningMachineState,
  resource: WorkspaceSetupProvisioningResourceKind,
  scope: WorkspaceSetupRouteScope["provisioningScope"],
): {
  state: WorkspaceSetupProvisioningMachineState;
  request: WorkspaceSetupProvisioningRequest;
} => {
  const requestId = state.nextRequestId;
  const request: WorkspaceSetupProvisioningRequest = { resource, requestId, scope };
  const nextRequestId = requestId + 1;

  switch (resource) {
    case "authImport":
      return {
        state: {
          ...state,
          nextRequestId,
          authImport: {
            scope,
            status: "loading",
            data: scopeMatches(state.authImport, scope) ? state.authImport.data : null,
            error: null,
            requestId,
          },
        },
        request,
      };
    case "harnessCandidates":
      return {
        state: {
          ...state,
          nextRequestId,
          harnessCandidates: {
            scope,
            status: "loading",
            data: scopeMatches(state.harnessCandidates, scope) ? state.harnessCandidates.data : null,
            error: null,
            requestId,
          },
        },
        request,
      };
    case "titlingProbe":
      return {
        state: {
          ...state,
          nextRequestId,
          titlingProbe: {
            scope,
            status: "loading",
            data: scopeMatches(state.titlingProbe, scope) ? state.titlingProbe.data : null,
            error: null,
            requestId,
          },
        },
        request,
      };
    default: {
      const exhaustiveCheck: never = resource;
      return exhaustiveCheck;
    }
  }
};

export const beginWorkspaceSetupProvisioningRefresh = (
  state: WorkspaceSetupProvisioningMachineState,
  args: BeginWorkspaceSetupProvisioningRefreshArgs,
): BeginWorkspaceSetupProvisioningRefreshResult => {
  const resources = args.resources ?? ["authImport", "harnessCandidates", "titlingProbe"];
  const force = args.force ?? false;
  const routeChanged = !state.routeScope || !sameWorkspaceSetupRouteScope(state.routeScope, args.routeScope);

  let nextState: WorkspaceSetupProvisioningMachineState = {
    ...state,
    provisioningScope: args.routeScope.provisioningScope,
    routeScope: args.routeScope,
    refreshReason: args.refreshReason,
    refreshError: null,
    titlingMode: args.titlingMode,
    allowTitlingInsertion: args.allowTitlingInsertion ?? true,
    previousPlan: args.previousPlan,
    routePlan: routeChanged ? null : state.routePlan,
    insertionStep: routeChanged ? null : state.insertionStep,
  };

  const requests: WorkspaceSetupProvisioningRequest[] = [];
  for (const resource of resources) {
    const resourceState = nextState[resource];
    if (!shouldRefreshResource(resourceState, args.routeScope.provisioningScope, force)) {
      continue;
    }
    const refreshed = beginResourceRefresh(nextState, resource, args.routeScope.provisioningScope);
    nextState = refreshed.state;
    requests.push(refreshed.request);
  }

  return {
    state: updateRefreshSummary(nextState),
    requests,
  };
};

const isCurrentRequest = (
  state: WorkspaceSetupProvisioningResourceState<unknown>,
  scope: WorkspaceSetupRouteScope["provisioningScope"],
  requestId: number,
): boolean =>
  scopeMatches(state, scope) && state.requestId === requestId;

export const completeWorkspaceSetupAuthImportRefresh = (
  state: WorkspaceSetupProvisioningMachineState,
  args: CompleteWorkspaceSetupProvisioningRequestArgs<ProviderAuthImportCandidate[]>,
): WorkspaceSetupProvisioningMachineState => {
  if (!isCurrentRequest(state.authImport, args.scope, args.requestId)) {
    return state;
  }
  return updateRefreshSummary({
    ...state,
    authImport: {
      scope: args.scope,
      status: "ready",
      data: args.data,
      error: null,
      requestId: args.requestId,
    },
  });
};

export const failWorkspaceSetupAuthImportRefresh = (
  state: WorkspaceSetupProvisioningMachineState,
  args: FailWorkspaceSetupProvisioningRequestArgs,
): WorkspaceSetupProvisioningMachineState => {
  if (!isCurrentRequest(state.authImport, args.scope, args.requestId)) {
    return state;
  }
  return updateRefreshSummary({
    ...state,
    authImport: {
      ...state.authImport,
      scope: args.scope,
      status: "error",
      data: null,
      error: args.error,
      requestId: args.requestId,
    },
  });
};

export const completeWorkspaceSetupHarnessCandidatesRefresh = (
  state: WorkspaceSetupProvisioningMachineState,
  args: CompleteWorkspaceSetupProvisioningRequestArgs<HarnessInstallProviderRow[]>,
): WorkspaceSetupProvisioningMachineState => {
  if (!isCurrentRequest(state.harnessCandidates, args.scope, args.requestId)) {
    return state;
  }
  return updateRefreshSummary({
    ...state,
    harnessCandidates: {
      scope: args.scope,
      status: "ready",
      data: args.data,
      error: null,
      requestId: args.requestId,
    },
  });
};

export const failWorkspaceSetupHarnessCandidatesRefresh = (
  state: WorkspaceSetupProvisioningMachineState,
  args: FailWorkspaceSetupProvisioningRequestArgs,
): WorkspaceSetupProvisioningMachineState => {
  if (!isCurrentRequest(state.harnessCandidates, args.scope, args.requestId)) {
    return state;
  }
  return updateRefreshSummary({
    ...state,
    harnessCandidates: {
      ...state.harnessCandidates,
      scope: args.scope,
      status: "error",
      error: args.error,
      requestId: args.requestId,
    },
  });
};

export const completeWorkspaceSetupTitlingProbeRefresh = (
  state: WorkspaceSetupProvisioningMachineState,
  args: CompleteWorkspaceSetupProvisioningRequestArgs<WorkspaceSetupTitlingProbeData>,
): WorkspaceSetupProvisioningMachineState => {
  if (!isCurrentRequest(state.titlingProbe, args.scope, args.requestId)) {
    return state;
  }
  return updateRefreshSummary({
    ...state,
    titlingProbe: {
      scope: args.scope,
      status: "ready",
      data: args.data,
      error: null,
      requestId: args.requestId,
    },
  });
};

export const failWorkspaceSetupTitlingProbeRefresh = (
  state: WorkspaceSetupProvisioningMachineState,
  args: FailWorkspaceSetupProvisioningRequestArgs,
): WorkspaceSetupProvisioningMachineState => {
  if (!isCurrentRequest(state.titlingProbe, args.scope, args.requestId)) {
    return state;
  }
  return updateRefreshSummary({
    ...state,
    titlingProbe: {
      ...state.titlingProbe,
      scope: args.scope,
      status: "error",
      error: args.error,
      requestId: args.requestId,
    },
  });
};
