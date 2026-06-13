import Editor from "@monaco-editor/react";
import { Check, Loader2 } from "lucide-react";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../../../components/ui/select";
import { isContainerizedEnvironment, promptAutosaveStatusLabel } from "../SettingsPage.utils";
import { Card, Row } from "../SettingsPage.components";
import type { WorkspaceExecutionConfig } from "../../../api/client";
import { useContainerNetworkController } from "../hooks/useContainerNetworkController";
import type { SettingsSandboxingController } from "../hooks/useSettingsDaemonDocumentController";
import { GeneralSection } from "./GeneralSection";
import { SandboxingSection } from "./SandboxingSection";

type WorkspaceNetworkMode = NonNullable<WorkspaceExecutionConfig["network_mode"]>;

type ContainerNetworkSectionProps = {
  workspaceId: string | null;
  active: boolean;
  themeVariant: "dark" | "light";
  sandboxRuntimeLoaded: boolean;
  sandboxRuntimeLoadError: string | null;
  sandboxRuntime: SettingsSandboxingController;
};

export function ContainerNetworkSection({
  workspaceId,
  active,
  themeVariant,
  sandboxRuntimeLoaded,
  sandboxRuntimeLoadError,
  sandboxRuntime,
}: ContainerNetworkSectionProps) {
  const {
    workspaceExecution,
    workspaceExecutionLoading,
    workspaceNetworkPolicySaving,
    workspaceAllowlistSaving,
    workspaceAllowlistAutosaveState,
    workspaceAllowlistText,
    setWorkspaceAllowlistText,
    workspaceExecutionError,
    handleUpdateWorkspaceNetworkPolicy,
  } = useContainerNetworkController({
    workspaceId,
    enabled: active,
  });

  const exec = workspaceExecution;
  const allowlistActive = isContainerizedEnvironment(exec?.environment) && exec?.network_mode === "allowlist";
  const allowlistEditorDisabled = workspaceExecutionLoading || workspaceAllowlistSaving || !allowlistActive;
  const allowlistAutosaveStatusLabel = promptAutosaveStatusLabel(workspaceAllowlistAutosaveState);
  const allowlistAutosaveActive = workspaceAllowlistAutosaveState !== "idle";

  return (
    <GeneralSection>
      <div className="settings-preferences-flat">
        <div className="settings-preferences-group settings-preferences-group-center-controls">
          <Row
            title="Sandbox Mode"
            description="Sandbox mode is set during workspace creation. To use a different mode, launch your project in a new workspace."
            control={
              <Select value={exec?.environment ?? "host"} disabled>
                <SelectTrigger className="settings-control settings-select tw-min-w-[16rem]" aria-label="Sandbox mode">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="host">Host</SelectItem>
                  <SelectItem value="sandbox">Sandbox</SelectItem>
                </SelectContent>
              </Select>
            }
          />
          <Row
            title="Network Policy"
            description="Network policy allows you to restrict what outbound network access your agents have, such as blocking all access or only allowing certain hostnames. Network policy is only available for sandboxed workspaces."
            control={
              isContainerizedEnvironment(exec?.environment) ? (
                <Select
                  value={exec?.network_mode ?? "llm_only"}
                  onValueChange={(value) => {
                    void handleUpdateWorkspaceNetworkPolicy(value as WorkspaceNetworkMode);
                  }}
                  disabled={workspaceExecutionLoading || workspaceNetworkPolicySaving}
                >
                  <SelectTrigger className="settings-control settings-select tw-min-w-[16rem]" aria-label="Network policy">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="llm_only">LLM providers only</SelectItem>
                    <SelectItem value="allowlist">Allowlist</SelectItem>
                    <SelectItem value="all">Full access</SelectItem>
                  </SelectContent>
                </Select>
              ) : (
                <Select value="host_all_outbound_allowed" disabled>
                  <SelectTrigger className="settings-control settings-select tw-min-w-[16rem]" aria-label="Network policy">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="host_all_outbound_allowed">All outbound allowed</SelectItem>
                  </SelectContent>
                </Select>
              )
            }
          />
        </div>

        <div className="settings-preferences-group">
          <div className="settings-row settings-row-stack">
            <div className="settings-row-inline-head">
              <div className="settings-row-left">
                <div className="settings-row-title">Network allowlist</div>
                <div className="settings-row-desc">
                  Edit allowlist entries (one per line). Changes apply to future sandbox runs for this workspace.
                </div>
              </div>
              <div
                className={`settings-autosave-status settings-autosave-status-${workspaceAllowlistAutosaveState} ${allowlistAutosaveActive ? "" : "settings-autosave-status-placeholder"}`}
              >
                {workspaceAllowlistAutosaveState === "pending" || workspaceAllowlistAutosaveState === "saving" ? (
                  <Loader2 size={14} className="settings-autosave-spin" aria-hidden="true" />
                ) : null}
                {workspaceAllowlistAutosaveState === "saved" ? <Check size={14} aria-hidden="true" /> : null}
                <span>{allowlistAutosaveStatusLabel}</span>
              </div>
            </div>
          </div>
          <div className="settings-allowlist-block">
            <div
              className={`settings-monaco-wrap ${allowlistEditorDisabled ? "settings-monaco-wrap-disabled" : ""}`}
              aria-disabled={allowlistEditorDisabled}
            >
              <Editor
                width="100%"
                height="170px"
                defaultLanguage="plaintext"
                theme={themeVariant === "dark" ? "vs-dark" : "vs"}
                value={workspaceAllowlistText}
                onChange={(value: string | undefined) => {
                  if (allowlistEditorDisabled) return;
                  setWorkspaceAllowlistText(value ?? "");
                }}
                options={{
                  minimap: { enabled: false },
                  lineNumbers: "off",
                  wordWrap: "off",
                  scrollBeyondLastLine: false,
                  automaticLayout: true,
                  readOnly: allowlistEditorDisabled,
                  domReadOnly: allowlistEditorDisabled,
                  padding: { top: 10, bottom: 10 },
                }}
                loading="Loading editor..."
              />
            </div>
          </div>
        </div>
      </div>
      {sandboxRuntimeLoaded ? (
        sandboxRuntimeLoadError ? (
          <Card title="Local Sandbox Runtime">
            <div className="settings-empty settings-empty-error">{sandboxRuntimeLoadError}</div>
          </Card>
        ) : (
          <SandboxingSection
            loaded={sandboxRuntimeLoaded}
            resolvedMachineMemoryMb={sandboxRuntime.machineResolvedMemoryMb}
            idleShutdownSeconds={sandboxRuntime.machineIdleShutdownSeconds}
            onIdleShutdownSecondsChange={sandboxRuntime.setMachineIdleShutdownSeconds}
            hostPressureSwapThresholdMb={sandboxRuntime.machineHostPressureSwapThresholdMb}
            onHostPressureSwapThresholdMbChange={sandboxRuntime.setMachineHostPressureSwapThresholdMb}
            canSaveMachineSettings={sandboxRuntime.sandboxMachineCanSave}
          />
        )
      ) : (
        <Card title="Local Sandbox Runtime">
          <div className="settings-empty">Loading…</div>
        </Card>
      )}
      {workspaceExecutionError ? <div className="settings-banner settings-banner-error">{workspaceExecutionError}</div> : null}
    </GeneralSection>
  );
}
