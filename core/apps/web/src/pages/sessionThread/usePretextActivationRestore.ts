import { useLayoutEffect, useRef, type MutableRefObject, type RefObject } from "react";
import type { PretextVirtualizerItemLocation } from "@pretext-virtualizer/interface";
import { isBottomOpenLocation } from "./pretextVirtualizerDisplayState";

type UsePretextActivationRestoreParams = {
  containerRef: RefObject<HTMLDivElement | null>;
  followBottomRef: MutableRefObject<boolean>;
  initialLocation: PretextVirtualizerItemLocation | null | undefined;
  isActive: boolean;
  restoreBottom: (behavior?: ScrollBehavior) => void;
  syncFromDom: () => void;
};

export function usePretextActivationRestore({
  containerRef,
  followBottomRef,
  initialLocation,
  isActive,
  restoreBottom,
  syncFromDom,
}: UsePretextActivationRestoreParams): void {
  const lastIsActiveRef = useRef(isActive);

  useLayoutEffect(() => {
    const wasActive = lastIsActiveRef.current;
    const becameActive = isActive && !wasActive;
    lastIsActiveRef.current = isActive;
    if (!becameActive) return;
    const scroller = containerRef.current;
    if (!scroller || scroller.clientWidth <= 0 || scroller.clientHeight <= 0) return;
    if (isBottomOpenLocation(initialLocation)) {
      followBottomRef.current = true;
      restoreBottom("auto");
      return;
    }
    syncFromDom();
  }, [containerRef, followBottomRef, initialLocation, isActive, restoreBottom, syncFromDom]);
}
