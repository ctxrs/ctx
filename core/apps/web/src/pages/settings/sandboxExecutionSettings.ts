import type {
  ExecutionSettings as ApiExecutionSettings,
  PublicExecutionSettings,
  UpdateExecutionSettingsRequest,
} from "../../api/client";

export const DEFAULT_MACHINE_IDLE_SHUTDOWN_SECONDS = 60 * 60;
export const DEFAULT_MACHINE_HOST_PRESSURE_SWAP_THRESHOLD_MB = 1024;
export const MIN_MACHINE_IDLE_SHUTDOWN_SECONDS = 60;

export function defaultContainerMountMode(
  _runtime: ApiExecutionSettings["container"]["runtime"],
): ApiExecutionSettings["container"]["mount_mode"] {
  return "disk_isolated";
}

export function defaultExecutionSettings(): ApiExecutionSettings {
  const runtime: ApiExecutionSettings["container"]["runtime"] = "native_container";
  return {
    mode: "host",
    container: {
      runtime,
      mount_mode: defaultContainerMountMode(runtime),
      network_mode: "llm_only",
      allowlist: [],
      image: null,
      machine: {
        memory_profile: "economy",
        custom_memory_mb: null,
        idle_shutdown_seconds: DEFAULT_MACHINE_IDLE_SHUTDOWN_SECONDS,
        host_pressure_swap_threshold_mb: DEFAULT_MACHINE_HOST_PRESSURE_SWAP_THRESHOLD_MB,
      },
    },
  };
}

export function normalizeExecutionSettings(
  value: ApiExecutionSettings | PublicExecutionSettings | null | undefined,
): ApiExecutionSettings {
  const fallback = defaultExecutionSettings();
  const runtime =
    value?.container && "runtime" in value.container
      ? value.container.runtime
      : fallback.container.runtime;
  const mountMode =
    value?.container && "mount_mode" in value.container
      ? value.container.mount_mode
      : fallback.container.mount_mode;
  return {
    mode: value?.mode ?? fallback.mode,
    container: {
      runtime,
      mount_mode: mountMode,
      network_mode: value?.container?.network_mode ?? fallback.container.network_mode,
      allowlist: value?.container?.allowlist ?? fallback.container.allowlist,
      image: value?.container?.image ?? fallback.container.image,
      machine: {
        memory_profile: value?.container?.machine?.memory_profile ?? fallback.container.machine.memory_profile,
        custom_memory_mb: value?.container?.machine?.custom_memory_mb ?? fallback.container.machine.custom_memory_mb,
        idle_shutdown_seconds:
          value?.container?.machine?.idle_shutdown_seconds ?? fallback.container.machine.idle_shutdown_seconds,
        host_pressure_swap_threshold_mb:
          value?.container?.machine?.host_pressure_swap_threshold_mb
          ?? fallback.container.machine.host_pressure_swap_threshold_mb,
      },
    },
  };
}

export function toExecutionUpdateRequest(
  value: ApiExecutionSettings,
): UpdateExecutionSettingsRequest {
  return {
    mode: value.mode,
    container: {
      network_mode: value.container.network_mode,
      allowlist: value.container.allowlist,
      image: value.container.image,
      machine: value.container.machine,
    },
  };
}

export function parseUnsignedInteger(value: string): number | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  if (!Number.isFinite(parsed) || parsed < 0) return null;
  return Math.round(parsed);
}

export function canSaveSandboxMachineSettings({
  machineHostPressureSwapThresholdMb,
  machineIdleShutdownSeconds,
}: {
  machineHostPressureSwapThresholdMb: string;
  machineIdleShutdownSeconds: string;
}): boolean {
  const idle = parseUnsignedInteger(machineIdleShutdownSeconds);
  if (idle === null || idle < MIN_MACHINE_IDLE_SHUTDOWN_SECONDS) return false;
  const threshold = parseUnsignedInteger(machineHostPressureSwapThresholdMb);
  return threshold !== null;
}
