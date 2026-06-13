import { test, expect } from "./fixtures";
import type { Page, TestInfo } from "@playwright/test";
import {
  collectThreadSamples,
  forceScrollToBottom,
  forceScrollToDistanceFromBottom,
  readThreadSurfaceSample,
  type ThreadSurfaceSample,
} from "./utils/messageListAcceptanceProbes";
import { clearMessageListDebugStore, readMessageListDebugStore } from "./utils/taskOpenHistoryRegression";

const workspaceId = process.env.MESSAGE_LIST_WORKSPACE_ID ?? "3d6ade3f-f141-4e64-8156-7f746879decf";
const workspaceToken =
  process.env.MESSAGE_LIST_WORKSPACE_TOKEN ??
  process.env.CTX_E2E_AUTH_TOKEN ??
  process.env.MESSAGE_LIST_AUTH_TOKEN ??
  "74978489-8632-45bb-b60f-aa01a288c84e";
const scrollSelector = ".wb-thread-scroller";
const viewportWidth = Math.max(1200, Number(process.env.MESSAGE_LIST_BOTTOM_HIT_VIEWPORT_WIDTH ?? "1440") || 1440);
const viewportHeight = Math.max(700, Number(process.env.MESSAGE_LIST_BOTTOM_HIT_VIEWPORT_HEIGHT ?? "960") || 960);
const sampleIntervalMs = Math.max(40, Number(process.env.MESSAGE_LIST_BOTTOM_HIT_SAMPLE_INTERVAL_MS ?? "60") || 60);
const approachSampleDurationMs = Math.max(
  sampleIntervalMs,
  Number(process.env.MESSAGE_LIST_BOTTOM_HIT_APPROACH_SAMPLE_MS ?? "220") || 220,
);
const postHitSampleDurationMs = Math.max(
  sampleIntervalMs,
  Number(process.env.MESSAGE_LIST_BOTTOM_HIT_POST_SAMPLE_MS ?? "420") || 420,
);
const bottomHitCycles = Math.max(3, Number(process.env.MESSAGE_LIST_BOTTOM_HIT_CYCLES ?? "6") || 6);
const nearBottomDistancePx = Math.max(48, Number(process.env.MESSAGE_LIST_BOTTOM_HIT_NEAR_BOTTOM_PX ?? "160") || 160);
const minNearBottomDistancePx = Math.max(32, Math.min(nearBottomDistancePx - 24, 96));
const maxBottomDistancePx = Math.max(2, Number(process.env.MESSAGE_LIST_MAX_BOTTOM_DISTANCE_PX ?? "4") || 4);
const maxBlankTailPx = Math.max(24, Number(process.env.MESSAGE_LIST_BOTTOM_HIT_MAX_BLANK_TAIL_PX ?? "64") || 64);
const maxBlankTailGrowthPx = Math.max(
  8,
  Number(process.env.MESSAGE_LIST_BOTTOM_HIT_MAX_BLANK_TAIL_GROWTH_PX ?? "24") || 24,
);
const maxScrollDriftPx = Math.max(2, Number(process.env.MESSAGE_LIST_BOTTOM_HIT_MAX_SCROLL_DRIFT_PX ?? "4") || 4);
const maxTaskProbeCount = Math.max(1, Number(process.env.MESSAGE_LIST_BOTTOM_HIT_TASK_PROBE_COUNT ?? "6") || 6);

type BottomHitCycleSummary = {
  cycle: number;
  approachDistanceFromMaxScrollPx: number | null;
  approachVisibleRowCount: number;
  finalDistanceFromMaxScrollPx: number | null;
  finalBlankTailPx: number | null;
  finalVisibleRowCount: number;
  maxDistanceFromBottomPx: number;
  maxBlankTailPx: number;
  blankTailGrowthPx: number;
  maxScrollDriftPx: number;
  overlappingVisiblePairs: number;
  maxAdjacentVisibleOverlapPx: number;
  scrollDirectionChanges: number;
  newFlashTraceCount: number;
  snapbackDetected: boolean;
};

type LongTaskSelection = {
  taskIndex: number;
  taskLabel: string;
  initialBottom: ThreadSurfaceSample;
};

function asFiniteNumbers(values: Array<number | null | undefined>): number[] {
  return values.filter((value): value is number => typeof value === "number" && Number.isFinite(value));
}

function maxOrZero(values: Array<number | null | undefined>): number {
  const finiteValues = asFiniteNumbers(values);
  return finiteValues.length > 0 ? Math.max(...finiteValues) : 0;
}

function countDirectionChanges(values: number[]): number {
  let changes = 0;
  let lastSign = 0;
  for (const value of values) {
    if (Math.abs(value) < 1) continue;
    const sign = Math.sign(value);
    if (sign === 0) continue;
    if (lastSign !== 0 && sign !== lastSign) changes += 1;
    lastSign = sign;
  }
  return changes;
}

function summarizeBottomHitCycle(cycle: number, samples: ThreadSurfaceSample[], newTraceCount: number, snapbackDetected: boolean): BottomHitCycleSummary {
  const final = samples.at(-1);
  const blankTails = asFiniteNumbers(samples.map((sample) => sample.blankTailPx));
  const baselineScrollTop = samples[0]?.scrollTop ?? null;
  const scrollDeltas =
    baselineScrollTop == null
      ? []
      : asFiniteNumbers(samples.map((sample) => (sample.scrollTop == null ? null : sample.scrollTop - baselineScrollTop)));
  const maxBlankTail = blankTails.length > 0 ? Math.max(...blankTails) : 0;
  const minBlankTail = blankTails.length > 0 ? Math.min(...blankTails) : 0;

  return {
    cycle,
    approachDistanceFromMaxScrollPx: samples[0]?.distanceFromMaxScrollPx ?? null,
    approachVisibleRowCount: samples[0]?.visibleRowCount ?? 0,
    finalDistanceFromMaxScrollPx: final?.distanceFromMaxScrollPx ?? null,
    finalBlankTailPx: final?.blankTailPx ?? null,
    finalVisibleRowCount: final?.visibleRowCount ?? 0,
    maxDistanceFromBottomPx: maxOrZero(samples.map((sample) => sample.distanceFromMaxScrollPx)),
    maxBlankTailPx: maxBlankTail,
    blankTailGrowthPx: Math.max(0, maxBlankTail - minBlankTail),
    maxScrollDriftPx: maxOrZero(scrollDeltas),
    overlappingVisiblePairs: Math.max(...samples.map((sample) => sample.overlappingVisiblePairs ?? 0)),
    maxAdjacentVisibleOverlapPx: maxOrZero(samples.map((sample) => sample.maxAdjacentVisibleOverlapPx)),
    scrollDirectionChanges: countDirectionChanges(scrollDeltas),
    newFlashTraceCount: newTraceCount,
    snapbackDetected,
  };
}

async function attachJson(testInfo: TestInfo, name: string, payload: unknown) {
  await testInfo.attach(`${name}.json`, {
    body: JSON.stringify(payload, null, 2),
    contentType: "application/json",
  });
}

async function seedAcceptanceToken(page: Page) {
  await page.addInitScript((token: string) => {
    const raw = window.sessionStorage.getItem("ctxDaemonConnectionV1");
    let current: Record<string, unknown> = {};
    if (raw) {
      try {
        current = JSON.parse(raw) as Record<string, unknown>;
      } catch {
        current = {};
      }
    }
    window.sessionStorage.setItem(
      "ctxDaemonConnectionV1",
      JSON.stringify({
        ...current,
        v: 1,
        authToken: token,
      }),
    );
  }, workspaceToken);
}

async function openTaskRow(page: Page, taskIndex: number): Promise<void> {
  const row = page.locator(".wb-task-row").nth(taskIndex);
  await row.click();
  await expect(page.locator("textarea.wb-active-textarea")).toBeVisible({ timeout: 30_000 });
  await expect(page.locator(scrollSelector).first()).toBeVisible({ timeout: 30_000 });
}

async function selectLongTask(page: Page): Promise<LongTaskSelection> {
  const taskRows = page.locator(".wb-task-row");
  await expect(taskRows.first()).toBeVisible({ timeout: 20_000 });
  const taskCount = await taskRows.count();
  const probeCount = Math.min(taskCount, maxTaskProbeCount);
  let bestCandidate: LongTaskSelection | null = null;

  for (let taskIndex = 0; taskIndex < probeCount; taskIndex += 1) {
    const taskLabel = ((await taskRows.nth(taskIndex).textContent()) ?? "").trim();
    await openTaskRow(page, taskIndex);
    await forceScrollToBottom(page, scrollSelector);
    const initialBottom = await readThreadSurfaceSample(page, scrollSelector);
    const scrollRange = Math.max(0, (initialBottom.scrollHeight ?? 0) - (initialBottom.clientHeight ?? 0));
    const candidate = { taskIndex, taskLabel, initialBottom };
    if (scrollRange > nearBottomDistancePx * 2 && initialBottom.visibleRowCount > 2) {
      return candidate;
    }
    if (
      !bestCandidate ||
      scrollRange >
        Math.max(
          0,
          (bestCandidate.initialBottom.scrollHeight ?? 0) - (bestCandidate.initialBottom.clientHeight ?? 0),
        )
    ) {
      bestCandidate = candidate;
    }
  }

  if (bestCandidate) return bestCandidate;
  throw new Error("no task rows were available for bottom-hit acceptance");
}

test("message list: repeated bottom re-hit stays pinned with no overlap or snapback", async ({ page }, testInfo) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: viewportWidth, height: viewportHeight });
  await seedAcceptanceToken(page);

  const query = new URLSearchParams({ token: workspaceToken, debug: "1" }).toString();
  await page.goto(`/workspaces/${workspaceId}?${query}`, {
    waitUntil: "domcontentloaded",
    timeout: 30_000,
  });

  const selection = await selectLongTask(page);
  const initialBottom = selection.initialBottom;
  expect((initialBottom.scrollHeight ?? 0) - (initialBottom.clientHeight ?? 0), "long-task scroll range").toBeGreaterThan(
    nearBottomDistancePx * 2,
  );
  expect(initialBottom.distanceFromMaxScrollPx ?? Number.POSITIVE_INFINITY, "initial bottom distance").toBeLessThanOrEqual(
    maxBottomDistancePx,
  );
  expect(initialBottom.visibleRowCount, "initial visible rows").toBeGreaterThan(2);
  expect(initialBottom.overlappingVisiblePairs, "initial overlap pairs").toBe(0);

  await attachJson(testInfo, "bottom-hit-setup", {
    taskIndex: selection.taskIndex,
    taskLabel: selection.taskLabel,
    initialBottom,
  });

  await clearMessageListDebugStore(page);

  const cycleSummaries: BottomHitCycleSummary[] = [];
  const cycleArtifacts: Array<{
    cycle: number;
    approachSamples: ThreadSurfaceSample[];
    hitSamples: ThreadSurfaceSample[];
  }> = [];

  for (let cycle = 0; cycle < bottomHitCycles; cycle += 1) {
    await test.step(`bottom-hit-cycle-${cycle + 1}`, async () => {
      await forceScrollToDistanceFromBottom(page, nearBottomDistancePx, scrollSelector);
      const approachSamples = await collectThreadSamples(page, {
        scrollerSelector: scrollSelector,
        sampleDurationMs: approachSampleDurationMs,
        sampleIntervalMs,
      });
      const approach = approachSamples.at(-1);
      expect(approach?.distanceFromMaxScrollPx ?? 0, `cycle ${cycle + 1}: moved off bottom before re-hit`).toBeGreaterThanOrEqual(
        minNearBottomDistancePx,
      );
      expect(approach?.visibleRowCount ?? 0, `cycle ${cycle + 1}: rows stay visible near bottom`).toBeGreaterThan(0);

      const traceCountBefore = (await readMessageListDebugStore(page)).flashTraces.length;
      await forceScrollToBottom(page, scrollSelector);
      const hitSamples = await collectThreadSamples(page, {
        scrollerSelector: scrollSelector,
        sampleDurationMs: postHitSampleDurationMs,
        sampleIntervalMs,
      });
      const newTraces = (await readMessageListDebugStore(page)).flashTraces.slice(traceCountBefore);
      const summary = summarizeBottomHitCycle(
        cycle + 1,
        hitSamples,
        newTraces.length,
        newTraces.some((trace) => Boolean(trace.snapbackDetected)),
      );

      cycleSummaries.push(summary);
      cycleArtifacts.push({ cycle: cycle + 1, approachSamples, hitSamples });

      expect(summary.maxDistanceFromBottomPx, `cycle ${cycle + 1}: distance from bottom after re-hit`).toBeLessThanOrEqual(
        maxBottomDistancePx,
      );
      expect(summary.maxBlankTailPx, `cycle ${cycle + 1}: blank tail stays bounded`).toBeLessThanOrEqual(maxBlankTailPx);
      expect(summary.blankTailGrowthPx, `cycle ${cycle + 1}: blank tail does not grow after re-hit`).toBeLessThanOrEqual(
        maxBlankTailGrowthPx,
      );
      expect(summary.overlappingVisiblePairs, `cycle ${cycle + 1}: no visible overlap pairs`).toBe(0);
      expect(summary.maxAdjacentVisibleOverlapPx, `cycle ${cycle + 1}: no visible overlap`).toBeLessThanOrEqual(1);
      expect(summary.maxScrollDriftPx, `cycle ${cycle + 1}: no post-hit scroll drift`).toBeLessThanOrEqual(maxScrollDriftPx);
      expect(summary.scrollDirectionChanges, `cycle ${cycle + 1}: no post-hit oscillation`).toBe(0);
      expect(summary.snapbackDetected, `cycle ${cycle + 1}: no debug snapback traces`).toBeFalsy();
      expect(summary.finalVisibleRowCount, `cycle ${cycle + 1}: viewport remains populated`).toBeGreaterThan(0);
    });
  }

  await attachJson(testInfo, "bottom-hit-cycle-summaries", cycleSummaries);
  await attachJson(testInfo, "bottom-hit-cycle-samples", cycleArtifacts);
});
