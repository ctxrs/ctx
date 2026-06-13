import { useCallback, useEffect, useRef, useState } from "react";
import type { ExecutionLaunchSnapshot } from "../../api/client";
import {
  currentLaunchStepLabel as deriveCurrentLaunchStepLabel,
  formatLaunchElapsed,
  formatLaunchRemaining,
  formatLaunchTime,
  launchElapsedMs,
  launchEtaRemainingMs,
  stabilizeLaunchEtaRemainingMs,
  workspaceSetupProvisioningPhaseLabel,
  workspaceSetupProvisioningRemainingMs,
  type WorkspaceSetupProvisioningExecutionMode,
  type WorkspaceSetupProvisioningPhase,
  type WorkspaceSetupProvisioningSource,
  type WorkspaceSetupLaunchLogLine,
} from "./launchProgress";
import { copyWorkspaceSetupLaunchDiagnostics } from "./workspaceSetupLaunchHelpers";

type WorkspaceProvisioningState = {
  phase: WorkspaceSetupProvisioningPhase;
  stepLabel: string;
  startedAtMs: number;
  phaseStartedAtMs: number;
  updatedAtMs: number;
  state: "running" | "ready" | "error";
  error: string | null;
  workspaceId: string | null;
};

type UseWorkspaceSetupCreateProgressArgs = {
  creating: boolean;
  provisioningSource: WorkspaceSetupProvisioningSource;
  provisioningExecutionMode: WorkspaceSetupProvisioningExecutionMode;
};

const SYNTHETIC_WORKSPACE_SETUP_JOB_ID = "workspace-setup-provisioning";

const buildSyntheticLaunchSnapshot = (
  state: WorkspaceProvisioningState | null,
): ExecutionLaunchSnapshot | null => {
  if (!state) return null;
  const startedAt = new Date(state.startedAtMs).toISOString();
  return {
    job_id: SYNTHETIC_WORKSPACE_SETUP_JOB_ID,
    workspace_id: state.workspaceId ?? "pending",
    kind: "workspace_launch",
    state: state.state,
    created_at: startedAt,
    started_at: startedAt,
    updated_at: new Date(state.updatedAtMs).toISOString(),
    finished_at: state.state === "running" ? null : new Date(state.updatedAtMs).toISOString(),
    current_phase: null,
    current_step_label: state.stepLabel,
    progress_pct: null,
    eta_ms: null,
    active_download: null,
    phases: [],
    logs: [],
    error: state.error,
  };
};

export function useWorkspaceSetupCreateProgress({
  creating,
  provisioningSource,
  provisioningExecutionMode,
}: UseWorkspaceSetupCreateProgressArgs) {
  const [sandboxPrepareMessage, setSandboxPrepareMessage] = useState<string | null>(null);
  const [launchSnapshot, setLaunchSnapshot] = useState<ExecutionLaunchSnapshot | null>(null);
  const [launchLogs, setLaunchLogs] = useState<WorkspaceSetupLaunchLogLine[]>([]);
  const [provisioningState, setProvisioningState] = useState<WorkspaceProvisioningState | null>(null);
  const [launchEtaDisplayMs, setLaunchEtaDisplayMs] = useState<number | null>(null);
  const [launchTick, setLaunchTick] = useState(0);
  const [launchCopyState, setLaunchCopyState] = useState<"idle" | "copied" | "failed">("idle");
  const provisioningStateRef = useRef<WorkspaceProvisioningState | null>(null);
  const syntheticLogSeqRef = useRef(-1);
  const launchEtaDisplayRef = useRef<{
    nowMs: number | null;
    rawRemainingMs: number | null;
    recalibrationTargetMs: number | null;
    remainingMs: number | null;
  }>({
    nowMs: null,
    rawRemainingMs: null,
    recalibrationTargetMs: null,
    remainingMs: null,
  });

  useEffect(() => {
    provisioningStateRef.current = provisioningState;
  }, [provisioningState]);

  const appendSyntheticLaunchLog = useCallback((
    phase: WorkspaceSetupProvisioningPhase,
    message: string,
    level: "info" | "warn" | "error" = "info",
  ) => {
    const ts = new Date().toISOString();
    const seq = syntheticLogSeqRef.current;
    syntheticLogSeqRef.current -= 1;
    setLaunchLogs((prev) => prev.concat({
      seq,
      ts,
      phase: "machine_check",
      level,
      message,
      phaseLabel: workspaceSetupProvisioningPhaseLabel(phase),
      provisioningPhase: phase,
      timeLabel: formatLaunchTime(ts),
    }).slice(-400));
  }, []);

  const beginProvisioningPhase = useCallback((
    phase: WorkspaceSetupProvisioningPhase,
    stepLabel: string,
    message: string,
    workspaceId?: string | null,
  ) => {
    const nowMs = Date.now();
    const startedAtMs = provisioningStateRef.current?.startedAtMs ?? nowMs;
    const nextState: WorkspaceProvisioningState = {
      phase,
      stepLabel,
      startedAtMs,
      phaseStartedAtMs: nowMs,
      updatedAtMs: nowMs,
      state: "running",
      error: null,
      workspaceId: workspaceId ?? provisioningStateRef.current?.workspaceId ?? null,
    };
    provisioningStateRef.current = nextState;
    setProvisioningState(nextState);
    appendSyntheticLaunchLog(phase, message);
  }, [appendSyntheticLaunchLog]);

  const clearProvisioningState = useCallback(() => {
    provisioningStateRef.current = null;
    setProvisioningState(null);
  }, []);

  const markProvisioningError = useCallback((message: string) => {
    const current = provisioningStateRef.current;
    if (!current) return;
    const nowMs = Date.now();
    const nextState: WorkspaceProvisioningState = {
      ...current,
      updatedAtMs: nowMs,
      state: "error",
      error: message,
    };
    provisioningStateRef.current = nextState;
    setProvisioningState(nextState);
    appendSyntheticLaunchLog(current.phase, message, "error");
  }, [appendSyntheticLaunchLog]);

  const setProvisioningWorkspaceId = useCallback((workspaceId: string) => {
    setProvisioningState((current) => current ? { ...current, workspaceId } : current);
  }, []);

  const markProvisioningReady = useCallback(() => {
    setProvisioningState((current) => current ? {
      ...current,
      updatedAtMs: Date.now(),
      state: "ready",
    } : current);
  }, []);

  const resetLaunchProgressState = useCallback(() => {
    setLaunchSnapshot(null);
    setLaunchLogs([]);
    clearProvisioningState();
    launchEtaDisplayRef.current = {
      nowMs: null,
      rawRemainingMs: null,
      recalibrationTargetMs: null,
      remainingMs: null,
    };
    setLaunchEtaDisplayMs(null);
    setLaunchCopyState("idle");
    setLaunchTick(0);
    syntheticLogSeqRef.current = -1;
    setSandboxPrepareMessage(null);
  }, [clearProvisioningState]);

  const syntheticLaunchSnapshot = buildSyntheticLaunchSnapshot(provisioningState);
  const effectiveLaunchSnapshot =
    provisioningState?.state === "running"
      ? syntheticLaunchSnapshot
      : launchSnapshot ?? syntheticLaunchSnapshot;

  useEffect(() => {
    if (!creating || !effectiveLaunchSnapshot || effectiveLaunchSnapshot.state !== "running") return;
    const handle = window.setInterval(() => setLaunchTick((value) => value + 1), 1000);
    return () => window.clearInterval(handle);
  }, [
    creating,
    launchSnapshot?.job_id,
    launchSnapshot?.state,
    provisioningState?.phase,
    provisioningState?.state,
  ]);

  useEffect(() => {
    if (
      provisioningState?.state !== "running"
      || provisioningState.phase !== "launch_runtime"
      || !launchSnapshot
    ) {
      return;
    }
    clearProvisioningState();
  }, [
    clearProvisioningState,
    launchSnapshot,
    provisioningState?.phase,
    provisioningState?.state,
  ]);

  useEffect(() => {
    const nowMs = Date.now();
    if (!creating || !effectiveLaunchSnapshot) {
      launchEtaDisplayRef.current = {
        nowMs: null,
        rawRemainingMs: null,
        recalibrationTargetMs: null,
        remainingMs: null,
      };
      setLaunchEtaDisplayMs(null);
      return;
    }
    if (effectiveLaunchSnapshot.state === "ready") {
      launchEtaDisplayRef.current = {
        nowMs,
        rawRemainingMs: 0,
        recalibrationTargetMs: null,
        remainingMs: 0,
      };
      setLaunchEtaDisplayMs(0);
      return;
    }
    if (effectiveLaunchSnapshot.state === "error") {
      launchEtaDisplayRef.current = {
        nowMs,
        rawRemainingMs: null,
        recalibrationTargetMs: null,
        remainingMs: null,
      };
      setLaunchEtaDisplayMs(null);
      return;
    }
    const rawRemainingMs = provisioningState?.state === "running"
      ? workspaceSetupProvisioningRemainingMs({
        phase: provisioningState.phase,
        source: provisioningSource,
        executionMode: provisioningExecutionMode,
        phaseStartedAtMs: provisioningState.phaseStartedAtMs,
        nowMs,
      })
      : launchEtaRemainingMs(launchSnapshot, nowMs);
    const nextRemainingMs = stabilizeLaunchEtaRemainingMs({
      previousRemainingMs: launchEtaDisplayRef.current.remainingMs,
      previousRawRemainingMs: launchEtaDisplayRef.current.rawRemainingMs,
      previousRecalibrationTargetMs: launchEtaDisplayRef.current.recalibrationTargetMs,
      previousNowMs: launchEtaDisplayRef.current.nowMs,
      nowMs,
      rawRemainingMs,
    });
    launchEtaDisplayRef.current = {
      nowMs,
      rawRemainingMs,
      recalibrationTargetMs: nextRemainingMs.recalibrationTargetMs,
      remainingMs: nextRemainingMs.remainingMs,
    };
    setLaunchEtaDisplayMs(nextRemainingMs.remainingMs);
  }, [
    creating,
    launchSnapshot?.current_phase,
    launchSnapshot?.current_step_label,
    launchSnapshot?.job_id,
    launchSnapshot?.state,
    launchSnapshot?.updated_at,
    launchTick,
    provisioningExecutionMode,
    provisioningSource,
    provisioningState?.phase,
    provisioningState?.phaseStartedAtMs,
    provisioningState?.state,
    provisioningState?.stepLabel,
    provisioningState?.updatedAtMs,
    provisioningState?.workspaceId,
  ]);

  const onCopyLaunchDiagnostics = useCallback(async () => {
    await copyWorkspaceSetupLaunchDiagnostics(
      effectiveLaunchSnapshot,
      launchLogs,
      setLaunchCopyState,
    );
  }, [effectiveLaunchSnapshot, launchLogs]);

  const currentLaunchElapsed = formatLaunchElapsed(launchElapsedMs(effectiveLaunchSnapshot, Date.now()));
  const currentLaunchStepLabel = deriveCurrentLaunchStepLabel(effectiveLaunchSnapshot);
  const currentLaunchEtaLabel = effectiveLaunchSnapshot?.state === "ready"
    ? "Ready"
    : effectiveLaunchSnapshot?.state === "error"
      ? "Launch failed"
      : formatLaunchRemaining(launchEtaDisplayMs);

  return {
    sandboxPrepareMessage,
    setSandboxPrepareMessage,
    launchSnapshot: effectiveLaunchSnapshot,
    setLaunchSnapshot,
    launchLogs,
    setLaunchLogs,
    appendSyntheticLaunchLog,
    beginProvisioningPhase,
    markProvisioningError,
    setProvisioningWorkspaceId,
    markProvisioningReady,
    resetLaunchProgressState,
    currentLaunchStepLabel,
    currentLaunchElapsed,
    currentLaunchEtaLabel,
    launchCopyLabel: launchCopyState === "copied"
      ? "Copied"
      : launchCopyState === "failed"
        ? "Copy failed"
        : "Copy diagnostics",
    showLaunchPanel: Boolean(effectiveLaunchSnapshot) && (creating || effectiveLaunchSnapshot?.state === "error"),
    onCopyLaunchDiagnostics,
  };
}
