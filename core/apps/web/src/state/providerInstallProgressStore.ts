import type { InstallInfo } from "../api/client";
import type { OwnerScope } from "./scopeIdentity";
import { serializeOwnerScope } from "./scopeIdentity";
import { getProviderHostOwnerScope } from "./providerScopeAdapters";

export type ProviderInstallProgressSession = {
  installId: string;
  state: InstallInfo["state"];
  pct: number | null;
  target?: InstallInfo["target"];
  errorCode?: InstallInfo["error_code"];
  error?: string;
  updatedAtMs: number;
};

export const UNKNOWN_PROVIDER_INSTALL_TARGET = "__unknown__";

type ProviderInstallProgressTarget = Exclude<InstallInfo["target"], undefined>;

export type ProviderInstallProgressTargetKey =
  | ProviderInstallProgressTarget
  | typeof UNKNOWN_PROVIDER_INSTALL_TARGET;

export type ProviderInstallProgressSnapshot = Record<
  string,
  Partial<Record<ProviderInstallProgressTargetKey, ProviderInstallProgressSession>>
>;

type Listener = (snapshot: ProviderInstallProgressSnapshot) => void;
type ProviderInstallProgressOwnerState = Map<
  string,
  Map<ProviderInstallProgressTargetKey, ProviderInstallProgressSession>
>;

const installsByOwnerScope = new Map<string, ProviderInstallProgressOwnerState>();
const listenersByOwnerScope = new Map<string, Set<Listener>>();

const scopeKeyForOwner = (ownerScope: OwnerScope): string => serializeOwnerScope(ownerScope);

const toTargetKey = (
  target: InstallInfo["target"] | undefined,
): ProviderInstallProgressTargetKey => target ?? UNKNOWN_PROVIDER_INSTALL_TARGET;

const sameTarget = (
  target: InstallInfo["target"] | undefined,
  targetKey: ProviderInstallProgressTargetKey,
): boolean => toTargetKey(target) === targetKey;

function sameSession(
  lhs: ProviderInstallProgressSession | undefined,
  rhs: ProviderInstallProgressSession,
): boolean {
  if (!lhs) return false;
  return lhs.installId === rhs.installId
    && lhs.state === rhs.state
    && lhs.pct === rhs.pct
    && lhs.target === rhs.target
    && lhs.errorCode === rhs.errorCode
    && lhs.error === rhs.error;
}

const cloneOwnerSnapshot = (ownerScope: OwnerScope): ProviderInstallProgressSnapshot => {
  const snapshot: ProviderInstallProgressSnapshot = {};
  const ownerState = installsByOwnerScope.get(scopeKeyForOwner(ownerScope));
  if (!ownerState) return snapshot;
  for (const [providerId, sessionsByTarget] of ownerState.entries()) {
    snapshot[providerId] = Object.fromEntries(
      Array.from(sessionsByTarget.entries()).map(([targetKey, session]) => [targetKey, { ...session }]),
    ) as Partial<Record<ProviderInstallProgressTargetKey, ProviderInstallProgressSession>>;
  }
  return snapshot;
};

const emitChangeForOwner = (ownerScope: OwnerScope): void => {
  const listeners = listenersByOwnerScope.get(scopeKeyForOwner(ownerScope));
  if (!listeners || listeners.size === 0) return;
  const snapshot = cloneOwnerSnapshot(ownerScope);
  for (const listener of listeners) {
    listener(snapshot);
  }
};

const getOrCreateOwnerState = (ownerScope: OwnerScope): ProviderInstallProgressOwnerState => {
  const ownerKey = scopeKeyForOwner(ownerScope);
  let ownerState = installsByOwnerScope.get(ownerKey);
  if (!ownerState) {
    ownerState = new Map<string, Map<ProviderInstallProgressTargetKey, ProviderInstallProgressSession>>();
    installsByOwnerScope.set(ownerKey, ownerState);
  }
  return ownerState;
};

export function getProviderInstallProgressSnapshotForScope(
  ownerScope: OwnerScope,
): ProviderInstallProgressSnapshot {
  return cloneOwnerSnapshot(ownerScope);
}

export function getProviderInstallProgressSnapshot(): ProviderInstallProgressSnapshot {
  return getProviderInstallProgressSnapshotForScope(getProviderHostOwnerScope());
}

export function subscribeProviderInstallProgressForScope(
  ownerScope: OwnerScope,
  listener: Listener,
): () => void {
  const ownerKey = scopeKeyForOwner(ownerScope);
  let listeners = listenersByOwnerScope.get(ownerKey);
  if (!listeners) {
    listeners = new Set<Listener>();
    listenersByOwnerScope.set(ownerKey, listeners);
  }
  listeners.add(listener);
  listener(cloneOwnerSnapshot(ownerScope));
  return () => {
    const current = listenersByOwnerScope.get(ownerKey);
    if (!current) return;
    current.delete(listener);
    if (current.size === 0) {
      listenersByOwnerScope.delete(ownerKey);
    }
  };
}

export function subscribeProviderInstallProgress(listener: Listener): () => void {
  return subscribeProviderInstallProgressForScope(getProviderHostOwnerScope(), listener);
}

export function upsertProviderInstallProgressForScope(
  ownerScope: OwnerScope,
  providerId: string,
  session: Omit<ProviderInstallProgressSession, "updatedAtMs"> & { updatedAtMs?: number },
): void {
  if (!providerId || !session.installId) return;
  const ownerState = getOrCreateOwnerState(ownerScope);
  const targetKey = toTargetKey(session.target);
  const nextSession: ProviderInstallProgressSession = {
    ...session,
    updatedAtMs: session.updatedAtMs ?? Date.now(),
  };
  let sessionsByTarget = ownerState.get(providerId);
  if (!sessionsByTarget) {
    sessionsByTarget = new Map<ProviderInstallProgressTargetKey, ProviderInstallProgressSession>();
    ownerState.set(providerId, sessionsByTarget);
  }

  let changed = false;
  if (targetKey !== UNKNOWN_PROVIDER_INSTALL_TARGET) {
    const unknownSession = sessionsByTarget.get(UNKNOWN_PROVIDER_INSTALL_TARGET);
    if (unknownSession?.installId === nextSession.installId) {
      sessionsByTarget.delete(UNKNOWN_PROVIDER_INSTALL_TARGET);
      changed = true;
    }
  }

  const existing = sessionsByTarget.get(targetKey);
  if (!changed && sameSession(existing, nextSession)) {
    return;
  }
  sessionsByTarget.set(targetKey, nextSession);
  emitChangeForOwner(ownerScope);
}

export function upsertProviderInstallProgress(
  providerId: string,
  session: Omit<ProviderInstallProgressSession, "updatedAtMs"> & { updatedAtMs?: number },
): void {
  upsertProviderInstallProgressForScope(getProviderHostOwnerScope(), providerId, session);
}

export function resolveProviderInstallProgressSession(
  snapshot: ProviderInstallProgressSnapshot,
  providerId: string,
  target?: InstallInfo["target"],
): ProviderInstallProgressSession | undefined {
  const sessionsByTarget = snapshot[providerId];
  if (!sessionsByTarget) return undefined;

  if (target) {
    const exact = sessionsByTarget[toTargetKey(target)];
    if (exact) return exact;
    return sessionsByTarget[UNKNOWN_PROVIDER_INSTALL_TARGET];
  }

  const unknown = sessionsByTarget[UNKNOWN_PROVIDER_INSTALL_TARGET];
  if (unknown) return unknown;

  const sessions = Object.values(sessionsByTarget);
  if (sessions.length === 0) return undefined;
  if (sessions.length === 1) return sessions[0];
  return sessions.reduce((latest, current) => (
    current.updatedAtMs > latest.updatedAtMs ? current : latest
  ));
}

export function removeProviderInstallProgressForScope(
  ownerScope: OwnerScope,
  providerId: string,
  options?: { target?: InstallInfo["target"]; installId?: string },
): void {
  if (!providerId) return;
  const ownerKey = scopeKeyForOwner(ownerScope);
  const ownerState = installsByOwnerScope.get(ownerKey);
  if (!ownerState) return;
  const sessionsByTarget = ownerState.get(providerId);
  if (!sessionsByTarget) return;

  const hasTargetFilter = Boolean(options && "target" in options);
  let changed = false;
  for (const [targetKey, session] of sessionsByTarget.entries()) {
    if (hasTargetFilter && !sameTarget(options?.target, targetKey)) {
      continue;
    }
    if (options?.installId && session.installId !== options.installId) {
      continue;
    }
    sessionsByTarget.delete(targetKey);
    changed = true;
  }

  if (sessionsByTarget.size === 0) {
    ownerState.delete(providerId);
  }
  if (ownerState.size === 0) {
    installsByOwnerScope.delete(ownerKey);
  }
  if (changed) {
    emitChangeForOwner(ownerScope);
  }
}

export function removeProviderInstallProgress(
  providerId: string,
  options?: { target?: InstallInfo["target"]; installId?: string },
): void {
  removeProviderInstallProgressForScope(getProviderHostOwnerScope(), providerId, options);
}

export function clearProviderInstallProgress(): void {
  if (installsByOwnerScope.size === 0) return;
  installsByOwnerScope.clear();
  for (const listeners of listenersByOwnerScope.values()) {
    for (const listener of listeners) {
      listener({});
    }
  }
}
