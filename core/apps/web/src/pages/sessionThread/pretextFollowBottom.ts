export type ResolveFollowBottomAfterScrollParams = {
  followBottom: boolean;
  previousScrollTop: number;
  currentScrollTop: number;
  bottomOffsetPx: number;
  thresholdPx: number;
  programmaticScroll: boolean;
};

type BottomAttachmentParams = {
  followBottom: boolean;
  atBottom: boolean;
};
export function computeBottomOffsetPx(params: {
  totalHeight: number;
  scrollTop: number;
  viewportHeight: number;
}): number {
  const totalHeight = Number.isFinite(params.totalHeight) ? Math.max(0, params.totalHeight) : 0;
  const scrollTop = Number.isFinite(params.scrollTop) ? Math.max(0, params.scrollTop) : 0;
  const viewportHeight = Number.isFinite(params.viewportHeight) ? Math.max(0, params.viewportHeight) : 0;
  return Math.max(0, totalHeight - (scrollTop + viewportHeight));
}

export function resolveFollowBottomAfterScroll({
  followBottom,
  previousScrollTop,
  currentScrollTop,
  bottomOffsetPx,
  thresholdPx,
  programmaticScroll,
}: ResolveFollowBottomAfterScrollParams): boolean {
  const scrolledUp = !programmaticScroll && currentScrollTop < previousScrollTop - 1;
  if (scrolledUp) {
    return false;
  }
  if (bottomOffsetPx <= thresholdPx && (programmaticScroll || bottomOffsetPx <= 1)) {
    return true;
  }
  return followBottom;
}

function isBottomAttached({ followBottom, atBottom }: BottomAttachmentParams): boolean {
  return followBottom || atBottom;
}

export function shouldRestoreBottomOnViewportResize(
  sizeChanged: boolean,
  params: BottomAttachmentParams,
): boolean {
  return sizeChanged && isBottomAttached(params);
}

export function shouldFollowBottomOnItemsUpdate(
  params: BottomAttachmentParams,
  bottomOffsetPx: number,
  thresholdPx: number,
): boolean {
  if (params.followBottom) {
    return true;
  }
  return params.atBottom && bottomOffsetPx <= thresholdPx;
}
