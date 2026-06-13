import { useCallback, useEffect, useRef, useState, type Dispatch, type SetStateAction } from "react";
import type { MessageAttachment } from "../../api/client";
import { registerDropScope } from "../../utils/dragDropScopes";
import { imageAttachmentsFromPaths, imageAttachmentsFromTransfer } from "../../utils/droppedImageAttachments";
import { errorMessage } from "../../utils/errorMessage";

type UseWorkbenchDragDropAttachmentsArgs = {
  scopeElement: HTMLElement | null;
  activeTaskId: string | null;
  setDraftAttachments: Dispatch<SetStateAction<MessageAttachment[]>>;
  onError?: (message: string | null) => void;
};

export function useWorkbenchDragDropAttachments({
  scopeElement,
  activeTaskId,
  setDraftAttachments,
  onError,
}: UseWorkbenchDragDropAttachmentsArgs) {
  const [dropActive, setDropActive] = useState(false);
  const dropHideTimerRef = useRef<number | null>(null);

  const appendAttachments = useCallback(
    (next: MessageAttachment[]) => {
      if (next.length === 0) return;
      setDraftAttachments((prev) => [...prev, ...next]);
    },
    [setDraftAttachments],
  );

  const showDropOverlay = useCallback(() => {
    setDropActive(true);
    if (dropHideTimerRef.current) window.clearTimeout(dropHideTimerRef.current);
    dropHideTimerRef.current = window.setTimeout(() => setDropActive(false), 140);
  }, []);

  const hideDropOverlay = useCallback(() => {
    if (dropHideTimerRef.current) window.clearTimeout(dropHideTimerRef.current);
    dropHideTimerRef.current = null;
    setDropActive(false);
  }, []);

  useEffect(() => {
    if (!scopeElement) return;

    return registerDropScope({
      element: scopeElement,
      onDragOver: () => showDropOverlay(),
      onDragLeave: () => hideDropOverlay(),
      onDrop: (transfer) => {
        hideDropOverlay();
        void (async () => {
          onError?.(null);
          try {
            appendAttachments(await imageAttachmentsFromTransfer(transfer));
          } catch (error: unknown) {
            onError?.(errorMessage(error));
          }
        })();
      },
      onDropPaths: (paths) => {
        hideDropOverlay();
        void (async () => {
          onError?.(null);
          try {
            appendAttachments(await imageAttachmentsFromPaths(paths));
          } catch (error: unknown) {
            onError?.(errorMessage(error));
          }
        })();
      },
    });
  }, [activeTaskId, appendAttachments, hideDropOverlay, onError, scopeElement, showDropOverlay]);

  useEffect(
    () => () => {
      if (dropHideTimerRef.current) {
        window.clearTimeout(dropHideTimerRef.current);
        dropHideTimerRef.current = null;
      }
    },
    [],
  );

  return {
    dropActive,
  };
}
