import type { Page } from "playwright/test";

const SHORT_THREAD_SLACK_PX = 2;
const SAMPLE_X_FRACTIONS = [0.2, 0.5, 0.8] as const;
const SAMPLE_TOP_Y_FRACTIONS = [0.2, 0.35, 0.5] as const;
const SAMPLE_MIDDLE_Y_FRACTIONS = [0.58, 0.7, 0.82] as const;
const SAMPLE_BOTTOM_Y_FRACTIONS = [0.88, 0.94, 0.98] as const;
const PRETEXT_VIRTUALIZER_ROW_SELECTOR = "[data-pretext-virtualizer-row='1'][data-pretext-virtualizer-item-id]";

type OpenProbeShift = {
  startTime: number;
  value: number;
  hadRecentInput: boolean;
};

type OpenProbeSample = {
  t: number;
  sampledPoints: number;
  topNonBlankPoints: number;
  middleNonBlankPoints: number;
  bottomNonBlankPoints: number;
  topBlankPoints: number;
  middleBlankPoints: number;
  bottomBlankPoints: number;
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
  isShortThread: boolean;
};

type OpenProbeState = {
  clickStartedAt: number;
  shifts: OpenProbeShift[];
  stop: () => void;
};

type WindowWithOpenProbe = Window & {
  __pretextVirtualizerAcceptance?: OpenProbeState;
};

export type ThreadSurfaceSample = {
  sampledPoints: number;
  topNonBlankPoints: number;
  middleNonBlankPoints: number;
  bottomNonBlankPoints: number;
  totalNonBlankPoints: number;
  isShortThread: boolean;
  scrollHeight: number;
  clientHeight: number;
  scrollTop: number;
};

export type OpenProbeResult = {
  clickStartedAt: number;
  sampleCount: number;
  usableSampleCount: number;
  firstUsableSampledPoints: number;
  firstTopPaintMs: number | null;
  firstMeaningfulPaintMs: number | null;
  firstBottomPaintMs: number | null;
  topBlankRunMs: number;
  maxBlankVisiblePoints: number;
  maxBottomBlankVisiblePoints: number;
  maxBottomBlankRunMs: number;
  maxBottomNonBlankPoints: number;
  maxBottomBlankRatio: number;
  isShortThread: boolean;
  clsNoInput: number;
  clsTotal: number;
  samples: OpenProbeSample[];
  shifts: OpenProbeShift[];
};

export type BottomHitProbeSample = {
  t: number;
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
  distanceFromBottomPx: number;
  atBottom: boolean;
  snapshotScrollTop: number | null;
  programmaticPending: boolean;
  pendingRestore: boolean;
  renderedFirstIndex: number | null;
  renderedLastIndex: number | null;
  firstItemId: string | null;
  firstItemBottomPx: number | null;
  lastItemId: string | null;
  lastItemBottomPx: number | null;
  shortThread: boolean;
};

export type BottomHitProbeResult = {
  sampleCount: number;
  isShortThread: boolean;
  maxDistanceFromBottomPx: number;
  maxJitterPx: number;
  maxLastItemBottomJitterPx: number;
  samples: BottomHitProbeSample[];
};

export type BottomRehitProbeCycle = {
  cycle: number;
  leftDistanceFromBottomPx: number;
  maxDistanceFromBottomPx: number;
  maxJitterPx: number;
  maxLastItemBottomJitterPx: number;
  maxScrollHeightJitterPx: number;
  samples: BottomHitProbeSample[];
};

export type BottomRehitProbeResult = {
  cycleCount: number;
  sampleCount: number;
  isShortThread: boolean;
  maxDistanceFromBottomPx: number;
  maxJitterPx: number;
  maxLastItemBottomJitterPx: number;
  maxScrollHeightJitterPx: number;
  cycles: BottomRehitProbeCycle[];
};

type BottomProbeReadMode = "observe" | "snap-bottom" | "leave-bottom";

export async function installOpenProbe(page: Page, selector: string): Promise<void> {
  await page.evaluate(
    () => {
      const win = window as WindowWithOpenProbe;
      win.__pretextVirtualizerAcceptance?.stop?.();
      const shifts: OpenProbeState["shifts"] = [];
      let stopped = false;
      let shiftObserver: PerformanceObserver | null = null;

      try {
        shiftObserver = new PerformanceObserver((list) => {
          for (const entry of list.getEntries()) {
            const shift = entry as PerformanceEntry & {
              value?: number;
              hadRecentInput?: boolean;
            };
            shifts.push({
              startTime: shift.startTime,
              value: shift.value ?? 0,
              hadRecentInput: shift.hadRecentInput ?? false,
            });
          }
        });
        shiftObserver.observe({ type: "layout-shift", buffered: true });
      } catch {
        shiftObserver = null;
      }

      win.__pretextVirtualizerAcceptance = {
        clickStartedAt: 0,
        shifts,
        stop: () => {
          stopped = true;
          shiftObserver?.disconnect();
        },
      };
    },
  );
}

export async function startOpenProbe(page: Page): Promise<number> {
  return page.evaluate(() => {
    const win = window as WindowWithOpenProbe;
    if (!win.__pretextVirtualizerAcceptance) {
      throw new Error("pretextVirtualizer acceptance probe is not installed");
    }
    win.__pretextVirtualizerAcceptance.clickStartedAt = performance.now();
    return win.__pretextVirtualizerAcceptance.clickStartedAt;
  });
}

async function collectOpenProbeSample(page: Page, selector: string): Promise<OpenProbeSample> {
  return page.evaluate(
    (config) => {
      const {
        threadSelector,
        sampleXFractions,
        topYFractions,
        middleYFractions,
        bottomYFractions,
        shortThreadSlackPx,
      } = config;
      const scroller = document.querySelector(threadSelector) as HTMLElement | null;
      if (!scroller) {
        return {
          t: performance.now(),
          sampledPoints: 0,
          topNonBlankPoints: 0,
          middleNonBlankPoints: 0,
          bottomNonBlankPoints: 0,
          topBlankPoints: 0,
          middleBlankPoints: 0,
          bottomBlankPoints: 0,
          scrollTop: 0,
          scrollHeight: 0,
          clientHeight: 0,
          isShortThread: false,
        };
      }

      const scrollerRect = scroller.getBoundingClientRect();
      let topNonBlankPoints = 0;
      let middleNonBlankPoints = 0;
      let bottomNonBlankPoints = 0;
      let topBlankPoints = 0;
      let middleBlankPoints = 0;
      let bottomBlankPoints = 0;
      let sampledPoints = 0;

      const sampleBand = (ys: readonly number[], isTop: boolean, isBottom: boolean) => {
        for (const yRatio of ys) {
          const y = scrollerRect.top + scrollerRect.height * yRatio;
          for (const xRatio of sampleXFractions) {
            sampledPoints += 1;
            const hit = document.elementFromPoint(scrollerRect.left + scrollerRect.width * xRatio, y);
            const hitElement = hit instanceof HTMLElement ? hit : null;
            const threadElement =
              hitElement?.closest<HTMLElement>("[data-pretext-virtualizer-item-id], [data-thread-item-id]") ?? hitElement;
            const hasText = (threadElement?.textContent ?? "").trim().length > 0;
            if (isTop) {
              if (hasText) topNonBlankPoints += 1;
              else topBlankPoints += 1;
            } else if (isBottom) {
              if (hasText) bottomNonBlankPoints += 1;
              else bottomBlankPoints += 1;
            } else if (hasText) {
              middleNonBlankPoints += 1;
            } else {
              middleBlankPoints += 1;
            }
          }
        }
      };

      sampleBand(topYFractions, true, false);
      sampleBand(middleYFractions, false, false);
      sampleBand(bottomYFractions, false, true);

      return {
        t: performance.now(),
        sampledPoints,
        topNonBlankPoints,
        middleNonBlankPoints,
        bottomNonBlankPoints,
        topBlankPoints,
        middleBlankPoints,
        bottomBlankPoints,
        scrollTop: scroller.scrollTop,
        scrollHeight: scroller.scrollHeight,
        clientHeight: scroller.clientHeight,
        isShortThread:
          Math.max(0, scroller.scrollHeight - scroller.clientHeight) <= shortThreadSlackPx,
      };
    },
    {
      threadSelector: selector,
      sampleXFractions: SAMPLE_X_FRACTIONS,
      topYFractions: SAMPLE_TOP_Y_FRACTIONS,
      middleYFractions: SAMPLE_MIDDLE_Y_FRACTIONS,
      bottomYFractions: SAMPLE_BOTTOM_Y_FRACTIONS,
      shortThreadSlackPx: SHORT_THREAD_SLACK_PX,
    },
  );
}

function summarizeOpenProbe(
  clickStartedAt: number,
  samples: OpenProbeSample[],
  shifts: OpenProbeShift[],
): OpenProbeResult {
  const afterClick = samples.filter((sample) => sample.t >= clickStartedAt);
  const usableSamples = afterClick.filter((sample) => sample.sampledPoints > 0);
  const hasMeaningful = (sample: OpenProbeSample) =>
    sample.topNonBlankPoints + sample.middleNonBlankPoints + sample.bottomNonBlankPoints > 0;

  const firstTopPaint = usableSamples.find((sample) => sample.topNonBlankPoints > 0) ?? null;
  const firstBottomPaint = usableSamples.find((sample) => sample.bottomNonBlankPoints > 0) ?? null;
  const firstMeaningfulPaint = usableSamples.find((sample) => hasMeaningful(sample)) ?? null;

  const computeBlankRun = (extractor: (sample: OpenProbeSample) => number) => {
    let maxBlankRunMs = 0;
    let currentBlankRunMs = 0;
    for (let index = 1; index < usableSamples.length; index += 1) {
      const prev = usableSamples[index - 1];
      const next = usableSamples[index];
      const delta = Math.max(0, next.t - prev.t);
      if (extractor(next) <= 0) {
        currentBlankRunMs += delta;
        if (currentBlankRunMs > maxBlankRunMs) {
          maxBlankRunMs = currentBlankRunMs;
        }
      } else {
        currentBlankRunMs = 0;
      }
    }
    return maxBlankRunMs;
  };

  const topBlankRunMs = computeBlankRun((sample) => sample.topNonBlankPoints);
  const bottomBlankRunMs = computeBlankRun((sample) => sample.bottomNonBlankPoints);

  const shortThreadSamples = usableSamples.filter((sample) => sample.isShortThread).length;
  const isShortThread = usableSamples.length > 0 && shortThreadSamples / usableSamples.length >= 0.5;

  let maxBlankVisiblePoints = 0;
  let maxBottomBlankVisiblePoints = 0;
  let maxBottomNonBlankPoints = 0;
  let maxBottomBlankRatio = 0;
  for (const sample of usableSamples) {
    const totalBlank = sample.topBlankPoints + sample.middleBlankPoints + sample.bottomBlankPoints;
    const totalBottom = sample.bottomBlankPoints + sample.bottomNonBlankPoints;
    if (totalBlank > maxBlankVisiblePoints) {
      maxBlankVisiblePoints = totalBlank;
    }
    if (sample.bottomBlankPoints > maxBottomBlankVisiblePoints) {
      maxBottomBlankVisiblePoints = sample.bottomBlankPoints;
    }
    if (sample.bottomNonBlankPoints > maxBottomNonBlankPoints) {
      maxBottomNonBlankPoints = sample.bottomNonBlankPoints;
    }
    const bottomRatio = totalBottom === 0 ? 0 : sample.bottomBlankPoints / totalBottom;
    if (bottomRatio > maxBottomBlankRatio) {
      maxBottomBlankRatio = bottomRatio;
    }
  }

  const clsNoInput = shifts
    .filter((entry) => entry.startTime >= clickStartedAt && !entry.hadRecentInput)
    .reduce((sum, entry) => sum + entry.value, 0);
  const clsTotal = shifts
    .filter((entry) => entry.startTime >= clickStartedAt)
    .reduce((sum, entry) => sum + entry.value, 0);
  const firstUsableSampledPoints = usableSamples[0]?.sampledPoints ?? 0;

  return {
    clickStartedAt,
    sampleCount: afterClick.length,
    usableSampleCount: usableSamples.length,
    firstUsableSampledPoints,
    firstTopPaintMs: firstTopPaint ? firstTopPaint.t - clickStartedAt : null,
    firstMeaningfulPaintMs: firstMeaningfulPaint ? firstMeaningfulPaint.t - clickStartedAt : null,
    firstBottomPaintMs: firstBottomPaint ? firstBottomPaint.t - clickStartedAt : null,
    topBlankRunMs,
    maxBlankVisiblePoints,
    maxBottomBlankVisiblePoints,
    maxBottomBlankRunMs: bottomBlankRunMs,
    maxBottomNonBlankPoints,
    maxBottomBlankRatio,
    isShortThread,
    clsNoInput,
    clsTotal,
    samples: usableSamples,
    shifts,
  };
}

export async function finishOpenProbe(
  page: Page,
  selector: string,
  options: { captureMs: number; sampleMs?: number },
): Promise<OpenProbeResult> {
  const sampleMs = Math.max(25, Math.floor(options.sampleMs ?? 50));
  const captureMs = Math.max(sampleMs, Math.floor(options.captureMs));
  const clickStartedAt = await page.evaluate(() => {
    const win = window as WindowWithOpenProbe;
    return Number(win.__pretextVirtualizerAcceptance?.clickStartedAt ?? performance.now());
  });

  const samples: OpenProbeSample[] = [];
  const startedAt = Date.now();
  while (Date.now() - startedAt <= captureMs) {
    samples.push(await collectOpenProbeSample(page, selector));
    await page.waitForTimeout(sampleMs);
  }

  const shifts = await page.evaluate(() => {
    const win = window as WindowWithOpenProbe;
    const state = win.__pretextVirtualizerAcceptance;
    state?.stop();
    return Array.isArray(state?.shifts) ? state.shifts : [];
  });

  return summarizeOpenProbe(clickStartedAt, samples, shifts);
}

export async function readThreadSurfaceCounts(page: Page, selector: string): Promise<ThreadSurfaceSample> {
  return page.evaluate((config) => {
    const {
      threadSelector,
      sampleXFractions,
      topYFractions,
      middleYFractions,
      bottomYFractions,
      shortThreadSlackPx,
    } = config;
    const scroller = document.querySelector(threadSelector) as HTMLElement | null;
    if (!scroller) {
      return {
        sampledPoints: 0,
        topNonBlankPoints: 0,
        middleNonBlankPoints: 0,
        bottomNonBlankPoints: 0,
        totalNonBlankPoints: 0,
        isShortThread: true,
        scrollHeight: 0,
        clientHeight: 0,
        scrollTop: 0,
      };
    }

    const scrollerRect = scroller.getBoundingClientRect();
    let topNonBlankPoints = 0;
    let middleNonBlankPoints = 0;
    let bottomNonBlankPoints = 0;
    let topBlankPoints = 0;
    let middleBlankPoints = 0;
    let bottomBlankPoints = 0;
    let sampledPoints = 0;

    const sampleBand = (ys: readonly number[], isTop: boolean, isBottom: boolean) => {
      for (const yRatio of ys) {
        const y = scrollerRect.top + scrollerRect.height * yRatio;
        for (const xRatio of sampleXFractions) {
          sampledPoints += 1;
          const hit = document.elementFromPoint(scrollerRect.left + scrollerRect.width * xRatio, y);
          const hitElement = hit instanceof HTMLElement ? hit : null;
          const threadElement =
            hitElement?.closest<HTMLElement>("[data-pretext-virtualizer-item-id], [data-thread-item-id]") ?? hitElement;
          const hasText = (threadElement?.textContent ?? "").trim().length > 0;
          if (isTop) {
            if (hasText) topNonBlankPoints += 1;
            else topBlankPoints += 1;
          } else if (isBottom) {
            if (hasText) bottomNonBlankPoints += 1;
            else bottomBlankPoints += 1;
          } else if (hasText) {
            middleNonBlankPoints += 1;
          } else {
            middleBlankPoints += 1;
          }
        }
      }
    };
    sampleBand(topYFractions, true, false);
    sampleBand(middleYFractions, false, false);
    sampleBand(bottomYFractions, false, true);

    return {
      sampledPoints,
      topNonBlankPoints,
      middleNonBlankPoints,
      bottomNonBlankPoints,
      totalNonBlankPoints: topNonBlankPoints + middleNonBlankPoints + bottomNonBlankPoints,
      isShortThread:
        Math.max(0, scroller.scrollHeight - scroller.clientHeight) <= shortThreadSlackPx,
      scrollHeight: scroller.scrollHeight,
      clientHeight: scroller.clientHeight,
      scrollTop: scroller.scrollTop,
    };
  }, {
    threadSelector: selector,
    sampleXFractions: SAMPLE_X_FRACTIONS,
    topYFractions: SAMPLE_TOP_Y_FRACTIONS,
    middleYFractions: SAMPLE_MIDDLE_Y_FRACTIONS,
    bottomYFractions: SAMPLE_BOTTOM_Y_FRACTIONS,
    shortThreadSlackPx: SHORT_THREAD_SLACK_PX,
  });
}

export async function collectBottomHitProbe(
  page: Page,
  selector: string,
  options: {
    sampleCount?: number;
    settleMs?: number;
  } = {},
): Promise<BottomHitProbeResult> {
  const sampleCount = Math.max(3, Math.floor(options.sampleCount ?? 20));
  const settleMs = Math.max(0, Math.floor(options.settleMs ?? 40));
  const samples: BottomHitProbeSample[] = [];

  for (let attempt = 0; attempt < sampleCount; attempt += 1) {
    const sample = await page.evaluate((config) => {
      const { threadSelector, shortThreadSlackPx } = config;
      const parseOptionalInt = (value: string | undefined): number | null => {
        const parsed = Number.parseInt(value ?? "", 10);
        return Number.isFinite(parsed) ? parsed : null;
      };
      const scroller = document.querySelector(threadSelector) as HTMLElement | null;
      if (!scroller) {
        return {
          t: performance.now(),
          scrollTop: 0,
          scrollHeight: 0,
          clientHeight: 0,
          distanceFromBottomPx: 0,
          atBottom: false,
          snapshotScrollTop: null,
          programmaticPending: false,
          pendingRestore: false,
          renderedFirstIndex: null,
          renderedLastIndex: null,
          firstItemId: null,
          firstItemBottomPx: null,
          lastItemId: null,
          lastItemBottomPx: null,
          shortThread: false,
        };
      }

      const scrollHeight = scroller.scrollHeight;
      const clientHeight = scroller.clientHeight;
      const maxScrollTop = Math.max(0, scrollHeight - clientHeight);
      scroller.scrollTop = maxScrollTop;
      scroller.dispatchEvent(new Event("scroll"));

      const listItems = Array.from(scroller.querySelectorAll<HTMLElement>(config.rowSelector));
      const firstItem = listItems[0] ?? null;
      const lastItem = listItems[listItems.length - 1] ?? null;
      const firstItemRect = firstItem?.getBoundingClientRect() ?? null;
      const lastItemRect = lastItem?.getBoundingClientRect() ?? null;

      const firstItemId = firstItem?.getAttribute("data-pretext-virtualizer-item-id") ?? null;
      const lastItemId = lastItem?.getAttribute("data-pretext-virtualizer-item-id") ?? null;

      return {
        t: performance.now(),
        scrollTop: scroller.scrollTop,
        scrollHeight,
        clientHeight,
        distanceFromBottomPx: maxScrollTop - scroller.scrollTop,
        atBottom: Math.abs(maxScrollTop - scroller.scrollTop) <= 1,
        snapshotScrollTop: parseOptionalInt(scroller.dataset.pretextVirtualizerSnapshotScrollTop),
        programmaticPending: scroller.dataset.pretextVirtualizerProgrammaticPending === "1",
        pendingRestore: scroller.dataset.pretextVirtualizerPendingRestore === "1",
        renderedFirstIndex: parseOptionalInt(scroller.dataset.pretextVirtualizerRenderedFirstIndex),
        renderedLastIndex: parseOptionalInt(scroller.dataset.pretextVirtualizerRenderedLastIndex),
        firstItemId,
        firstItemBottomPx: firstItemRect ? firstItemRect.bottom : null,
        lastItemId,
        lastItemBottomPx: lastItemRect ? lastItemRect.bottom : null,
        shortThread: maxScrollTop <= shortThreadSlackPx,
      };
    }, {
      threadSelector: selector,
      shortThreadSlackPx: SHORT_THREAD_SLACK_PX,
      rowSelector: PRETEXT_VIRTUALIZER_ROW_SELECTOR,
    });
    samples.push(sample);
    if (attempt < sampleCount - 1) {
      await page.waitForTimeout(settleMs);
    }
  }

  const isShortThread = samples.some((sample) => sample.shortThread);
  let maxDistanceFromBottomPx = 0;
  let maxJitterPx = 0;
  let maxLastItemBottomJitterPx = 0;

  for (let index = 1; index < samples.length; index += 1) {
    const prev = samples[index - 1];
    const next = samples[index];
    const jitter = Math.abs(next.distanceFromBottomPx - prev.distanceFromBottomPx);
    if (jitter > maxJitterPx) {
      maxJitterPx = jitter;
    }
    const absoluteDistance = Math.abs(next.distanceFromBottomPx);
    if (absoluteDistance > maxDistanceFromBottomPx) {
      maxDistanceFromBottomPx = absoluteDistance;
    }
    if (next.lastItemBottomPx != null && prev.lastItemBottomPx != null) {
      const lastItemBottomDelta = Math.abs(next.lastItemBottomPx - prev.lastItemBottomPx);
      if (lastItemBottomDelta > maxLastItemBottomJitterPx) {
        maxLastItemBottomJitterPx = lastItemBottomDelta;
      }
    }
  }

  if (samples.length > 0) {
    const firstDistance = Math.abs(samples[0]?.distanceFromBottomPx ?? 0);
    if (firstDistance > maxDistanceFromBottomPx) {
      maxDistanceFromBottomPx = firstDistance;
    }
  }

  return {
    sampleCount: samples.length,
    isShortThread,
    maxDistanceFromBottomPx,
    maxJitterPx,
    maxLastItemBottomJitterPx,
    samples,
  };
}

async function readBottomProbeSample(
  page: Page,
  selector: string,
  mode: BottomProbeReadMode,
  leaveBottomPx = 0,
): Promise<BottomHitProbeSample> {
  return page.evaluate((config) => {
    const { threadSelector, shortThreadSlackPx, readMode, leaveDistancePx } = config;
    const parseOptionalInt = (value: string | undefined): number | null => {
      const parsed = Number.parseInt(value ?? "", 10);
      return Number.isFinite(parsed) ? parsed : null;
    };
    const scroller = document.querySelector(threadSelector) as HTMLElement | null;
    if (!scroller) {
      return {
        t: performance.now(),
        scrollTop: 0,
        scrollHeight: 0,
        clientHeight: 0,
        distanceFromBottomPx: 0,
        atBottom: false,
        snapshotScrollTop: null,
        programmaticPending: false,
        pendingRestore: false,
        renderedFirstIndex: null,
        renderedLastIndex: null,
        firstItemId: null,
        firstItemBottomPx: null,
        lastItemId: null,
        lastItemBottomPx: null,
        shortThread: false,
      };
    }

    const scrollHeight = scroller.scrollHeight;
    const clientHeight = scroller.clientHeight;
    const maxScrollTop = Math.max(0, scrollHeight - clientHeight);
    if (readMode === "snap-bottom") {
      scroller.scrollTop = maxScrollTop;
      scroller.dispatchEvent(new Event("scroll"));
    } else if (readMode === "leave-bottom") {
      scroller.scrollTop = Math.max(0, maxScrollTop - leaveDistancePx);
      scroller.dispatchEvent(new Event("scroll"));
    }

    const listItems = Array.from(scroller.querySelectorAll<HTMLElement>(config.rowSelector));
    const firstItem = listItems[0] ?? null;
    const lastItem = listItems[listItems.length - 1] ?? null;
    const firstItemRect = firstItem?.getBoundingClientRect() ?? null;
    const lastItemRect = lastItem?.getBoundingClientRect() ?? null;

    const firstItemId = firstItem?.getAttribute("data-pretext-virtualizer-item-id") ?? null;
    const lastItemId = lastItem?.getAttribute("data-pretext-virtualizer-item-id") ?? null;

    return {
      t: performance.now(),
      scrollTop: scroller.scrollTop,
      scrollHeight: scroller.scrollHeight,
      clientHeight: scroller.clientHeight,
      distanceFromBottomPx: Math.max(0, maxScrollTop - scroller.scrollTop),
      atBottom: Math.abs(maxScrollTop - scroller.scrollTop) <= 1,
      snapshotScrollTop: parseOptionalInt(scroller.dataset.pretextVirtualizerSnapshotScrollTop),
      programmaticPending: scroller.dataset.pretextVirtualizerProgrammaticPending === "1",
      pendingRestore: scroller.dataset.pretextVirtualizerPendingRestore === "1",
      renderedFirstIndex: parseOptionalInt(scroller.dataset.pretextVirtualizerRenderedFirstIndex),
      renderedLastIndex: parseOptionalInt(scroller.dataset.pretextVirtualizerRenderedLastIndex),
      firstItemId,
      firstItemBottomPx: firstItemRect ? firstItemRect.bottom : null,
      lastItemId,
      lastItemBottomPx: lastItemRect ? lastItemRect.bottom : null,
      shortThread: maxScrollTop <= shortThreadSlackPx,
    };
  }, {
    threadSelector: selector,
    shortThreadSlackPx: SHORT_THREAD_SLACK_PX,
    readMode: mode,
    leaveDistancePx: leaveBottomPx,
    rowSelector: PRETEXT_VIRTUALIZER_ROW_SELECTOR,
  });
}

export async function collectBottomRehitProbe(
  page: Page,
  selector: string,
  options: {
    cycleCount?: number;
    leaveBottomPx?: number;
    sampleCount?: number;
    settleMs?: number;
  } = {},
): Promise<BottomRehitProbeResult> {
  const cycleCount = Math.max(1, Math.floor(options.cycleCount ?? 6));
  const leaveBottomPx = Math.max(24, Math.floor(options.leaveBottomPx ?? 1600));
  const sampleCount = Math.max(3, Math.floor(options.sampleCount ?? 12));
  const settleMs = Math.max(0, Math.floor(options.settleMs ?? 55));
  const cycles: BottomRehitProbeCycle[] = [];
  let isShortThread = false;
  let maxDistanceFromBottomPx = 0;
  let maxJitterPx = 0;
  let maxLastItemBottomJitterPx = 0;
  let maxScrollHeightJitterPx = 0;

  for (let cycle = 0; cycle < cycleCount; cycle += 1) {
    const leftSample = await readBottomProbeSample(page, selector, "leave-bottom", leaveBottomPx);
    isShortThread ||= leftSample.shortThread;
    await page.waitForTimeout(settleMs);
    await readBottomProbeSample(page, selector, "snap-bottom");

    const samples: BottomHitProbeSample[] = [];
    for (let attempt = 0; attempt < sampleCount; attempt += 1) {
      samples.push(await readBottomProbeSample(page, selector, "observe"));
      if (attempt < sampleCount - 1) {
        await page.waitForTimeout(settleMs);
      }
    }

    let cycleMaxDistance = 0;
    let cycleMaxJitter = 0;
    let cycleMaxLastItemBottomJitter = 0;
    let cycleMaxScrollHeightJitter = 0;
    for (let index = 0; index < samples.length; index += 1) {
      const sample = samples[index];
      cycleMaxDistance = Math.max(cycleMaxDistance, Math.abs(sample.distanceFromBottomPx));
      if (index === 0) continue;
      const prev = samples[index - 1];
      cycleMaxJitter = Math.max(
        cycleMaxJitter,
        Math.abs(sample.distanceFromBottomPx - prev.distanceFromBottomPx),
      );
      cycleMaxScrollHeightJitter = Math.max(
        cycleMaxScrollHeightJitter,
        Math.abs(sample.scrollHeight - prev.scrollHeight),
      );
      if (sample.lastItemBottomPx != null && prev.lastItemBottomPx != null) {
        cycleMaxLastItemBottomJitter = Math.max(
          cycleMaxLastItemBottomJitter,
          Math.abs(sample.lastItemBottomPx - prev.lastItemBottomPx),
        );
      }
    }

    maxDistanceFromBottomPx = Math.max(maxDistanceFromBottomPx, cycleMaxDistance);
    maxJitterPx = Math.max(maxJitterPx, cycleMaxJitter);
    maxLastItemBottomJitterPx = Math.max(maxLastItemBottomJitterPx, cycleMaxLastItemBottomJitter);
    maxScrollHeightJitterPx = Math.max(maxScrollHeightJitterPx, cycleMaxScrollHeightJitter);

    cycles.push({
      cycle,
      leftDistanceFromBottomPx: leftSample.distanceFromBottomPx,
      maxDistanceFromBottomPx: cycleMaxDistance,
      maxJitterPx: cycleMaxJitter,
      maxLastItemBottomJitterPx: cycleMaxLastItemBottomJitter,
      maxScrollHeightJitterPx: cycleMaxScrollHeightJitter,
      samples,
    });
  }

  return {
    cycleCount: cycles.length,
    sampleCount,
    isShortThread,
    maxDistanceFromBottomPx,
    maxJitterPx,
    maxLastItemBottomJitterPx,
    maxScrollHeightJitterPx,
    cycles,
  };
}
