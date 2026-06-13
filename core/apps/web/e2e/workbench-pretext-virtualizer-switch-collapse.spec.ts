import fs from "fs/promises";
import { test, expect } from "./fixtures";
import { resolveAnchorstreamAcceptanceWorkspace } from "./utils/seedPretextVirtualizerAcceptanceWorkspace";

const parseEnvNumber = (value: string | undefined, fallback: number): number => {
  const normalized = String(value ?? "").trim();
  if (normalized.length === 0) return fallback;
  const parsed = Number(normalized);
  return Number.isFinite(parsed) ? parsed : fallback;
};

const SAMPLE_COUNT = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_SWITCH_SAMPLE_COUNT, 20);
const SAMPLE_DELAY_MS = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_SWITCH_SAMPLE_DELAY_MS, 125);
const SETTLED_SAMPLE_COUNT = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_SWITCH_SETTLED_SAMPLE_COUNT, 5);
const MAX_SCROLL_HEIGHT_COLLAPSE_PX = parseEnvNumber(
  process.env.PRETEXT_VIRTUALIZER_SWITCH_MAX_SCROLL_HEIGHT_COLLAPSE_PX,
  2000,
);
const MAX_SCROLL_TOP_COLLAPSE_PX = parseEnvNumber(
  process.env.PRETEXT_VIRTUALIZER_SWITCH_MAX_SCROLL_TOP_COLLAPSE_PX,
  2000,
);
const MAX_GAP_TO_BOTTOM_PX = parseEnvNumber(
  process.env.PRETEXT_VIRTUALIZER_SWITCH_MAX_GAP_TO_BOTTOM_PX,
  16,
);
const scrollSelector = ".wb-session-slot[aria-hidden=\"false\"] [data-pretext-virtualizer-list=\"1\"]";

type SwitchSample = {
  t: number;
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
  gapToBottom: number;
  rowCount: number;
  firstText: string;
  lastText: string;
};

const median = (values: number[]): number => {
  const sorted = [...values].sort((left, right) => left - right);
  if (sorted.length === 0) return 0;
  const middle = Math.floor(sorted.length / 2);
  if (sorted.length % 2 === 1) {
    return sorted[middle] ?? 0;
  }
  return ((sorted[middle - 1] ?? 0) + (sorted[middle] ?? 0)) / 2;
};

async function clickTaskByTitle(page: Parameters<typeof test>[0]["page"], title: string) {
  const row = page.locator(".wb-task-row").filter({ hasText: title }).first();
  await expect(row).toBeVisible({ timeout: 20_000 });
  await row.click();
  await expect(page.locator(scrollSelector).first()).toBeVisible({ timeout: 20_000 });
}

async function readSwitchSample(page: Parameters<typeof test>[0]["page"]): Promise<SwitchSample> {
  return page.locator(scrollSelector).first().evaluate((scroller) => {
    const rows = Array.from(scroller.querySelectorAll<HTMLElement>("[data-pretext-virtualizer-row='1'][data-pretext-virtualizer-item-id]"));
    return {
      t: performance.now(),
      scrollTop: scroller.scrollTop,
      scrollHeight: scroller.scrollHeight,
      clientHeight: scroller.clientHeight,
      gapToBottom: Math.max(0, scroller.scrollHeight - (scroller.scrollTop + scroller.clientHeight)),
      rowCount: rows.length,
      firstText: (rows[0]?.textContent ?? "").trim().slice(0, 120),
      lastText: (rows.at(-1)?.textContent ?? "").trim().slice(0, 120),
    };
  });
}

test("workbench: pretextVirtualizer task switch does not collapse measured height while settling at bottom", async ({
  page,
  request,
}, testInfo) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  const target = await resolveAnchorstreamAcceptanceWorkspace(request);

  await page.goto(target.workspacePath, { waitUntil: "domcontentloaded" });

  await clickTaskByTitle(page, target.titles.warm);
  await page.waitForTimeout(800);
  await clickTaskByTitle(page, target.titles.long);

  const samples: SwitchSample[] = [];
  for (let index = 0; index < SAMPLE_COUNT; index += 1) {
    samples.push(await readSwitchSample(page));
    if (index < SAMPLE_COUNT - 1) {
      await page.waitForTimeout(SAMPLE_DELAY_MS);
    }
  }

  const usableSamples = samples.filter((sample) => sample.scrollHeight > 0 && sample.clientHeight > 0);
  expect(usableSamples.length, "switch probe never observed a mounted thread scroller").toBeGreaterThan(0);

  const settledWindow = usableSamples.slice(-Math.max(1, Math.min(SETTLED_SAMPLE_COUNT, usableSamples.length)));
  const settledScrollHeight = median(settledWindow.map((sample) => sample.scrollHeight));
  const settledScrollTop = median(settledWindow.map((sample) => sample.scrollTop));
  const maxScrollHeightCollapsePx = Math.max(
    0,
    ...usableSamples.map((sample) => sample.scrollHeight - settledScrollHeight),
  );
  const maxScrollTopCollapsePx = Math.max(
    0,
    ...usableSamples.map((sample) => sample.scrollTop - settledScrollTop),
  );
  const maxGapToBottomPx = Math.max(...usableSamples.map((sample) => sample.gapToBottom));

  const summary = {
    workspacePath: target.workspacePath,
    warmTaskTitle: target.titles.warm,
    targetTaskTitle: target.titles.long,
    targetMode: target.mode,
    sampleCount: usableSamples.length,
    settledScrollHeight,
    settledScrollTop,
    maxScrollHeightCollapsePx,
    maxScrollTopCollapsePx,
    maxGapToBottomPx,
    samples: usableSamples,
  };

  await testInfo.attach("pretext-virtualizer-switch-collapse.json", {
    body: JSON.stringify(summary, null, 2),
    contentType: "application/json",
  });
  await fs.writeFile(testInfo.outputPath("pretext-virtualizer-switch-collapse.json"), JSON.stringify(summary, null, 2), "utf8");

  expect(
    maxScrollHeightCollapsePx,
    `task switch inflated scrollHeight by ${maxScrollHeightCollapsePx}px before settling`,
  ).toBeLessThanOrEqual(MAX_SCROLL_HEIGHT_COLLAPSE_PX);
  expect(
    maxScrollTopCollapsePx,
    `task switch inflated scrollTop by ${maxScrollTopCollapsePx}px before settling`,
  ).toBeLessThanOrEqual(MAX_SCROLL_TOP_COLLAPSE_PX);
  expect(
    maxGapToBottomPx,
    `task switch drifted ${maxGapToBottomPx}px away from bottom while settling`,
  ).toBeLessThanOrEqual(MAX_GAP_TO_BOTTOM_PX);
});
