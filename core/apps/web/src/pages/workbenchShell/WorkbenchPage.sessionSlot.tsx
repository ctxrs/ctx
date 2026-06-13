import { copyTextToClipboard } from "../../utils/clipboard";
import { SessionView } from "../sessionView";
import { sessionDraftKey, useWorkbenchDraft, useWorkbenchStore } from "../../workbench/store";

export type WorkbenchSessionSlotProps = {
  sessionId: string;
  optimisticFailure?: { prompt: string; error: string | null } | null;
};

export function WorkbenchSessionSlot({
  sessionId,
  optimisticFailure,
}: WorkbenchSessionSlotProps) {
  const workbenchStore = useWorkbenchStore();
  const draft = useWorkbenchDraft(sessionDraftKey(sessionId), { text: "", modeId: "default", attachments: [] });

  if (optimisticFailure) {
    return (
      <div className="wb-session-slot" aria-hidden="false">
        <div className="wb-session-start-failure">
          <div className="wb-banner" role="alert">
            <div className="wb-session-start-failure-header">
              <strong>Failed to start</strong>
              <button
                type="button"
                className="wb-link"
                onClick={() => void copyTextToClipboard(optimisticFailure.prompt)}
              >
                Copy prompt
              </button>
            </div>
            {optimisticFailure.error ? (
              <div className="error">{optimisticFailure.error}</div>
            ) : null}
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="wb-session-slot" aria-hidden="false">
      <div className="wb-session-slot-body">
        <SessionView
          sessionId={sessionId}
          autoOpenSession={false}
          hideSessionLoadIssuesBanner
          draft={draft.value}
          onDraftChange={(text) => draft.setValue((prev) => ({ ...prev, text }))}
          onDraftAttachmentsChange={(attachments) =>
            draft.setValue((prev) => ({ ...prev, attachments }))
          }
          onDraftPersistNow={() => workbenchStore.flushDraft(sessionDraftKey(sessionId))}
          onModeChange={(modeId) => draft.setValue((prev) => ({ ...prev, modeId }))}
        />
      </div>
    </div>
  );
}
