import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";

const visibleSessionSelector = '.wb-session-slot [data-testid="session-view"]';
const visibleScrollerSelector = `${visibleSessionSelector} .wb-thread-scroller`;

type InterruptSample = {
  t: number;
  statuses: string[];
  visibleRowCount: number;
  visibleRowIds: string[];
  promptVisible: boolean;
  projectionStatuses: string[];
  projectionSources: string[];
};

async function readVisibleSessionId(page: Parameters<typeof test>[0]["page"]): Promise<string> {
  return page.evaluate((selector) => {
    const node = document.querySelector(selector) as HTMLElement | null;
    return node?.getAttribute("data-session-id")?.trim() ?? "";
  }, visibleSessionSelector);
}

const buildInterruptPrompt = (marker: string) => {
  const toolCalls = Array.from({ length: 6 }, (_, index) => ({
    kind: "execute",
    title: `interrupt tool ${index + 1}`,
    input: { command: `printf '${marker}-${index + 1}'` },
    output_text: `${marker} output ${index + 1}`,
  }));
  const body = Array.from({ length: 80 }, (_, index) => `interrupt fixture line ${index + 1}`).join("\n");
  return `interrupt transition stability ${marker}
slow-diff-test
${body}
[[tool_calls]]
${JSON.stringify(toolCalls)}
[[/tool_calls]]`;
};

async function readInterruptSample(page: Parameters<typeof test>[0]["page"], marker: string): Promise<InterruptSample> {
  return page.evaluate(({ inputMarker, sessionSelector, scrollerSelector }) => {
    const root = document.querySelector(sessionSelector) as HTMLElement | null;
    const scroller = document.querySelector(scrollerSelector) as HTMLElement | null;
    const scrollerRect = scroller?.getBoundingClientRect() ?? null;
    const rows = Array.from(
      root?.querySelectorAll<HTMLElement>('.wb-thread-scroller [role="listitem"][data-thread-item-id]') ?? [],
    )
      .map((node) => {
        const rect = node.getBoundingClientRect();
        const clone = node.cloneNode(true) as HTMLElement;
        clone.querySelectorAll(".wb-turn-status-label").forEach((element) => element.remove());
        return {
          id: node.getAttribute("data-thread-item-id") ?? "",
          top: rect.top,
          bottom: rect.bottom,
          text: clone.innerText.trim(),
        };
      })
      .filter((row) => {
        if (!scrollerRect) return row.text.length > 0;
        return row.bottom > scrollerRect.top + 1 && row.top < scrollerRect.bottom - 1;
      })
      .sort((left, right) => left.top - right.top);
    const projectionStore = (window as Window & {
      __wbSessionThreadProjectionDebug?: {
        entries?: Array<{
          lastTurnStatus?: string | null;
          source?: string;
        }>;
      };
    }).__wbSessionThreadProjectionDebug;
    const projectionEntries = Array.isArray(projectionStore?.entries)
      ? projectionStore.entries.slice(-30)
      : [];
    return {
      t: performance.now(),
      statuses: Array.from(root?.querySelectorAll<HTMLElement>(".wb-turn-status-label") ?? []).map((node) =>
        node.innerText.trim(),
      ),
      visibleRowCount: rows.length,
      visibleRowIds: rows.map((row) => row.id),
      promptVisible: rows.some((row) => row.text.includes(inputMarker)),
      projectionStatuses: projectionEntries
        .map((entry) => String(entry.lastTurnStatus ?? "").trim())
        .filter((value) => value.length > 0),
      projectionSources: projectionEntries
        .map((entry) => String(entry.source ?? "").trim())
        .filter((value) => value.length > 0),
    };
  }, {
    inputMarker: marker,
    sessionSelector: visibleSessionSelector,
    scrollerSelector: visibleScrollerSelector,
  });
}

test("workbench: interrupt transition does not bounce status or visibly reset the active thread", async ({
  page,
}, testInfo) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1400, height: 900 });

  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceName = `ws-${Date.now()}`;
  const workspaceId = await createWorkspaceAndOpenWorkbench({ page, request: page.request, repo, workspaceName });
  await page.goto(`/workspaces/${workspaceId}?debug=1`, { waitUntil: "domcontentloaded" });
  await expect(page.locator(".wb-main")).toBeVisible({ timeout: 20_000 });

  await selectHarnessBySearch(page, "fake", /fake/i);

  const marker = `interrupt-marker-${Date.now()}`;
  await page.locator("textarea.wb-composer-textarea").first().fill(buildInterruptPrompt(marker));
  await page.getByRole("button", { name: "Send" }).click();

  await expect(page.locator(visibleScrollerSelector).first()).toBeVisible({ timeout: 20_000 });
  await expect(page.getByText("Agent output updating.")).toBeVisible({ timeout: 20_000 });
  await expect
    .poll(async () => readVisibleSessionId(page), {
      timeout: 20_000,
      message: "active session view never exposed data-session-id after sending the prompt",
    })
    .not.toBe("");
  const sessionId = await readVisibleSessionId(page);
  expect(sessionId).not.toBe("");

  const baseline = await readInterruptSample(page, marker);
  expect(baseline.visibleRowCount, "baseline visible rows missing before interrupt").toBeGreaterThan(0);
  expect(baseline.promptVisible, "prompt row should be visible before interrupt").toBe(true);

  const beforeInterruptShot = await page.screenshot({ type: "png", animations: "disabled", caret: "hide" });
  const interruptResponse = await page.request.post(`/api/sessions/${sessionId}/interrupt`, {
    data: {},
    timeout: 10_000,
  });
  expect(interruptResponse.ok(), `interrupt POST failed: ${interruptResponse.url()}`).toBeTruthy();
  await page.waitForTimeout(1600);
  const afterInterruptShot = await page.screenshot({ type: "png", animations: "disabled", caret: "hide" });

  const samples: InterruptSample[] = [];
  for (let index = 0; index < 24; index += 1) {
    samples.push(await readInterruptSample(page, marker));
    if (index < 23) await page.waitForTimeout(60);
  }

  const debugState = await page.evaluate(() => {
    const messageListStore = (window as Window & {
      __wbSessionMessageListDebug?: {
        flashTraces?: unknown[];
        entries?: unknown[];
      };
    }).__wbSessionMessageListDebug;
    const projectionStore = (window as Window & {
      __wbSessionThreadProjectionDebug?: {
        entries?: unknown[];
      };
    }).__wbSessionThreadProjectionDebug;
    return {
      flashTraces: Array.isArray(messageListStore?.flashTraces) ? messageListStore.flashTraces.slice(-30) : [],
      debugEntries: Array.isArray(messageListStore?.entries) ? messageListStore.entries.slice(-60) : [],
      projectionEntries: Array.isArray(projectionStore?.entries) ? projectionStore.entries.slice(-60) : [],
    };
  });

  const firstInterruptedIndex = samples.findIndex((sample) =>
    sample.statuses.some((status) => /interrupted/i.test(status)),
  );

  const summary = {
    marker,
    baseline,
    firstInterruptedIndex,
    samples,
    debugState,
  };

  await testInfo.attach("interrupt-before.png", {
    body: beforeInterruptShot,
    contentType: "image/png",
  });
  await testInfo.attach("interrupt-after.png", {
    body: afterInterruptShot,
    contentType: "image/png",
  });
  await testInfo.attach("interrupt-transition-summary.json", {
    body: JSON.stringify(summary, null, 2),
    contentType: "application/json",
  });

  expect(firstInterruptedIndex, "interrupt transition never surfaced interrupted status").toBeGreaterThanOrEqual(0);
  const samplesAfterInterrupted = firstInterruptedIndex >= 0 ? samples.slice(firstInterruptedIndex + 1) : [];
  expect(
    samplesAfterInterrupted.some((sample) =>
      sample.statuses.some((status) => /running|working|queued/i.test(status)),
    ),
    "status bounced back to a non-terminal working state after interrupted became visible",
  ).toBe(false);
  expect(
    samples.some((sample) => sample.visibleRowCount === 0),
    "active thread visibly dropped all rendered rows during interrupt transition",
  ).toBe(false);
  expect(
    samples.some((sample) => !sample.promptVisible),
    "prompt row disappeared from the visible thread during interrupt transition",
  ).toBe(false);
  expect(
    debugState.flashTraces.some((trace) => {
      const record = trace as { cause?: string; snapbackDetected?: boolean };
      return Boolean(record.snapbackDetected) || /^data:replace$/.test(String(record.cause ?? ""));
    }),
    "message list recorded a replace/snapback flash during interrupt transition",
  ).toBe(false);
  expect(
    (() => {
      if (firstInterruptedIndex < 0 || samplesAfterInterrupted.length === 0) return false;
      const projectionEntries = Array.isArray(debugState.projectionEntries) ? debugState.projectionEntries : [];
      const firstInterruptedProjectionIndex = projectionEntries.findIndex((entry) => {
        const record = entry as { lastTurnStatus?: string | null };
        return /interrupted/i.test(String(record.lastTurnStatus ?? ""));
      });
      if (firstInterruptedProjectionIndex < 0) return false;
      return projectionEntries.slice(firstInterruptedProjectionIndex + 1).some((entry) => {
        const record = entry as { lastTurnStatus?: string | null };
        return /running|queued/i.test(String(record.lastTurnStatus ?? ""));
      });
    })(),
    "projection debug still shows working/queued states after interrupt settled",
  ).toBe(false);
});
