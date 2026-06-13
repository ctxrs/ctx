import type { WorkspaceSetupPageViewProps } from "./WorkspaceSetupPageView.types";

export function WorkspaceSetupConfirmStep({
  selections,
  remoteHostInput,
  repoBranch,
  repoUrl,
  sourcePath,
  useSandboxStaging,
  workspaceName,
  setupHook,
  mergeQueueSkipped,
  targetBranch,
  verifyCommand,
  pushOnSuccess,
  pushRemote,
  pushBranch,
  harnessSummaryValue,
  titlingSummaryValue,
}: WorkspaceSetupPageViewProps) {
  return (
    <div className="wizard-step-summary">
      <div className="wizard-summary">
        <div className="wizard-summary-row">
          <div className="wizard-summary-k">Location</div>
          <div className="wizard-summary-v">
            {selections.location === "remote" ? "Remote" : "Local"}
            {selections.location === "remote" && remoteHostInput.trim()
              ? ` (${remoteHostInput.trim()})`
              : ""}
          </div>
        </div>
        <div className="wizard-summary-row">
          <div className="wizard-summary-k">Source</div>
          <div className="wizard-summary-v">
            {selections.source === "clone"
              ? `Clone repo${repoBranch.trim() ? ` (${repoBranch.trim()})` : ""}`
              : selections.source === "import"
                ? "Import folder"
                : "New empty"}
          </div>
        </div>
        {selections.source === "clone" && repoUrl.trim() && (
          <div className="wizard-summary-row">
            <div className="wizard-summary-k">Repo</div>
            <div className="wizard-summary-v">{repoUrl.trim()}</div>
          </div>
        )}
        {selections.source === "clone" && (sourcePath.trim() || useSandboxStaging) && (
          <div className="wizard-summary-row">
            <div className="wizard-summary-k">Destination</div>
            <div className="wizard-summary-v">
              {useSandboxStaging ? "Managed staging (sandbox)" : sourcePath.trim()}
            </div>
          </div>
        )}
        {selections.source === "import" && sourcePath.trim() && (
          <div className="wizard-summary-row">
            <div className="wizard-summary-k">Folder</div>
            <div className="wizard-summary-v">{sourcePath.trim()}</div>
          </div>
        )}
        {selections.source === "new" && (sourcePath.trim() || useSandboxStaging) && (
          <div className="wizard-summary-row">
            <div className="wizard-summary-k">Destination</div>
            <div className="wizard-summary-v">
              {useSandboxStaging ? "Managed staging (sandbox)" : sourcePath.trim()}
            </div>
          </div>
        )}
        {(selections.source === "new" || selections.source === "import") && workspaceName.trim() && (
          <div className="wizard-summary-row">
            <div className="wizard-summary-k">Name</div>
            <div className="wizard-summary-v">{workspaceName.trim()}</div>
          </div>
        )}
        <div className="wizard-summary-row">
          <div className="wizard-summary-k">Sandbox</div>
          <div className="wizard-summary-v">
            {selections.container === "host"
              ? "Host"
              : "Sandbox"}
          </div>
        </div>
        {selections.container !== "host" && (
          <div className="wizard-summary-row">
            <div className="wizard-summary-k">Network</div>
            <div className="wizard-summary-v">
              {selections.network === "allowlist"
                ? "Allowlist"
                : selections.network === "full"
                  ? "Full access"
                  : "LLM providers only"}
            </div>
          </div>
        )}
        <div className="wizard-summary-row">
          <div className="wizard-summary-k">Harness downloads</div>
          <div className="wizard-summary-v">{harnessSummaryValue}</div>
        </div>
        <div className="wizard-summary-row">
          <div className="wizard-summary-k">Task titling</div>
          <div className="wizard-summary-v">{titlingSummaryValue}</div>
        </div>
        <div className="wizard-summary-row">
          <div className="wizard-summary-k">Worktree hook</div>
          <div className="wizard-summary-v">{setupHook.trim() || "(none)"}</div>
        </div>
        <div className="wizard-summary-row">
          <div className="wizard-summary-k">Merge queue</div>
          <div className="wizard-summary-v">
            {mergeQueueSkipped
              ? "Disabled"
              : `Target ${targetBranch.trim() || "main"}${verifyCommand.trim() ? `, verify: ${verifyCommand.trim()}` : ""}`}
          </div>
        </div>
        {!mergeQueueSkipped && pushOnSuccess && (
          <div className="wizard-summary-row">
            <div className="wizard-summary-k">Merge push</div>
            <div className="wizard-summary-v">
              {`${(pushRemote.trim() || "origin")}:${pushBranch.trim() || targetBranch.trim() || "main"}`}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
