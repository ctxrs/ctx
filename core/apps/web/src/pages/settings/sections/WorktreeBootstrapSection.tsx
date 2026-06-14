import { Check, Info, Loader2, X } from "lucide-react";
import { promptAutosaveStatusLabel } from "../SettingsPage.utils";
import { TextInput } from "../../../components/ui/text-input";
import { Row, Toggle } from "../SettingsPage.components";
import { useWorktreeBootstrapController } from "../hooks/useWorktreeBootstrapController";
import { GeneralSection } from "./GeneralSection";

type WorktreeBootstrapSectionProps = {
  workspaceId: string | null;
  active: boolean;
};

export function WorktreeBootstrapSection({ workspaceId, active }: WorktreeBootstrapSectionProps) {
  const {
    worktreeBootstrapLoading,
    worktreeBootstrapSaving,
    worktreeBootstrapError,
    worktreeBootstrapAutosaveState,
    worktreeBootstrapForm,
    setWorktreeBootstrapForm,
    worktreeWaitInfoOpen,
    setWorktreeWaitInfoOpen,
  } = useWorktreeBootstrapController({
    workspaceId,
    enabled: active,
  });

  const bootstrapDisabled = !workspaceId || worktreeBootstrapLoading || worktreeBootstrapSaving;
  const hasSetupCommand = worktreeBootstrapForm.setup_command.trim().length > 0;
  const hasCleanupCommand = worktreeBootstrapForm.cleanup_command.trim().length > 0;
  const bootstrapDerivedControlsDisabled = bootstrapDisabled || !hasSetupCommand;
  const cleanupDerivedControlsDisabled = bootstrapDisabled || !hasCleanupCommand;
  const bootstrapAutosaveStatusLabel = promptAutosaveStatusLabel(worktreeBootstrapAutosaveState);
  const bootstrapAutosaveActive = worktreeBootstrapAutosaveState !== "idle";

  return (
    <GeneralSection>
      <div className="settings-preferences-flat">
        <div className="settings-preferences-group">
          <div className="settings-row settings-row-stack">
            <div className="settings-row-inline-head">
              <div className="settings-row-left">
                <div className="settings-row-title">Setup command</div>
                <div className="settings-row-desc">
                  Shell command to run when a new worktree starts (for example, install dependencies).
                </div>
              </div>
              <div
                className={`settings-autosave-status settings-autosave-status-${worktreeBootstrapAutosaveState} ${bootstrapAutosaveActive ? "" : "settings-autosave-status-placeholder"}`}
              >
                {worktreeBootstrapAutosaveState === "pending" || worktreeBootstrapAutosaveState === "saving" ? (
                  <Loader2 size={14} className="settings-autosave-spin" aria-hidden="true" />
                ) : null}
                {worktreeBootstrapAutosaveState === "saved" ? <Check size={14} aria-hidden="true" /> : null}
                <span>{bootstrapAutosaveStatusLabel}</span>
              </div>
            </div>
            <div className="settings-row-field">
              <TextInput
                className="settings-control settings-control-block wb-mono"
                value={worktreeBootstrapForm.setup_command}
                onChange={(e) =>
                  setWorktreeBootstrapForm((prev) => ({
                    ...prev,
                    setup_command: e.target.value,
                    timeout_sec: e.target.value.trim().length > 0 ? prev.timeout_sec : "",
                    wait_for_completion: e.target.value.trim().length > 0 ? prev.wait_for_completion : false,
                  }))}
                disabled={bootstrapDisabled}
                placeholder="./prepare-worktree.sh"
                aria-label="Worktree bootstrap setup command"
              />
            </div>
          </div>
          <Row
            title="Timeout (seconds)"
            description="Timeout for the worktree bootstrap command."
            control={
              <TextInput
                className="settings-control"
                type="number"
                min={1}
                step={1}
                value={worktreeBootstrapForm.timeout_sec}
                onChange={(e) =>
                  setWorktreeBootstrapForm((prev) => ({
                    ...prev,
                    timeout_sec: e.target.value,
                  }))}
                disabled={bootstrapDerivedControlsDisabled}
                placeholder="60"
                inputMode="numeric"
                aria-label="Worktree bootstrap timeout seconds"
              />
            }
          />
          <div className="settings-row">
            <div className="settings-row-left">
              <div className="settings-row-title settings-row-title-with-info">
                <span>Delay agent start until completion</span>
                <button
                  type="button"
                  className="settings-inline-info-btn"
                  onClick={() => setWorktreeWaitInfoOpen(true)}
                  aria-label="Learn about wait for completion"
                  title="Learn more"
                >
                  <Info size={14} aria-hidden="true" />
                </button>
              </div>
              <div className="settings-row-desc">
                If enabled, agents will wait for worktree to finish bootstrapping before starting.
              </div>
            </div>
            <div className="settings-row-right">
              <Toggle
                checked={worktreeBootstrapForm.wait_for_completion}
                onChange={(value) =>
                  setWorktreeBootstrapForm((prev) => ({
                    ...prev,
                    wait_for_completion: value,
                  }))}
                disabled={bootstrapDerivedControlsDisabled}
                ariaLabel="Wait for worktree bootstrap completion"
              />
            </div>
          </div>
          <div className="settings-row settings-row-stack">
            <div className="settings-row-inline-head">
              <div className="settings-row-left">
                <div className="settings-row-title">Cleanup command</div>
                <div className="settings-row-desc">
                  Shell command to run before an archived worktree is removed.
                </div>
              </div>
            </div>
            <div className="settings-row-field">
              <TextInput
                className="settings-control settings-control-block wb-mono"
                value={worktreeBootstrapForm.cleanup_command}
                onChange={(e) =>
                  setWorktreeBootstrapForm((prev) => ({
                    ...prev,
                    cleanup_command: e.target.value,
                    cleanup_timeout_sec: e.target.value.trim().length > 0 ? prev.cleanup_timeout_sec : "",
                  }))}
                disabled={bootstrapDisabled}
                placeholder="./cleanup-worktree.sh"
                aria-label="Worktree cleanup command"
              />
            </div>
          </div>
          <Row
            title="Cleanup timeout (seconds)"
            description="Timeout for the worktree cleanup command."
            control={
              <TextInput
                className="settings-control"
                type="number"
                min={1}
                step={1}
                value={worktreeBootstrapForm.cleanup_timeout_sec}
                onChange={(e) =>
                  setWorktreeBootstrapForm((prev) => ({
                    ...prev,
                    cleanup_timeout_sec: e.target.value,
                  }))}
                disabled={cleanupDerivedControlsDisabled}
                placeholder="60"
                inputMode="numeric"
                aria-label="Worktree cleanup timeout seconds"
              />
            }
          />
        </div>
      </div>
      {worktreeBootstrapError ? <div className="settings-banner settings-banner-error">{worktreeBootstrapError}</div> : null}

      {worktreeWaitInfoOpen ? (
        <div
          className="modal-overlay"
          role="dialog"
          aria-modal="true"
          aria-label="Wait for completion information"
          onClick={() => setWorktreeWaitInfoOpen(false)}
        >
          <div className="modal settings-info-modal" onClick={(e) => e.stopPropagation()}>
            <div className="settings-harness-modal-header">
              <div className="settings-main-title settings-info-modal-title">
                Should agents wait for worktree bootstrap completion?
              </div>
              <button
                type="button"
                className="settings-harness-modal-close"
                onClick={() => setWorktreeWaitInfoOpen(false)}
                aria-label="Close"
              >
                <X size={16} aria-hidden="true" />
              </button>
            </div>
            <div className="settings-info-modal-body">
              <p>
                Whether to wait depends on what your worktree bootstrap command does.
              </p>
              <p>
                If bootstrap prepares immediate agent inputs, such as generating AGENTS.md, enable this so agents
                start only after setup completes.
              </p>
              <p>
                If bootstrap does longer-running setup, such as installing dependencies, you can disable this and let
                agents start immediately. Agents usually begin with repository discovery and can often recover if
                dependencies are not ready yet.
              </p>
              <p>
                When disabled, bootstrap still runs in the background while agents start. You will still be notified
                if your setup script fails or times out.
              </p>
            </div>
          </div>
        </div>
      ) : null}
    </GeneralSection>
  );
}
