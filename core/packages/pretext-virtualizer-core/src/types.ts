export type PretextVirtualizerLayoutRevision = string | number;
export type PretextVirtualizerWidthBucket = `w${number}`;
export type PretextVirtualizerAnchorRestoreMode = "offset" | "ratio";
export type PretextVirtualizerAnchorCaptureMode = "default" | "detached";

export type PretextVirtualizerPlannedLayout = {
  height: number;
};

export type PretextVirtualizerLogicalAnchor =
  | { kind: "bottom" }
  | {
      kind: "item";
      id: string;
      index: number;
      offsetPx: number;
      offsetRatio: number;
    };

export type PretextVirtualizerVisibleItem<Item> = {
  id: string;
  index: number;
  item: Item;
  layoutRevision: PretextVirtualizerLayoutRevision;
  top: number;
  height: number;
  widthBucket: PretextVirtualizerWidthBucket;
};

export type PretextVirtualizerSnapshot<Item> = {
  scrollTop: number;
  viewportHeight: number;
  viewportWidth: number;
  totalHeight: number;
  widthBucket: PretextVirtualizerWidthBucket;
  anchor: PretextVirtualizerLogicalAnchor;
  visibleItems: readonly PretextVirtualizerVisibleItem<Item>[];
};

export type PretextVirtualizerDiagnosticEvent<Item> = {
  type: string;
  snapshot: PretextVirtualizerSnapshot<Item>;
  detail?: Record<string, unknown> | null;
};

export type PretextVirtualizerCoreOptions<Item> = {
  initialItems?: readonly Item[];
  getPlannedLayout: (
    item: Item,
    viewport: {
      width: number;
      widthBucket: PretextVirtualizerWidthBucket;
    },
  ) => PretextVirtualizerPlannedLayout;
  getId: (item: Item) => string;
  getLayoutRevision: (item: Item) => PretextVirtualizerLayoutRevision;
  overscanPx?: number;
  bottomThresholdPx?: number;
  widthBucketSize?: number;
  viewportHeight?: number;
  viewportWidth?: number;
  onDiagnosticEvent?: (event: PretextVirtualizerDiagnosticEvent<Item>) => void;
};

export type PretextVirtualizerCore<Item> = {
  getSnapshot: () => PretextVirtualizerSnapshot<Item>;
  getAnchor: (mode?: PretextVirtualizerAnchorCaptureMode) => PretextVirtualizerLogicalAnchor;
  syncViewport: (viewport: {
    height: number;
    width: number;
    scrollTop: number;
  }) => PretextVirtualizerSnapshot<Item>;
  replaceItems: (
    items: readonly Item[],
    anchorOverride?: PretextVirtualizerLogicalAnchor | null,
  ) => PretextVirtualizerSnapshot<Item>;
  appendItems: (
    items: readonly Item[],
    anchorOverride?: PretextVirtualizerLogicalAnchor | null,
  ) => PretextVirtualizerSnapshot<Item>;
  prependItems: (
    items: readonly Item[],
    anchorOverride?: PretextVirtualizerLogicalAnchor | null,
  ) => PretextVirtualizerSnapshot<Item>;
  syncItems: (
    items: readonly Item[],
    anchorOverride?: PretextVirtualizerLogicalAnchor | null,
  ) => PretextVirtualizerSnapshot<Item>;
  patchItems: (
    items: readonly Item[],
    changedItemIds: readonly string[],
    remeasureItemIds: readonly string[],
    anchorOverride?: PretextVirtualizerLogicalAnchor | null,
  ) => PretextVirtualizerSnapshot<Item>;
  restoreAnchor: (
    anchor: PretextVirtualizerLogicalAnchor,
    mode?: PretextVirtualizerAnchorRestoreMode,
  ) => PretextVirtualizerSnapshot<Item>;
  getOffsetForIndex: (index: number) => number;
  getHeightForIndex: (index: number) => number;
};

export type PretextVirtualizerComputedLayout<Item> = {
  widthBucket: PretextVirtualizerWidthBucket;
  heights: Array<{
    id: string;
    item: Item;
    layoutRevision: PretextVirtualizerLayoutRevision;
    height: number;
  }>;
  offsets: number[];
  totalHeight: number;
};
