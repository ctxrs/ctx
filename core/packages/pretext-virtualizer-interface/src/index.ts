export type PretextVirtualizerItemAlign = "start" | "center" | "end";

export type PretextVirtualizerItemIndex = number | "LAST";

export type PretextVirtualizerItemLocation = {
  index: PretextVirtualizerItemIndex;
  align?: PretextVirtualizerItemAlign;
  behavior?: ScrollBehavior;
};

export type PretextVirtualizerScrollLocation = {
  listOffset: number;
  visibleListHeight: number;
  bottomOffset: number;
};

export type PretextVirtualizerShortSizeAlign = "top" | "bottom";

export type PretextVirtualizerListMethods<Item, _Context> = {
  cancelSmoothScroll: () => void;
  scrollerElement: () => HTMLElement | null;
  restoreAnchor: (anchor: import("@pretext-virtualizer/core").PretextVirtualizerLogicalAnchor) => void;
  scrollToBottom: (behavior?: ScrollBehavior) => void;
  scrollToOffset: (scrollTop: number, behavior?: ScrollBehavior) => void;
  scrollToItem: (location: PretextVirtualizerItemLocation) => void;
};
