import fs from "node:fs";
import { test, expect } from "./fixtures";
import type { APIRequestContext, Page } from "playwright/test";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";
import { analyzeScrollJankVideo, canUseFfmpeg } from "./utils/videoJankAnalyzer";

const scrollSelector = ".wb-thread-scroller";
const debugEnabled = process.env.CTX_E2E_JANK_VIDEO_DEBUG === "1";

type RowSizeMismatch = {
  reason?: string;
  id?: string;
  itemKind?: string;
  itemKey?: string;
  knownVsActualDeltaPx?: number;
  knownVsParentDeltaPx?: number;
  parentVsActualDeltaPx?: number;
};

type FlashTrace = {
  cause?: string;
  sampleCount?: number;
  snapbackDetected?: boolean;
  maxAbsScrollTopDeltaPx?: number;
  maxAbsFirstItemTopDeltaPx?: number;
  maxAbsScrollHeightDeltaPx?: number;
};

type MessageListDebugWindow = Window & {
  __wbSessionMessageListDebug?: {
    seq: number;
    entries: Array<Record<string, unknown>>;
    flashSeq?: number;
    flashTraces?: FlashTrace[];
    rowSizeMismatchSeq?: number;
    rowSizeMismatches?: RowSizeMismatch[];
  };
};

const shouldRun = process.env.CTX_E2E_JANK_VIDEO === "1";
test.use({ video: shouldRun ? "on" : "off" });

const describe = shouldRun ? test.describe : test.describe.skip;

async function suppressGlobalUpdateNotice(page: Page) {
  await page.addInitScript(() => {
    window.localStorage.removeItem("ctx_update_check_v1");
    window.localStorage.removeItem("ctx_update_prompt_next_allowed_at_v1");
    window.localStorage.removeItem("ctx_update_prompt_idle_versions_v1");
    window.sessionStorage.removeItem("ctx_update_restart_required_version_v1");
  });
  await page.route("**/api/updates/check**", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        channel: "stable",
        base_url: "https://example.com",
        platform: "linux-x64",
        current_version: "1.0.0",
        latest_version: null,
        min_supported_version: null,
        platform_supported: true,
        in_place_update_supported: false,
        in_place_update_reason: null,
        update_available: false,
      }),
    });
  });
}

async function clearDebugStore(page: Page) {
  await page.evaluate(() => {
    const win = window as MessageListDebugWindow;
    win.__wbSessionMessageListDebug = {
      seq: 0,
      entries: [],
      flashSeq: 0,
      flashTraces: [],
      rowSizeMismatchSeq: 0,
      rowSizeMismatches: [],
    };
  });
}

async function readDebugStore(page: Page) {
  return page.evaluate(() => {
    const win = window as MessageListDebugWindow;
    const store = win.__wbSessionMessageListDebug;
    return {
      seq: store?.seq ?? 0,
      flashSeq: store?.flashSeq ?? 0,
      entries: Array.isArray(store?.entries) ? store.entries.slice(-100) : [],
      flashTraces: Array.isArray(store?.flashTraces) ? store.flashTraces.slice(-20) : [],
      rowSizeMismatchSeq: store?.rowSizeMismatchSeq ?? 0,
      rowSizeMismatches: Array.isArray(store?.rowSizeMismatches) ? store.rowSizeMismatches.slice(-100) : [],
    };
  });
}

function summarizeRowSizeMismatches(mismatches: RowSizeMismatch[]) {
  const absDeltas = mismatches.map((mismatch) => Math.abs(Number(mismatch.knownVsActualDeltaPx ?? 0)));
  return {
    count: mismatches.length,
    maxKnownVsActualDeltaPx: absDeltas.reduce((max, value) => Math.max(max, value), 0),
    reasons: Array.from(
      mismatches.reduce((counts, mismatch) => {
        const reason = String(mismatch.reason ?? "unknown");
        counts.set(reason, (counts.get(reason) ?? 0) + 1);
        return counts;
      }, new Map<string, number>()),
    ),
    samples: mismatches.slice(-5).map((mismatch) => ({
      id: mismatch.id ?? null,
      itemKind: mismatch.itemKind ?? null,
      itemKey: mismatch.itemKey ?? null,
      reason: mismatch.reason ?? null,
      knownVsActualDeltaPx: mismatch.knownVsActualDeltaPx ?? null,
      knownVsParentDeltaPx: mismatch.knownVsParentDeltaPx ?? null,
      parentVsActualDeltaPx: mismatch.parentVsActualDeltaPx ?? null,
    })),
  };
}

describe("workbench: scrollback jank video", () => {
  async function addLongMessages(request: APIRequestContext, sessionId: string, count: number) {
    const longText = Array.from({ length: 220 }, (_, i) => `history line ${i + 1}`).join("\n");
    for (let i = 0; i < count; i += 1) {
      await request.post(`/api/sessions/${sessionId}/messages`, {
        data: { content: `${longText}\nhistory scroll ${i + 1}`, delivery: "immediate" },
      });
    }
  }

  test("workbench: detect scrollback jank during upward scroll", async ({ page, request }, testInfo) => {
    test.setTimeout(180000);
    if (!canUseFfmpeg()) {
      test.skip(true, "ffmpeg not available");
    }

    await page.setViewportSize({ width: 1400, height: 900 });

    const seed = await seedDummyWorkspace(request, {
      tasks: 1,
      sessionsPerTask: 1,
      turnsPerSession: 10,
      messageBytes: { min: 220, max: 320 },
      messagePrefix: "scrollback-jank",
    });

    const taskId = seed.taskIds[0];
    const sessionId = seed.sessionIdsByTask[taskId][0];
    await addLongMessages(request, sessionId, 12);
    await suppressGlobalUpdateNotice(page);

    const params = new URLSearchParams();
    if (debugEnabled) {
      params.set("debug", "1");
    }
    const suffix = params.toString() ? `?${params.toString()}` : "";
    await page.goto(`/workspaces/${seed.workspaceId}${suffix}`, { waitUntil: "domcontentloaded" });
    const rows = page.locator(".wb-task-row");
    await expect(rows).toHaveCount(1, { timeout: 20000 });
    await rows.first().click();

    await expect(page.locator("textarea.wb-active-textarea")).toBeVisible({
      timeout: 20000,
    });

    const scroller = page.locator(scrollSelector).first();
    await expect(scroller).toBeVisible({ timeout: 20000 });

    if (debugEnabled) {
      await clearDebugStore(page);
    }

    await scroller.evaluate((el) => {
      el.scrollTop = Math.max(0, el.scrollHeight - el.clientHeight);
      el.dispatchEvent(new Event("scroll"));
    });
    await page.waitForTimeout(100);

    const historyResponse = page
      .waitForResponse((response) => response.url().includes(`/api/sessions/${sessionId}/history`), {
        timeout: 30000,
      })
      .catch(() => null);

    await scroller.hover();
    for (let i = 0; i < 40; i += 1) {
      await page.mouse.wheel(0, -240);
      await page.waitForTimeout(60);
    }

    await historyResponse;
    await page.waitForTimeout(1200);

    const debugState = debugEnabled ? await readDebugStore(page) : null;
    if (debugState) {
      await testInfo.attach("scrollback-jank-debug.json", {
        body: JSON.stringify(debugState, null, 2),
        contentType: "application/json",
      });
    }

    const video = page.video();
    await page.close();
    const videoPath = testInfo.outputPath("scrollback-jank-video.webm");
    if (!video) {
      throw new Error("Playwright video not available for scrollback jank analysis.");
    }
    await video.saveAs(videoPath);
    if (!fs.existsSync(videoPath)) {
      throw new Error("Playwright video not found for scrollback jank analysis.");
    }

    const thresholdPx = Number(process.env.CTX_E2E_JANK_SHIFT_PX ?? 20);
    const skipFrames = Number(process.env.CTX_E2E_JANK_SKIP_FRAMES ?? 20);
    const analysis = analyzeScrollJankVideo(videoPath, {
      outputDir: testInfo.outputDir,
      fps: 10,
      scaleWidth: 960,
      maxShiftPx: 40,
      thresholdPx,
      jankThresholdPx: thresholdPx,
      skipFrames,
      minBaselineAbsPx: 2,
    });

    await testInfo.attach("scrollback-jank-summary.json", {
      path: analysis.summaryPath,
      contentType: "application/json",
    });
    await testInfo.attach("scrollback-jank-shifts.csv", {
      path: analysis.csvPath,
      contentType: "text/csv",
    });
    await testInfo.attach("scrollback-jank-shifts.json", {
      path: analysis.jsonPath,
      contentType: "application/json",
    });
    for (const diffPath of analysis.diffPaths) {
      await testInfo.attach(`scrollback-jank-${diffPath.split("/").pop()}`, {
        path: diffPath,
        contentType: "image/png",
      });
    }

    if (process.env.CTX_E2E_JANK_VIDEO_LOG === "1") {
      // eslint-disable-next-line no-console
      console.log(`[scrollback-jank-summary] ${JSON.stringify({
        rows: analysis.rows.length,
        baselineShift: analysis.baselineShift,
        baselineAbs: analysis.baselineAbs,
        maxAbsShift: analysis.maxAbsShift,
        maxJankAbsShift: analysis.maxJankAbsShift,
        flaggedCount: analysis.flagged.length,
        snapbackPairs: analysis.snapbackPairs.length,
        ...(debugState
          ? {
              rowSizeMismatchSummary: summarizeRowSizeMismatches(debugState.rowSizeMismatches),
              flashTraces: debugState.flashTraces.map((trace) => ({
                cause: trace.cause ?? null,
                sampleCount: trace.sampleCount ?? null,
                snapbackDetected: trace.snapbackDetected ?? null,
                maxAbsScrollTopDeltaPx: trace.maxAbsScrollTopDeltaPx ?? null,
                maxAbsFirstItemTopDeltaPx: trace.maxAbsFirstItemTopDeltaPx ?? null,
                maxAbsScrollHeightDeltaPx: trace.maxAbsScrollHeightDeltaPx ?? null,
              })),
            }
          : {}),
      })}`);
    }

    expect(analysis.rows.length).toBeGreaterThan(5);
    expect(analysis.maxJankAbsShift).toBeLessThanOrEqual(thresholdPx);
  });
});
