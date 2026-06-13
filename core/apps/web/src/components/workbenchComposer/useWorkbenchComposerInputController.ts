import { flushSync } from "react-dom";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
  type WheelEvent as ReactWheelEvent,
} from "react";
import { errorMessage } from "../../utils/errorMessage";
import {
  extractImageFilesFromClipboardTransfer,
  clipboardHasImagePayload,
  imageAttachmentsFromClipboardTransfer,
} from "../../utils/pastedImageAttachments";
import { clamp } from "./WorkbenchComposer.utils";
import {
  findComposerTranscriptScroller,
  normalizeComposerWheelDeltaY,
  resolveComposerWheelTarget,
} from "./workbenchComposerScrollOwnership";
import type { WorkbenchComposerProps } from "./WorkbenchComposer.types";
import type { WorkbenchComposerOpenMenuId } from "./useWorkbenchComposerFloatingMenu";

type WorkbenchComposerPasteDebug = {
  attachedCount: number;
  hitCount: number;
  lastAttachedClassName: string | null;
  lastHit?: {
    fileCount: number;
    itemCount: number;
    extractedFileCount: number;
    hasImagePayload: boolean;
    types: string[];
  };
  lastQueuedAttachmentCount?: number;
  lastRenderedAttachmentCount?: number;
  lastResolvedAttachmentCount?: number;
  lastError?: string | null;
};

export function useWorkbenchComposerInputController({
  attachments,
  onAttachmentError,
  recording,
  setAttachments,
  setOpenMenu,
  setValue,
  value,
  variant,
}: {
  attachments: WorkbenchComposerProps["attachments"];
  onAttachmentError: WorkbenchComposerProps["onAttachmentError"];
  recording: WorkbenchComposerProps["recording"];
  setAttachments: WorkbenchComposerProps["setAttachments"];
  setOpenMenu: Dispatch<SetStateAction<WorkbenchComposerOpenMenuId | null>>;
  setValue: WorkbenchComposerProps["setValue"];
  value: string;
  variant: WorkbenchComposerProps["variant"];
}) {
  const [textareaNode, setTextareaNode] = useState<HTMLTextAreaElement | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const mirrorRef = useRef<HTMLDivElement | null>(null);
  const lastHeightRef = useRef<number>(0);
  const restoreDraftTailRef = useRef(true);
  const pendingSelectionRef = useRef<number | null>(null);

  const setTextareaElement = useCallback((node: HTMLTextAreaElement | null) => {
    textareaRef.current = node;
    setTextareaNode(node);
  }, []);

  const resizeTextarea = useCallback(() => {
    const el = textareaRef.current;
    const mirror = mirrorRef.current;
    if (!el || !mirror) return;

    const minHeightPx = variant === "newSession" ? 88 : 28;
    const maxHeightPx = variant === "newSession" ? 380 : 220;

    mirror.style.width = `${el.clientWidth}px`;
    mirror.textContent = value.length > 0 ? `${value}\n` : "\n";
    let measured = mirror.scrollHeight;
    if (!measured) {
      const styles = window.getComputedStyle(el);
      const lineHeight = Number.parseFloat(styles.lineHeight || "") || 20;
      const paddingTop = Number.parseFloat(styles.paddingTop || "") || 0;
      const paddingBottom = Number.parseFloat(styles.paddingBottom || "") || 0;
      const lines = Math.max(1, value.split("\n").length);
      measured = Math.ceil(lines * lineHeight + paddingTop + paddingBottom);
    }
    if (!measured) {
      measured = el.scrollHeight;
    }
    const next = Math.min(maxHeightPx, Math.max(minHeightPx, measured));
    const currentHeight =
      lastHeightRef.current ||
      Number.parseFloat(el.style.height || "0") ||
      el.getBoundingClientRect().height ||
      minHeightPx;
    const hasInlineHeight = el.style.height !== "";
    if (!hasInlineHeight || !Number.isFinite(currentHeight) || Math.abs(next - currentHeight) > 0.5) {
      el.style.height = `${next}px`;
      lastHeightRef.current = next;
    } else {
      lastHeightRef.current = currentHeight;
    }

    if (recording) el.scrollTop = el.scrollHeight;
  }, [recording, value, variant]);

  const clearSelectionBeforeSubmit = useCallback(() => {
    if (variant !== "newSession") return;
    setOpenMenu(null);

    const textarea = textareaRef.current;
    if (textarea) {
      const cursor =
        textarea.selectionEnd
        ?? textarea.selectionStart
        ?? textarea.value.length;
      textarea.setSelectionRange(cursor, cursor);
      if (document.activeElement === textarea) {
        textarea.blur();
      }
    }

    window.getSelection()?.removeAllRanges();
  }, [setOpenMenu, variant]);

  const insertPastedText = useCallback((textarea: HTMLTextAreaElement, text: string) => {
    if (!text) return;
    const start = textarea.selectionStart ?? textarea.value.length;
    const end = textarea.selectionEnd ?? start;
    pendingSelectionRef.current = start + text.length;
    restoreDraftTailRef.current = false;
    setValue(`${textarea.value.slice(0, start)}${text}${textarea.value.slice(end)}`);
  }, [setValue]);

  const handleClipboardPaste = useCallback((
    textarea: HTMLTextAreaElement,
    transfer: DataTransfer | null,
    preventDefault: () => void,
  ) => {
    const debugWindow = window as Window & { __ctxComposerPasteDebug?: WorkbenchComposerPasteDebug };
    const debug = debugWindow.__ctxComposerPasteDebug;
    const extractedFiles = extractImageFilesFromClipboardTransfer(transfer);
    const hasImagePayload = extractedFiles.length > 0 || clipboardHasImagePayload(transfer);
    if (debug) {
      debug.hitCount += 1;
      debug.lastHit = {
        fileCount: Array.from(transfer?.files ?? []).length,
        itemCount: Array.from(transfer?.items ?? []).length,
        extractedFileCount: extractedFiles.length,
        hasImagePayload,
        types: Array.from(transfer?.types ?? []),
      };
      debug.lastResolvedAttachmentCount = undefined;
      debug.lastError = null;
    }
    if (!hasImagePayload) return;
    preventDefault();
    const pastedText = transfer?.getData?.("text/plain") ?? "";
    if (pastedText.length > 0) {
      flushSync(() => {
        insertPastedText(textarea, pastedText);
      });
    }
    onAttachmentError?.(null);
    void imageAttachmentsFromClipboardTransfer(transfer)
      .then((next) => {
        if (debug) {
          debug.lastResolvedAttachmentCount = next.length;
        }
        if (next.length === 0) return;
        flushSync(() => {
          setAttachments((prev) => {
            const merged = [...prev, ...next];
            if (debug) {
              debug.lastQueuedAttachmentCount = merged.length;
            }
            return merged;
          });
        });
      })
      .catch((error: unknown) => {
        if (debug) {
          debug.lastError = errorMessage(error);
        }
        onAttachmentError?.(errorMessage(error));
      });
  }, [insertPastedText, onAttachmentError, setAttachments]);

  const handleTextareaWheelCapture = useCallback((event: ReactWheelEvent<HTMLTextAreaElement>) => {
    const textarea = textareaRef.current;
    if (!textarea || event.ctrlKey) return;

    const lineHeightPx = Number.parseFloat(window.getComputedStyle(textarea).lineHeight || "") || 20;
    const deltaY = normalizeComposerWheelDeltaY(event.deltaY, event.deltaMode, lineHeightPx);
    const target = resolveComposerWheelTarget(
      {
        scrollTop: textarea.scrollTop,
        clientHeight: textarea.clientHeight,
        scrollHeight: textarea.scrollHeight,
      },
      deltaY,
    );

    if (target === "ignore") return;

    if (target === "composer") {
      const maxScrollTop = Math.max(0, textarea.scrollHeight - textarea.clientHeight);
      const nextTop = clamp(textarea.scrollTop + deltaY, 0, maxScrollTop);
      event.preventDefault();
      event.stopPropagation();
      if (Math.abs(nextTop - textarea.scrollTop) > 0.5) {
        textarea.scrollTop = nextTop;
      }
      return;
    }

    const scroller = findComposerTranscriptScroller(textarea);
    if (!scroller) return;

    const maxScrollTop = Math.max(0, scroller.scrollHeight - scroller.clientHeight);
    const nextTop = Math.max(0, Math.min(maxScrollTop, scroller.scrollTop + deltaY));
    event.preventDefault();
    event.stopPropagation();
    if (Math.abs(nextTop - scroller.scrollTop) <= 0.5) return;
    scroller.scrollTop = nextTop;
    scroller.dispatchEvent(new Event("scroll"));
  }, []);

  const handleTextareaChange = useCallback((nextValue: string) => {
    restoreDraftTailRef.current = false;
    setValue(nextValue);
  }, [setValue]);

  useLayoutEffect(() => {
    resizeTextarea();
    const textarea = textareaRef.current;
    if (!textarea) return;
    const pendingSelection = pendingSelectionRef.current;
    if (pendingSelection !== null) {
      textarea.setSelectionRange(pendingSelection, pendingSelection);
      pendingSelectionRef.current = null;
      return;
    }
    if (!restoreDraftTailRef.current || value.length === 0) return;
    const end = textarea.value.length;
    textarea.setSelectionRange(end, end);
    textarea.scrollTop = textarea.scrollHeight;
    restoreDraftTailRef.current = false;
  }, [resizeTextarea, value]);

  useEffect(() => {
    const textarea = textareaNode;
    if (!textarea) return;
    const params = new URLSearchParams(window.location.search);
    const e2eEnabled = window.sessionStorage.getItem("ctxE2E") === "1" || params.get("ctxE2E") === "1";
    const debugWindow = window as Window & { __ctxComposerPasteDebug?: WorkbenchComposerPasteDebug };
    if (e2eEnabled) {
      debugWindow.__ctxComposerPasteDebug ??= {
        attachedCount: 0,
        hitCount: 0,
        lastAttachedClassName: null,
      };
      debugWindow.__ctxComposerPasteDebug.attachedCount += 1;
      debugWindow.__ctxComposerPasteDebug.lastAttachedClassName = textarea.className || null;
    }
    const onNativePaste = (event: ClipboardEvent) => {
      handleClipboardPaste(textarea, event.clipboardData, () => event.preventDefault());
    };
    textarea.addEventListener("paste", onNativePaste);
    return () => textarea.removeEventListener("paste", onNativePaste);
  }, [handleClipboardPaste, textareaNode]);

  useEffect(() => {
    const debugWindow = window as Window & { __ctxComposerPasteDebug?: WorkbenchComposerPasteDebug };
    if (!debugWindow.__ctxComposerPasteDebug) return;
    debugWindow.__ctxComposerPasteDebug.lastRenderedAttachmentCount = attachments.length;
  }, [attachments]);

  return {
    clearSelectionBeforeSubmit,
    handleTextareaChange,
    handleTextareaWheelCapture,
    mirrorRef,
    setTextareaElement,
    textareaRef,
  };
}
