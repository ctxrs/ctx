import { writeFile } from "node:fs/promises";
import type { Page, TestInfo } from "playwright/test";
import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

test.skip(
  process.platform === "linux" && process.env.CTX_E2E_BROWSER === "chromium",
  "WebKit-only layout regression is excluded from the Linux Chromium promotion lane.",
);

const SYNTHETIC_TRANSCRIPT_MESSAGE = `Saved the synthetic layout fixture to \`local/demo-branch\`. The final sample revision is \`abc1234\`, with the main fixture update in \`def5678\` and a small follow-up wording correction in \`fedcba9\`.

What landed:
- \`#1\`: paragraph spacing stays stable when inline code wraps across visual lines.
- \`#2\`: the trailing status row remains below the assistant content.
- \`#3\`: the local command fixture uses a long inline chip and never becomes a code block.
- \`#4\`: screenshots and diagnostics are written only when the regression fails.
- \`#5\`: the row shell keeps enough bottom padding for the completed status label.

Verification passed for the fixture:
- \`node --test fixtures/row-height-contract.test.cjs fixtures/inline-code-wrap.test.cjs fixtures/status-row-gap.test.cjs fixtures/markdown-spacing.test.cjs fixtures/virtualizer-shell.test.cjs\`
- Result: \`26\` passing, \`0\` failing
- Local browser measurement verification was green`;

type OverlapMetrics = {
  overlapPx: number;
  contentToNextGapPx: number;
  slotToNextGapPx: number;
  contentOverflowPastSlotPx: number;
  maxContentBottom: number;
  slotBottom: number;
  nextTop: number;
  codeRects: Array<{ text: string; rectCount: number; bottom: number; height: number }>;
};

test.use({
  browserName: "webkit",
  viewport: { width: 1344, height: 768 },
});

async function attachDiagnostics(
  page: Page,
  testInfo: TestInfo,
  metrics: OverlapMetrics | null,
  reason: string,
): Promise<never> {
  const screenshotPath = testInfo.outputPath("inline-code-overlap-webkit.png");
  const detailsPath = testInfo.outputPath("inline-code-overlap-webkit.json");
  await page.screenshot({ path: screenshotPath, fullPage: true });
  await writeFile(detailsPath, JSON.stringify({ reason, metrics }, null, 2), "utf8");
  await testInfo.attach("inline-code-overlap-webkit.png", {
    path: screenshotPath,
    contentType: "image/png",
  });
  await testInfo.attach("inline-code-overlap-webkit.json", {
    path: detailsPath,
    contentType: "application/json",
  });
  throw new Error(reason);
}

async function measureOverlap(page: Page): Promise<OverlapMetrics | null> {
  return page.evaluate(() => {
    const session = document.querySelector<HTMLElement>('.wb-session-slot[aria-hidden="false"] [data-testid="session-view"]');
    const entry = Array.from(
      session?.querySelectorAll<HTMLElement>(".wb-assistant-entry") ?? [],
    ).find((element) => element.innerText.includes("Saved the synthetic layout fixture to local/demo-branch"));
    const slot = entry?.closest(".wb-pretext-virtualizer-row")?.parentElement as HTMLElement | null;
    const nextSlot = slot?.nextElementSibling as HTMLElement | null;
    const nextRow = nextSlot?.querySelector<HTMLElement>(".wb-pretext-virtualizer-row") ?? null;
    if (!entry || !slot || !nextRow) {
      return null;
    }

    const slotRect = slot.getBoundingClientRect();
    const nextRect = nextRow.getBoundingClientRect();
    const descendants = [entry, ...Array.from(entry.querySelectorAll<HTMLElement>("*"))];
    let maxContentBottom = entry.getBoundingClientRect().bottom;
    for (const node of descendants) {
      const rect = node.getBoundingClientRect();
      if (Number.isFinite(rect.bottom)) {
        maxContentBottom = Math.max(maxContentBottom, rect.bottom);
      }
    }

    const codeRects = Array.from(entry.querySelectorAll<HTMLElement>("code")).map((code) => {
      const rect = code.getBoundingClientRect();
      return {
        text: (code.textContent ?? "").slice(0, 120),
        rectCount: code.getClientRects().length,
        bottom: rect.bottom,
        height: rect.height,
      };
    });

    return {
      overlapPx: maxContentBottom - nextRect.top,
      contentToNextGapPx: nextRect.top - maxContentBottom,
      slotToNextGapPx: nextRect.top - slotRect.bottom,
      contentOverflowPastSlotPx: maxContentBottom - slotRect.bottom,
      maxContentBottom,
      slotBottom: slotRect.bottom,
      nextTop: nextRect.top,
      codeRects,
    };
  });
}

test("workbench: webkit keeps a synthetic wrapped inline-code transcript above the trailing Completed row", async ({
  page,
  request,
}, testInfo) => {
  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 0,
  });

  await page.goto(`/workspaces/${seed.workspaceId}?debug=1`, { waitUntil: "domcontentloaded" });
  const task = page.locator(".wb-task-row").filter({ hasText: "fixture task 1" }).first();
  await expect(task).toBeVisible({ timeout: 30_000 });
  await task.click();
  await expect(page.locator('.wb-session-slot[aria-hidden="false"] textarea.wb-active-textarea')).toBeVisible({
    timeout: 20_000,
  });

  const sessionId = seed.sessionIdsByTask[seed.taskIds[0]!]?.[0];
  expect(sessionId).toBeTruthy();
  const response = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: { content: SYNTHETIC_TRANSCRIPT_MESSAGE, delivery: "immediate" },
  });
  expect(response.ok()).toBeTruthy();

  const assistantEntry = page
    .locator('.wb-session-slot[aria-hidden="false"] .wb-assistant-entry')
    .filter({ hasText: "Saved the synthetic layout fixture to local/demo-branch" })
    .last();
  await expect(assistantEntry).toBeVisible({ timeout: 20_000 });
  await expect(page.locator('.wb-session-slot[aria-hidden="false"] .wb-turn-status-label').last()).toHaveText(
    /Completed/i,
    { timeout: 20_000 },
  );
  await page.waitForTimeout(400);

  const metrics = await measureOverlap(page);
  if (!metrics) {
    await attachDiagnostics(page, testInfo, null, "failed to locate the assistant row or trailing status row");
  }

  const longWrappedCommand = metrics.codeRects.find((code) => code.text.startsWith("node --test"));
  if (!longWrappedCommand || longWrappedCommand.rectCount <= 1) {
    await attachDiagnostics(
      page,
      testInfo,
      metrics,
      "expected the synthetic transcript fixture to wrap the long inline command onto multiple visual lines",
    );
  }

  if (metrics.overlapPx > 1 || metrics.contentToNextGapPx < -1 || metrics.slotToNextGapPx < -1) {
    await attachDiagnostics(
      page,
      testInfo,
      metrics,
      `wrapped inline-code fixture overlapped the trailing Completed row (overlap=${metrics.overlapPx.toFixed(2)}px, contentGap=${metrics.contentToNextGapPx.toFixed(2)}px, slotGap=${metrics.slotToNextGapPx.toFixed(2)}px)`,
    );
  }

  expect(metrics.contentOverflowPastSlotPx).toBeLessThanOrEqual(1);
});
