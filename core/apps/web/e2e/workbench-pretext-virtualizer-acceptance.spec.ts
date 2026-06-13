import fs from "fs/promises";
import { test, expect } from "./fixtures";
import {
  collectBottomHitProbe,
  finishOpenProbe,
  installOpenProbe,
  readThreadSurfaceCounts,
  startOpenProbe,
} from "./utils/pretextVirtualizerAcceptanceProbes";
import { recordMeaningfulPaintShifts } from "./utils/recordMeaningfulPaintShifts";
import { resolveAnchorstreamAcceptanceWorkspace } from "./utils/seedPretextVirtualizerAcceptanceWorkspace";
import { SESSION_THREAD_HORIZONTAL_INSET_PX } from "../src/pages/sessionThread/sessionThreadLayoutTokens";
const parseEnvNumber = (value: string | undefined, fallback: number): number => {
  const normalized = String(value ?? "").trim();
  if (normalized.length === 0) return fallback;
  const parsed = Number(normalized);
  return Number.isFinite(parsed) ? parsed : fallback;
};
const OPEN_CAPTURE_MS = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_OPEN_CAPTURE_MS, 3000);
const MAX_OPEN_MS = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_MAX_OPEN_MS, 1500);
const MAX_BLANK_MS = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_MAX_BLANK_MS, 250);
const MAX_SHORT_THREAD_LOWER_BLANK_RATIO = Number(
  parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_MAX_SHORT_THREAD_LOWER_BLANK_RATIO, 0.9),
);
const MAX_SHORT_THREAD_LOWER_BLANK_MS = Number(
  parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_MAX_SHORT_THREAD_LOWER_BLANK_MS, 2000),
);
const MAX_BOTTOM_HIT_DISTANCE_PX = Number(
  parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_MAX_BOTTOM_HIT_DISTANCE_PX, 3),
);
const MAX_BOTTOM_HIT_JITTER_PX = Number(
  parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_MAX_BOTTOM_HIT_JITTER_PX, 4),
);
const MAX_BOTTOM_HIT_LAST_ITEM_JITTER_PX = Number(
  parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_MAX_BOTTOM_HIT_LAST_ITEM_JITTER_PX, 6),
);
const BOTTOM_HIT_SAMPLE_COUNT = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_BOTTOM_HIT_SAMPLE_COUNT, 24);
const BOTTOM_HIT_SAMPLE_SETTLE_MS = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_BOTTOM_HIT_SAMPLE_SETTLE_MS, 55);
const MAX_CLS_NO_INPUT = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_MAX_CLS_NO_INPUT, 0.02);
const MAX_MEANINGFUL_SHIFTS = Number(
  parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_MAX_MEANINGFUL_SHIFTS, 8),
);
const MAX_WIDEST_ROW_COMPOSER_DELTA_PX = Number(
  parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_MAX_WIDEST_ROW_COMPOSER_DELTA_PX, 4),
);
const MAX_WIDEST_ROW_CENTER_DRIFT_PX = Number(
  parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_MAX_WIDEST_ROW_CENTER_DRIFT_PX, 8),
);
const SCROLL_STEP_PX = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_SCROLL_STEP_PX, 320);
const SCROLL_ATTEMPTS = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_SCROLL_ATTEMPTS, 90);
const SCROLL_SETTLE_MS = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_SCROLL_SETTLE_MS, 90);
const SCROLL_HISTORY_SETTLE_MS = parseEnvNumber(
  process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_SCROLL_HISTORY_SETTLE_MS,
  1500,
);
const SCROLL_HISTORY_POLL_MS = parseEnvNumber(
  process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_SCROLL_HISTORY_POLL_MS,
  100,
);
const scrollSelector = ".wb-session-slot[aria-hidden=\"false\"] [data-pretext-virtualizer-list=\"1\"]";

type ScrollSnapshot = {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
};

async function stepScrollUp(page: Parameters<typeof test>[0]["page"], pixels: number) {
  await page.locator(scrollSelector).first().evaluate((element, step) => {
    element.scrollTop = Math.max(0, element.scrollTop - step);
    element.dispatchEvent(new Event("scroll"));
  }, pixels);
}

async function readScrollSnapshot(page: Parameters<typeof test>[0]["page"]): Promise<ScrollSnapshot> {
  return page.locator(scrollSelector).first().evaluate((element) => ({
    scrollTop: element.scrollTop,
    scrollHeight: element.scrollHeight,
    clientHeight: element.clientHeight,
  }));
}

async function waitForEarliestAfterHistoryChange(
  page: Parameters<typeof test>[0]["page"],
  earliestLocator: ReturnType<Parameters<typeof test>[0]["page"]["locator"]>,
): Promise<boolean> {
  const deadline = Date.now() + SCROLL_HISTORY_SETTLE_MS;
  while (Date.now() < deadline) {
    if (await earliestLocator.first().isVisible().catch(() => false)) {
      return true;
    }
    await page.waitForTimeout(SCROLL_HISTORY_POLL_MS);
  }
  return earliestLocator.first().isVisible().catch(() => false);
}

test("workbench: pretextVirtualizer acceptance stays stable on open and reaches earliest content", async ({
  page,
  request,
}, testInfo) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  const target = await resolveAnchorstreamAcceptanceWorkspace(request);

  const targetRow = page.locator(".wb-task-row").filter({ hasText: target.titles.long }).first();
  const earliestLocator = page.locator(".wb-session-slot[aria-hidden=\"false\"]").getByText(target.earliestText, {
    exact: false,
  });

  await page.goto(target.workspacePath, { waitUntil: "domcontentloaded" });
  await expect(targetRow).toBeVisible({ timeout: 20_000 });

  await installOpenProbe(page, scrollSelector);
  await startOpenProbe(page);
  const openMetricsPromise = finishOpenProbe(page, scrollSelector, {
    captureMs: OPEN_CAPTURE_MS,
    sampleMs: 50,
  });

  const captureDir = testInfo.outputPath("pretext-virtualizer-open");
  const paintResult = await recordMeaningfulPaintShifts(
    page,
    async () => {
      await targetRow.click();
      await expect(page.locator(scrollSelector).first()).toBeVisible({ timeout: 20_000 });
      await page.waitForTimeout(OPEN_CAPTURE_MS);
    },
    {
      dir: captureDir,
      pollMs: 100,
      stableForMs: 250,
      timeoutMs: OPEN_CAPTURE_MS + 1500,
      diffPixelRatio: 0.006,
      pixelmatchThreshold: 0.12,
    },
  );

  const openMetrics = await openMetricsPromise;
  const meaningfulShiftCount = Math.max(0, paintResult.frames.length - 1);
  const bottomHitProbe = await collectBottomHitProbe(page, scrollSelector, {
    sampleCount: BOTTOM_HIT_SAMPLE_COUNT,
    settleMs: BOTTOM_HIT_SAMPLE_SETTLE_MS,
  });
  const rowLayout = await page.locator(scrollSelector).first().evaluate((element) => {
    const rect = element.getBoundingClientRect();
    const parentRect = element.parentElement?.getBoundingClientRect() ?? null;
    const stackRect = element.closest(".wb-pretext-thread-stack, .wb-thread-stack")?.getBoundingClientRect() ?? null;
    const sessionLeftRect = element.closest(".wb-session-left")?.getBoundingClientRect() ?? null;
    const sessionViewRect = element.closest(".wb-session-view")?.getBoundingClientRect() ?? null;
    const composerRect =
      element.closest(".wb-session-view")?.querySelector(".wb-active-composer")?.getBoundingClientRect() ?? null;
    const rows = Array.from(
      element.querySelectorAll<HTMLElement>("[data-pretext-virtualizer-row='1'][data-pretext-virtualizer-item-id]"),
    ).map((row) => {
      const rowRect = row.getBoundingClientRect();
      return {
        id: row.getAttribute("data-pretext-virtualizer-item-id") ?? "",
        width: rowRect.width,
        leftGap: rowRect.left - rect.left,
        rightGap: rect.right - rowRect.right,
      };
    });
    const widestRow = rows.reduce<typeof rows[number] | null>(
      (widest, row) => (widest == null || row.width > widest.width ? row : widest),
      null,
    );
    const firstIndent = element.querySelector<HTMLElement>(".wb-thread-indent");
    const firstAssistantEntry = element.querySelector<HTMLElement>(".wb-assistant-entry");
    const firstAssistantBody = element.querySelector<HTMLElement>(".wb-assistant-body");
    return {
      scrollerWidth: rect.width,
      parentWidth: parentRect?.width ?? null,
      stackWidth: stackRect?.width ?? null,
      sessionLeftWidth: sessionLeftRect?.width ?? null,
      sessionViewWidth: sessionViewRect?.width ?? null,
      composerWidth: composerRect?.width ?? null,
      rowCount: rows.length,
      widestRow,
      widestRowWidthRatio: widestRow != null && rect.width > 0 ? widestRow.width / rect.width : null,
      widestRowComposerDeltaPx:
        widestRow != null && composerRect != null ? Math.abs(widestRow.width - composerRect.width) : null,
      widestRowCenterDriftPx:
        widestRow != null ? Math.abs(widestRow.leftGap - widestRow.rightGap) : null,
      threadIndentWidth: firstIndent?.getBoundingClientRect().width ?? null,
      assistantEntryWidth: firstAssistantEntry?.getBoundingClientRect().width ?? null,
      assistantBodyWidth: firstAssistantBody?.getBoundingClientRect().width ?? null,
    };
  });
  const debugEntries = await page.evaluate(() => {
    const store = (window as Window & {
      __wbSessionMessageListDebug?: {
        entries?: Array<{
          cause: string;
          atMs: number;
          loaded: boolean;
          listCount: number;
          scrollTop: number | null;
          clientHeight: number | null;
          scrollHeight: number | null;
          blankTailPx: number | null;
          firstItemId: string | null;
          firstItemTopPx: number | null;
          lastItemId: string | null;
          lastItemBottomPx: number | null;
          lastItemOffscreenAbove: boolean | null;
          impossibleTail: boolean;
          detail: Record<string, unknown> | null;
        }>;
      };
    }).__wbSessionMessageListDebug;
    return (store?.entries ?? []).slice(-40);
  });
  const acceptanceSummary = {
    baseURL: page.url(),
    workspacePath: target.workspacePath,
    taskTitle: target.titles.long,
    earliestText: target.earliestText,
    targetMode: target.mode,
    openMetrics,
    meaningfulShiftCount,
    paintFrames: paintResult.frames,
    paintNetwork: paintResult.network,
    debugEntries,
    bottomHitProbeSummary: {
      sampleCount: bottomHitProbe.sampleCount,
      isShortThread: bottomHitProbe.isShortThread,
      maxDistanceFromBottomPx: bottomHitProbe.maxDistanceFromBottomPx,
      maxJitterPx: bottomHitProbe.maxJitterPx,
      maxLastItemBottomJitterPx: bottomHitProbe.maxLastItemBottomJitterPx,
    },
    rowLayout,
  };

  await testInfo.attach("pretext-virtualizer-open-metrics.json", {
    body: JSON.stringify(acceptanceSummary, null, 2),
    contentType: "application/json",
  });
  await fs.writeFile(
    testInfo.outputPath("pretext-virtualizer-open-metrics.json"),
    JSON.stringify(acceptanceSummary, null, 2),
    "utf8",
  );

  expect(
    openMetrics.firstTopPaintMs,
    "task open never produced visible non-blank upper-thread content",
  ).not.toBeNull();
  expect(
    openMetrics.usableSampleCount,
    "acceptance probe never observed a mounted thread surface",
  ).toBeGreaterThan(0);
  expect(
    Number(openMetrics.firstTopPaintMs),
    `task open exceeded the quick-open budget of ${MAX_OPEN_MS}ms`,
  ).toBeLessThanOrEqual(MAX_OPEN_MS);
  expect(
    openMetrics.topBlankRunMs,
    `thread stayed blank too long after open (budget ${MAX_BLANK_MS}ms)`,
  ).toBeLessThanOrEqual(MAX_BLANK_MS);
  if (openMetrics.isShortThread) {
    expect(
      openMetrics.maxBottomBlankRatio,
      `short-thread lower viewport blankness unexpectedly below threshold ${MAX_SHORT_THREAD_LOWER_BLANK_RATIO}`,
    ).toBeGreaterThanOrEqual(MAX_SHORT_THREAD_LOWER_BLANK_RATIO);
    expect(
      openMetrics.maxBottomBlankRunMs,
      `short-thread lower viewport blank run exceeded ${MAX_SHORT_THREAD_LOWER_BLANK_MS}ms`,
    ).toBeLessThanOrEqual(MAX_SHORT_THREAD_LOWER_BLANK_MS);
  } else {
    expect(
      openMetrics.firstBottomPaintMs,
      "lower surface never painted for a long thread during open",
    ).not.toBeNull();
    expect(
      openMetrics.maxBottomBlankRatio,
      `long-thread lower viewport unexpectedly remained mostly blank (${openMetrics.maxBottomBlankRatio})`,
    ).toBeLessThan(MAX_SHORT_THREAD_LOWER_BLANK_RATIO);
  }
  if (!bottomHitProbe.isShortThread) {
    expect(
      bottomHitProbe.maxDistanceFromBottomPx,
      `bottom anchor drifted too far from bottom (${bottomHitProbe.maxDistanceFromBottomPx}px)`,
    ).toBeLessThanOrEqual(MAX_BOTTOM_HIT_DISTANCE_PX);
    expect(
      bottomHitProbe.maxJitterPx,
      `bottom anchor jittered (${bottomHitProbe.maxJitterPx}px) beyond budget ${MAX_BOTTOM_HIT_JITTER_PX}px`,
    ).toBeLessThanOrEqual(MAX_BOTTOM_HIT_JITTER_PX);
    expect(
      bottomHitProbe.maxLastItemBottomJitterPx,
      `bottom item jittered (${bottomHitProbe.maxLastItemBottomJitterPx}px) beyond budget ${MAX_BOTTOM_HIT_LAST_ITEM_JITTER_PX}px`,
    ).toBeLessThanOrEqual(MAX_BOTTOM_HIT_LAST_ITEM_JITTER_PX);
  }
  expect(
    openMetrics.maxBlankVisiblePoints,
    "top-band open sampling indicates blank thread surface",
  ).toBeLessThan(openMetrics.firstUsableSampledPoints || 15);
  expect(
    openMetrics.clsNoInput,
    `unexpected layout shift without input exceeded ${MAX_CLS_NO_INPUT}`,
  ).toBeLessThanOrEqual(MAX_CLS_NO_INPUT);
  expect(
    meaningfulShiftCount,
    `meaningful paint shifts exceeded ${MAX_MEANINGFUL_SHIFTS}`,
  ).toBeLessThanOrEqual(MAX_MEANINGFUL_SHIFTS);
  expect(rowLayout.rowCount, "acceptance target should render at least one PretextVirtualizer row").toBeGreaterThan(0);
  expect(rowLayout.widestRow, "acceptance target did not produce a measurable widest row").not.toBeNull();
  expect(rowLayout.composerWidth, "acceptance target did not resolve the active composer width").not.toBeNull();
  expect(
    Number(rowLayout.widestRowComposerDeltaPx),
    `PretextVirtualizer widest row drifted ${rowLayout.widestRowComposerDeltaPx}px from the composer width ${rowLayout.composerWidth}px`,
  ).toBeLessThanOrEqual(MAX_WIDEST_ROW_COMPOSER_DELTA_PX);
  expect(
    Number(rowLayout.widestRowCenterDriftPx),
    `PretextVirtualizer widest row drifted ${rowLayout.widestRowCenterDriftPx}px off center`,
  ).toBeLessThanOrEqual(MAX_WIDEST_ROW_CENTER_DRIFT_PX);
  expect(
    Number(rowLayout.widestRowWidthRatio),
    `PretextVirtualizer scroller is too narrow for its centered row contract (ratio ${rowLayout.widestRowWidthRatio})`,
  ).toBeLessThanOrEqual(0.8);
  expect(rowLayout.threadIndentWidth, "acceptance target did not produce a measurable thread indent width").not.toBeNull();
  if (rowLayout.widestRow != null && rowLayout.threadIndentWidth != null) {
    const expectedIndentWidth = rowLayout.widestRow.width - SESSION_THREAD_HORIZONTAL_INSET_PX * 2;
    expect(
      Math.abs(rowLayout.threadIndentWidth - expectedIndentWidth),
      `PretextVirtualizer thread indent width drifted from the shared inset contract: expected ${expectedIndentWidth}px, saw ${rowLayout.threadIndentWidth}px`,
    ).toBeLessThanOrEqual(2);
  }

  const scroller = page.locator(scrollSelector).first();
  await expect(scroller).toBeVisible({ timeout: 20_000 });
  await scroller.hover();

  let minNonBlankVisiblePoints = Number.POSITIVE_INFINITY;
  let minTopNonBlankVisiblePoints = Number.POSITIVE_INFINITY;
  let minBottomNonBlankVisiblePoints = Number.POSITIVE_INFINITY;
  let reachedEarliest = false;
  let stableTopHits = 0;
  let previousSnapshot = await readScrollSnapshot(page);
  for (let attempt = 0; attempt < SCROLL_ATTEMPTS; attempt += 1) {
    await stepScrollUp(page, SCROLL_STEP_PX);
    await page.waitForTimeout(SCROLL_SETTLE_MS);
    const surface = await readThreadSurfaceCounts(page, scrollSelector);
    const snapshot = await readScrollSnapshot(page);
    minNonBlankVisiblePoints = Math.min(
      minNonBlankVisiblePoints,
      surface.totalNonBlankPoints,
    );
    minTopNonBlankVisiblePoints = Math.min(minTopNonBlankVisiblePoints, surface.topNonBlankPoints);
    minBottomNonBlankVisiblePoints = Math.min(minBottomNonBlankVisiblePoints, surface.bottomNonBlankPoints);
    if (await earliestLocator.first().isVisible().catch(() => false)) {
      reachedEarliest = true;
      break;
    }
    const sawHistoryGrowth = snapshot.scrollHeight > previousSnapshot.scrollHeight + 1;
    if ((snapshot.scrollTop <= 1 || sawHistoryGrowth) && (await waitForEarliestAfterHistoryChange(page, earliestLocator))) {
      reachedEarliest = true;
      break;
    }
    const top = snapshot.scrollTop;
    if (top <= 1) {
      stableTopHits += 1;
      if (stableTopHits >= 4) break;
    } else {
      stableTopHits = 0;
    }
    previousSnapshot = snapshot;
  }

  const scrollMetrics = {
    minNonBlankVisiblePoints:
      Number.isFinite(minNonBlankVisiblePoints) ? minNonBlankVisiblePoints : 0,
    minTopNonBlankVisiblePoints:
      Number.isFinite(minTopNonBlankVisiblePoints) ? minTopNonBlankVisiblePoints : 0,
    minBottomNonBlankVisiblePoints:
      Number.isFinite(minBottomNonBlankVisiblePoints) ? minBottomNonBlankVisiblePoints : 0,
    reachedEarliest,
    bottomHitProbeSummary: {
      sampleCount: bottomHitProbe.sampleCount,
      isShortThread: bottomHitProbe.isShortThread,
      maxDistanceFromBottomPx: bottomHitProbe.maxDistanceFromBottomPx,
      maxJitterPx: bottomHitProbe.maxJitterPx,
      maxLastItemBottomJitterPx: bottomHitProbe.maxLastItemBottomJitterPx,
    },
  };

  await testInfo.attach("pretext-virtualizer-scroll-metrics.json", {
    body: JSON.stringify(scrollMetrics, null, 2),
    contentType: "application/json",
  });
  await fs.writeFile(
    testInfo.outputPath("pretext-virtualizer-scroll-metrics.json"),
    JSON.stringify(scrollMetrics, null, 2),
    "utf8",
  );

  expect(
    Number.isFinite(minNonBlankVisiblePoints) ? minNonBlankVisiblePoints : 0,
    "the visible thread surface went fully blank during upward scroll",
  ).toBeGreaterThan(0);
  expect(
    Number.isFinite(minTopNonBlankVisiblePoints) ? minTopNonBlankVisiblePoints : 0,
    "the upper thread surface never painted during upward scroll",
  ).toBeGreaterThan(0);
  if (openMetrics.isShortThread) {
    expect(
      Number.isFinite(minBottomNonBlankVisiblePoints) ? minBottomNonBlankVisiblePoints : 0,
      "short-thread lower band was unexpectedly full-blank during upward scroll",
    ).toBeLessThanOrEqual(1);
  }
  expect(reachedEarliest, "earliest seeded content was not reachable by upward scroll").toBeTruthy();
});
