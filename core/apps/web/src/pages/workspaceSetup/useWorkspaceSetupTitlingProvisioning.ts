import { useCallback, useEffect, useRef, useState, type MutableRefObject } from "react";
import type {
  TitleGenerationLocalStatus,
  TitleGenerationSettings,
  UpdateTitleGenerationSettingsRequest,
} from "../../api/client";
import {
  getSettings,
  getTitleGenerationLocalStatus,
  installTitleGenerationLocal,
  updateSettings,
} from "../../api/client";
import {
  buildSessionTitlingDraft,
  buildSessionTitlingPayload,
  DEFAULT_TITLE_LOCAL_MODEL_ID,
  DEFAULT_TITLE_REMOTE_BASE_URL,
  DEFAULT_TITLE_REMOTE_MODEL,
  resolveSessionTitlingReadiness,
  sessionTitlingPayloadHash,
  type SessionTitlingMode,
} from "./WorkspaceSetupPage.logic";
import {
  completeWorkspaceSetupTitlingProbeRefresh,
  failWorkspaceSetupTitlingProbeRefresh,
  type WorkspaceSetupProvisioningMachineState,
  type WorkspaceSetupProvisioningRequest,
} from "./workspaceSetupProvisioningMachine";
import {
  messageFromError,
  type LocalInstallState,
  type RemoteStatus,
} from "./wizardTypes";
import { observeInstall } from "../../state/installProgressMonitor";
import type { WizardSelections } from "./wizardFlowReducer";
import { withTimeout } from "./promiseTimeout";
import { buildTitlingSummaryValue, isTitlingRemoteValid } from "./workspaceSetupTitlingSummary";
import { TITLING_PROBE_TIMEOUT_MS } from "./useWorkspaceSetupProvisioning.types";
import { useWorkspaceSetupTitlingInstallProgress } from "./useWorkspaceSetupTitlingInstallProgress";

type UseWorkspaceSetupTitlingProvisioningArgs = {
  selections: WizardSelections;
  titlingStepVisible: boolean;
  desktopApp: boolean;
  parsedRemoteHost: string | undefined;
  selectedDaemonTargetKey: string | null;
  canProbeTitling: boolean;
  remoteStatusRef: MutableRefObject<RemoteStatus>;
  connectDaemonForImport: (locationOverride?: "local" | "remote") => Promise<void>;
  commitProvisioningMachineState: (
    updater:
      | WorkspaceSetupProvisioningMachineState
      | ((current: WorkspaceSetupProvisioningMachineState) => WorkspaceSetupProvisioningMachineState),
  ) => WorkspaceSetupProvisioningMachineState;
  isCurrentProvisioningRequest: (
    resource: WorkspaceSetupProvisioningRequest["resource"],
    request: WorkspaceSetupProvisioningRequest,
  ) => boolean;
  shouldTreatRemoteSpeculativeConnectFailureAsSkipped: (error: unknown) => boolean;
};

export function useWorkspaceSetupTitlingProvisioning({
  selections,
  titlingStepVisible,
  desktopApp,
  parsedRemoteHost,
  selectedDaemonTargetKey,
  canProbeTitling,
  remoteStatusRef,
  connectDaemonForImport,
  commitProvisioningMachineState,
  isCurrentProvisioningRequest,
  shouldTreatRemoteSpeculativeConnectFailureAsSkipped,
}: UseWorkspaceSetupTitlingProvisioningArgs) {
  const [titlingProbeBusy, setTitlingProbeBusy] = useState(false);
  const [titlingProbeError, setTitlingProbeError] = useState<string | null>(null);
  const [titlingProbeDone, setTitlingProbeDone] = useState(false);
  const [titlingConfiguredReady, setTitlingConfiguredReady] = useState(false);
  const [titlingStepRequired, setTitlingStepRequired] = useState(false);
  const [titlingProbeTargetKey, setTitlingProbeTargetKey] = useState<string | null>(null);
  const [titlingMode, setTitlingMode] = useState<SessionTitlingMode>("unset");
  const [titlingRemoteBaseUrl, setTitlingRemoteBaseUrl] = useState(DEFAULT_TITLE_REMOTE_BASE_URL);
  const [titlingRemoteApiKey, setTitlingRemoteApiKey] = useState("");
  const [titlingRemoteModel, setTitlingRemoteModel] = useState(DEFAULT_TITLE_REMOTE_MODEL);
  const [titlingRemoteUseJson, setTitlingRemoteUseJson] = useState(true);
  const [titlingRemoteAdvancedOpen, setTitlingRemoteAdvancedOpen] = useState(false);
  const [titlingLocalUseJson, setTitlingLocalUseJson] = useState(true);
  const [titlingLocalStatus, setTitlingLocalStatus] = useState<TitleGenerationLocalStatus | null>(null);
  const [titlingLocalStatusBusy, setTitlingLocalStatusBusy] = useState(false);
  const [titlingLocalStatusRequestedTargetKey, setTitlingLocalStatusRequestedTargetKey] = useState<string | null>(null);
  const [titlingStatusError, setTitlingStatusError] = useState<string | null>(null);
  const [titlingLocalInstallBusy, setTitlingLocalInstallBusy] = useState(false);
  const [titlingLocalInstall, setTitlingLocalInstall] = useState<LocalInstallState | null>(null);
  const [titlingPersistBusy, setTitlingPersistBusy] = useState(false);
  const [titlingPersistError, setTitlingPersistError] = useState<string | null>(null);
  const [titlingPersistedTargetKey, setTitlingPersistedTargetKey] = useState<string | null>(null);
  const [titlingPersistedHash, setTitlingPersistedHash] = useState<string | null>(null);
  const [titlingExistingSettings, setTitlingExistingSettings] = useState<TitleGenerationSettings | null>(null);

  const selectedDaemonTargetKeyRef = useRef<string | null>(null);
  const titlingInstallObserverRef = useRef<{ installId: string; stop: () => void } | null>(null);
  const titlingInstallStateRef = useRef<LocalInstallState | null>(null);

  const resetTitlingDraft = useCallback(() => {
    setTitlingMode("unset");
    setTitlingRemoteBaseUrl(DEFAULT_TITLE_REMOTE_BASE_URL);
    setTitlingRemoteApiKey("");
    setTitlingRemoteModel(DEFAULT_TITLE_REMOTE_MODEL);
    setTitlingRemoteUseJson(true);
    setTitlingLocalUseJson(true);
  }, []);

  const invalidateTitlingPersisted = useCallback(() => {
    setTitlingPersistError(null);
    setTitlingPersistedTargetKey(null);
    setTitlingPersistedHash(null);
  }, []);

  const clearTitlingInstallObserver = useCallback(() => {
    titlingInstallObserverRef.current?.stop();
    titlingInstallObserverRef.current = null;
  }, []);

  const resetTitlingProvisioningState = useCallback(() => {
    clearTitlingInstallObserver();
    setTitlingProbeBusy(false);
    setTitlingProbeError(null);
    setTitlingProbeDone(false);
    setTitlingConfiguredReady(false);
    setTitlingStepRequired(false);
    setTitlingProbeTargetKey(null);
    setTitlingLocalStatus(null);
    setTitlingLocalStatusRequestedTargetKey(null);
    setTitlingStatusError(null);
    setTitlingLocalInstall(null);
    setTitlingExistingSettings(null);
    invalidateTitlingPersisted();
    resetTitlingDraft();
  }, [clearTitlingInstallObserver, invalidateTitlingPersisted, resetTitlingDraft]);

  const refreshTitlingLocalStatus = useCallback(async (
    opts?: { silent?: boolean },
  ): Promise<TitleGenerationLocalStatus | null> => {
    if (!opts?.silent) {
      setTitlingLocalStatusBusy(true);
    }
    setTitlingStatusError(null);
    try {
      const status = await getTitleGenerationLocalStatus();
      setTitlingLocalStatus(status);
      return status;
    } catch (error) {
      setTitlingStatusError(messageFromError(error));
      return null;
    } finally {
      if (!opts?.silent) {
        setTitlingLocalStatusBusy(false);
      }
    }
  }, []);

  const attachTitlingInstall = useCallback(async (installId: string) => {
    if (!installId) return;
    if (titlingInstallObserverRef.current?.installId === installId) return;
    clearTitlingInstallObserver();
    setTitlingLocalInstall({
      installId,
      state: "running",
      pct: null,
    });
    titlingInstallObserverRef.current = {
      installId,
      stop: observeInstall(installId, {
        providerId: "title_generation_local",
        initialState: titlingLocalInstall ?? { installId, state: "running", pct: null },
      }),
    };
  }, [clearTitlingInstallObserver, titlingLocalInstall]);

  const probeTitlingForTarget = useCallback(async (
    request: WorkspaceSetupProvisioningRequest,
    targetKey: string,
  ): Promise<boolean | null> => {
    setTitlingProbeBusy(true);
    setTitlingProbeTargetKey(targetKey);
    setTitlingProbeDone(false);
    setTitlingProbeError(null);
    setTitlingStatusError(null);
    try {
      await withTimeout(
        connectDaemonForImport(),
        TITLING_PROBE_TIMEOUT_MS,
        "Timed out loading daemon settings.",
      );
      if (!isCurrentProvisioningRequest("titlingProbe", request) || selectedDaemonTargetKeyRef.current !== targetKey) {
        return null;
      }

      const settings = await withTimeout(
        getSettings(),
        TITLING_PROBE_TIMEOUT_MS,
        "Timed out loading daemon settings.",
      );
      if (!isCurrentProvisioningRequest("titlingProbe", request) || selectedDaemonTargetKeyRef.current !== targetKey) {
        return null;
      }

      setTitlingExistingSettings(settings.title_generation ?? null);
      const draft = buildSessionTitlingDraft(settings);
      setTitlingRemoteBaseUrl(draft.remote.baseUrl);
      setTitlingRemoteApiKey(draft.remote.apiKey);
      setTitlingRemoteModel(draft.remote.model);
      setTitlingRemoteUseJson(draft.remote.useJson);
      setTitlingLocalUseJson(draft.local.useJson);
      setTitlingMode((currentMode) => (currentMode === "skip" ? "skip" : draft.mode));

      let localStatus: TitleGenerationLocalStatus | null = null;
      if (!settings.title_generation || settings.title_generation.mode === "local") {
        localStatus = await withTimeout(
          refreshTitlingLocalStatus({ silent: true }),
          TITLING_PROBE_TIMEOUT_MS,
          "Timed out loading daemon settings.",
        );
        if (!isCurrentProvisioningRequest("titlingProbe", request) || selectedDaemonTargetKeyRef.current !== targetKey) {
          return null;
        }
      } else {
        setTitlingLocalStatus(null);
        setTitlingStatusError(null);
      }

      const readiness = resolveSessionTitlingReadiness(settings, localStatus);
      setTitlingConfiguredReady(readiness.ready);
      setTitlingStepRequired(!readiness.ready);
      setTitlingProbeTargetKey(targetKey);
      setTitlingProbeDone(true);
      if (localStatus?.install_running && localStatus.install_id) {
        void attachTitlingInstall(localStatus.install_id).catch(() => {});
      }
      return !readiness.ready;
    } catch (error) {
      if (!isCurrentProvisioningRequest("titlingProbe", request) || selectedDaemonTargetKeyRef.current !== targetKey) {
        return null;
      }
      if (shouldTreatRemoteSpeculativeConnectFailureAsSkipped(error)) {
        setTitlingProbeTargetKey(targetKey);
        setTitlingProbeDone(true);
        setTitlingConfiguredReady(false);
        setTitlingStepRequired(false);
        setTitlingProbeError(null);
        return false;
      }
      setTitlingProbeTargetKey(targetKey);
      setTitlingProbeDone(true);
      setTitlingConfiguredReady(false);
      setTitlingStepRequired(true);
      setTitlingProbeError(messageFromError(error));
      throw error;
    } finally {
      if (isCurrentProvisioningRequest("titlingProbe", request) && selectedDaemonTargetKeyRef.current === targetKey) {
        setTitlingProbeBusy(false);
      }
    }
  }, [
    attachTitlingInstall,
    connectDaemonForImport,
    isCurrentProvisioningRequest,
    refreshTitlingLocalStatus,
    shouldTreatRemoteSpeculativeConnectFailureAsSkipped,
  ]);

  const currentTitlingPayload = useCallback((
    modeOverride?: "remote" | "local",
  ): UpdateTitleGenerationSettingsRequest | null => {
    const mode = modeOverride ?? titlingMode;
    if (mode !== "remote" && mode !== "local") return null;
    return buildSessionTitlingPayload({
      mode,
      draft: {
        mode,
        remote: {
          baseUrl: titlingRemoteBaseUrl,
          apiKey: titlingRemoteApiKey,
          model: titlingRemoteModel,
          useJson: titlingRemoteUseJson,
        },
        local: {
          modelId: DEFAULT_TITLE_LOCAL_MODEL_ID,
          useJson: titlingLocalUseJson,
        },
      },
      existing: titlingExistingSettings,
    });
  }, [
    titlingExistingSettings,
    titlingLocalUseJson,
    titlingMode,
    titlingRemoteApiKey,
    titlingRemoteBaseUrl,
    titlingRemoteModel,
    titlingRemoteUseJson,
  ]);

  const ensureTitlingPersistedForCurrentTarget = useCallback(async (
    modeOverride?: "remote" | "local",
  ): Promise<boolean> => {
    if (!modeOverride && titlingMode === "skip") return true;
    const payload = currentTitlingPayload(modeOverride);
    if (!payload || !selectedDaemonTargetKey) return false;
    const targetKey = selectedDaemonTargetKey;
    const payloadHash = sessionTitlingPayloadHash(payload);
    if (titlingPersistedTargetKey === targetKey && titlingPersistedHash === payloadHash) {
      return true;
    }

    setTitlingPersistBusy(true);
    setTitlingPersistError(null);
    try {
      await connectDaemonForImport();
      if (selectedDaemonTargetKeyRef.current !== targetKey) {
        return false;
      }
      const next = await updateSettings({ title_generation: payload });
      setTitlingExistingSettings(next.title_generation ?? null);
      setTitlingPersistedTargetKey(targetKey);
      setTitlingPersistedHash(payloadHash);
      if (next.title_generation?.mode === "remote") {
        const readiness = resolveSessionTitlingReadiness(next, null);
        setTitlingConfiguredReady(readiness.ready);
      } else {
        const localStatus = await refreshTitlingLocalStatus({ silent: true });
        const readiness = resolveSessionTitlingReadiness(next, localStatus);
        setTitlingConfiguredReady(readiness.ready);
        if (localStatus?.install_running && localStatus.install_id) {
          void attachTitlingInstall(localStatus.install_id).catch(() => {});
        }
      }
      return true;
    } catch (error) {
      setTitlingPersistError(messageFromError(error));
      return false;
    } finally {
      setTitlingPersistBusy(false);
    }
  }, [
    attachTitlingInstall,
    connectDaemonForImport,
    currentTitlingPayload,
    refreshTitlingLocalStatus,
    selectedDaemonTargetKey,
    titlingMode,
    titlingPersistedHash,
    titlingPersistedTargetKey,
  ]);

  const onSelectTitlingLocal = useCallback(() => {
    if (titlingLocalInstallBusy || titlingPersistBusy) return false;
    invalidateTitlingPersisted();
    setTitlingMode("local");
    setTitlingLocalInstallBusy(true);
    setTitlingStatusError(null);
    setTitlingPersistError(null);
    void (async () => {
      try {
        const persisted = await ensureTitlingPersistedForCurrentTarget("local");
        if (!persisted) return;
        const { install_id } = await installTitleGenerationLocal();
        void attachTitlingInstall(install_id).catch((error) => {
          setTitlingStatusError(messageFromError(error));
        });
      } catch (error) {
        setTitlingStatusError(messageFromError(error));
      } finally {
        setTitlingLocalInstallBusy(false);
      }
    })();
    return true;
  }, [
    attachTitlingInstall,
    ensureTitlingPersistedForCurrentTarget,
    invalidateTitlingPersisted,
    titlingLocalInstallBusy,
    titlingPersistBusy,
  ]);

  const scanTitlingProbeForRequest = useCallback(async (
    request: WorkspaceSetupProvisioningRequest,
  ): Promise<void> => {
    if (!desktopApp) {
      if (!isCurrentProvisioningRequest("titlingProbe", request)) {
        return;
      }
      setTitlingProbeBusy(false);
      setTitlingProbeError(null);
      setTitlingProbeDone(true);
      setTitlingConfiguredReady(false);
      setTitlingStepRequired(false);
      setTitlingProbeTargetKey(selectedDaemonTargetKeyRef.current);
      commitProvisioningMachineState((current) => completeWorkspaceSetupTitlingProbeRefresh(current, {
        scope: request.scope,
        requestId: request.requestId,
        data: { required: false },
      }));
      return;
    }
    const targetKey = selectedDaemonTargetKeyRef.current;
    if (!targetKey) return;
    try {
      const required = await probeTitlingForTarget(request, targetKey);
      if (!isCurrentProvisioningRequest("titlingProbe", request)) {
        return;
      }
      commitProvisioningMachineState((current) => completeWorkspaceSetupTitlingProbeRefresh(current, {
        scope: request.scope,
        requestId: request.requestId,
        data: { required: required === true },
      }));
    } catch (error) {
      const message = messageFromError(error);
      if (!isCurrentProvisioningRequest("titlingProbe", request)) {
        return;
      }
      commitProvisioningMachineState((current) => failWorkspaceSetupTitlingProbeRefresh(current, {
        scope: request.scope,
        requestId: request.requestId,
        error: message,
      }));
    }
  }, [
    commitProvisioningMachineState,
    desktopApp,
    isCurrentProvisioningRequest,
    probeTitlingForTarget,
  ]);

  const prefetchTitlingForCurrentTarget = useCallback(async (
    locationOverride?: "local" | "remote",
  ): Promise<void> => {
    if (!desktopApp) return;
    const location = locationOverride ?? selections.location;
    if (location !== "local" && location !== "remote") {
      return;
    }
    if (location === "remote" && (!parsedRemoteHost || remoteStatusRef.current !== "connected")) {
      return;
    }
    await withTimeout(
      connectDaemonForImport(location),
      TITLING_PROBE_TIMEOUT_MS,
      "Timed out loading daemon settings.",
    );
    const settings = await withTimeout(
      getSettings(),
      TITLING_PROBE_TIMEOUT_MS,
      "Timed out loading daemon settings.",
    );
    if (!settings.title_generation || settings.title_generation.mode === "local") {
      await withTimeout(
        refreshTitlingLocalStatus({ silent: true }),
        TITLING_PROBE_TIMEOUT_MS,
        "Timed out loading daemon settings.",
      );
    }
  }, [
    connectDaemonForImport,
    desktopApp,
    parsedRemoteHost,
    refreshTitlingLocalStatus,
    remoteStatusRef,
    selections.location,
  ]);

  const titlingRemoteValid = isTitlingRemoteValid(
    titlingRemoteBaseUrl,
    titlingRemoteApiKey,
    titlingRemoteModel,
  );
  const titlingSummaryValue = buildTitlingSummaryValue({
    titlingMode,
    titlingRemoteBaseUrl,
    titlingRemoteApiKey,
    titlingRemoteModel,
    titlingConfiguredReady,
    titlingLocalStatus,
    titlingExistingSettings,
  });

  useWorkspaceSetupTitlingInstallProgress({
    titlingLocalInstall,
    setTitlingLocalInstall,
    titlingInstallObserverRef,
    titlingInstallStateRef,
    clearTitlingInstallObserver,
    refreshTitlingLocalStatus,
  });

  useEffect(() => {
    selectedDaemonTargetKeyRef.current = selectedDaemonTargetKey;
  }, [selectedDaemonTargetKey]);

  useEffect(() => {
    if (!selectedDaemonTargetKey || !canProbeTitling) {
      resetTitlingProvisioningState();
      return;
    }

    if (titlingProbeTargetKey !== selectedDaemonTargetKey) {
      setTitlingProbeError(null);
      setTitlingProbeDone(false);
      setTitlingConfiguredReady(false);
      setTitlingStepRequired(false);
      setTitlingPersistError(null);
      setTitlingPersistedTargetKey(null);
      setTitlingPersistedHash(null);
      setTitlingExistingSettings(null);
      setTitlingLocalStatus(null);
      setTitlingLocalStatusRequestedTargetKey(null);
      setTitlingStatusError(null);
      setTitlingLocalInstall(null);
      clearTitlingInstallObserver();
      resetTitlingDraft();
    }
  }, [
    canProbeTitling,
    clearTitlingInstallObserver,
    resetTitlingDraft,
    resetTitlingProvisioningState,
    selectedDaemonTargetKey,
    titlingProbeTargetKey,
  ]);

  useEffect(() => {
    return () => {
      clearTitlingInstallObserver();
    };
  }, [clearTitlingInstallObserver]);

  useEffect(() => {
    if (titlingMode === "local") return;
    setTitlingLocalStatusRequestedTargetKey(null);
  }, [titlingMode]);

  useEffect(() => {
    if (titlingMode !== "local") return;
    if (!selectedDaemonTargetKey || !canProbeTitling) return;
    if (titlingLocalStatus || titlingLocalStatusBusy) return;
    if (titlingLocalStatusRequestedTargetKey === selectedDaemonTargetKey) return;
    setTitlingLocalStatusRequestedTargetKey(selectedDaemonTargetKey);
    void refreshTitlingLocalStatus().catch(() => {});
  }, [
    canProbeTitling,
    refreshTitlingLocalStatus,
    selectedDaemonTargetKey,
    titlingLocalStatus,
    titlingLocalStatusBusy,
    titlingLocalStatusRequestedTargetKey,
    titlingMode,
  ]);

  return {
    titlingStepVisible,
    titlingProbeBusy,
    titlingProbeError,
    titlingConfiguredReady,
    titlingMode,
    setTitlingMode,
    titlingRemoteBaseUrl,
    setTitlingRemoteBaseUrl,
    titlingRemoteApiKey,
    setTitlingRemoteApiKey,
    titlingRemoteModel,
    setTitlingRemoteModel,
    titlingRemoteUseJson,
    setTitlingRemoteUseJson,
    titlingRemoteAdvancedOpen,
    setTitlingRemoteAdvancedOpen,
    titlingLocalStatus,
    titlingStatusError,
    titlingLocalInstallBusy,
    titlingLocalInstall,
    titlingPersistBusy,
    titlingPersistError,
    setTitlingPersistError,
    titlingRemoteValid,
    titlingSummaryValue,
    invalidateTitlingPersisted,
    ensureTitlingPersistedForCurrentTarget,
    prefetchTitlingForCurrentTarget,
    onSelectTitlingLocal,
    scanTitlingProbeForRequest,
    resetTitlingProvisioningState,
  };
}
