import { useEffect, useRef, type ReactNode } from "react";
import type { WorkbenchListItem } from "../sessionView/SessionPage.types";
import { AuditedPretextRow } from "./pretextVirtualizerRowAudit";

const ROW_SIZE_DELTA_PX = 1;

export function MeasuredPretextRow({
  id,
  itemKind,
  itemKey,
  plannedHeight,
  onHeightMismatch,
  children,
}: {
  id: string;
  itemKind: WorkbenchListItem["kind"];
  itemKey: string;
  plannedHeight: number;
  onHeightMismatch?: (params: { actualHeight: number; plannedHeight: number }) => void;
  children: ReactNode;
}) {
  const rowRef = useRef<HTMLDivElement | null>(null);
  const onHeightMismatchRef = useRef(onHeightMismatch);
  const plannedHeightRef = useRef(plannedHeight);
  const hasHeightMismatchHandler = typeof onHeightMismatch === "function";
  onHeightMismatchRef.current = onHeightMismatch;
  plannedHeightRef.current = plannedHeight;

  useEffect(() => {
    if (!hasHeightMismatchHandler) {
      return;
    }
    const rowEl = rowRef.current;
    if (!rowEl) {
      return;
    }
    const shellEl = rowEl.closest("[data-pretext-virtualizer-row-shell='1']") as HTMLElement | null;
    if (!shellEl) {
      return;
    }

    const emitMismatch = () => {
      const actualHeight = rowEl.getBoundingClientRect().height;
      const nextPlannedHeight = plannedHeightRef.current;
      if (Math.abs(actualHeight - nextPlannedHeight) <= ROW_SIZE_DELTA_PX) {
        return;
      }
      onHeightMismatchRef.current?.({ actualHeight, plannedHeight: nextPlannedHeight });
    };

    emitMismatch();
    const observer = new ResizeObserver(() => emitMismatch());
    observer.observe(rowEl);
    observer.observe(shellEl);
    const rafId = requestAnimationFrame(() => emitMismatch());
    return () => {
      cancelAnimationFrame(rafId);
      observer.disconnect();
    };
  }, [hasHeightMismatchHandler, plannedHeight]);

  return (
    <AuditedPretextRow
      id={id}
      itemKind={itemKind}
      itemKey={itemKey}
      plannedHeight={plannedHeight}
      listItemRef={rowRef}
    >
      {children}
    </AuditedPretextRow>
  );
}
