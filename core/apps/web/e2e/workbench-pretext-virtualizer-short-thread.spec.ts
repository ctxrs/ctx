import fs from "fs/promises";
import { test, expect } from "./fixtures";
import {
  collectBottomHitProbe,
  finishOpenProbe,
  installOpenProbe,
  readThreadSurfaceCounts,
  startOpenProbe,
} from "./utils/pretextVirtualizerAcceptanceProbes";
import { resolveAnchorstreamAcceptanceWorkspace } from "./utils/seedPretextVirtualizerAcceptanceWorkspace";

const parseEnvNumber = (value: string | undefined, fallback: number): number => {
  const normalized = String(value ?? "").trim();
  if (normalized.length === 0) return fallback;
  const parsed = Number(normalized);
  return Number.isFinite(parsed) ? parsed : fallback;
};

const OPEN_CAPTURE_MS = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_SHORT_THREAD_OPEN_CAPTURE_MS, 1500);
const MAX_OPEN_MS = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_SHORT_THREAD_MAX_OPEN_MS, 1500);
const MAX_BLANK_MS = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_SHORT_THREAD_MAX_BLANK_MS, 250);
const MAX_SHORT_SCROLL_RANGE_PX = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_SHORT_THREAD_MAX_SCROLL_RANGE_PX, 2);
const MAX_ALIGNMENT_GAP_PX = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_SHORT_THREAD_MAX_ALIGNMENT_GAP_PX, 32);
const scrollSelector = ".wb-session-slot[aria-hidden=\"false\"] [data-pretext-virtualizer-list=\"1\"]";

test("workbench: pretextVirtualizer short-thread open stays visible and bottom-aligned", async ({
  page,
  request,
}, testInfo) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  const target = await resolveAnchorstreamAcceptanceWorkspace(request);
  await page.goto(target.workspacePath, { waitUntil: "domcontentloaded" });

  const row = page.locator(".wb-task-row").filter({ hasText: target.titles.short }).first();
  await expect(row).toBeVisible({ timeout: 20_000 });

  await installOpenProbe(page, scrollSelector);
  await startOpenProbe(page);
  const openMetricsPromise = finishOpenProbe(page, scrollSelector, {
    captureMs: OPEN_CAPTURE_MS,
    sampleMs: 50,
  });

  await row.click();
  const scroller = page.locator(scrollSelector).first();
  await expect(scroller).toBeVisible({ timeout: 20_000 });
  await page.waitForTimeout(OPEN_CAPTURE_MS);

  const openMetrics = await openMetricsPromise;
  const surface = await readThreadSurfaceCounts(page, scrollSelector);
  const layout = await scroller.evaluate((element) => {
    const rect = element.getBoundingClientRect();
    const rows = Array.from(
      element.querySelectorAll<HTMLElement>("[data-pretext-virtualizer-row='1'][data-pretext-virtualizer-item-id]"),
    );
    const lastRow = rows[rows.length - 1] ?? null;
    const lastRowRect = lastRow?.getBoundingClientRect() ?? null;
    return {
      scrollTop: element.scrollTop,
      scrollHeight: element.scrollHeight,
      clientHeight: element.clientHeight,
      maxScrollTop: Math.max(0, element.scrollHeight - element.clientHeight),
      renderedFirstIndex: Number(element.getAttribute("data-pretext-virtualizer-rendered-first-index") ?? -1),
      renderedLastIndex: Number(element.getAttribute("data-pretext-virtualizer-rendered-last-index") ?? -1),
      snapshotFirstIndex: Number(element.getAttribute("data-pretext-virtualizer-snapshot-first-index") ?? -1),
      snapshotLastIndex: Number(element.getAttribute("data-pretext-virtualizer-snapshot-last-index") ?? -1),
      lastRowBottomGapPx: lastRowRect ? Math.max(0, rect.bottom - lastRowRect.bottom) : null,
      rowCount: rows.length,
    };
  });
  const bottomHitProbe = await collectBottomHitProbe(page, scrollSelector, {
    sampleCount: 12,
    settleMs: 40,
  });

  const summary = {
    workspacePath: target.workspacePath,
    taskTitle: target.titles.short,
    targetMode: target.mode,
    openMetrics,
    surface,
    layout,
    bottomHitProbe,
  };

  await testInfo.attach("pretext-virtualizer-short-thread.json", {
    body: JSON.stringify(summary, null, 2),
    contentType: "application/json",
  });
  await fs.writeFile(testInfo.outputPath("pretext-virtualizer-short-thread.json"), JSON.stringify(summary, null, 2), "utf8");

  expect(openMetrics.usableSampleCount, "short-thread probe never observed a mounted thread").toBeGreaterThan(0);
  expect(openMetrics.isShortThread, "short-thread target unexpectedly rendered as scrollable content").toBe(true);
  expect(
    Number(openMetrics.firstTopPaintMs),
    `short-thread open exceeded ${MAX_OPEN_MS}ms`,
  ).toBeLessThanOrEqual(MAX_OPEN_MS);
  expect(
    openMetrics.topBlankRunMs,
    `short-thread stayed blank for ${openMetrics.topBlankRunMs}ms`,
  ).toBeLessThanOrEqual(MAX_BLANK_MS);
  expect(
    openMetrics.maxBlankVisiblePoints,
    "short-thread open sampled a fully blank thread surface",
  ).toBeLessThan(openMetrics.firstUsableSampledPoints || 15);
  expect(surface.totalNonBlankPoints, "short-thread surface never showed visible content").toBeGreaterThan(0);
  expect(layout.maxScrollTop, `short-thread unexpectedly became scrollable by ${layout.maxScrollTop}px`).toBeLessThanOrEqual(
    MAX_SHORT_SCROLL_RANGE_PX,
  );
  expect(layout.scrollTop, "short-thread should stay pinned at scrollTop 0").toBe(0);
  expect(layout.snapshotFirstIndex, "short-thread should render from the first item").toBe(0);
  expect(layout.renderedFirstIndex, "short-thread should keep the first rendered item in view").toBe(0);
  expect(layout.renderedLastIndex).toBeGreaterThanOrEqual(0);
  expect(layout.snapshotLastIndex).toBeGreaterThanOrEqual(layout.snapshotFirstIndex);
  expect(layout.rowCount, "short-thread should render at least one PretextVirtualizer row").toBeGreaterThan(0);
  expect(
    layout.lastRowBottomGapPx,
    `short-thread content drifted ${layout.lastRowBottomGapPx}px above the bottom-aligned shell`,
  ).not.toBeNull();
  expect(
    Number(layout.lastRowBottomGapPx),
    `short-thread content sat ${layout.lastRowBottomGapPx}px above the bottom-aligned shell`,
  ).toBeLessThanOrEqual(MAX_ALIGNMENT_GAP_PX);
  expect(bottomHitProbe.isShortThread, "bottom-hit probe should classify the target as a short thread").toBe(true);
  expect(bottomHitProbe.maxDistanceFromBottomPx).toBeLessThanOrEqual(MAX_SHORT_SCROLL_RANGE_PX);
  expect(bottomHitProbe.maxJitterPx).toBeLessThanOrEqual(1);
});
