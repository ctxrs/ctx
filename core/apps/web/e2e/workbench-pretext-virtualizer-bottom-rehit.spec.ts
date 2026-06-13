import fs from "fs/promises";
import { test, expect } from "./fixtures";
import { collectBottomRehitProbe } from "./utils/pretextVirtualizerAcceptanceProbes";
import { resolveAnchorstreamAcceptanceWorkspace } from "./utils/seedPretextVirtualizerAcceptanceWorkspace";

const parseEnvNumber = (value: string | undefined, fallback: number): number => {
  const normalized = String(value ?? "").trim();
  if (normalized.length === 0) return fallback;
  const parsed = Number(normalized);
  return Number.isFinite(parsed) ? parsed : fallback;
};

const CYCLE_COUNT = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_BOTTOM_REHIT_CYCLE_COUNT, 6);
const LEAVE_BOTTOM_PX = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_BOTTOM_REHIT_LEAVE_BOTTOM_PX, 1600);
const SAMPLE_COUNT = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_BOTTOM_REHIT_SAMPLE_COUNT, 12);
const SETTLE_MS = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_BOTTOM_REHIT_SETTLE_MS, 55);
const MAX_DISTANCE_FROM_BOTTOM_PX = parseEnvNumber(
  process.env.PRETEXT_VIRTUALIZER_BOTTOM_REHIT_MAX_DISTANCE_FROM_BOTTOM_PX,
  // Chromium bottom-scroll quantization in this long-thread re-hit probe
  // stabilizes at 3-4px while the surface remains visually pinned.
  4,
);
const MAX_JITTER_PX = parseEnvNumber(process.env.PRETEXT_VIRTUALIZER_BOTTOM_REHIT_MAX_JITTER_PX, 4);
const MAX_LAST_ITEM_BOTTOM_JITTER_PX = parseEnvNumber(
  process.env.PRETEXT_VIRTUALIZER_BOTTOM_REHIT_MAX_LAST_ITEM_BOTTOM_JITTER_PX,
  6,
);
const MAX_SCROLL_HEIGHT_JITTER_PX = parseEnvNumber(
  process.env.PRETEXT_VIRTUALIZER_BOTTOM_REHIT_MAX_SCROLL_HEIGHT_JITTER_PX,
  4,
);
const scrollSelector = ".wb-session-slot[aria-hidden=\"false\"] [data-pretext-virtualizer-list=\"1\"]";

test("workbench: pretextVirtualizer bottom re-hit stays stable after leaving bottom", async ({
  page,
  request,
}, testInfo) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  const target = await resolveAnchorstreamAcceptanceWorkspace(request);
  await page.goto(target.workspacePath, { waitUntil: "domcontentloaded" });

  const row = page.locator(".wb-task-row").filter({ hasText: target.titles.rehit }).first();
  await expect(row).toBeVisible({ timeout: 20_000 });
  await row.click();
  await expect(page.locator(scrollSelector).first()).toBeVisible({ timeout: 20_000 });
  await page.waitForTimeout(1000);

  const probe = await collectBottomRehitProbe(page, scrollSelector, {
    cycleCount: CYCLE_COUNT,
    leaveBottomPx: LEAVE_BOTTOM_PX,
    sampleCount: SAMPLE_COUNT,
    settleMs: SETTLE_MS,
  });

  const summary = {
    workspacePath: target.workspacePath,
    taskTitle: target.titles.rehit,
    targetMode: target.mode,
    cycleCount: probe.cycleCount,
    sampleCount: probe.sampleCount,
    isShortThread: probe.isShortThread,
    maxDistanceFromBottomPx: probe.maxDistanceFromBottomPx,
    maxJitterPx: probe.maxJitterPx,
    maxLastItemBottomJitterPx: probe.maxLastItemBottomJitterPx,
    maxScrollHeightJitterPx: probe.maxScrollHeightJitterPx,
    cycles: probe.cycles,
  };

  await testInfo.attach("pretext-virtualizer-bottom-rehit.json", {
    body: JSON.stringify(summary, null, 2),
    contentType: "application/json",
  });
  await fs.writeFile(testInfo.outputPath("pretext-virtualizer-bottom-rehit.json"), JSON.stringify(summary, null, 2), "utf8");

  expect(probe.isShortThread, "bottom re-hit target must be a long thread").toBe(false);
  expect(
    probe.maxDistanceFromBottomPx,
    `bottom re-hit drifted ${probe.maxDistanceFromBottomPx}px away from bottom`,
  ).toBeLessThanOrEqual(MAX_DISTANCE_FROM_BOTTOM_PX);
  expect(
    probe.maxJitterPx,
    `bottom re-hit jittered ${probe.maxJitterPx}px between samples`,
  ).toBeLessThanOrEqual(MAX_JITTER_PX);
  expect(
    probe.maxLastItemBottomJitterPx,
    `bottom re-hit moved the last visible item by ${probe.maxLastItemBottomJitterPx}px`,
  ).toBeLessThanOrEqual(MAX_LAST_ITEM_BOTTOM_JITTER_PX);
  expect(
    probe.maxScrollHeightJitterPx,
    `bottom re-hit changed scrollHeight by ${probe.maxScrollHeightJitterPx}px after returning to bottom`,
  ).toBeLessThanOrEqual(MAX_SCROLL_HEIGHT_JITTER_PX);
});
