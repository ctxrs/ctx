import { test, expect } from "./fixtures";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";

const ASSISTANT_ENTRY_BOTTOM_PADDING_PX = 10;
const ASSISTANT_ENTRY_PADDING_TOLERANCE_PX = 2;
const MAX_ASSISTANT_TO_NEXT_ROW_CHROME_GAP_PX = 20;

test.use({ browserName: "chromium" });

test("workbench: inline code wraps without horizontal scroll", async ({ page, request }) => {
  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 0,
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1);
  await rows.first().click();
  await page.waitForTimeout(400);
  await expect(page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea")).toBeVisible({ timeout: 20000 });

  const sessionId = seed.sessionIdsByTask[seed.taskIds[0]][0];
  const longSegment = "--flag=abcdefghijklmnopqrstuvwxyz0123456789";
  const longCommand = `bash -lc "cd core/apps/web && pnpm playwright test ${longSegment.repeat(20)}"`;
  const marker = `wrap-test-${Date.now()}`;
  const content = `${marker} \`${longCommand}\``;

  const resp = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: { content, delivery: "immediate" },
  });
  expect(resp.ok()).toBeTruthy();

  const assistantEntry = page.locator(".wb-assistant-entry").filter({ hasText: `done: ${marker}` });
  await expect(assistantEntry).toBeVisible({ timeout: 20000 });

  await expect(assistantEntry.locator(".codeblock")).toHaveCount(0);
  const resolveWrapMetrics = () =>
    page.evaluate((markerValue) => {
      const entries = Array.from(document.querySelectorAll(".wb-assistant-entry"));
      const entry = entries.find((el) => el.textContent?.includes(`done: ${markerValue}`));
      const code = entry?.querySelector("code");
      if (!code) return null;
      const style = window.getComputedStyle(code);
      const lineHeight = Number.parseFloat(style.lineHeight);
      const rectCount = code.getClientRects().length;
      const boxHeight = code.getBoundingClientRect().height;
      const estimatedLines = Number.isFinite(lineHeight) && lineHeight > 0 ? Math.round(boxHeight / lineHeight) : 0;
      return { rectCount, estimatedLines };
    }, marker);

  await expect.poll(resolveWrapMetrics, { timeout: 2000 }).not.toBeNull();
  const wrapMetrics = await resolveWrapMetrics();
  expect(wrapMetrics).not.toBeNull();

  if (wrapMetrics) {
    expect(Math.max(wrapMetrics.rectCount, wrapMetrics.estimatedLines)).toBeGreaterThan(1);
  }

  const inlineCodeStyles = await assistantEntry.locator("code").first().evaluate((element) => {
    const style = window.getComputedStyle(element);
    return {
      paddingTop: style.paddingTop,
      paddingRight: style.paddingRight,
      paddingBottom: style.paddingBottom,
      paddingLeft: style.paddingLeft,
      borderRadius: style.borderRadius,
      borderTopWidth: style.borderTopWidth,
      fontSize: style.fontSize,
      fontFamily: style.fontFamily,
    };
  });
  expect(inlineCodeStyles.paddingTop).toBe("1px");
  expect(inlineCodeStyles.paddingRight).toBe("6px");
  expect(inlineCodeStyles.paddingBottom).toBe("1px");
  expect(inlineCodeStyles.paddingLeft).toBe("6px");
  expect(inlineCodeStyles.borderRadius).toBe("6px");
  expect(inlineCodeStyles.borderTopWidth).toBe("1px");
  expect(inlineCodeStyles.fontSize).toBe("13px");
  expect(inlineCodeStyles.fontFamily.toLowerCase()).toContain("mono");

  const metrics = await page.evaluate(() => {
    const doc = document.documentElement;
    const body = document.body;
    const sessionView = document.querySelector(".wb-session-view") as HTMLElement | null;
    return {
      docScrollWidth: doc.scrollWidth,
      docClientWidth: doc.clientWidth,
      bodyScrollWidth: body.scrollWidth,
      bodyClientWidth: body.clientWidth,
      sessionScrollWidth: sessionView?.scrollWidth ?? 0,
      sessionClientWidth: sessionView?.clientWidth ?? 0,
    };
  });

  expect(metrics.docScrollWidth).toBeLessThanOrEqual(metrics.docClientWidth + 1);
  expect(metrics.bodyScrollWidth).toBeLessThanOrEqual(metrics.bodyClientWidth + 1);
  expect(metrics.sessionScrollWidth).toBeLessThanOrEqual(metrics.sessionClientWidth + 1);
});

test("workbench: dense inline links and inline-code chips stay deterministic", async ({ page, request }) => {
  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 0,
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1);
  await rows.first().click();
  await page.waitForTimeout(400);
  await expect(page.locator('.wb-session-slot[aria-hidden="false"] textarea.wb-active-textarea')).toBeVisible({
    timeout: 20_000,
  });

  const sessionId = seed.sessionIdsByTask[seed.taskIds[0]][0];
  const marker = `inline-link-code-${Date.now()}`;
  const content = [
    marker,
    "",
    "Yes, with an important qualifier: this fixture is useful for layout coverage, but it is still a synthetic browser measurement case rather than a product claim.",
    "",
    "The row contains several inline links. [Alpha docs](https://example.com/alpha), [Beta docs](https://example.com/beta), [Gamma notes](https://example.com/gamma), and [Delta notes](https://example.com/delta) all provide enough adjacent link text to exercise wrapping.",
    "",
    "The measurement should account for `inline`, `code`, and `chip` tokens without allowing the trailing status row to overlap the assistant content.",
  ].join("\n");

  const resp = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: { content, delivery: "immediate" },
  });
  expect(resp.ok()).toBeTruthy();

  const assistantEntry = page.locator(".wb-assistant-entry").filter({ hasText: `done: ${marker}` });
  await expect(assistantEntry).toBeVisible({ timeout: 20_000 });
  await expect(assistantEntry.locator("a")).toHaveCount(4);
  await expect(assistantEntry.locator("code")).toHaveCount(5);
  await expect(page.locator(".wb-turn-status")).toBeVisible({ timeout: 20_000 });

  const overlapAndGap = await page.evaluate((markerValue) => {
    const entries = Array.from(document.querySelectorAll<HTMLElement>(".wb-assistant-entry"));
    const entry = entries.find((element) => element.textContent?.includes(`done: ${markerValue}`));
    if (!entry) return null;
    const slot = entry.closest(".wb-pretext-virtualizer-row")?.parentElement ?? null;
    const nextSlot = slot?.nextElementSibling ?? null;
    const nextRow = nextSlot?.querySelector<HTMLElement>(".wb-pretext-virtualizer-row") ?? null;
    if (!slot || !nextRow) return null;
    const slotRect = slot.getBoundingClientRect();
    const nextRect = nextRow.getBoundingClientRect();
    const lastBlock = Array.from(entry.querySelectorAll<HTMLElement>(".wb-assistant-body > div > *")).at(-1) ?? null;
    const lastBlockRect = lastBlock?.getBoundingClientRect() ?? null;
    return {
      slotToNextGapPx: nextRect.top - slotRect.bottom,
      lastBlockToNextGapPx: lastBlockRect ? nextRect.top - lastBlockRect.bottom : null,
      slotContentSlackPx: lastBlockRect ? slotRect.bottom - lastBlockRect.bottom : null,
      overlapPx: slotRect.bottom - nextRect.top,
    };
  }, marker);

  expect(overlapAndGap).not.toBeNull();
  expect(overlapAndGap?.overlapPx ?? 0).toBeLessThanOrEqual(1);
  expect(overlapAndGap?.slotToNextGapPx ?? Number.NEGATIVE_INFINITY).toBeGreaterThanOrEqual(-1);
  expect(overlapAndGap?.lastBlockToNextGapPx ?? Number.NEGATIVE_INFINITY).toBeGreaterThanOrEqual(-1);
  expect(
    Math.abs((overlapAndGap?.slotContentSlackPx ?? Number.NEGATIVE_INFINITY) - ASSISTANT_ENTRY_BOTTOM_PADDING_PX),
  ).toBeLessThanOrEqual(ASSISTANT_ENTRY_PADDING_TOLERANCE_PX);
});

test("workbench: a long wrapped inline-code chip does not overlap the trailing turn status row", async ({ page, request }) => {
  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 0,
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1);
  await rows.first().click();
  await page.waitForTimeout(400);
  await expect(page.locator('.wb-session-slot[aria-hidden="false"] textarea.wb-active-textarea')).toBeVisible({
    timeout: 20_000,
  });

  const sessionId = seed.sessionIdsByTask[seed.taskIds[0]][0];
  const marker = `inline-code-overlap-${Date.now()}`;
  const content =
    `${marker} begin agent message here with plain text, and now some inline code block: ` +
    "`inline-thing-that-actually-gets-really-long-so-much-so-that-it-actually-wraps-to-2-lines-and-keeps-going-with-extra-path-segments/core/apps/web/src/pages/sessionThread/sessionMarkdownMeasurement.ts`";

  const resp = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: { content, delivery: "immediate" },
  });
  expect(resp.ok()).toBeTruthy();

  const assistantEntry = page.locator(".wb-assistant-entry").filter({ hasText: `done: ${marker}` });
  await expect(assistantEntry).toBeVisible({ timeout: 20_000 });
  await expect(page.locator(".wb-turn-status")).toBeVisible({ timeout: 20_000 });

  const overlapAndGap = await page.evaluate((markerValue) => {
    const entries = Array.from(document.querySelectorAll<HTMLElement>(".wb-assistant-entry"));
    const entry = entries.find((element) => element.textContent?.includes(`done: ${markerValue}`));
    if (!entry) return null;
    const slot = entry.closest(".wb-pretext-virtualizer-row")?.parentElement ?? null;
    const nextSlot = slot?.nextElementSibling ?? null;
    const nextRow = nextSlot?.querySelector<HTMLElement>(".wb-pretext-virtualizer-row") ?? null;
    const code = entry.querySelector<HTMLElement>("code");
    if (!slot || !nextRow || !code) return null;
    const slotRect = slot.getBoundingClientRect();
    const nextRect = nextRow.getBoundingClientRect();
    const codeRect = code.getBoundingClientRect();
    return {
      slotToNextGapPx: nextRect.top - slotRect.bottom,
      codeToNextGapPx: nextRect.top - codeRect.bottom,
      slotContentSlackPx: slotRect.bottom - codeRect.bottom,
      overlapPx: slotRect.bottom - nextRect.top,
      codeClientRectCount: code.getClientRects().length,
    };
  }, marker);

  expect(overlapAndGap).not.toBeNull();
  expect(overlapAndGap?.codeClientRectCount ?? 0).toBeGreaterThan(1);
  expect(overlapAndGap?.overlapPx ?? 0).toBeLessThanOrEqual(1);
  expect(overlapAndGap?.slotToNextGapPx ?? Number.NEGATIVE_INFINITY).toBeGreaterThanOrEqual(-1);
  expect(overlapAndGap?.codeToNextGapPx ?? Number.NEGATIVE_INFINITY).toBeGreaterThanOrEqual(-1);
  expect(
    Math.abs((overlapAndGap?.slotContentSlackPx ?? Number.NEGATIVE_INFINITY) - ASSISTANT_ENTRY_BOTTOM_PADDING_PX),
  ).toBeLessThanOrEqual(ASSISTANT_ENTRY_PADDING_TOLERANCE_PX);
});

test("workbench: rich markdown assistant rows do not overlap a trailing turn status row", async ({ page, request }) => {
  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 0,
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1);
  await rows.first().click();
  await page.waitForTimeout(400);
  await expect(page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea")).toBeVisible({
    timeout: 20_000,
  });

  const sessionId = seed.sessionIdsByTask[seed.taskIds[0]][0];
  const marker = `overlap-test-${Date.now()}`;
  const content = [
    marker,
    "",
    "The synthetic layout problems were:",
    "",
    "- The first paragraph only made `compact row first, status second` a preference.",
    "- The sample template still teaches `paragraph + list + inline code`.",
    "- The narrow-width case still allowed `RECHECK` rows through.",
    "",
    "The real fix is to change the synthetic fixture and rerun with zero hand edits.",
    "",
    "- make `row measurement first, status measurement second` a hard requirement",
    "- hard-fail any fixture whose measurement step returns `RECHECK`",
  ].join("\n");

  const resp = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: { content, delivery: "immediate" },
  });
  expect(resp.ok()).toBeTruthy();

  const assistantEntry = page.locator(".wb-assistant-entry").filter({ hasText: `done: ${marker}` });
  await expect(assistantEntry).toBeVisible({ timeout: 20_000 });
  await expect(page.locator(".wb-turn-status")).toBeVisible({ timeout: 20_000 });

  const overlapReport = await page.evaluate(() => {
    const scroller = document.querySelector(
      '.wb-session-slot[aria-hidden="false"] [data-pretext-virtualizer-list="1"]',
    ) as HTMLElement | null;
    if (!scroller) return null;
    const shells = Array.from(scroller.querySelectorAll<HTMLElement>('[data-pretext-virtualizer-row-shell="1"]'));
    const overlaps = shells.flatMap((shell, index) => {
      if (index >= shells.length - 1) return [] as Array<{
        overlapPx: number;
        kind: string;
        nextKind: string;
        itemId: string | null;
        nextItemId: string | null;
      }>;
      const nextShell = shells[index + 1];
      if (!nextShell) return [];
      const row = shell.querySelector<HTMLElement>('[data-pretext-virtualizer-row="1"]');
      const nextRow = nextShell.querySelector<HTMLElement>('[data-pretext-virtualizer-row="1"]');
      const currentRect = row?.getBoundingClientRect() ?? shell.getBoundingClientRect();
      const nextRect = nextShell.getBoundingClientRect();
      const overlapPx = currentRect.bottom - nextRect.top;
      if (overlapPx <= 1) return [];
      const resolveKind = (element: HTMLElement) => {
        if (element.querySelector(".wb-turn-status")) return "turn_status";
        if (element.querySelector(".wb-thought-row")) return "thought";
        if (element.querySelector(".wb-turn-header")) return "turn_header";
        if (element.querySelector(".wb-user-message")) return "message";
        if (element.querySelector(".wb-assistant-entry")) return "assistant";
        if (element.querySelector(".wb-tool-group")) return "tool_group";
        if (element.querySelector(".wb-tool")) return "tool";
        if (element.querySelector(".ask-user-question-card")) return "ask_user_question";
        return "unknown";
      };
      return [
        {
          overlapPx,
          kind: resolveKind(shell),
          nextKind: resolveKind(nextShell),
          itemId: row?.getAttribute("data-pretext-virtualizer-item-id") ?? null,
          nextItemId: nextRow?.getAttribute("data-pretext-virtualizer-item-id") ?? null,
        },
      ];
    });
    return {
      overlapCount: overlaps.length,
      maxOverlapPx: overlaps.reduce((max, entry) => Math.max(max, entry.overlapPx), 0),
      overlaps,
    };
  });

  expect(overlapReport).not.toBeNull();
  expect(overlapReport?.overlapCount).toBe(0);
  expect(overlapReport?.maxOverlapPx ?? 0).toBeLessThanOrEqual(1);
});

test("workbench: list-heavy assistant rows do not leave a blank gap before a trailing turn status row", async ({
  page,
  request,
}) => {
  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 0,
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1);
  await rows.first().click();
  await page.waitForTimeout(400);
  await expect(page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea")).toBeVisible({
    timeout: 20_000,
  });

  const sessionId = seed.sessionIdsByTask[seed.taskIds[0]][0];
  const marker = `list-gap-test-${Date.now()}`;
  const content = [
    marker,
    "",
    "Short answer:",
    "",
    "- Against `renderer-a`: yes on determinism for this workload.",
    "- Against `Virtuoso`: likely yes on user-visible stability if rows stay deterministic.",
    "- On raw throughput: not proven yet.",
    "",
    "How I would measure it:",
    "",
    "1. Build an A/B renderer switch in the app.",
    "   - Current deterministic transcript",
    "   - Existing MessageList/Virtuoso path",
    "   - Synthetic `react-window` path",
    "2. Use identical seeded workloads.",
    "   - Long rich markdown thread",
    "   - Detached-from-bottom while streaming",
    "   - Width change / resize",
    "3. Compare p50 and p95, not one run.",
    "   - Same machine, same viewport, same seeded data",
    "",
    "The important point is that the benchmark should be workload-shaped, not library-shaped.",
    "",
    "If you want, I can build the A/B benchmark harness next and make it output a comparable report.",
  ].join("\n");

  const resp = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: { content, delivery: "immediate" },
  });
  expect(resp.ok()).toBeTruthy();

  const assistantEntry = page.locator(".wb-assistant-entry").filter({ hasText: `done: ${marker}` });
  await expect(assistantEntry).toBeVisible({ timeout: 20_000 });

  const gapReport = await page.evaluate((markerValue) => {
    const entries = Array.from(document.querySelectorAll<HTMLElement>(".wb-assistant-entry"));
    const entry = entries.find((element) => element.textContent?.includes(`done: ${markerValue}`));
    if (!entry) return null;
    const slot = entry.closest(".wb-pretext-virtualizer-row")?.parentElement ?? null;
    const nextSlot = slot?.nextElementSibling ?? null;
    const nextRow = nextSlot?.querySelector<HTMLElement>(".wb-pretext-virtualizer-row") ?? null;
    if (!slot || !nextRow) return null;
    const slotRect = slot.getBoundingClientRect();
    const nextRect = nextRow.getBoundingClientRect();
    const lastBlock = Array.from(entry.querySelectorAll<HTMLElement>(".wb-assistant-body > div > *")).at(-1) ?? null;
    const lastBlockRect = lastBlock?.getBoundingClientRect() ?? null;
    return {
      nextRowText: nextRow.textContent?.replace(/\s+/g, " ").trim() ?? null,
      slotToNextGapPx: nextRect.top - slotRect.bottom,
      lastBlockToNextGapPx: lastBlockRect ? nextRect.top - lastBlockRect.bottom : null,
      slotContentSlackPx: lastBlockRect ? slotRect.bottom - lastBlockRect.bottom : null,
      overlapPx: slotRect.bottom - nextRect.top,
    };
  }, marker);

  expect(gapReport).not.toBeNull();
  expect(gapReport?.nextRowText ?? "").not.toHaveLength(0);
  expect(gapReport?.overlapPx ?? 0).toBeLessThanOrEqual(1);
  expect(
    Math.abs((gapReport?.slotContentSlackPx ?? Number.NEGATIVE_INFINITY) - ASSISTANT_ENTRY_BOTTOM_PADDING_PX),
  ).toBeLessThanOrEqual(ASSISTANT_ENTRY_PADDING_TOLERANCE_PX);
  expect(gapReport?.lastBlockToNextGapPx ?? Number.POSITIVE_INFINITY).toBeLessThanOrEqual(
    MAX_ASSISTANT_TO_NEXT_ROW_CHROME_GAP_PX,
  );
});

test("workbench: markdown tables stay deterministic without overlap or shell overflow", async ({ page, request }) => {
  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 0,
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1);
  await rows.first().click();
  await page.waitForTimeout(400);
  await expect(page.locator('.wb-session-slot[aria-hidden="false"] textarea.wb-active-textarea')).toBeVisible({
    timeout: 20_000,
  });

  const sessionId = seed.sessionIdsByTask[seed.taskIds[0]][0];
  const marker = `table-contract-${Date.now()}`;
  const content = [
    marker,
    "",
    "Using the repo's canonical KPI definition, I queried local metrics for UTC days `2026-04-03` through `2026-04-06`.",
    "",
    "| Day (UTC) | Daily active people | Return users |",
    "|---|---:|---:|",
    "| 2026-04-03 | 34 | 2 |",
    "| 2026-04-04 | 21 | 7 |",
    "| 2026-04-05 | 23 | 4 |",
    "| 2026-04-06 | 8 | `5` |",
    "",
    "Return users means people active on that day whose first valid activity was before that day.",
  ].join("\n");

  const resp = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: { content, delivery: "immediate" },
  });
  expect(resp.ok()).toBeTruthy();

  const assistantEntry = page.locator(".wb-assistant-entry").filter({ hasText: `done: ${marker}` });
  await expect(assistantEntry).toBeVisible({ timeout: 20_000 });
  await expect(assistantEntry.locator(".wb-md-table-scroll")).toHaveCount(1);
  await expect(assistantEntry.locator(".wb-md-table")).toHaveCount(1);

  const report = await page.evaluate((markerValue) => {
    const entries = Array.from(document.querySelectorAll<HTMLElement>(".wb-assistant-entry"));
    const entry = entries.find((element) => element.textContent?.includes(`done: ${markerValue}`));
    if (!entry) return null;
    const tableWrapper = entry.querySelector<HTMLElement>(".wb-md-table-scroll");
    const table = tableWrapper?.querySelector<HTMLElement>(".wb-md-table") ?? null;
    const inlineCode = table?.querySelector<HTMLElement>("code") ?? null;
    const slot = entry.closest(".wb-pretext-virtualizer-row")?.parentElement ?? null;
    const nextSlot = slot?.nextElementSibling ?? null;
    const nextRow = nextSlot?.querySelector<HTMLElement>(".wb-pretext-virtualizer-row") ?? null;
    if (!tableWrapper || !table || !slot || !nextRow) return null;
    const blocks = Array.from(entry.querySelectorAll<HTMLElement>(".wb-assistant-body > div > *"));
    const lastBlock = blocks.at(-1) ?? null;
    const slotRect = slot.getBoundingClientRect();
    const nextRect = nextRow.getBoundingClientRect();
    const tableRect = tableWrapper.getBoundingClientRect();
    const lastBlockRect = lastBlock?.getBoundingClientRect() ?? null;
    const tableStyle = window.getComputedStyle(table);
    const wrapperStyle = window.getComputedStyle(tableWrapper);
    const inlineCodeStyle = inlineCode ? window.getComputedStyle(inlineCode) : null;
    const doc = document.documentElement;
    const body = document.body;
    const sessionView = document.querySelector(".wb-session-view") as HTMLElement | null;
    return {
      overlapPx: slotRect.bottom - nextRect.top,
      slotToNextGapPx: nextRect.top - slotRect.bottom,
      lastBlockToNextGapPx: lastBlockRect ? nextRect.top - lastBlockRect.bottom : null,
      slotContentSlackPx: lastBlockRect ? slotRect.bottom - lastBlockRect.bottom : null,
      tableLayout: tableStyle.tableLayout,
      borderCollapse: tableStyle.borderCollapse,
      wrapperOverflowX: wrapperStyle.overflowX,
      inlineCodeStyles: inlineCodeStyle
        ? {
            paddingTop: inlineCodeStyle.paddingTop,
            paddingRight: inlineCodeStyle.paddingRight,
            paddingBottom: inlineCodeStyle.paddingBottom,
            paddingLeft: inlineCodeStyle.paddingLeft,
            borderTopWidth: inlineCodeStyle.borderTopWidth,
            fontFamily: inlineCodeStyle.fontFamily,
          }
        : null,
      blocks: blocks.map((block) => ({
        tag: block.tagName,
        className: block.className,
        text: (block.textContent ?? "").replace(/\s+/g, " ").trim().slice(0, 120),
        top: block.getBoundingClientRect().top,
        bottom: block.getBoundingClientRect().bottom,
        height: block.getBoundingClientRect().height,
      })),
      docScrollWidth: doc.scrollWidth,
      docClientWidth: doc.clientWidth,
      bodyScrollWidth: body.scrollWidth,
      bodyClientWidth: body.clientWidth,
      sessionScrollWidth: sessionView?.scrollWidth ?? 0,
      sessionClientWidth: sessionView?.clientWidth ?? 0,
    };
  }, marker);

  expect(report).not.toBeNull();
  expect(report?.tableLayout).toBe("fixed");
  expect(report?.borderCollapse).toBe("collapse");
  expect(report?.wrapperOverflowX).toBe("auto");
  expect(report?.inlineCodeStyles).toMatchObject({
    paddingTop: "1px",
    paddingRight: "6px",
    paddingBottom: "1px",
    paddingLeft: "6px",
    borderTopWidth: "1px",
  });
  expect(report?.inlineCodeStyles?.fontFamily.toLowerCase()).toContain("mono");
  expect(report?.overlapPx ?? 0).toBeLessThanOrEqual(1);
  expect(
    Math.abs((report?.slotContentSlackPx ?? Number.NEGATIVE_INFINITY) - ASSISTANT_ENTRY_BOTTOM_PADDING_PX),
  ).toBeLessThanOrEqual(ASSISTANT_ENTRY_PADDING_TOLERANCE_PX);
  expect(report?.lastBlockToNextGapPx ?? Number.POSITIVE_INFINITY).toBeLessThanOrEqual(
    MAX_ASSISTANT_TO_NEXT_ROW_CHROME_GAP_PX,
  );
  expect(report?.docScrollWidth ?? Number.POSITIVE_INFINITY).toBeLessThanOrEqual((report?.docClientWidth ?? 0) + 1);
  expect(report?.bodyScrollWidth ?? Number.POSITIVE_INFINITY).toBeLessThanOrEqual((report?.bodyClientWidth ?? 0) + 1);
  expect(report?.sessionScrollWidth ?? Number.POSITIVE_INFINITY).toBeLessThanOrEqual(
    (report?.sessionClientWidth ?? 0) + 1,
  );
});

test("workbench: fenced code blocks stay within thread width", async ({ page, request }) => {
  await page.context().grantPermissions(["clipboard-write"]);
  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 0,
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1);
  await rows.first().click();
  await page.waitForTimeout(400);
  await expect(page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea")).toBeVisible({ timeout: 20000 });

  const sessionId = seed.sessionIdsByTask[seed.taskIds[0]][0];
  const marker = `fenced-test-${Date.now()}`;
  const longLine = "--flag=abcdefghijklmnopqrstuvwxyz0123456789".repeat(40);
  const content = `${marker}\n\n\`\`\`text\n${longLine}\n\`\`\``;

  const resp = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: { content, delivery: "immediate" },
  });
  expect(resp.ok()).toBeTruthy();

  const assistantEntry = page.locator(".wb-assistant-entry").filter({ hasText: `done: ${marker}` });
  await expect(assistantEntry).toBeVisible({ timeout: 20000 });

  const codeblock = assistantEntry.locator(".codeblock");
  await expect(codeblock).toBeVisible();

  const copyButton = assistantEntry.locator(".codeblock-copy");
  await expect(copyButton).toBeVisible();
  await copyButton.click();
  await expect(copyButton).toHaveAttribute("title", "Copied");

  const widths = await page.evaluate(() => {
    const sessionView = document.querySelector(".wb-session-view") as HTMLElement | null;
    const codeblockEl = document.querySelector(".wb-assistant-entry .codeblock") as HTMLElement | null;
    if (!sessionView || !codeblockEl) return null;
    const sessionRect = sessionView.getBoundingClientRect();
    const codeRect = codeblockEl.getBoundingClientRect();
    return {
      sessionWidth: sessionRect.width,
      codeWidth: codeRect.width,
    };
  });

  expect(widths).not.toBeNull();
  if (widths) {
    expect(widths.codeWidth).toBeLessThanOrEqual(widths.sessionWidth + 1);
  }

  const scrollMetrics = await page.evaluate(() => {
    const scroller = document.querySelector(
      ".wb-assistant-entry .codeblock-body > pre, .wb-assistant-entry .codeblock-body > div",
    ) as HTMLElement | null;
    if (!scroller) return null;
    return {
      scrollWidth: scroller.scrollWidth,
      clientWidth: scroller.clientWidth,
    };
  });

  expect(scrollMetrics).not.toBeNull();
  if (scrollMetrics) {
    expect(scrollMetrics.scrollWidth).toBeGreaterThan(scrollMetrics.clientWidth);
  }

  const pageMetrics = await page.evaluate(() => {
    const doc = document.documentElement;
    const body = document.body;
    const sessionView = document.querySelector(".wb-session-view") as HTMLElement | null;
    return {
      docScrollWidth: doc.scrollWidth,
      docClientWidth: doc.clientWidth,
      bodyScrollWidth: body.scrollWidth,
      bodyClientWidth: body.clientWidth,
      sessionScrollWidth: sessionView?.scrollWidth ?? 0,
      sessionClientWidth: sessionView?.clientWidth ?? 0,
    };
  });

  expect(pageMetrics.docScrollWidth).toBeLessThanOrEqual(pageMetrics.docClientWidth + 1);
  expect(pageMetrics.bodyScrollWidth).toBeLessThanOrEqual(pageMetrics.bodyClientWidth + 1);
  expect(pageMetrics.sessionScrollWidth).toBeLessThanOrEqual(pageMetrics.sessionClientWidth + 1);
});

test("workbench: nested lists, blockquotes, and long markdown tokens stay deterministic", async ({ page, request }) => {
  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 0,
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1);
  await rows.first().click();
  await page.waitForTimeout(400);
  await expect(page.locator('.wb-session-slot[aria-hidden="false"] textarea.wb-active-textarea')).toBeVisible({
    timeout: 20_000,
  });

  const sessionId = seed.sessionIdsByTask[seed.taskIds[0]][0];
  const marker = `complex-markdown-${Date.now()}`;
  const longToken = `artifact${"segment".repeat(40)}`;
  const longUrl = `https://example.com/${Array.from({ length: 16 }, (_, index) => `section-${index + 1}`).join("/")}/${longToken}`;
  const content = [
    marker,
    "",
    "# Release Readiness",
    "",
    "> We only want one scroll owner at a time, and we need deterministic layout even when markdown gets awkward.",
    "",
    "1. Transcript checks",
    "   - keep semantic invalidation wired through the runtime",
    `   - verify long token wrapping for ${longToken}`,
    "   - confirm there is no overlap before the trailing status row",
    "2. UI checks",
    "   - route composer wheel input to exactly one owner",
    `   - preserve blockquotes and links like [release canary](${longUrl}) without blowing out the session width`,
    "",
    "> Follow-up",
    "> - blockquotes should stay visually bounded",
    "> - nested markdown should not leave a fake tail gap",
    "",
    `Tail paragraph with one more raw token: ${longToken}`,
  ].join("\n");

  const resp = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: { content, delivery: "immediate" },
  });
  expect(resp.ok()).toBeTruthy();

  const assistantEntry = page.locator(".wb-assistant-entry").filter({ hasText: `done: ${marker}` });
  await expect(assistantEntry).toBeVisible({ timeout: 20_000 });
  await expect(assistantEntry.locator("blockquote")).toHaveCount(2);

  const overlapAndGap = await page.evaluate((markerValue) => {
    const entries = Array.from(document.querySelectorAll<HTMLElement>(".wb-assistant-entry"));
    const entry = entries.find((element) => element.textContent?.includes(`done: ${markerValue}`));
    if (!entry) return null;
    const slot = entry.closest(".wb-pretext-virtualizer-row")?.parentElement ?? null;
    const nextSlot = slot?.nextElementSibling ?? null;
    const nextRow = nextSlot?.querySelector<HTMLElement>(".wb-pretext-virtualizer-row") ?? null;
    if (!slot || !nextRow) return null;
    const slotRect = slot.getBoundingClientRect();
    const nextRect = nextRow.getBoundingClientRect();
    const lastBlock = Array.from(entry.querySelectorAll<HTMLElement>(".wb-assistant-body > div > *")).at(-1) ?? null;
    const lastBlockRect = lastBlock?.getBoundingClientRect() ?? null;
    return {
      slotToNextGapPx: nextRect.top - slotRect.bottom,
      lastBlockToNextGapPx: lastBlockRect ? nextRect.top - lastBlockRect.bottom : null,
      overlapPx: slotRect.bottom - nextRect.top,
    };
  }, marker);

  expect(overlapAndGap).not.toBeNull();
  expect(overlapAndGap?.overlapPx ?? 0).toBeLessThanOrEqual(1);
  expect(overlapAndGap?.slotToNextGapPx ?? Number.NEGATIVE_INFINITY).toBeGreaterThanOrEqual(-1);
  expect(overlapAndGap?.lastBlockToNextGapPx ?? Number.NEGATIVE_INFINITY).toBeGreaterThanOrEqual(-1);

  const pageMetrics = await page.evaluate(() => {
    const doc = document.documentElement;
    const body = document.body;
    const sessionView = document.querySelector(".wb-session-view") as HTMLElement | null;
    return {
      docScrollWidth: doc.scrollWidth,
      docClientWidth: doc.clientWidth,
      bodyScrollWidth: body.scrollWidth,
      bodyClientWidth: body.clientWidth,
      sessionScrollWidth: sessionView?.scrollWidth ?? 0,
      sessionClientWidth: sessionView?.clientWidth ?? 0,
    };
  });

  expect(pageMetrics.docScrollWidth).toBeLessThanOrEqual(pageMetrics.docClientWidth + 1);
  expect(pageMetrics.bodyScrollWidth).toBeLessThanOrEqual(pageMetrics.bodyClientWidth + 1);
  expect(pageMetrics.sessionScrollWidth).toBeLessThanOrEqual(pageMetrics.sessionClientWidth + 1);
});
