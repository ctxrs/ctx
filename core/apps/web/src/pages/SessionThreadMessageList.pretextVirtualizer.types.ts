import type { CSSProperties, MutableRefObject, ReactNode } from "react";
import type {
  PretextVirtualizerItemLocation,
  PretextVirtualizerListMethods,
  PretextVirtualizerScrollLocation,
  PretextVirtualizerShortSizeAlign,
} from "@pretext-virtualizer/interface";
import type { PretextVirtualizerDiagnosticEvent } from "@pretext-virtualizer/core";
import type { WorkbenchListItem } from "./SessionPage.types";
import type { WorkbenchMessageListContext } from "./SessionPage.thread";
import type { WorkbenchThreadProjectionOp } from "./sessionThreadProjection";

export type SessionThreadPretextVirtualizerListProps = {
  style: CSSProperties;
  sessionId: string;
  isActive: boolean;
  listItems: WorkbenchListItem[];
  sourceKey?: string;
  threadProjectionOp: WorkbenchThreadProjectionOp;
  initialLocation?: PretextVirtualizerItemLocation | null;
  itemContent: (index: number, item: WorkbenchListItem) => ReactNode;
  itemKey: (item: WorkbenchListItem) => string;
  context: WorkbenchMessageListContext;
  onScroll?: (location: PretextVirtualizerScrollLocation) => void;
  onRenderedDataChange?: (range: readonly WorkbenchListItem[]) => void;
  onAtBottomChange?: (atBottom: boolean) => void;
  onDiagnosticEvent?: (event: PretextVirtualizerDiagnosticEvent<WorkbenchListItem>) => void;
  methodsRef?: MutableRefObject<PretextVirtualizerListMethods<WorkbenchListItem, WorkbenchMessageListContext> | null>;
  shortSizeAlign?: PretextVirtualizerShortSizeAlign;
};
