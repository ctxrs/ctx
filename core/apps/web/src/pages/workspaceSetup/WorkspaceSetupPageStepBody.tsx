import { ChevronRight } from "lucide-react";
import { TextInput, Textarea } from "../../components/ui/text-input";
import {
  AuthImportStepPanel,
  HarnessDownloadsStepPanel,
} from "./WorkspaceSetupPanels";
import {
  WorkspaceLaunchLogPanel,
  WorkspaceSetupStepOptions,
} from "./WorkspaceSetupChrome";
import type { WorkspaceSetupPageViewProps } from "./WorkspaceSetupPageView.types";
import { WorkspaceSetupTitlingStep } from "./WorkspaceSetupTitlingStep";
import { WorkspaceSetupConfirmStep } from "./WorkspaceSetupConfirmStep";

export function WorkspaceSetupPageStepBody(props: WorkspaceSetupPageViewProps) {
  const {
    step,
    selections,
    createError,
    setCreateError,
    showLaunchPanel,
    launchSnapshot,
    currentLaunchStepLabel,
    currentLaunchElapsed,
    currentLaunchEtaLabel,
    launchCopyLabel,
    onCopyLaunchDiagnostics,
    launchLogs,
    onSelectOption,
    networkAllowlist,
    setNetworkAllowlist,
    remoteHostInput,
    onRemoteInputChange,
    remotePortInput,
    onRemotePortInputChange,
    remoteDataDirInput,
    onRemoteDataDirInputChange,
    localAdminPasswordPromptVisible,
    localAdminPasswordInput,
    setLocalAdminPasswordInput,
    remotePasswordPromptVisible,
    remotePasswordPromptMode,
    remotePasswordInput,
    onRemotePasswordInputChange,
    remoteStatus,
    setRemoteStatus,
    remoteError,
    setRemoteError,
    sshSuggestions,
    authImportBusy,
    authImportError,
    authImportCandidates,
    authImportSelected,
    setAuthImportSelected,
    harnessByProviderId,
    onSkipAuthImport,
    harnessInstallBusy,
    harnessInstallError,
    selectedHarnessRunningCount,
    selectedHarnessBlockedCount,
    harnessInstallCandidates,
    harnessDownloadsCanScroll,
    harnessDownloadsAtBottom,
    harnessDownloadsScrollRef,
    updateHarnessDownloadsScrollState,
    harnessInstallSelected,
    setHarnessInstallSelected,
    harnessInstallRows,
    selectedHarnessInstallTarget,
    cancelHarnessInstall,
    onSkipHarnessDownloads,
    needsSourcePath,
    sourcePath,
    setSourcePath,
    onPickLocalFolder,
    importRepoStatus,
    importRepoNote,
    remotePathSuggestions,
    remotePathStatus,
    remotePathError,
    repoUrl,
    setRepoUrl,
    repoBranch,
    setRepoBranch,
    useSandboxStaging,
    setupHook,
    setSetupHook,
    workspaceName,
    setWorkspaceName,
    mergeQueueSkipped,
    targetBranch,
    setTargetBranch,
    setTargetBranchTouched,
    verifyCommand,
    setVerifyCommand,
    mergeAdvancedOpen,
    setMergeAdvancedOpen,
    pushOnSuccess,
    setPushOnSuccess,
    pushRemote,
    setPushRemote,
    pushBranch,
    setPushBranch,
    setPushBranchTouched,
    enableMergeQueueIfSkipped,
    onMergeSkip,
    onNext,
  } = props;

  return (
    <div className="wizard-step-body">
      {createError && (
        <div className="wizard-error">{createError}</div>
      )}
      <WorkspaceLaunchLogPanel
        showLaunchPanel={showLaunchPanel && step.key === "confirm"}
        launchSnapshot={launchSnapshot}
        currentLaunchStepLabel={currentLaunchStepLabel}
        currentLaunchElapsed={currentLaunchElapsed}
        currentLaunchEtaLabel={currentLaunchEtaLabel}
        launchCopyLabel={launchCopyLabel}
        onCopyLaunchDiagnostics={onCopyLaunchDiagnostics}
        launchLogs={launchLogs}
      />
      <WorkspaceSetupStepOptions
        step={step}
        selections={selections}
        onSelectOption={onSelectOption}
      />
      {step.key === "network" && selections.network === "allowlist" && (
        <div className="wizard-input">
          <label>
            Allowed hosts (one per line)
            <Textarea
              data-testid="wizard-network-allowlist"
              placeholder={"github.com\nregistry.npmjs.org\npypi.org"}
              value={networkAllowlist}
              onChange={(event) => {
                setCreateError(null);
                setNetworkAllowlist(event.target.value);
              }}
              rows={6}
            />
          </label>
        </div>
      )}
      {step.key === "location" && selections.location === "remote" && (
        <div className="wizard-remote">
          <div className="wizard-input">
            <label>
              Remote host
              <TextInput
                data-testid="wizard-remote-host"
                placeholder="user@host"
                value={remoteHostInput}
                onChange={(event) => onRemoteInputChange(event.target.value)}
              />
            </label>
          </div>
          <div className="wizard-input">
            <label>
              Remote daemon port
              <TextInput
                data-testid="wizard-remote-port"
                inputMode="numeric"
                placeholder="4399"
                value={remotePortInput}
                onChange={(event) => onRemotePortInputChange(event.target.value)}
              />
            </label>
          </div>
          <div className="wizard-input">
            <label>
              Remote data directory
              <TextInput
                data-testid="wizard-remote-data-dir"
                placeholder="Optional; defaults to ~/.ctx"
                value={remoteDataDirInput}
                onChange={(event) => onRemoteDataDirInputChange(event.target.value)}
              />
            </label>
          </div>
          {remotePasswordPromptVisible ? (
            <div className="wizard-input">
              <label>
                {remotePasswordPromptMode === "admin" ? "Remote Admin Password" : "SSH Password"}
                <TextInput
                  data-testid="wizard-remote-password-once"
                  type="password"
                  value={remotePasswordInput}
                  onChange={(event) => {
                    setCreateError(null);
                    onRemotePasswordInputChange(event.target.value);
                    if (remoteStatus !== "idle") {
                      setRemoteStatus("idle");
                      setRemoteError(null);
                    }
                  }}
                />
              </label>
              <div className="wizard-note">
                {remotePasswordPromptMode === "admin"
                  ? "Used once to finish sandbox setup on this host; never stored"
                  : "Used to install key-based SSH auth; never stored"}
              </div>
            </div>
          ) : null}
          {sshSuggestions.length > 0 && (
            <div className="wizard-remote-list">
              {sshSuggestions.map((entry) => {
                const label = entry.user ? `${entry.user}@${entry.host}` : entry.host;
                return (
                  <button
                    key={label}
                    type="button"
                    className="wizard-remote-suggestion"
                    onClick={() => onRemoteInputChange(label)}
                  >
                    {label}
                  </button>
                );
              })}
            </div>
          )}
          {remoteStatus === "connecting" && (
            <div className="wizard-note">Connecting…</div>
          )}
          {remoteStatus === "connected" && (
            <div className="wizard-note">Connection verified.</div>
          )}
          {remoteStatus === "error" && remoteError && (
            <div className="wizard-error">{remoteError}</div>
          )}
        </div>
      )}
      {(step.key === "location" || step.key === "harness-downloads")
        && selections.location === "local"
        && localAdminPasswordPromptVisible && (
        <div className="wizard-remote">
          <div className="wizard-input">
            <label>
              Linux Admin Password
              <TextInput
                data-testid="wizard-local-admin-password-once"
                type="password"
                value={localAdminPasswordInput}
                onChange={(event) => {
                  setCreateError(null);
                  setLocalAdminPasswordInput(event.target.value);
                }}
              />
            </label>
            <div className="wizard-note">
              Used once to finish sandbox setup on this machine; never stored
            </div>
          </div>
        </div>
      )}
      {step.key === "auth-import" && (
        <AuthImportStepPanel
          busy={authImportBusy}
          error={authImportError}
          candidates={authImportCandidates}
          selected={authImportSelected}
          setSelected={setAuthImportSelected}
          harnessByProviderId={harnessByProviderId}
          onSkip={onSkipAuthImport}
        />
      )}
      {step.key === "harness-downloads" && (
        <HarnessDownloadsStepPanel
          busy={harnessInstallBusy}
          error={harnessInstallError}
          selectedRunningCount={selectedHarnessRunningCount}
          selectedBlockedCount={selectedHarnessBlockedCount}
          candidates={harnessInstallCandidates}
          canScroll={harnessDownloadsCanScroll}
          atBottom={harnessDownloadsAtBottom}
          scrollRef={harnessDownloadsScrollRef}
          onScroll={updateHarnessDownloadsScrollState}
          selected={harnessInstallSelected}
          setSelected={setHarnessInstallSelected}
          rows={harnessInstallRows}
          selectedInstallTarget={selectedHarnessInstallTarget}
          harnessByProviderId={harnessByProviderId}
          onCancelInstall={cancelHarnessInstall}
          onSkip={onSkipHarnessDownloads}
        />
      )}
      {step.key === "session-titling" && (
        <WorkspaceSetupTitlingStep {...props} />
      )}
      {step.key === "source" && needsSourcePath && (
        <div className="wizard-input">
          <label>
            {selections.source === "import"
              ? "Existing folder"
              : "Destination folder (host)"}
            <div className="wizard-input-row">
              <TextInput
                data-testid="wizard-source-path"
                placeholder={selections.source === "import" ? "/home/you/project" : "/home/you/projects/"}
                value={sourcePath}
                onChange={(event) => {
                  setCreateError(null);
                  setSourcePath(event.target.value);
                }}
              />
              {selections.location === "local" && (
                <button
                  type="button"
                  className="wizard-input-button"
                  onClick={onPickLocalFolder}
                >
                  Browse
                </button>
              )}
            </div>
          </label>
          {selections.container !== "host" && (
            <div className="wizard-note">
              This is the project folder on the host. In sandbox mode, ctx will copy the workspace into an isolated managed filesystem for execution.
            </div>
          )}
          {selections.source === "import" && importRepoStatus !== "idle" && importRepoNote && (
            <div className={importRepoStatus === "error" ? "wizard-error" : "wizard-note"}>
              {importRepoNote}
            </div>
          )}
          {selections.location === "remote" && remotePathSuggestions.length > 0 && (
            <div className="wizard-path-list">
              {remotePathSuggestions.map((entry) => (
                <button
                  key={entry.path}
                  type="button"
                  className="wizard-path-suggestion"
                  onClick={() => setSourcePath(`${entry.path}/`)}
                >
                  {entry.name}
                </button>
              ))}
            </div>
          )}
          {selections.location === "remote" && remotePathStatus === "loading" && (
            <div className="wizard-note">Loading folders…</div>
          )}
          {selections.location === "remote" && remotePathStatus === "error" && remotePathError && (
            <div className="wizard-error">{remotePathError}</div>
          )}
        </div>
      )}
      {step.key === "source" && selections.source === "clone" && (
        <div className="wizard-input">
          <label>
            Repo URL
            <TextInput
              data-testid="wizard-repo-url"
              placeholder="https://github.com/org/repo.git"
              value={repoUrl}
              onChange={(event) => {
                setCreateError(null);
                setRepoUrl(event.target.value);
              }}
            />
          </label>
          <label>
            Branch (optional)
            <TextInput
              data-testid="wizard-repo-branch"
              placeholder="main"
              value={repoBranch}
              onChange={(event) => {
                setCreateError(null);
                setRepoBranch(event.target.value);
              }}
            />
          </label>
          {useSandboxStaging && (
            <div className="wizard-note">
              Ctx will clone into a managed staging path under <code>~/.ctx/workspaces/staging/</code> by default. Your workspace will live in the sandbox.
            </div>
          )}
          {!useSandboxStaging && (
            <div className="wizard-note">
              Tip: If you enter a folder ending in <code>/</code>, ctx will derive the repo name from the URL.
            </div>
          )}
        </div>
      )}
      {step.key === "setup" && (
        <div className="wizard-input">
          <TextInput
            data-testid="wizard-setup-hook"
            placeholder="./prepare-worktree.sh"
            value={setupHook}
            onChange={(event) => {
              setCreateError(null);
              setSetupHook(event.target.value);
            }}
          />
        </div>
      )}
      {step.key === "source" && (selections.source === "new" || selections.source === "import") && (
        <div className="wizard-input">
          <label>
            Workspace name (optional)
            <TextInput
              data-testid="wizard-workspace-name"
              placeholder="workspace"
              value={workspaceName}
              onChange={(event) => {
                setCreateError(null);
                setWorkspaceName(event.target.value);
              }}
            />
          </label>
          {selections.source === "new" && useSandboxStaging && (
            <div className="wizard-note">
              Ctx will create the repo in a managed staging path. Your workspace will live in the sandbox.
            </div>
          )}
        </div>
      )}
      {step.key === "merge-queue" && (
        <div className="wizard-input">
          <label>
            Target branch
            <TextInput
              data-testid="wizard-merge-target-branch"
              placeholder="main"
              value={targetBranch}
              onChange={(event) => {
                setCreateError(null);
                enableMergeQueueIfSkipped();
                setTargetBranch(event.target.value);
                setTargetBranchTouched(true);
              }}
              disabled={mergeQueueSkipped}
            />
          </label>
          <label>
            Verification command (optional)
            <TextInput
              data-testid="wizard-merge-verify-command"
              placeholder="./verify.sh"
              value={verifyCommand}
              onChange={(event) => {
                setCreateError(null);
                enableMergeQueueIfSkipped();
                setVerifyCommand(event.target.value);
              }}
              disabled={mergeQueueSkipped}
            />
          </label>
          <button
            type="button"
            className="wizard-advanced-link"
            data-testid="wizard-merge-advanced-toggle"
            onClick={() => setMergeAdvancedOpen((open) => !open)}
            aria-expanded={mergeAdvancedOpen}
            disabled={mergeQueueSkipped}
          >
            <ChevronRight
              size={14}
              className={mergeAdvancedOpen ? "is-open" : undefined}
              aria-hidden="true"
            />
            Advanced
          </button>
          {mergeAdvancedOpen && (
            <div className="wizard-advanced-panel">
              <label className="wizard-checkbox">
                <input
                  data-testid="wizard-merge-push-on-success"
                  type="checkbox"
                  checked={pushOnSuccess}
                  onChange={(event) => {
                    setCreateError(null);
                    setPushOnSuccess(event.target.checked);
                  }}
                  disabled={mergeQueueSkipped}
                />
                Push to remote on success
              </label>
              {pushOnSuccess && (
                <div className="wizard-input">
                  <label>
                    Push remote
                    <TextInput
                      data-testid="wizard-merge-push-remote"
                      placeholder="origin"
                      value={pushRemote}
                      onChange={(event) => {
                        setCreateError(null);
                        setPushRemote(event.target.value);
                      }}
                      disabled={mergeQueueSkipped}
                    />
                  </label>
                  <label>
                    Push branch
                    <TextInput
                      data-testid="wizard-merge-push-branch"
                      placeholder={targetBranch || "main"}
                      value={pushBranch}
                      onChange={(event) => {
                        setCreateError(null);
                        setPushBranch(event.target.value);
                        setPushBranchTouched(true);
                      }}
                      disabled={mergeQueueSkipped}
                    />
                  </label>
                </div>
              )}
            </div>
          )}
          <button
            type="button"
            className="wizard-skip wizard-skip--left wizard-skip--below"
            data-testid="wizard-merge-skip"
            onClick={onMergeSkip}
          >
            Skip for now
          </button>
        </div>
      )}
      {step.key === "confirm" && (
        <WorkspaceSetupConfirmStep {...props} />
      )}
      {step.key === "setup" && (
        <button
          type="button"
          className="wizard-skip wizard-skip--left wizard-skip--below"
          onClick={onNext}
        >
          Skip for now
        </button>
      )}
    </div>
  );
}
