import type { CSSProperties, MutableRefObject, ReactNode } from "react";
import type {
  PretextVirtualizerItemLocation,
  PretextVirtualizerListMethods,
  PretextVirtualizerScrollLocation,
  PretextVirtualizerShortSizeAlign,
} from "@pretext-virtualizer/interface";
import type { WorkbenchMessageListContext } from "../sessionThread";
import type { WorkbenchListItem } from "../sessionView/SessionPage.types";
import type { WorkbenchThreadProjectionOp } from "../sessionThreadProjection";
import { SessionThreadMessageList } from "../SessionThreadMessageList";

export function SessionThreadPane({
  sessionId,
  isActive,
  style,
  initialData,
  sourceKey,
  itemContent,
  itemIdentity,
  itemKey,
  increaseViewportBy,
  initialLocation,
  threadProjectionOp,
  context,
  onScroll,
  onRenderedDataChange,
  methodsRef,
  licenseKey,
  shortSizeAlign,
  children,
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
  children: ReactNode;
}) {
  const shouldRenderList = initialData.length > 0;

  return (
    <>
      {shouldRenderList ? (
        <SessionThreadMessageList
          sessionId={sessionId}
          isActive={isActive}
          style={style}
          initialData={initialData}
          sourceKey={sourceKey}
          itemContent={itemContent}
          itemIdentity={itemIdentity}
          itemKey={itemKey}
          increaseViewportBy={increaseViewportBy}
          initialLocation={initialLocation}
          threadProjectionOp={threadProjectionOp}
          context={context}
          onScroll={onScroll}
          onRenderedDataChange={onRenderedDataChange}
          methodsRef={methodsRef}
          licenseKey={licenseKey}
          shortSizeAlign={shortSizeAlign}
        />
      ) : (
        <div
          className="wb-session-thread-hydrating"
          style={style}
          aria-hidden="true"
        />
      )}
      <div className="wb-session-bottom">{children}</div>
    </>
  );
}
