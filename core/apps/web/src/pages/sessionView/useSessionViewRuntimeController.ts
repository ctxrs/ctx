import { useCallback, useEffect, useMemo, useState } from "react";
import {
  authenticateSession,
  idToString,
  type Session,
} from "../../api/client";
import {
  type SessionCacheEntry,
  type SessionSupervisor,
} from "../../state/sessionSupervisor";
import { selectSessionThreadProjection } from "../../state/sessionThreadProjection/selectors";
import {
  buildModelsForProvider,
  normalizeModelDisplayNamesForProvider,
} from "../../components/workbenchComposer/WorkbenchComposer.utils";
import {
  deriveAuthUi,
  deriveProviderGuardNotice,
  normalizeContextWindowMetrics,
} from "../workbenchViewModel/SessionPage.workbenchViewModel";
import { errorMessage } from "../../utils/errorMessage";
import { hasSessionActiveTurn } from "../../utils/sessionActivity";
import { collectSessionLoadIssues } from "./sessionLoadIssues";
import { useSessionProviderGuard } from "./useSessionProviderGuard";
import { useSharedSessionProviderOptions } from "./useSharedSessionProviderOptions";
import { composeModelId } from "../../utils/modelEffort";
import { useSessionModelSwitcher } from "./useSessionModelSwitcher";
import {
  buildModelsFromAcpMeta,
  resolveModelDisplayLabel,
} from "./SessionPage.viewHelpers";
import { isSameContextWindow } from "./estimateHeuristics";
import type { ContextWindowInfo } from "../../components/WorkbenchComposer";

type ThreadProjection = ReturnType<typeof selectSessionThreadProjection>;

type Params = {
  sessionId: string;
  entry: SessionCacheEntry | null;
  session: Session | null;
  threadProjection: ThreadProjection;
  supervisor: SessionSupervisor;
};

type Result = {
  worktreeId: string | null;
  sessionLoadIssues: Array<{ key: "state" | "subagentInvocations"; message: string }>;
  subagentInvocations: NonNullable<SessionCacheEntry["subagentInvocations"]>;
  contextWindow: ContextWindowInfo | null;
  hasActiveTurn: boolean;
  interruptSessionId: string;
  authUi: ReturnType<typeof deriveAuthUi>;
  authMethodId: string;
  setAuthMethodId: (value: string) => void;
  authBusy: boolean;
  authError: string | null;
  onAuthenticate: () => Promise<void>;
  providerGuardNotice: ReturnType<typeof deriveProviderGuardNotice>;
  providerGuardActionError: string | null;
  providerGuardActionBusy: boolean;
  providerGuardMemoryLimitMb?: number | null;
  providerGuardHeading: string;
  providerGuardMessage: string;
  providerGuardLimitLabel: string;
  providerGuardProviderLabel?: string;
  providerGuardPidLabel: string | null;
  canRaiseProviderGuard: boolean;
  onRaiseProviderGuardLimit: () => Promise<void>;
  onDisableProviderGuard: () => Promise<void>;
  availableModels: Array<{ id: string; name?: string }>;
  currentModelId: string;
  currentModelDisplayLabel?: string;
  onSetModelId: (next: string) => Promise<void>;
  modelSwitchError: string | null;
};

export function useSessionViewRuntimeController({
  sessionId,
  entry,
  session,
  threadProjection,
  supervisor,
}: Params): Result {
  const [authMethodId, setAuthMethodId] = useState("");
  const [authBusy, setAuthBusy] = useState(false);
  const [authError, setAuthError] = useState<string | null>(null);
  const [modelSwitchError, setModelSwitchError] = useState<string | null>(null);
  const [optimisticModelId, setOptimisticModelId] = useState<string | null>(null);
  const [lastContextWindow, setLastContextWindow] = useState<ContextWindowInfo | null>(null);

  useEffect(() => {
    setAuthMethodId("");
    setAuthBusy(false);
    setAuthError(null);
    setModelSwitchError(null);
    setOptimisticModelId(null);
    setLastContextWindow(null);
  }, [sessionId]);

  const worktreeId = session ? idToString(session.worktree_id) : null;
  const sessionLoadIssues = useMemo(
    () => collectSessionLoadIssues(entry?.loadErrors),
    [entry?.loadErrors?.state, entry?.loadErrors?.subagentInvocations],
  );
  const subagentInvocations = entry?.subagentInvocations ?? [];

  const computedContextWindow = useMemo<ContextWindowInfo | null>(() => {
    for (let index = threadProjection.turns.length - 1; index >= 0; index -= 1) {
      const metrics = threadProjection.turns[index]?.metrics_json;
      if (!metrics) continue;
      const normalized = normalizeContextWindowMetrics(metrics);
      if (normalized) return normalized;
    }
    return null;
  }, [threadProjection.turns]);

  useEffect(() => {
    if (!computedContextWindow) return;
    setLastContextWindow((previous) =>
      isSameContextWindow(previous, computedContextWindow) ? previous : computedContextWindow,
    );
  }, [computedContextWindow]);

  const contextWindow = computedContextWindow ?? lastContextWindow;
  const latestTurnStatus = useMemo(
    () => threadProjection.turns.at(-1)?.status ?? entry?.turns.at(-1)?.status ?? null,
    [entry?.turns, threadProjection.turns],
  );
  const hasActiveTurn = useMemo(
    () => hasSessionActiveTurn(entry?.activity, latestTurnStatus),
    [entry?.activity, latestTurnStatus],
  );
  const interruptSessionId = useMemo(() => {
    if (!hasActiveTurn || !worktreeId) return "";
    return sessionId;
  }, [hasActiveTurn, sessionId, worktreeId]);

  const authUi = useMemo(
    () => deriveAuthUi(threadProjection.events),
    [threadProjection.events, threadProjection.eventsStamp],
  );

  useEffect(() => {
    if (authMethodId) return;
    if (authUi.methods.length > 0) {
      setAuthMethodId(authUi.methods[0].id);
    }
  }, [authMethodId, authUi.methods]);

  const onAuthenticate = useCallback(async () => {
    if (!sessionId) return;
    setAuthBusy(true);
    setAuthError(null);
    try {
      await authenticateSession(sessionId, authMethodId);
      supervisor.refreshSession(sessionId, { watchDiff: true });
    } catch (error: unknown) {
      setAuthError(errorMessage(error));
    } finally {
      setAuthBusy(false);
    }
  }, [authMethodId, sessionId, supervisor]);

  const providerGuardNotice = useMemo(
    () => deriveProviderGuardNotice(threadProjection.events),
    [threadProjection.events, threadProjection.eventsStamp],
  );
  const {
    providerGuardActionError,
    providerGuardActionBusy,
    providerGuardMemoryLimitMb,
    providerGuardHeading,
    providerGuardMessage,
    providerGuardLimitLabel,
    providerGuardProviderLabel,
    providerGuardPidLabel,
    canRaiseProviderGuard,
    raiseProviderGuardLimit,
    disableProviderGuard,
  } = useSessionProviderGuard({
    providerGuardNotice,
    sessionProviderId: session?.provider_id,
  });

  const currentModelId = useMemo(
    () => composeModelId(String(session?.model_id ?? ""), session?.reasoning_effort ?? null),
    [session?.model_id, session?.reasoning_effort],
  );
  const providerId = String(session?.provider_id ?? "").trim();
  const sharedProviderOptions = useSharedSessionProviderOptions(session);
  const acpModelOptions = useMemo(() => (
    normalizeModelDisplayNamesForProvider(providerId, buildModelsFromAcpMeta(entry?.acpModels))
  ), [entry?.acpModels, providerId]);
  const availableModels = useMemo(() => {
    const parsed = buildModelsForProvider(providerId, sharedProviderOptions);
    if (parsed.length > 0) return parsed;
    if (acpModelOptions.length > 0) return acpModelOptions;
    return currentModelId
      ? normalizeModelDisplayNamesForProvider(providerId, [{ id: currentModelId, name: currentModelId }])
      : [];
  }, [acpModelOptions, currentModelId, providerId, sharedProviderOptions]);
  const displayedModelId = optimisticModelId ?? currentModelId;
  const currentModelDisplayLabel = useMemo(
    () =>
      resolveModelDisplayLabel(availableModels, [
        optimisticModelId,
        entry?.acpCurrentModelId,
        currentModelId,
      ]),
    [availableModels, currentModelId, entry?.acpCurrentModelId, optimisticModelId],
  );
  const onSetModelId = useSessionModelSwitcher({
    sessionId,
    supervisor,
    setModelSwitchError,
    setOptimisticModelId,
  });

  return {
    worktreeId,
    sessionLoadIssues,
    subagentInvocations,
    contextWindow,
    hasActiveTurn,
    interruptSessionId,
    authUi,
    authMethodId,
    setAuthMethodId,
    authBusy,
    authError,
    onAuthenticate,
    providerGuardNotice,
    providerGuardActionError,
    providerGuardActionBusy,
    providerGuardMemoryLimitMb,
    providerGuardHeading,
    providerGuardMessage,
    providerGuardLimitLabel,
    providerGuardProviderLabel,
    providerGuardPidLabel,
    canRaiseProviderGuard,
    onRaiseProviderGuardLimit: raiseProviderGuardLimit,
    onDisableProviderGuard: disableProviderGuard,
    availableModels,
    currentModelId: displayedModelId,
    currentModelDisplayLabel,
    onSetModelId,
    modelSwitchError,
  };
}
