import { memo, useCallback, useEffect, useRef, type CSSProperties, type MutableRefObject, type ReactNode } from "react";
import {
  VirtuosoMessageList,
  VirtuosoMessageListLicense,
  type DataWithScrollModifier,
  type ItemLocation,
  type ItemContent as MessageItemContent,
  type ListScrollLocation,
  type ShortSizeAlign,
  type VirtuosoMessageListMethods,
} from "@virtuoso.dev/message-list";
import type { WorkbenchListItem } from "../sessionView/SessionPage.types";
import {
  WorkbenchMessageListEmptyPlaceholder,
  WorkbenchMessageListStickyFooter,
} from "./SessionThreadMessageListChrome";
import { recordSessionMessageListRowSizeMismatch } from "../sessionMessageListDebug";

type WorkbenchMessageListStackProps = {
  virtuosoStyle: CSSProperties;
  initialData: WorkbenchListItem[];
  itemContent: (index: number, item: WorkbenchListItem) => ReactNode;
  itemIdentity: (item: WorkbenchListItem) => unknown;
  itemKey: (item: WorkbenchListItem) => string;
  increaseViewportBy: number;
  initialLocation?: ItemLocation;
  dataState?: DataWithScrollModifier<WorkbenchListItem>;
  context: WorkbenchMessageListContext;
  onScroll: (location: ListScrollLocation) => void;
  onRenderedDataChange: (range: WorkbenchListItem[]) => void;
  listRef: MutableRefObject<VirtuosoMessageListMethods<WorkbenchListItem, WorkbenchMessageListContext> | null>;
  licenseKey: string;
  shortSizeAlign: ShortSizeAlign;
};

export type WorkbenchMessageListContext = {
  loaded: boolean;
  loadingOlder: boolean;
  renderRevision?: string;
  renderRevisionByItemId?: Readonly<Record<string, number>>;
  expandedTurnHeaders?: Readonly<Record<string, boolean>>;
  expandedTurnDetailsById?: Readonly<Record<string, boolean>>;
  expandedToolById?: Readonly<Record<string, boolean>>;
  expandedMessageById?: Readonly<Record<string, boolean>>;
  turnToolsLoading?: readonly string[];
  verbosity?: string;
};

const DEBUG_ROW_SIZE_DELTA_PX = 8;
function MeasuredThreadRow({
  id,
  itemKind,
  itemKey,
  children,
}: {
  id: string;
  itemKind: WorkbenchListItem["kind"];
  itemKey: string;
  children: ReactNode;
}) {
  const rowRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    let debugEnabled = false;
    try {
      debugEnabled = new URLSearchParams(window.location.search).get("debug") === "1";
    } catch {
      debugEnabled = false;
    }
    if (!debugEnabled) return;

    const rowEl = rowRef.current;
    if (!rowEl) return;
    const parentEl = rowEl.parentElement as HTMLElement | null;
    if (!parentEl) return;

    let lastSignature = "";
    const emitMismatch = (reason: string) => {
      const knownSizeRaw = parentEl.getAttribute("data-known-size");
      const dataIndexRaw = parentEl.getAttribute("data-index");
      const actualHeight = rowEl.getBoundingClientRect().height;
      const parentHeight = parentEl.getBoundingClientRect().height;
      const knownSize = knownSizeRaw == null ? Number.NaN : Number(knownSizeRaw);
      const dataIndex = dataIndexRaw == null ? null : Number(dataIndexRaw);
      if (!Number.isFinite(knownSize)) return;
      const knownVsActualDeltaPx = actualHeight - knownSize;
      const knownVsParentDeltaPx = parentHeight - knownSize;
      const parentVsActualDeltaPx = actualHeight - parentHeight;
      if (Math.abs(knownVsActualDeltaPx) <= DEBUG_ROW_SIZE_DELTA_PX) return;
      const signature = `${reason}:${knownSize}:${Math.round(actualHeight)}:${Math.round(parentHeight)}`;
      if (signature === lastSignature) return;
      lastSignature = signature;
      recordSessionMessageListRowSizeMismatch({
        id,
        itemKind,
        itemKey,
        reason,
        dataIndex: Number.isFinite(dataIndex) ? dataIndex : null,
        knownSize,
        actualHeight,
        parentHeight,
        knownVsActualDeltaPx,
        knownVsParentDeltaPx,
        parentVsActualDeltaPx,
      });
      // eslint-disable-next-line no-console
      console.log("[MessageList][row-size-mismatch]", {
        id,
        itemKind,
        itemKey,
        reason,
        dataIndex: parentEl.getAttribute("data-index"),
        knownSize,
        actualHeight,
        parentHeight,
        knownVsActualDeltaPx,
        knownVsParentDeltaPx,
        parentVsActualDeltaPx,
      });
    };

    emitMismatch("mount");
    const observer = new ResizeObserver(() => emitMismatch("resize"));
    observer.observe(rowEl);
    observer.observe(parentEl);
    const rafId = requestAnimationFrame(() => emitMismatch("raf"));
    return () => {
      cancelAnimationFrame(rafId);
      observer.disconnect();
    };
  }, [id]);

  return (
    <div ref={rowRef} role="listitem" data-thread-item-id={id}>
      {children}
    </div>
  );
}

export const WorkbenchMessageListStack = memo(function WorkbenchMessageListStack({
  virtuosoStyle,
  initialData,
  itemContent,
  itemIdentity,
  itemKey,
  increaseViewportBy,
  initialLocation,
  dataState,
  context,
  onScroll,
  onRenderedDataChange,
  listRef,
  licenseKey,
  shortSizeAlign,
}: WorkbenchMessageListStackProps) {
  // Keep ItemContent component identity stable so React doesn't remount visible rows
  // (which clears text selection / hover state) when SessionView rerenders.
  const itemContentRef = useRef(itemContent);
  itemContentRef.current = itemContent;
  const itemKeyRef = useRef(itemKey);
  itemKeyRef.current = itemKey;
  const ItemContent = useCallback<MessageItemContent<WorkbenchListItem, WorkbenchMessageListContext>>(
    ({ index, data }) => {
      if (!data) return <div style={{ height: 1 }} />;
      const measuredItemKey = itemKeyRef.current(data);
      return (
        <MeasuredThreadRow key={measuredItemKey} id={data.id} itemKind={data.kind} itemKey={measuredItemKey}>
          {itemContentRef.current(index, data)}
        </MeasuredThreadRow>
      );
    },
    [],
  );

  return (
    <div className="thread-stack wb-thread-stack wb-thread-scroller--message-list">
      <VirtuosoMessageListLicense licenseKey={licenseKey}>
        <VirtuosoMessageList<WorkbenchListItem, WorkbenchMessageListContext>
          ref={listRef}
          style={virtuosoStyle}
          className="wb-thread-scroller"
          role="list"
          initialData={initialData}
          data={dataState}
          context={context}
          itemIdentity={itemIdentity}
          computeItemKey={({ data }) => itemKeyRef.current(data)}
          ItemContent={ItemContent}
          initialLocation={initialLocation}
          onScroll={onScroll}
          onRenderedDataChange={onRenderedDataChange}
          EmptyPlaceholder={WorkbenchMessageListEmptyPlaceholder}
          StickyFooter={WorkbenchMessageListStickyFooter}
          shortSizeAlign={shortSizeAlign}
          increaseViewportBy={increaseViewportBy}
        />
      </VirtuosoMessageListLicense>
    </div>
  );
});
