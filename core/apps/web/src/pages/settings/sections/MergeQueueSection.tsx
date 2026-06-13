import { Row, Toggle } from "../SettingsPage.components";
import { TextInput } from "../../../components/ui/text-input";
import { useMergeQueueController } from "../hooks/useMergeQueueController";
import { GeneralSection } from "./GeneralSection";

type MergeQueueSectionProps = {
  workspaceId: string | null;
  active: boolean;
};

export function MergeQueueSection({ workspaceId, active }: MergeQueueSectionProps) {
  const {
    mergeQueueConfigLoading,
    mergeQueueConfigSaving,
    mergeQueueConfigError,
    mergeQueueForm,
    setMergeQueueForm,
  } = useMergeQueueController({
    workspaceId,
    enabled: active,
  });

  const mergeQueueDisabled = !workspaceId || mergeQueueConfigLoading || mergeQueueConfigSaving;

  return (
    <GeneralSection>
      <div className="settings-preferences-flat">
        <div className="settings-preferences-group">
          <Row
            title="Target branch"
            control={
              <TextInput
                className="settings-control"
                value={mergeQueueForm.target_branch}
                onChange={(e) =>
                  setMergeQueueForm((prev) => ({
                    ...prev,
                    target_branch: e.target.value,
                  }))}
                disabled={mergeQueueDisabled}
                placeholder="main"
              />
            }
          />
          <div className="settings-row settings-row-stack">
            <div className="settings-row-left">
              <div className="settings-row-title">Verification command (optional)</div>
            </div>
            <div className="settings-row-field">
              <TextInput
                className="settings-control settings-control-block wb-mono"
                value={mergeQueueForm.verify_command}
                onChange={(e) =>
                  setMergeQueueForm((prev) => ({
                    ...prev,
                    verify_command: e.target.value,
                  }))}
                disabled={mergeQueueDisabled}
                placeholder="./verify.sh"
                aria-label="Merge queue verification command"
              />
            </div>
          </div>
        </div>

        <div className="settings-preferences-group">
          <Row
            title="Push to remote on success"
            control={
              <Toggle
                checked={mergeQueueForm.push_on_success}
                disabled={mergeQueueDisabled}
                onChange={(value) =>
                  setMergeQueueForm((prev) => ({
                    ...prev,
                    push_on_success: value,
                    push_remote: value ? (prev.push_remote.trim() || "origin") : prev.push_remote,
                    push_branch: value ? (prev.push_branch.trim() || prev.target_branch.trim() || "main") : prev.push_branch,
                  }))}
                ariaLabel="Push to remote on success"
              />
            }
          />
          {mergeQueueForm.push_on_success ? (
            <>
              <Row
                title="Push remote"
                control={
                  <TextInput
                    className="settings-control"
                    value={mergeQueueForm.push_remote}
                    onChange={(e) =>
                      setMergeQueueForm((prev) => ({
                        ...prev,
                        push_remote: e.target.value,
                      }))}
                    disabled={mergeQueueDisabled}
                    placeholder="origin"
                  />
                }
              />
              <Row
                title="Push branch"
                control={
                  <TextInput
                    className="settings-control"
                    value={mergeQueueForm.push_branch}
                    onChange={(e) =>
                      setMergeQueueForm((prev) => ({
                        ...prev,
                        push_branch: e.target.value,
                      }))}
                    disabled={mergeQueueDisabled}
                    placeholder={mergeQueueForm.target_branch.trim() || "main"}
                  />
                }
              />
            </>
          ) : null}
        </div>
      </div>
      {mergeQueueConfigError ? <div className="settings-banner settings-banner-error">{mergeQueueConfigError}</div> : null}
    </GeneralSection>
  );
}
