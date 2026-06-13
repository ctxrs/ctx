import { useEffect, useRef, type ReactNode, type Ref } from "react";
import type { WorkbenchListItem } from "../sessionView/SessionPage.types";
import { recordSessionMessageListRowSizeMismatch } from "../sessionMessageListDebug";

const DEBUG_ROW_SIZE_DELTA_PX = 1;

function isPretextVirtualizerDebugEnabled(): boolean {
  try {
    return new URLSearchParams(window.location.search).get("debug") === "1";
  } catch {
    return false;
  }
}

export function AuditedPretextRow({
  id,
  itemKind,
  itemKey,
  plannedHeight,
  listItemRef,
  children,
}: {
  id: string;
  itemKind: WorkbenchListItem["kind"];
  itemKey: string;
  plannedHeight: number;
  listItemRef?: Ref<HTMLDivElement>;
  children: ReactNode;
}) {
  const debugEnabled = isPretextVirtualizerDebugEnabled();
  const rowRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!debugEnabled) return;
    const rowEl = rowRef.current;
    if (!rowEl) return;
    const shellEl = rowEl.closest("[data-pretext-virtualizer-row-shell='1']") as HTMLElement | null;
    if (!shellEl) return;

    let lastSignature = "";
    const emitMismatch = (reason: string) => {
      const actualHeight = rowEl.getBoundingClientRect().height;
      const shellHeight = shellEl.getBoundingClientRect().height;
      const plannedVsActualDeltaPx = actualHeight - plannedHeight;
      const plannedVsShellDeltaPx = shellHeight - plannedHeight;
      const shellVsActualDeltaPx = actualHeight - shellHeight;
      if (Math.abs(plannedVsActualDeltaPx) <= DEBUG_ROW_SIZE_DELTA_PX) return;
      const signature = `${reason}:${plannedHeight}:${Math.round(actualHeight)}:${Math.round(shellHeight)}`;
      if (signature === lastSignature) return;
      lastSignature = signature;
      recordSessionMessageListRowSizeMismatch({
        id,
        itemKind,
        itemKey,
        reason,
        dataIndex: null,
        knownSize: plannedHeight,
        actualHeight,
        parentHeight: shellHeight,
        knownVsActualDeltaPx: plannedVsActualDeltaPx,
        knownVsParentDeltaPx: plannedVsShellDeltaPx,
        parentVsActualDeltaPx: shellVsActualDeltaPx,
      });
      // eslint-disable-next-line no-console
      console.log("[PretextVirtualizer][row-size-mismatch]", {
        id,
        itemKind,
        itemKey,
        reason,
        plannedHeight,
        actualHeight,
        shellHeight,
        plannedVsActualDeltaPx,
        plannedVsShellDeltaPx,
        shellVsActualDeltaPx,
      });
    };

    emitMismatch("mount");
    const observer = new ResizeObserver(() => emitMismatch("resize"));
    observer.observe(rowEl);
    observer.observe(shellEl);
    const rafId = requestAnimationFrame(() => emitMismatch("raf"));
    return () => {
      cancelAnimationFrame(rafId);
      observer.disconnect();
    };
  }, [debugEnabled, id, itemKey, itemKind, plannedHeight]);

  return (
    <div
      ref={(node) => {
        rowRef.current = node;
        if (typeof listItemRef === "function") {
          listItemRef(node);
          return;
        }
        if (listItemRef && "current" in listItemRef) {
          listItemRef.current = node;
        }
      }}
      role="listitem"
      data-thread-item-id={id}
    >
      {children}
    </div>
  );
}
