import { TextInput } from "../../../components/ui/text-input";
import { Card, Row } from "../SettingsPage.components";
import { formatGiB } from "../SettingsPage.utils";

export const MACHINE_MEMORY_DESCRIPTION =
  "ctx sizes the local sandbox runtime automatically for this machine. Changes apply when the sandbox runtime is recreated.";

export function formatResolvedMachineMemory(memoryMb: number | null | undefined): string {
  if (typeof memoryMb !== "number" || !Number.isFinite(memoryMb) || memoryMb <= 0) {
    return "Automatic";
  }
  const gib = formatGiB(memoryMb);
  return gib ? `${gib.replace(/\.0$/, "")} GiB` : "Automatic";
}

export function SandboxingSection({
  loaded,
  resolvedMachineMemoryMb,
  idleShutdownSeconds,
  onIdleShutdownSecondsChange,
  hostPressureSwapThresholdMb,
  onHostPressureSwapThresholdMbChange,
  canSaveMachineSettings,
}: {
  loaded: boolean;
  resolvedMachineMemoryMb: number | null;
  idleShutdownSeconds: string;
  onIdleShutdownSecondsChange: (value: string) => void;
  hostPressureSwapThresholdMb: string;
  onHostPressureSwapThresholdMbChange: (value: string) => void;
  canSaveMachineSettings: boolean;
}) {
  return (
    <>
      <Card title="Local Sandbox Runtime">
        <Row
          title="Machine memory target"
          description={MACHINE_MEMORY_DESCRIPTION}
          control={<span className="settings-control">{formatResolvedMachineMemory(resolvedMachineMemoryMb)}</span>}
        />
        <Row
          title="Idle shutdown (seconds)"
          description="Stop the local sandbox runtime after this much inactivity to reclaim RAM and swap."
          control={
            <TextInput
              className="settings-control"
              type="number"
              min={60}
              step={60}
              value={idleShutdownSeconds}
              onChange={(event) => onIdleShutdownSecondsChange(event.target.value)}
              disabled={!loaded}
              placeholder="900"
            />
          }
        />
        <Row
          title="Host pressure swap threshold (MiB)"
          description="If host swap use exceeds this threshold, ctx may stop an idle sandbox runtime sooner."
          control={
            <TextInput
              className="settings-control"
              type="number"
              min={0}
              step={128}
              value={hostPressureSwapThresholdMb}
              onChange={(event) => onHostPressureSwapThresholdMbChange(event.target.value)}
              disabled={!loaded}
              placeholder="1024"
            />
          }
        />
      </Card>

      {!canSaveMachineSettings ? (
        <div className="settings-banner settings-banner-error">
          Enter a valid idle timeout and swap threshold.
        </div>
      ) : null}
    </>
  );
}
