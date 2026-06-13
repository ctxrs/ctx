export type MobileSidebarSwipePoint = {
  clientX: number;
  clientY: number;
};

export const MOBILE_SIDEBAR_CLOSE_SWIPE_MIN_DISTANCE_PX = 64;
export const MOBILE_SIDEBAR_CLOSE_SWIPE_AXIS_RATIO = 1.35;

export function shouldCloseMobileSidebarSwipe(
  start: MobileSidebarSwipePoint,
  end: MobileSidebarSwipePoint,
): boolean {
  const deltaX = end.clientX - start.clientX;
  const deltaY = end.clientY - start.clientY;
  const horizontalDistance = Math.abs(deltaX);
  const verticalDistance = Math.abs(deltaY);

  return (
    deltaX <= -MOBILE_SIDEBAR_CLOSE_SWIPE_MIN_DISTANCE_PX &&
    horizontalDistance >= verticalDistance * MOBILE_SIDEBAR_CLOSE_SWIPE_AXIS_RATIO
  );
}
