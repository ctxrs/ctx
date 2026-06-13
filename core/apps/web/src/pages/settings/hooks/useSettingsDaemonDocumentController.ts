import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  type ExecutionSettings as ApiExecutionSettings,
  type PublicTelemetrySettings,
  type ResourceGovernanceLimits,
  type ResourceGovernanceSettings,
  type ResourceGovernanceStatus,
  type UpdateSettingsRequest,
  getSettings,
  updateSettings,
} from "../../../api/client";
import { errorMessage } from "../../../utils/errorMessage";
import {
  DEFAULT_MACHINE_HOST_PRESSURE_SWAP_THRESHOLD_MB,
  DEFAULT_MACHINE_IDLE_SHUTDOWN_SECONDS,
  canSaveSandboxMachineSettings,
  defaultExecutionSettings,
  normalizeExecutionSettings,
  parseUnsignedInteger,
  toExecutionUpdateRequest,
} from "../sandboxExecutionSettings";
import { executionSettingsStableKey, formatGiB, parseGiB } from "../SettingsPage.utils";

type SettingsTelemetryController = {
  enabled: boolean;
  source: PublicTelemetrySettings["source"];
  setEnabled: (next: boolean) => void;
};

type SettingsResourceGovernanceController = {
  enabled: boolean;
  setEnabled: (value: boolean) => void;
  mode: ResourceGovernanceSettings["mode"];
  setMode: (value: ResourceGovernanceSettings["mode"]) => void;
  cpuQuotaPct: string;
  setCpuQuotaPct: (value: string) => void;
  memoryHighGb: string;
  setMemoryHighGb: (value: string) => void;
  memoryMaxGb: string;
  setMemoryMaxGb: (value: string) => void;
  effective: ResourceGovernanceLimits | null;
  status: ResourceGovernanceStatus | null;
  canSave: boolean;
  payload: ResourceGovernanceSettings;
  onApplyNow: (payload: ResourceGovernanceSettings) => Promise<void>;
};

type SettingsSandboxingController = {
  machineResolvedMemoryMb: number | null;
  machineIdleShutdownSeconds: string;
  setMachineIdleShutdownSeconds: (value: string) => void;
  machineHostPressureSwapThresholdMb: string;
  setMachineHostPressureSwapThresholdMb: (value: string) => void;
  sandboxMachineCanSave: boolean;
};

type SettingsDaemonDocumentController = {
  loaded: boolean;
  loadError: string | null;
  saveError: string | null;
  saving: boolean;
  telemetry: SettingsTelemetryController;
  resourceGovernance: SettingsResourceGovernanceController;
  sandboxing: SettingsSandboxingController;
};

export function useSettingsDaemonDocumentController(): SettingsDaemonDocumentController {
  const [loaded, setLoaded] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const saveSeq = useRef(0);

  const telemetryHydrated = useRef(false);
  const resourceGovernanceHydrated = useRef(false);
  const executionHydrated = useRef(false);
  const savedExecutionPayloadKey = useRef<string | null>(null);

  const [telemetryEnabled, setTelemetryEnabled] = useState(true);
  const [telemetryEndpoint, setTelemetryEndpoint] = useState("");
  const [telemetrySource, setTelemetrySource] = useState<PublicTelemetrySettings["source"]>("default");

  const [resourceGovernanceEnabled, setResourceGovernanceEnabled] = useState(true);
  const [resourceGovernanceMode, setResourceGovernanceMode] =
    useState<ResourceGovernanceSettings["mode"]>("auto");
  const [resourceCpuQuotaPct, setResourceCpuQuotaPct] = useState("");
  const [resourceMemoryHighGb, setResourceMemoryHighGb] = useState("");
  const [resourceMemoryMaxGb, setResourceMemoryMaxGb] = useState("");
  const [resourceEffective, setResourceEffective] = useState<ResourceGovernanceLimits | null>(null);
  const [resourceStatus, setResourceStatus] = useState<ResourceGovernanceStatus | null>(null);

  const [executionSettings, setExecutionSettings] = useState<ApiExecutionSettings>(() => defaultExecutionSettings());
  const [machineResolvedMemoryMb, setMachineResolvedMemoryMb] = useState<number | null>(null);
  const [machineIdleShutdownSeconds, setMachineIdleShutdownSeconds] = useState(
    String(DEFAULT_MACHINE_IDLE_SHUTDOWN_SECONDS),
  );
  const [machineHostPressureSwapThresholdMb, setMachineHostPressureSwapThresholdMb] = useState(
    String(DEFAULT_MACHINE_HOST_PRESSURE_SWAP_THRESHOLD_MB),
  );

  const savePatch = useCallback(async (patch: UpdateSettingsRequest) => {
    setSaveError(null);
    setSaving(true);
    const seq = ++saveSeq.current;

    try {
      const next = await updateSettings(patch);
      if (seq !== saveSeq.current) return;

      if (next.telemetry) {
        setTelemetryEnabled(next.telemetry.enabled);
        setTelemetryEndpoint(next.telemetry.endpoint ?? "");
        setTelemetrySource(next.telemetry.source);
      }

      if (next.resource_governance) {
        setResourceGovernanceEnabled(next.resource_governance.enabled);
        setResourceGovernanceMode(next.resource_governance.mode ?? "auto");
        setResourceCpuQuotaPct(
          next.resource_governance.cpu_quota_pct ? String(next.resource_governance.cpu_quota_pct) : "",
        );
        setResourceMemoryHighGb(formatGiB(next.resource_governance.memory_high_mb));
        setResourceMemoryMaxGb(formatGiB(next.resource_governance.memory_max_mb));
        setResourceEffective(next.resource_governance.effective ?? null);
        setResourceStatus(next.resource_governance.status ?? null);
      }

      const nextExecution = normalizeExecutionSettings(next.execution ?? null);
      setMachineResolvedMemoryMb(next.execution?.container.machine.target_memory_mb ?? null);
      savedExecutionPayloadKey.current = executionSettingsStableKey(nextExecution);
      setExecutionSettings(nextExecution);
      setMachineIdleShutdownSeconds(String(nextExecution.container.machine.idle_shutdown_seconds));
      setMachineHostPressureSwapThresholdMb(
        String(nextExecution.container.machine.host_pressure_swap_threshold_mb),
      );
    } catch (error: unknown) {
      if (seq !== saveSeq.current) return;
      setSaveError(errorMessage(error));
    } finally {
      if (seq === saveSeq.current) {
        setSaving(false);
      }
    }
  }, []);

  useEffect(() => {
    let cancelled = false;

    (async () => {
      try {
        const settings = await getSettings();
        if (cancelled) return;

        const telemetry = settings.telemetry ?? null;
        if (telemetry) {
          setTelemetryEnabled(telemetry.enabled);
          setTelemetryEndpoint(telemetry.endpoint ?? "");
          setTelemetrySource(telemetry.source);
        } else {
          setTelemetryEnabled(true);
          setTelemetryEndpoint("");
          setTelemetrySource("default");
        }

        const resourceGovernance = settings.resource_governance ?? null;
        if (resourceGovernance) {
          setResourceGovernanceEnabled(resourceGovernance.enabled);
          setResourceGovernanceMode(resourceGovernance.mode ?? "auto");
          setResourceCpuQuotaPct(resourceGovernance.cpu_quota_pct ? String(resourceGovernance.cpu_quota_pct) : "");
          setResourceMemoryHighGb(formatGiB(resourceGovernance.memory_high_mb));
          setResourceMemoryMaxGb(formatGiB(resourceGovernance.memory_max_mb));
          setResourceEffective(resourceGovernance.effective ?? null);
          setResourceStatus(resourceGovernance.status ?? null);
        }

        const nextExecution = normalizeExecutionSettings(settings.execution ?? null);
        setMachineResolvedMemoryMb(settings.execution?.container.machine.target_memory_mb ?? null);
        savedExecutionPayloadKey.current = executionSettingsStableKey(nextExecution);
        setExecutionSettings(nextExecution);
        setMachineIdleShutdownSeconds(String(nextExecution.container.machine.idle_shutdown_seconds));
        setMachineHostPressureSwapThresholdMb(
          String(nextExecution.container.machine.host_pressure_swap_threshold_mb),
        );

        setLoaded(true);
      } catch (error: unknown) {
        if (cancelled) return;
        setLoadError(errorMessage(error));
        setLoaded(true);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, []);

  const executionPayload = useMemo((): ApiExecutionSettings => {
    const idleShutdownSeconds =
      parseUnsignedInteger(machineIdleShutdownSeconds) ?? DEFAULT_MACHINE_IDLE_SHUTDOWN_SECONDS;
    const hostPressureSwapThresholdMb =
      parseUnsignedInteger(machineHostPressureSwapThresholdMb)
      ?? DEFAULT_MACHINE_HOST_PRESSURE_SWAP_THRESHOLD_MB;

    return {
      ...executionSettings,
      container: {
        ...executionSettings.container,
        machine: {
          memory_profile: executionSettings.container.machine.memory_profile,
          custom_memory_mb: executionSettings.container.machine.custom_memory_mb,
          idle_shutdown_seconds: idleShutdownSeconds,
          host_pressure_swap_threshold_mb: hostPressureSwapThresholdMb,
        },
      },
    };
  }, [executionSettings, machineHostPressureSwapThresholdMb, machineIdleShutdownSeconds]);

  const executionPayloadKey = useMemo(
    () => executionSettingsStableKey(executionPayload),
    [executionPayload],
  );

  const resourceGovernancePayload = useMemo((): ResourceGovernanceSettings => {
    const cpuQuota = Number(resourceCpuQuotaPct);
    const cpuQuotaPct = Number.isFinite(cpuQuota) && cpuQuota > 0 ? Math.round(cpuQuota) : null;
    const memoryHighMb = parseGiB(resourceMemoryHighGb);
    const memoryMaxMb = parseGiB(resourceMemoryMaxGb);
    return {
      enabled: resourceGovernanceEnabled,
      mode: resourceGovernanceMode,
      cpu_quota_pct: resourceGovernanceMode === "custom" ? cpuQuotaPct : null,
      memory_high_mb: resourceGovernanceMode === "custom" ? memoryHighMb : null,
      memory_max_mb: resourceGovernanceMode === "custom" ? memoryMaxMb : null,
    };
  }, [
    resourceCpuQuotaPct,
    resourceGovernanceEnabled,
    resourceGovernanceMode,
    resourceMemoryHighGb,
    resourceMemoryMaxGb,
  ]);

  const resourceGovernanceCanSave = useMemo(() => {
    if (!resourceGovernanceEnabled) return true;
    if (resourceGovernanceMode !== "custom") return true;
    const high = parseGiB(resourceMemoryHighGb);
    const max = parseGiB(resourceMemoryMaxGb);
    if (high && max && high > max) return false;
    return true;
  }, [resourceGovernanceEnabled, resourceGovernanceMode, resourceMemoryHighGb, resourceMemoryMaxGb]);

  const sandboxMachineCanSave = useMemo(() => {
    return canSaveSandboxMachineSettings({
      machineHostPressureSwapThresholdMb,
      machineIdleShutdownSeconds,
    });
  }, [machineHostPressureSwapThresholdMb, machineIdleShutdownSeconds]);

  useEffect(() => {
    if (!loaded) return;
    if (!telemetryHydrated.current) {
      telemetryHydrated.current = true;
      return;
    }

    const timer = window.setTimeout(() => {
      const next = {
        enabled: telemetryEnabled,
        endpoint: telemetryEndpoint.trim() || telemetryEndpoint,
      };
      void savePatch({ telemetry: next });
    }, 250);

    return () => window.clearTimeout(timer);
  }, [loaded, savePatch, telemetryEnabled, telemetryEndpoint]);

  useEffect(() => {
    if (!loaded) return;
    if (!executionHydrated.current) {
      executionHydrated.current = true;
      return;
    }
    if (!sandboxMachineCanSave) return;
    if (savedExecutionPayloadKey.current === executionPayloadKey) return;

    const timer = window.setTimeout(() => {
      void savePatch({ execution: toExecutionUpdateRequest(executionPayload) });
    }, 450);

    return () => window.clearTimeout(timer);
  }, [executionPayload, executionPayloadKey, loaded, sandboxMachineCanSave, savePatch]);

  useEffect(() => {
    if (!loaded) return;
    if (!resourceGovernanceHydrated.current) {
      resourceGovernanceHydrated.current = true;
      return;
    }
    if (!resourceGovernanceCanSave) return;

    const timer = window.setTimeout(() => {
      void savePatch({ resource_governance: resourceGovernancePayload });
    }, 450);

    return () => window.clearTimeout(timer);
  }, [loaded, resourceGovernanceCanSave, resourceGovernancePayload, savePatch]);

  return {
    loaded,
    loadError,
    saveError,
    saving,
    telemetry: {
      enabled: telemetryEnabled,
      source: telemetrySource,
      setEnabled: setTelemetryEnabled,
    },
    resourceGovernance: {
      enabled: resourceGovernanceEnabled,
      setEnabled: setResourceGovernanceEnabled,
      mode: resourceGovernanceMode,
      setMode: setResourceGovernanceMode,
      cpuQuotaPct: resourceCpuQuotaPct,
      setCpuQuotaPct: setResourceCpuQuotaPct,
      memoryHighGb: resourceMemoryHighGb,
      setMemoryHighGb: setResourceMemoryHighGb,
      memoryMaxGb: resourceMemoryMaxGb,
      setMemoryMaxGb: setResourceMemoryMaxGb,
      effective: resourceEffective,
      status: resourceStatus,
      canSave: resourceGovernanceCanSave,
      payload: resourceGovernancePayload,
      onApplyNow: async (payload) => {
        await savePatch({ resource_governance: payload });
      },
    },
    sandboxing: {
      machineResolvedMemoryMb,
      machineIdleShutdownSeconds,
      setMachineIdleShutdownSeconds,
      machineHostPressureSwapThresholdMb,
      setMachineHostPressureSwapThresholdMb,
      sandboxMachineCanSave,
    },
  };
}

export type {
  SettingsDaemonDocumentController,
  SettingsResourceGovernanceController,
  SettingsSandboxingController,
  SettingsTelemetryController,
};
