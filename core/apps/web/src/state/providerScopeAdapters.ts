import {
  createHostOwnerScope,
  createWorkspaceOwnerScope,
  serializeOwnerScope,
  type DaemonTargetScope,
  type HostOwnerScope,
  type OwnerScope,
  type WorkspaceOwnerScope,
} from "./scopeIdentity";
import { getDaemonIdentityScopeOrNull } from "./daemonTargetScopeIdentity";

const MISSING_PROVIDER_OWNER_SCOPE_MESSAGE = "Daemon target scope is not available.";

const getCurrentDaemonTargetScope = (): DaemonTargetScope | null =>
  getDaemonIdentityScopeOrNull();

export const createMissingProviderOwnerScopeError = (): Error =>
  new Error(MISSING_PROVIDER_OWNER_SCOPE_MESSAGE);

const requireCurrentDaemonTargetScope = (): DaemonTargetScope => {
  const targetScope = getCurrentDaemonTargetScope();
  if (!targetScope) {
    throw createMissingProviderOwnerScopeError();
  }
  return targetScope;
};

export const getProviderHostOwnerScopeOrNull = (): HostOwnerScope | null => {
  const targetScope = getCurrentDaemonTargetScope();
  return targetScope ? createHostOwnerScope(targetScope) : null;
};

export const getProviderWorkspaceOwnerScopeOrNull = (
  workspaceId: string,
): WorkspaceOwnerScope | null => {
  const targetScope = getCurrentDaemonTargetScope();
  return targetScope ? createWorkspaceOwnerScope(targetScope, workspaceId) : null;
};

export const getProviderOwnerScopeOrNull = (workspaceId: string | null): OwnerScope | null =>
  workspaceId ? getProviderWorkspaceOwnerScopeOrNull(workspaceId) : getProviderHostOwnerScopeOrNull();

export const getProviderOwnerScopeKeyOrNull = (workspaceId: string | null): string | null => {
  const ownerScope = getProviderOwnerScopeOrNull(workspaceId);
  return ownerScope ? serializeOwnerScope(ownerScope) : null;
};

export const getProviderHostOwnerScope = (): HostOwnerScope =>
  createHostOwnerScope(requireCurrentDaemonTargetScope());

export const getProviderWorkspaceOwnerScope = (
  workspaceId: string,
): WorkspaceOwnerScope =>
  createWorkspaceOwnerScope(requireCurrentDaemonTargetScope(), workspaceId);

export const getProviderOwnerScope = (workspaceId: string | null): OwnerScope =>
  workspaceId ? getProviderWorkspaceOwnerScope(workspaceId) : getProviderHostOwnerScope();

export const getProviderOwnerScopeKey = (workspaceId: string | null): string =>
  serializeOwnerScope(getProviderOwnerScope(workspaceId));
