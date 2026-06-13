import type {
  ResourceGovernanceLimits,
  ResourceGovernanceSettings,
  ResourceGovernanceStatus,
} from "../../../api/client";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../../../components/ui/select";
import { TextInput } from "../../../components/ui/text-input";
import { Card, Row, Toggle } from "../SettingsPage.components";
import { formatGiB } from "../SettingsPage.utils";

export function ResourceGovernanceSection({
  loaded,
  enabled,
  onEnabledChange,
  mode,
  onModeChange,
  cpuQuotaPct,
  onCpuQuotaPctChange,
  memoryHighGb,
  onMemoryHighGbChange,
  memoryMaxGb,
  onMemoryMaxGbChange,
  effective,
  status,
  saving,
  canSave,
  payload,
  onApplyNow,
}: {
  loaded: boolean;
  enabled: boolean;
  onEnabledChange: (value: boolean) => void;
  mode: ResourceGovernanceSettings["mode"];
  onModeChange: (value: ResourceGovernanceSettings["mode"]) => void;
  cpuQuotaPct: string;
  onCpuQuotaPctChange: (value: string) => void;
  memoryHighGb: string;
  onMemoryHighGbChange: (value: string) => void;
  memoryMaxGb: string;
  onMemoryMaxGbChange: (value: string) => void;
  effective: ResourceGovernanceLimits | null;
  status: ResourceGovernanceStatus | null;
  saving: boolean;
  canSave: boolean;
  payload: ResourceGovernanceSettings;
  onApplyNow: (payload: ResourceGovernanceSettings) => void | Promise<void>;
}) {
  const effectiveCpu = effective?.cpu_quota_pct ?? null;
  const effectiveHigh = effective?.memory_high_mb ?? null;
  const effectiveMax = effective?.memory_max_mb ?? null;
  const statusState = status?.state ?? (enabled ? "pending" : "disabled");
  const statusLabel =
    statusState === "disabled"
      ? "Disabled"
      : statusState === "applied"
        ? "Applied"
        : statusState === "unsupported"
          ? "Unsupported"
          : statusState === "error"
            ? "Error"
            : "Pending";
  const statusMessage = status?.message ?? null;
  const showApplyNow = statusState === "pending" && Boolean(status?.can_apply_now);
  const showRestart = Boolean(status?.requires_restart);

  return (
    <>
      <Card title="Resource Governance">
        <Row
          title="Enable resource limits"
          description="Keep the host responsive by throttling agent workloads."
          control={
            <Toggle
              checked={enabled}
              disabled={!loaded}
              onChange={onEnabledChange}
              ariaLabel="Enable resource limits"
            />
          }
        />
        <Row
          title="Mode"
          description="Auto picks safe limits for this machine."
          control={
            <Select value={mode} onValueChange={(value) => onModeChange(value as ResourceGovernanceSettings["mode"])} disabled={!enabled}>
              <SelectTrigger className="tw-min-w-[10rem]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="auto">Auto (recommended)</SelectItem>
                <SelectItem value="custom">Custom</SelectItem>
              </SelectContent>
            </Select>
          }
        />
        {mode === "custom" ? (
          <>
            <Row
              title="CPU quota (%)"
              description="100% = 1 core. Leave empty to use auto."
              control={
                <TextInput
                  className="settings-control"
                  type="number"
                  min={50}
                  step={10}
                  value={cpuQuotaPct}
                  onChange={(event) => onCpuQuotaPctChange(event.target.value)}
                  disabled={!enabled}
                  placeholder="300"
                />
              }
            />
            <Row
              title="Memory high (GiB)"
              description="Soft limit for reclaim pressure."
              control={
                <TextInput
                  className="settings-control"
                  type="number"
                  min={0}
                  step={0.5}
                  value={memoryHighGb}
                  onChange={(event) => onMemoryHighGbChange(event.target.value)}
                  disabled={!enabled}
                  placeholder="48"
                />
              }
            />
            <Row
              title="Memory max (GiB)"
              description="Hard limit; processes are killed when exceeded."
              control={
                <TextInput
                  className="settings-control"
                  type="number"
                  min={0}
                  step={0.5}
                  value={memoryMaxGb}
                  onChange={(event) => onMemoryMaxGbChange(event.target.value)}
                  disabled={!enabled}
                  placeholder="54"
                />
              }
            />
          </>
        ) : null}
      </Card>

      <Card title="Effective limits">
        <Row
          title="CPU quota"
          description="Applied to the daemon and its child processes."
          control={<span className="settings-pill wb-mono">{effectiveCpu ? `${effectiveCpu}%` : "—"}</span>}
        />
        <Row
          title="Memory high / max"
          description="High is the soft threshold; max is the hard cap."
          control={
            <span className="settings-pill wb-mono">
              {effectiveHigh ? `${formatGiB(effectiveHigh)} GiB` : "—"} / {effectiveMax ? `${formatGiB(effectiveMax)} GiB` : "—"}
            </span>
          }
        />
        <Row
          title="Apply status"
          description={statusMessage ?? "Apply changes to update live limits."}
          control={
            <div className="row" style={{ gap: 8 }}>
              <span className="settings-pill">{statusLabel}</span>
              {showRestart ? <span className="settings-pill settings-pill-warn">Restart required</span> : null}
              {showApplyNow ? (
                <button
                  type="button"
                  className="settings-btn settings-btn-secondary"
                  onClick={() => void onApplyNow(payload)}
                  disabled={saving || !canSave}
                >
                  Apply now
                </button>
              ) : null}
            </div>
          }
        />
      </Card>

      {!canSave && mode === "custom" ? (
        <div className="settings-banner settings-banner-error">Memory high must be less than or equal to memory max.</div>
      ) : null}
    </>
  );
}
