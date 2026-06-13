import type { Page } from "@playwright/test";

const SCROLLER_SELECTOR = ".wb-thread-scroller";
const ACTIVE_SLOT_SELECTOR = ".wb-session-slot[aria-hidden='false']";
const ACTIVE_SESSION_VIEW_SELECTOR = `${ACTIVE_SLOT_SELECTOR} [data-testid="session-view"][data-session-id]`;

export type ThreadSurfaceSample = {
  atMs: number;
  sampleIndex: number;
  sessionId: string | null;
  sessionVisible: boolean;
  scrollerMounted: boolean;
  renderedItemCount: number;
  visibleRowCount: number;
  scrollTop: number | null;
  clientHeight: number | null;
  scrollHeight: number | null;
  distanceFromMaxScrollPx: number | null;
  blankTailPx: number | null;
  firstItemId: string | null;
  lastItemId: string | null;
  firstItemTopPx: number | null;
  firstItemBottomPx: number | null;
  lastItemTopPx: number | null;
  lastItemBottomPx: number | null;
  overlappingVisiblePairs: number;
  maxAdjacentVisibleOverlapPx: number | null;
  overlappingTextLinePairs: number;
  maxTextLineOverlapPx: number | null;
  impossibleTail: boolean;
  isBottom: boolean;
};

export type TopAnchorSample = {
  atMs: number;
  rowId: string | null;
  rowOffsetTopPx: number | null;
  renderedItemCount: number;
};

type ReadOptions = {
  scrollerSelector?: string;
  sampleIntervalMs?: number;
  sampleDurationMs?: number;
};

function toFiniteNumber(value: number | null | undefined): number | null {
  if (typeof value !== "number" || !Number.isFinite(value)) return null;
  return Math.round(value * 100) / 100;
}

export async function readActiveSessionId(page: Page): Promise<string | null> {
  return page.evaluate((activeSlotSelector: string) => {
    const session = document.querySelector<HTMLElement>(
      `${activeSlotSelector} [data-testid="session-view"][data-session-id]`,
    );
    return session?.getAttribute("data-session-id") ?? null;
  }, ACTIVE_SLOT_SELECTOR);
}

export async function readThreadSurfaceSample(
  page: Page,
  scrollerSelector = SCROLLER_SELECTOR,
): Promise<ThreadSurfaceSample> {
  return page.evaluate(
    ({ scrollerSelector, activeSlotSelector, activeSessionSelector }) => {
      const collectRenderedRows = (scroller: HTMLElement): HTMLElement[] => {
        const inner = scroller.firstElementChild;
        if (inner instanceof HTMLElement) {
          const directChildren = Array.from(inner.children).filter(
            (child): child is HTMLElement =>
              child instanceof HTMLElement && child.getBoundingClientRect().height > 0,
          );
          if (directChildren.length > 0) {
            return directChildren;
          }
        }
        return Array.from(scroller.querySelectorAll<HTMLElement>('[role="listitem"]'));
      };
      const resolveRowId = (row: HTMLElement | undefined): string | null =>
        row?.getAttribute("data-thread-item-id") ??
        row
          ?.querySelector<HTMLElement>("[data-thread-item-id]")
          ?.getAttribute("data-thread-item-id") ??
        null;
      const resolveRowWrapper = (scroller: HTMLElement, node: Node): HTMLElement | null => {
        const inner = scroller.firstElementChild;
        let current: HTMLElement | null = node instanceof HTMLElement ? node : node.parentElement;
        if (inner instanceof HTMLElement) {
          while (current && current.parentElement !== inner) {
            current = current.parentElement;
          }
          if (current instanceof HTMLElement && current.parentElement === inner) {
            return current;
          }
        }
        return current?.closest<HTMLElement>("[data-thread-item-id], [role='listitem']") ?? null;
      };
      const collectVisibleTextLineOverlap = (
        scroller: HTMLElement,
        viewportRect: DOMRect,
      ): { pairCount: number; maxOverlapPx: number } => {
        type LineRect = {
          rowKey: string | null;
          left: number;
          right: number;
          top: number;
          bottom: number;
        };

        const lineRects: LineRect[] = [];
        const walker = document.createTreeWalker(scroller, NodeFilter.SHOW_TEXT, {
          acceptNode(node) {
            return (node.textContent ?? "").trim().length > 0
              ? NodeFilter.FILTER_ACCEPT
              : NodeFilter.FILTER_REJECT;
          },
        });

        let textNode = walker.nextNode();
        while (textNode) {
          const range = document.createRange();
          range.selectNodeContents(textNode);
          const rowWrapper = resolveRowWrapper(scroller, textNode);
          const rowKey =
            resolveRowId(rowWrapper ?? undefined) ??
            (rowWrapper != null
              ? `wrapper:${Array.from(rowWrapper.parentElement?.children ?? []).indexOf(rowWrapper)}`
              : null);

          for (const rect of Array.from(range.getClientRects())) {
            const left = Math.max(rect.left, viewportRect.left);
            const right = Math.min(rect.right, viewportRect.right);
            const top = Math.max(rect.top, viewportRect.top);
            const bottom = Math.min(rect.bottom, viewportRect.bottom);
            if (right - left <= 4 || bottom - top <= 4) {
              continue;
            }
            lineRects.push({
              rowKey,
              left,
              right,
              top,
              bottom,
            });
          }
          textNode = walker.nextNode();
        }

        let pairCount = 0;
        let maxOverlapPx = 0;
        for (let index = 0; index < lineRects.length; index += 1) {
          const current = lineRects[index];
          if (!current) continue;
          for (let otherIndex = index + 1; otherIndex < lineRects.length; otherIndex += 1) {
            const other = lineRects[otherIndex];
            if (!other) continue;
            if (
              current.rowKey != null &&
              other.rowKey != null &&
              current.rowKey === other.rowKey
            ) {
              continue;
            }
            const overlapX = Math.min(current.right, other.right) - Math.max(current.left, other.left);
            const overlapY = Math.min(current.bottom, other.bottom) - Math.max(current.top, other.top);
            if (overlapX > 20 && overlapY > 4) {
              pairCount += 1;
              maxOverlapPx = Math.max(maxOverlapPx, overlapY);
            }
          }
        }

        return {
          pairCount,
          maxOverlapPx,
        };
      };
      const started = performance.now();
      const toFiniteNumber = (value: number | null | undefined): number | null =>
        typeof value === "number" && Number.isFinite(value) ? Math.round(value * 100) / 100 : null;
      const activeSlot = document.querySelector<HTMLElement>(activeSlotSelector);
      const session = activeSlot?.querySelector<HTMLElement>(activeSessionSelector) ?? null;
      const documentScroller = document.querySelector<HTMLElement>(scrollerSelector);
      const scroller = session?.querySelector<HTMLElement>(scrollerSelector) ?? documentScroller;
      if (!session || !scroller) {
        return {
          atMs: started,
          sampleIndex: 0,
          sessionId: null,
          sessionVisible: false,
          scrollerMounted: false,
          renderedItemCount: 0,
          visibleRowCount: 0,
          scrollTop: null,
          clientHeight: null,
          scrollHeight: null,
          distanceFromMaxScrollPx: null,
          blankTailPx: null,
          firstItemId: null,
          lastItemId: null,
          firstItemTopPx: null,
          firstItemBottomPx: null,
          lastItemTopPx: null,
          lastItemBottomPx: null,
          overlappingVisiblePairs: 0,
          maxAdjacentVisibleOverlapPx: null,
          overlappingTextLinePairs: 0,
          maxTextLineOverlapPx: null,
          impossibleTail: true,
          isBottom: false,
        };
      }

      const visibleScroller = session.querySelector<HTMLElement>(scrollerSelector) ?? scroller;
      const rows = collectRenderedRows(visibleScroller);
      const first = rows.at(0);
      const last = rows.at(-1);
      const scrollerRect = visibleScroller.getBoundingClientRect();
      const firstRect = first?.getBoundingClientRect() ?? null;
      const lastRect = last?.getBoundingClientRect() ?? null;
      const scrollTop = toFiniteNumber(visibleScroller.scrollTop);
      const clientHeight = toFiniteNumber(visibleScroller.clientHeight);
      const scrollHeight = toFiniteNumber(visibleScroller.scrollHeight);
      const maxScrollTop = scrollHeight != null && clientHeight != null ? Math.max(0, scrollHeight - clientHeight) : null;
      const distanceFromMaxScrollPx = scrollTop != null && maxScrollTop != null ? toFiniteNumber(maxScrollTop - scrollTop) : null;
      const blankTailPx = scrollerRect && lastRect ? toFiniteNumber(scrollerRect.bottom - lastRect.bottom) : null;
      const firstItemTopPx = scrollerRect && firstRect ? toFiniteNumber(firstRect.top - scrollerRect.top) : null;
      const firstItemBottomPx = scrollerRect && firstRect ? toFiniteNumber(firstRect.bottom - scrollerRect.top) : null;
      const lastItemTopPx = scrollerRect && lastRect ? toFiniteNumber(lastRect.top - scrollerRect.top) : null;
      const lastItemBottomPx = scrollerRect && lastRect ? toFiniteNumber(lastRect.bottom - scrollerRect.top) : null;
      const visibleRows = rows
        .map((row) => ({ row, rect: row.getBoundingClientRect() }))
        .filter(({ rect }) => rect.bottom > scrollerRect.top + 1 && rect.top < scrollerRect.bottom - 1);
      let overlappingVisiblePairs = 0;
      let maxAdjacentVisibleOverlapPx = 0;
      for (let index = 1; index < visibleRows.length; index += 1) {
        const prevRect = visibleRows[index - 1]?.rect;
        const nextRect = visibleRows[index]?.rect;
        if (!prevRect || !nextRect) continue;
        const overlapPx = prevRect.bottom - nextRect.top;
        if (overlapPx > 1) {
          overlappingVisiblePairs += 1;
          maxAdjacentVisibleOverlapPx = Math.max(maxAdjacentVisibleOverlapPx, overlapPx);
        }
      }
      const visibleTextOverlap = collectVisibleTextLineOverlap(visibleScroller, scrollerRect);
      const impossibleTail =
        distanceFromMaxScrollPx != null &&
        distanceFromMaxScrollPx <= 2 &&
        ((blankTailPx != null && blankTailPx > 96) || (lastRect != null && lastRect.bottom < scrollerRect.top - 1));
      const isBottom = distanceFromMaxScrollPx != null ? distanceFromMaxScrollPx <= 2 : false;

      return {
        atMs: started,
        sampleIndex: 0,
        sessionId: session.getAttribute("data-session-id"),
        sessionVisible: true,
        scrollerMounted: true,
        renderedItemCount: rows.length,
        visibleRowCount: visibleRows.length,
        scrollTop,
        clientHeight,
        scrollHeight,
        distanceFromMaxScrollPx,
        blankTailPx,
        firstItemId: resolveRowId(first),
        lastItemId: resolveRowId(last),
        firstItemTopPx,
        firstItemBottomPx,
        lastItemTopPx,
        lastItemBottomPx,
        overlappingVisiblePairs,
        maxAdjacentVisibleOverlapPx: toFiniteNumber(maxAdjacentVisibleOverlapPx),
        overlappingTextLinePairs: visibleTextOverlap.pairCount,
        maxTextLineOverlapPx: toFiniteNumber(visibleTextOverlap.maxOverlapPx),
        impossibleTail,
        isBottom,
      };
    },
    {
      scrollerSelector,
      activeSlotSelector: ACTIVE_SLOT_SELECTOR,
      activeSessionSelector: "[data-testid=\"session-view\"][data-session-id]",
    },
  );
}

export async function collectThreadSamples(page: Page, options: ReadOptions = {}): Promise<ThreadSurfaceSample[]> {
  const scrollerSelector = options.scrollerSelector ?? SCROLLER_SELECTOR;
  const sampleIntervalMs = Math.max(25, Math.floor(options.sampleIntervalMs ?? 80));
  const sampleDurationMs = Math.max(sampleIntervalMs, Math.floor(options.sampleDurationMs ?? 2200));
  const startedAt = Date.now();
  const deadline = startedAt + sampleDurationMs;
  const samples: ThreadSurfaceSample[] = [];
  let index = 0;

  while (Date.now() <= deadline) {
    const next = await readThreadSurfaceSample(page, scrollerSelector);
    samples.push({
      ...next,
      sampleIndex: index,
      atMs: Date.now() - startedAt,
    });
    index += 1;
    await page.waitForTimeout(sampleIntervalMs);
  }

  return samples;
}

export async function readTopAnchorSample(page: Page, scrollerSelector = SCROLLER_SELECTOR): Promise<TopAnchorSample | null> {
  return page.evaluate((payload) => {
    const { activeSlotSelector, scrollerSelector } = payload;
    const activeSession = document.querySelector<HTMLElement>(activeSlotSelector)?.querySelector<HTMLElement>(
      `[data-testid="session-view"][data-session-id]`,
    );
    const scroller = activeSession?.querySelector<HTMLElement>(scrollerSelector);
    if (!scroller) return null;

    const rows = Array.from(scroller.querySelectorAll<HTMLElement>("[role='listitem']"));
    const first = rows.at(0);
    if (!first) {
      return { atMs: performance.now(), rowId: null, rowOffsetTopPx: null, renderedItemCount: 0 };
    }
    const rowId =
      first.getAttribute("data-thread-item-id") ??
      first.querySelector<HTMLElement>("[data-thread-item-id]")?.getAttribute("data-thread-item-id") ??
      null;
    const rect = first.getBoundingClientRect();
    const scrollerRect = scroller.getBoundingClientRect();
    return {
      atMs: performance.now(),
      rowId,
      rowOffsetTopPx: Number.isFinite(rect.top - scrollerRect.top)
        ? Number((rect.top - scrollerRect.top).toFixed(2))
        : null,
      renderedItemCount: rows.length,
    };
  }, {
    activeSlotSelector: ACTIVE_SLOT_SELECTOR,
    scrollerSelector,
  });
}

export async function forceScrollToBottom(page: Page, scrollerSelector = SCROLLER_SELECTOR): Promise<void> {
  await page.locator(`${ACTIVE_SLOT_SELECTOR} ${scrollerSelector}`).first().evaluate((node) => {
    const element = node as HTMLElement;
    element.scrollTop = Math.max(0, element.scrollHeight - element.clientHeight);
    element.dispatchEvent(new Event("scroll"));
  });
}

export async function forceScrollToDistanceFromBottom(
  page: Page,
  distancePx: number,
  scrollerSelector = SCROLLER_SELECTOR,
): Promise<void> {
  await page
    .locator(`${ACTIVE_SLOT_SELECTOR} ${scrollerSelector}`)
    .first()
    .evaluate((node, requestedDistancePx) => {
      const element = node as HTMLElement;
      const maxScrollTop = Math.max(0, element.scrollHeight - element.clientHeight);
      const distance = Math.max(0, Number(requestedDistancePx) || 0);
      element.scrollTop = Math.max(0, maxScrollTop - distance);
      element.dispatchEvent(new Event("scroll"));
    }, distancePx);
}

export async function forceScrollToTop(page: Page, scrollerSelector = SCROLLER_SELECTOR): Promise<void> {
  await page.locator(`${ACTIVE_SLOT_SELECTOR} ${scrollerSelector}`).first().evaluate((node) => {
    const element = node as HTMLElement;
    element.scrollTop = 0;
    element.dispatchEvent(new Event("scroll"));
  });
}

export async function readRowOffsetById(
  page: Page,
  rowId: string,
  scrollerSelector = SCROLLER_SELECTOR,
): Promise<number | null> {
  return page.evaluate(
    (payload) => {
      const { rowId, selector } = payload;
      const activeSlotSelector = payload.activeSlotSelector as string;
      const toFiniteNumber = (value: number | null | undefined): number | null =>
        typeof value === "number" && Number.isFinite(value) ? Number(value.toFixed(2)) : null;
      const activeSession = document.querySelector<HTMLElement>(activeSlotSelector)?.querySelector<HTMLElement>(
        `[data-testid="session-view"][data-session-id]`,
      );
      const scroller = activeSession?.querySelector<HTMLElement>(selector);
      if (!scroller) return null;
      const row = scroller.querySelector<HTMLElement>(`[data-thread-item-id="${CSS.escape(rowId)}"]`);
      if (!row) return null;
      const scrollerRect = scroller.getBoundingClientRect();
      return toFiniteNumber(row.getBoundingClientRect().top - scrollerRect.top);
    },
    { rowId, selector: scrollerSelector, activeSlotSelector: ACTIVE_SLOT_SELECTOR },
  );
}
