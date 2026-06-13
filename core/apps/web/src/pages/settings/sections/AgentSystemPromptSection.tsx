import Editor from "@monaco-editor/react";
import { Check, Loader2 } from "lucide-react";
import { promptAutosaveStatusLabel } from "../SettingsPage.utils";
import { useAgentSystemPromptController } from "../hooks/useAgentSystemPromptController";
import { GeneralSection } from "./GeneralSection";

type AgentSystemPromptSectionProps = {
  workspaceId: string | null;
  active: boolean;
  themeVariant: "dark" | "light";
};

export function AgentSystemPromptSection({
  workspaceId,
  active,
  themeVariant,
}: AgentSystemPromptSectionProps) {
  const {
    agentPromptLoading,
    agentPromptSaving,
    agentPromptError,
    agentPromptText,
    setAgentPromptText,
    agentPromptAutosaveState,
    subagentPromptLoading,
    subagentPromptSaving,
    subagentPromptError,
    subagentPromptText,
    setSubagentPromptText,
    subagentPromptAutosaveState,
  } = useAgentSystemPromptController({
    workspaceId,
    enabled: active,
  });

  const autosaveStatusLabel = promptAutosaveStatusLabel(agentPromptAutosaveState);
  const autosaveActive = agentPromptAutosaveState !== "idle";
  const subagentAutosaveStatusLabel = promptAutosaveStatusLabel(subagentPromptAutosaveState);
  const subagentAutosaveActive = subagentPromptAutosaveState !== "idle";

  return (
    <GeneralSection>
      <div className="settings-preferences-flat">
        <div className="settings-preferences-group">
          <div className="settings-row settings-row-stack">
            <div className="settings-row-inline-head">
              <div className="settings-row-left">
                <div className="settings-row-title">Prompt Append</div>
                <div className="settings-row-desc">
                  In addition to the default prompt managed by your agent harness and any other prompts you provide directly or in AGENTS.md, this is an additional prompt that relates specifically to informing agents about their environment - specifically being run inside ctx, the sandbox and network policies, and the tools available. We recommend keeping this as the default value as we have optimized it accordingly.
                </div>
              </div>
              <div
                className={`settings-autosave-status settings-autosave-status-${agentPromptAutosaveState} ${autosaveActive ? "" : "settings-autosave-status-placeholder"}`}
              >
                {agentPromptAutosaveState === "pending" || agentPromptAutosaveState === "saving" ? (
                  <Loader2 size={14} className="settings-autosave-spin" aria-hidden="true" />
                ) : null}
                {agentPromptAutosaveState === "saved" ? <Check size={14} aria-hidden="true" /> : null}
                <span>{autosaveStatusLabel}</span>
              </div>
            </div>
            <div className="settings-row-field">
              <div className="settings-monaco-wrap">
                <Editor
                  width="100%"
                  height="220px"
                  defaultLanguage="markdown"
                  theme={themeVariant === "dark" ? "vs-dark" : "vs"}
                  value={agentPromptText}
                  onChange={(value: string | undefined) => setAgentPromptText(value ?? "")}
                  options={{
                    minimap: { enabled: false },
                    lineNumbers: "off",
                    wordWrap: "on",
                    scrollBeyondLastLine: false,
                    automaticLayout: true,
                    readOnly: !workspaceId || agentPromptLoading || agentPromptSaving,
                    padding: { top: 10, bottom: 10 },
                  }}
                  loading="Loading editor..."
                />
              </div>
            </div>
          </div>
          <div className="settings-row settings-row-stack">
            <div className="settings-row-inline-head">
              <div className="settings-row-left">
                <div className="settings-row-title">Subagent prompt append</div>
                <div className="settings-row-desc">
                  Saved in local per-workspace settings. Pre-filled with the default; edit to override.
                </div>
              </div>
              <div
                className={`settings-autosave-status settings-autosave-status-${subagentPromptAutosaveState} ${subagentAutosaveActive ? "" : "settings-autosave-status-placeholder"}`}
              >
                {subagentPromptAutosaveState === "pending" || subagentPromptAutosaveState === "saving" ? (
                  <Loader2 size={14} className="settings-autosave-spin" aria-hidden="true" />
                ) : null}
                {subagentPromptAutosaveState === "saved" ? <Check size={14} aria-hidden="true" /> : null}
                <span>{subagentAutosaveStatusLabel}</span>
              </div>
            </div>
            <div className="settings-row-field">
              <div className="settings-monaco-wrap">
                <Editor
                  width="100%"
                  height="170px"
                  defaultLanguage="markdown"
                  theme={themeVariant === "dark" ? "vs-dark" : "vs"}
                  value={subagentPromptText}
                  onChange={(value: string | undefined) => setSubagentPromptText(value ?? "")}
                  options={{
                    minimap: { enabled: false },
                    lineNumbers: "off",
                    wordWrap: "on",
                    scrollBeyondLastLine: false,
                    automaticLayout: true,
                    readOnly: !workspaceId || subagentPromptLoading || subagentPromptSaving,
                    padding: { top: 10, bottom: 10 },
                  }}
                  loading="Loading editor..."
                />
              </div>
            </div>
          </div>
        </div>
      </div>
      {agentPromptLoading ? <div className="settings-banner">Loading agent prompt…</div> : null}
      {agentPromptError ? <div className="settings-banner settings-banner-error">{agentPromptError}</div> : null}
      {subagentPromptLoading ? <div className="settings-banner">Loading subagent prompt…</div> : null}
      {subagentPromptError ? <div className="settings-banner settings-banner-error">{subagentPromptError}</div> : null}
    </GeneralSection>
  );
}
