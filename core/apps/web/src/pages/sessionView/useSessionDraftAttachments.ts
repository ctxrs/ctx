import { type SetStateAction, useCallback, useState } from "react";
import type { MessageAttachment } from "../../api/client";
import { useSessionImageDropScope } from "./useSessionImageDropScope";

type SessionDraftAttachmentState = {
  draft?: { attachments?: MessageAttachment[] } | null;
  onDraftAttachmentsChange?: ((attachments: MessageAttachment[]) => void) | null;
  onError?: ((message: string | null) => void) | null;
};

export function useSessionDraftAttachments({
  draft,
  onDraftAttachmentsChange,
  onError,
}: SessionDraftAttachmentState) {
  const [draftAttachmentsInternal, setDraftAttachmentsInternal] = useState<MessageAttachment[]>([]);
  const draftAttachments = draft?.attachments ?? draftAttachmentsInternal;

  const setDraftAttachments = useCallback(
    (next: SetStateAction<MessageAttachment[]>) => {
      if (draft) {
        const resolved = typeof next === "function" ? next(draft.attachments ?? []) : next;
        onDraftAttachmentsChange?.(resolved);
        return;
      }
      setDraftAttachmentsInternal(next);
    },
    [draft, onDraftAttachmentsChange],
  );

  const { dropScopeRef, dropActive } = useSessionImageDropScope({
    setDraftAttachments,
    onError: onError ?? undefined,
  });

  return {
    draftAttachmentsInternal,
    setDraftAttachmentsInternal,
    draftAttachments,
    setDraftAttachments,
    dropScopeRef,
    dropActive,
  };
}
