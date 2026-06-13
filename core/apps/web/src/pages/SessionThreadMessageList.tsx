import { memo, type CSSProperties, type MutableRefObject, type ReactNode } from "react";
import type {
  PretextVirtualizerItemLocation,
  PretextVirtualizerListMethods,
  PretextVirtualizerScrollLocation,
  PretextVirtualizerShortSizeAlign,
} from "@pretext-virtualizer/interface";
import { SessionThreadPretextVirtualizerList } from "./SessionThreadMessageList.pretextVirtualizer";
import type { WorkbenchListItem } from "./SessionPage.types";
import type { WorkbenchMessageListContext } from "./SessionPage.thread";
import type { WorkbenchThreadProjectionOp } from "./sessionThreadProjection";

export const SessionThreadMessageList = memo(function SessionThreadMessageList({
  sessionId,
  isActive,
  style,
  initialData,
  sourceKey,
  itemContent,
  itemKey,
  initialLocation,
  context,
  onScroll,
  onRenderedDataChange,
  methodsRef,
  shortSizeAlign,
  itemIdentity: _itemIdentity,
  increaseViewportBy: _increaseViewportBy,
  threadProjectionOp,
  licenseKey: _licenseKey,
}: {
  sessionId: string;
  isActive: boolean;
  style: CSSProperties;
  initialData: WorkbenchListItem[];
  sourceKey?: string;
  itemContent: (index: number, item: WorkbenchListItem) => ReactNode;
  itemIdentity: (item: WorkbenchListItem) => unknown;
  itemKey: (item: WorkbenchListItem) => string;
  increaseViewportBy: number;
  initialLocation: PretextVirtualizerItemLocation | null;
  threadProjectionOp: WorkbenchThreadProjectionOp;
  context: WorkbenchMessageListContext;
  onScroll: (location: PretextVirtualizerScrollLocation) => void;
  onRenderedDataChange: (range: readonly WorkbenchListItem[]) => void;
  methodsRef: MutableRefObject<PretextVirtualizerListMethods<WorkbenchListItem, WorkbenchMessageListContext> | null>;
  licenseKey: string;
  shortSizeAlign: PretextVirtualizerShortSizeAlign;
}) {
  return (
    <SessionThreadPretextVirtualizerList
      style={style}
      sessionId={sessionId}
      isActive={isActive}
      listItems={initialData}
      sourceKey={sourceKey}
      itemContent={itemContent}
      itemKey={itemKey}
      initialLocation={initialLocation}
      threadProjectionOp={threadProjectionOp}
      context={context}
      onScroll={onScroll}
      onRenderedDataChange={onRenderedDataChange}
      methodsRef={methodsRef}
      shortSizeAlign={shortSizeAlign}
    />
  );
});
