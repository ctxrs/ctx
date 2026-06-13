import { errorMessage } from "../../utils/errorMessage";
import { emitUiDiagnostic } from "../diagnosticsChannel";
import type { SubagentInvocation } from "../../api/client";
import type { InternalEntry, SessionSupportLoadErrorKey } from "./entryState";

const SUPPORT_LOAD_ERROR_LABELS: Record<SessionSupportLoadErrorKey, string> = {
  state: "session state",
  subagentInvocations: "subagent invocations",
};

export function formatSupportLoadError(key: SessionSupportLoadErrorKey, value: unknown): string {
  const detail = String(errorMessage(value) ?? "").trim();
  if (!detail || detail === "undefined" || detail === "null" || detail === "[object Object]") {
    return `Failed to load ${SUPPORT_LOAD_ERROR_LABELS[key]}.`;
  }
  if (detail.startsWith("Failed to load ")) {
    return detail;
  }
  return `Failed to load ${SUPPORT_LOAD_ERROR_LABELS[key]}: ${detail}`;
}

type SessionStateLoadStatus = {
  stateLoaded: boolean;
  stateLoading: boolean;
  stateRev?: number;
  stateAppliedRev?: number;
};

type SubagentInvocationsLoadStatus = {
  subagentInvocationsLoaded?: boolean;
  subagentInvocationsLoading: boolean;
  subagentInvocationsAppliedRev?: number;
};

export function shouldFetchSessionState(
  entry: SessionStateLoadStatus,
  opts?: { force?: boolean },
): boolean {
  if (entry.stateLoading) return false;
  if (opts?.force) return true;
  if (!entry.stateLoaded) return true;
  if (
    typeof entry.stateRev === "number" &&
    typeof entry.stateAppliedRev === "number" &&
    entry.stateAppliedRev < entry.stateRev
  ) {
    return true;
  }
  if (typeof entry.stateRev === "number" && typeof entry.stateAppliedRev !== "number") {
    return true;
  }
  return false;
}

export function shouldFetchSubagentInvocations(
  entry: SubagentInvocationsLoadStatus,
  requestedStateRev: number | undefined,
  opts?: { force?: boolean },
): boolean {
  if (entry.subagentInvocationsLoading) return false;
  if (opts?.force) return true;
  if (!entry.subagentInvocationsLoaded) return true;
  if (
    typeof requestedStateRev === "number" &&
    typeof entry.subagentInvocationsAppliedRev === "number" &&
    entry.subagentInvocationsAppliedRev < requestedStateRev
  ) {
    return true;
  }
  if (
    typeof requestedStateRev === "number" &&
    typeof entry.subagentInvocationsAppliedRev !== "number"
  ) {
    return true;
  }
  return false;
}

export function deriveSupportFreshnessKey(
  requestedStateRev: number | undefined,
  freshnessEpoch: number,
): string {
  if (typeof requestedStateRev === "number") {
    return `rev:${requestedStateRev}`;
  }
  return `epoch:${freshnessEpoch}`;
}

export function adoptLoadedStateRevision(
  stateLoaded: boolean,
  currentAppliedRev: number | undefined,
  _nextKnownRev: number | undefined,
): number | undefined {
  // Revisionless support must refetch once a revision becomes known instead of
  // being retroactively stamped as current.
  if (!stateLoaded) return currentAppliedRev;
  return currentAppliedRev;
}

type SupportLoadSyncDeps = {
  resolveRequestedStateRev(entry: InternalEntry): number | undefined;
  ensureState(entry: InternalEntry): Promise<void>;
  ensureSubagentInvocations(entry: InternalEntry): Promise<void>;
};

export function syncSupportLoadsForOpenSession(
  entry: InternalEntry,
  deps: SupportLoadSyncDeps,
): void {
  if (entry.refCount <= 0) return;
  const requestedStateRev = deps.resolveRequestedStateRev(entry);
  const support = entry.support;
  const freshnessKey = deriveSupportFreshnessKey(requestedStateRev, support.supportFreshnessEpoch);
  if (
    support.stateAutoLoadKey !== freshnessKey &&
    shouldFetchSessionState({ ...support, stateRev: requestedStateRev })
  ) {
    support.stateAutoLoadKey = freshnessKey;
    void deps.ensureState(entry);
  }
  if (
    support.subagentAutoLoadKey !== freshnessKey &&
    shouldFetchSubagentInvocations(support, requestedStateRev)
  ) {
    support.subagentAutoLoadKey = freshnessKey;
    void deps.ensureSubagentInvocations(entry);
  }
}

type SupportLoadInvalidationDeps = {
  resolveRequestedStateRev(entry: InternalEntry): number | undefined;
  subagentInvocationsCacheBySessionId: Map<
    string,
    { invocations: SubagentInvocation[]; stateRev: number }
  >;
  invalidateStateRequest(entry: InternalEntry): void;
  invalidateSubagentInvocationsRequest(entry: InternalEntry): void;
};

export function invalidateSupportLoadsWithoutAuthoritativeRevision(
  entry: InternalEntry,
  deps: SupportLoadInvalidationDeps,
): void {
  if (typeof deps.resolveRequestedStateRev(entry) === "number") return;
  const support = entry.support;
  support.supportFreshnessEpoch += 1;
  support.stateLoaded = false;
  support.stateAppliedRev = undefined;
  support.subagentInvocationsLoaded = false;
  support.subagentInvocationsAppliedRev = undefined;
  deps.invalidateStateRequest(entry);
  deps.invalidateSubagentInvocationsRequest(entry);
  deps.subagentInvocationsCacheBySessionId.delete(entry.sessionId);
}

export function adoptLoadedSubagentInvocationsRevision(
  entry: InternalEntry,
  _stateRev: number,
  _subagentInvocationsCacheBySessionId: Map<
    string,
    { invocations: SubagentInvocation[]; stateRev: number }
  >,
): void {
  if (!entry.support.subagentInvocationsLoaded) return;
}

export function clearSupportLoadError(
  entry: InternalEntry,
  key: SessionSupportLoadErrorKey,
): void {
  if (!entry.support.loadErrors[key]) return;
  delete entry.support.loadErrors[key];
}

export function setSupportLoadError(
  entry: InternalEntry,
  key: SessionSupportLoadErrorKey,
  value: unknown,
): void {
  const message = formatSupportLoadError(key, value);
  emitUiDiagnostic({
    source: "session_supervisor",
    code: `session.${key}_load_failed`,
    severity: "error",
    fatal: false,
    message,
    context: {
      sessionId: entry.sessionId,
      mode: entry.mode ?? null,
      target: key,
    },
  });
  entry.support.loadErrors[key] = message;
}
