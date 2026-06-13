export type ComposerScrollMetrics = {
  scrollTop: number;
  clientHeight: number;
  scrollHeight: number;
};

export type ComposerWheelTarget = "composer" | "transcript" | "ignore";

const EDGE_EPSILON_PX = 1;
const DEFAULT_LINE_HEIGHT_PX = 20;
const PAGE_DELTA_LINES = 16;

export function normalizeComposerWheelDeltaY(deltaY: number, deltaMode: number, lineHeightPx = DEFAULT_LINE_HEIGHT_PX): number {
  if (!Number.isFinite(deltaY) || deltaY === 0) return 0;
  if (deltaMode === 1) return deltaY * Math.max(1, lineHeightPx);
  if (deltaMode === 2) return deltaY * Math.max(1, lineHeightPx) * PAGE_DELTA_LINES;
  return deltaY;
}

export function resolveComposerWheelTarget(
  metrics: ComposerScrollMetrics,
  deltaY: number,
): ComposerWheelTarget {
  if (!Number.isFinite(deltaY) || Math.abs(deltaY) <= 0.1) return "ignore";

  const scrollTop = Number.isFinite(metrics.scrollTop) ? Math.max(0, metrics.scrollTop) : 0;
  const clientHeight = Number.isFinite(metrics.clientHeight) ? Math.max(0, metrics.clientHeight) : 0;
  const scrollHeight = Number.isFinite(metrics.scrollHeight) ? Math.max(0, metrics.scrollHeight) : 0;

  if (scrollHeight <= clientHeight + EDGE_EPSILON_PX) return "transcript";

  if (deltaY < 0) {
    return scrollTop > EDGE_EPSILON_PX ? "composer" : "transcript";
  }

  const remaining = scrollHeight - (scrollTop + clientHeight);
  return remaining > EDGE_EPSILON_PX ? "composer" : "transcript";
}

export function findComposerTranscriptScroller(textarea: HTMLTextAreaElement): HTMLElement | null {
  const sessionView = textarea.closest(".wb-session-view");
  if (!sessionView) return null;
  const scroller = sessionView.querySelector(".wb-thread-scroller");
  return scroller instanceof HTMLElement ? scroller : null;
}
