import type { Dispatch, MutableRefObject, SetStateAction } from "react";
import type {
  AutoscrollToBottom,
  ItemLocation,
  VirtuosoMessageListMethods,
} from "@virtuoso.dev/message-list";
import type { WorkbenchListItem } from "./SessionPage.types";
import type { WorkbenchMessageListContext } from "./SessionPage.thread";
import type { WorkbenchThreadProjectionOp } from "./sessionThreadProjection";

export type MessageListMethods = VirtuosoMessageListMethods<
  WorkbenchListItem,
  WorkbenchMessageListContext
>;

export type SessionMessageListReconcileParams = {
  sessionId: string;
  isActive: boolean;
  listItems: WorkbenchListItem[];
  visibleListItems: WorkbenchListItem[];
  loadingOlder: boolean;
  deferTrailingAppends: boolean;
  layoutRevision: string;
  itemSizeCacheKey: (item: WorkbenchListItem) => string | null;
  threadOp?: WorkbenchThreadProjectionOp | null;
  showDebug: boolean;
  initialLocation: ItemLocation;
  appendBehavior: AutoscrollToBottom<WorkbenchListItem, WorkbenchMessageListContext>;
  methodsRef: MutableRefObject<MessageListMethods | null>;
  lastSessionIdRef: MutableRefObject<string>;
  contractViolationLoggedRef: MutableRefObject<{ sessionId: string; violationKey: string } | null>;
  lastScrollLocationRef: MutableRefObject<unknown>;
  lastAtBottomRef: MutableRefObject<boolean | null>;
  lastListOffsetRef: MutableRefObject<number | null>;
  stickToBottomRef: MutableRefObject<boolean>;
  renderedAnchorIdRef: MutableRefObject<string | null>;
  renderedTopIdRef: MutableRefObject<string | null>;
  firstListItemIdRef: MutableRefObject<string | null>;
  pendingHistoryRef: MutableRefObject<boolean>;
  historyExpectedRef: MutableRefObject<boolean>;
  historyRequestedAtTopRef: MutableRefObject<boolean>;
  historyRequestedAnchorIdRef: MutableRefObject<string | null>;
  lastLayoutRevisionRef: MutableRefObject<string>;
  reconcileEpochRef: MutableRefObject<number>;
  suppressIdDiffLogsRef: MutableRefObject<{ sessionId: string; remainingTicks: number } | null>;
  setLoadingOlder: Dispatch<SetStateAction<boolean>>;
  setDeferTrailingAppends: Dispatch<SetStateAction<boolean>>;
  snapToBottom: (methods: MessageListMethods) => void;
  recordDebugSnapshot: (cause: string, detail?: Record<string, unknown> | null) => void;
  startFlashProbe: (cause: string, detail?: Record<string, unknown> | null) => void;
  logMessageListDebug: (label: string, detail: Record<string, unknown>) => void;
};
