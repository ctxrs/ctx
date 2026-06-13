import type { MutableRefObject } from "react";
import type {
  PretextVirtualizerLogicalAnchor,
  PretextVirtualizerSnapshot,
} from "@pretext-virtualizer/core";
import type {
  PretextVirtualizerItemLocation,
  PretextVirtualizerListMethods,
} from "@pretext-virtualizer/interface";
import type { WorkbenchListItem } from "../SessionPage.types";
import type { WorkbenchMessageListContext } from "../SessionPage.thread";

type ApplySnapshotOptions = {
  behavior?: ScrollBehavior;
  followBottom?: boolean;
  programmaticScroll?: boolean;
};

type SessionThreadPretextVirtualizerMethodsDeps = {
  applySnapshotToDom: (
    snapshot: PretextVirtualizerSnapshot<WorkbenchListItem>,
    options?: ApplySnapshotOptions,
  ) => void;
  containerRef: MutableRefObject<HTMLDivElement | null>;
  pendingProgrammaticBehaviorRef: MutableRefObject<ScrollBehavior>;
  pendingProgrammaticTopRef: MutableRefObject<number | null>;
  restoreAnchorSnapshot: (
    anchor: PretextVirtualizerLogicalAnchor,
  ) => PretextVirtualizerSnapshot<WorkbenchListItem>;
  restoreBottom: (behavior?: ScrollBehavior) => void;
  scrollToItemFn: (location: PretextVirtualizerItemLocation) => void;
  scrollToOffsetFn: (scrollTop: number, behavior?: ScrollBehavior) => void;
};

export function createSessionThreadPretextVirtualizerMethods({
  applySnapshotToDom,
  containerRef,
  pendingProgrammaticBehaviorRef,
  pendingProgrammaticTopRef,
  restoreAnchorSnapshot,
  restoreBottom,
  scrollToItemFn,
  scrollToOffsetFn,
}: SessionThreadPretextVirtualizerMethodsDeps): PretextVirtualizerListMethods<
  WorkbenchListItem,
  WorkbenchMessageListContext
> {
  return {
    cancelSmoothScroll: () => {
      const scroller = containerRef.current;
      if (!scroller) return;
      scroller.scrollTo({ top: scroller.scrollTop, behavior: "auto" });
      pendingProgrammaticTopRef.current = null;
      pendingProgrammaticBehaviorRef.current = "auto";
    },
    scrollerElement: () => containerRef.current,
    restoreAnchor: (anchor) => {
      const nextSnapshot = restoreAnchorSnapshot(anchor);
      applySnapshotToDom(nextSnapshot, {
        behavior: "auto",
        followBottom: anchor.kind === "bottom",
      });
    },
    scrollToBottom: (behavior = "auto") => {
      restoreBottom(behavior);
    },
    scrollToItem: (location) => {
      scrollToItemFn(location);
    },
    scrollToOffset: (scrollTop, behavior = "auto") => {
      scrollToOffsetFn(scrollTop, behavior);
    },
  };
}
