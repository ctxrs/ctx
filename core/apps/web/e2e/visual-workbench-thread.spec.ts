import { test, expect } from "./fixtures";
import type { Page, Response } from "@playwright/test";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";
import { createTempGitRepo } from "./utils/testRepo";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import {
  buildVisualName,
  captureVisual,
  setVisualTheme,
  waitForVisualSettled,
  visualViewportLabel,
  type VisualTheme,
  type VisualViewportName,
} from "./utils/visual";
import {
  activeSessionComposer,
  enableQueuedMessages,
  newTaskComposer,
  openFirstTaskSession,
  openWorkbenchVisualPage,
  selectFakeHarness,
} from "./utils/visualWorkbench";

const THEMES = ["dark", "light"] as const satisfies VisualTheme[];
const TRANSCRIPT_VIEWPORTS = ["desktop", "desktop-tight"] as const satisfies VisualViewportName[];

const toolMarkerFor = (seed: string) =>
  `[[tool_calls]]\n${JSON.stringify([
    {
      kind: "execute",
      title: `Run pwd ${seed}`,
      input: { command: "pwd" },
      output_text: "ok",
    },
  ])}\n[[/tool_calls]]`;

const buildRunningFixturePrompt = () => {
  const toolCalls = Array.from({ length: 6 }, (_, index) => ({
    kind: "execute",
    title: `t${index + 1}`,
    input: { command: `echo ${index + 1}` },
  }));
  const body = Array.from({ length: 60 }, (_, index) => `visual thread running line ${index + 1}`).join("\n");
  return `visual-thread-running
slow-diff-test
${body}
[[tool_calls]]
${JSON.stringify(toolCalls)}
[[/tool_calls]]`;
};

async function setupRunningSession(page: Page, theme: VisualTheme, opts: { queuedMessages?: boolean } = {}) {
  if (opts.queuedMessages) {
    await enableQueuedMessages(page);
  }
  await setVisualTheme(page, theme);
  const repo = createTempGitRepo({
    prefix: "ctx-e2e-visual-thread-",
    files: [{ path: "file.txt", content: "hello\n" }],
  });
  await createWorkspaceAndOpenWorkbench({
    page,
    request: page.request,
    repo,
    workspaceName: `ws-visual-thread-${Date.now()}`,
  });
  await setVisualTheme(page, theme);
  await selectFakeHarness(page);

  const prompt = buildRunningFixturePrompt();
  await newTaskComposer(page).fill(prompt);
  await page.getByRole("button", { name: "Send" }).click();
  await expect(activeSessionComposer(page)).toBeVisible({ timeout: 20_000 });
  await expect
    .poll(async () => readVisibleSessionId(page), {
      timeout: 20_000,
      message: "visible session never exposed a committed session id for running fixture",
    })
    .not.toBe("");
  await expect(page.getByRole("button", { name: "Stop" })).toBeVisible({
    timeout: 20_000,
  });
  await page.waitForTimeout(250);
}

async function queueMessage(page: Page, text: string) {
  const composer = activeSessionComposer(page);
  await composer.fill(text);
  const sendResponse = page.waitForResponse((response: Response) => {
    if (response.request().method() !== "POST") return false;
    if (!/\/api\/sessions\/[^/]+\/messages$/.test(response.url())) return false;
    return (response.request().postData() ?? "").includes(text);
  });
  await page.locator('.wb-session-slot button[aria-label="Send"]').click();
  const response = await sendResponse;
  expect(response.ok()).toBeTruthy();
}

async function queueMessageViaApi(page: Page, sessionId: string, text: string) {
  const response = await page.request.post(`/api/sessions/${sessionId}/messages`, {
    data: {
      content: text,
      delivery: "queued",
    },
  });
  expect(response.ok(), `queued message POST failed: ${response.url()}`).toBeTruthy();
}

async function readVisibleSessionId(page: Page): Promise<string> {
  return page.evaluate(() => {
    const node = document.querySelector('[data-testid="session-view"]') as HTMLElement | null;
    return node?.getAttribute("data-session-id")?.trim() ?? "";
  });
}

async function ensureToolRows(page: Page, sessionId: string) {
  const toolRows = page.locator(".wb-tool-row");
  if ((await toolRows.count()) > 0) {
    return toolRows;
  }

  const seed = `${Date.now()}`;
  const response = await page.request.post(`/api/sessions/${sessionId}/messages`, {
    data: {
      content: `visual dense tool seed ${seed}\n${toolMarkerFor(seed)}`,
      delivery: "immediate",
    },
  });
  expect(response.ok(), `tool seed POST failed: ${response.url()}`).toBeTruthy();
  await expect
    .poll(async () => toolRows.count(), { timeout: 30_000 })
    .toBeGreaterThan(0);
  return toolRows;
}

test.describe.serial("visual: workbench thread", () => {
  test.describe.configure({ timeout: 180_000 });
  let transcriptSeed = { workspaceId: "", taskId: "", sessionId: "" };
  let denseSeed = { workspaceId: "", taskId: "", sessionId: "" };

  test.beforeAll(async ({ request }) => {
    test.setTimeout(180_000);
    const transcript = await seedDummyWorkspace(request, {
      tasks: 1,
      sessionsPerTask: 1,
      turnsPerSession: 3,
      throttleMs: 0,
    });
    transcriptSeed = {
      workspaceId: transcript.workspaceId,
      taskId: transcript.taskIds[0] ?? "",
      sessionId: transcript.sessionIdsByTask[transcript.taskIds[0] ?? ""]?.[0] ?? "",
    };

    const dense = await seedDummyWorkspace(request, {
      tasks: 1,
      sessionsPerTask: 1,
      turnsPerSession: 3,
      throttleMs: 0,
      includeToolSummaries: true,
      toolSummariesPerTurn: 2,
    });
    denseSeed = {
      workspaceId: dense.workspaceId,
      taskId: dense.taskIds[0] ?? "",
      sessionId: dense.sessionIdsByTask[dense.taskIds[0] ?? ""]?.[0] ?? "",
    };
  });

  for (const theme of THEMES) {
    test(`tool rows use muted thread color ${theme}`, async ({ page }) => {
      await openWorkbenchVisualPage(page, denseSeed.workspaceId, { theme, viewport: "narrow" });
      await openFirstTaskSession(page);
      await ensureToolRows(page, denseSeed.sessionId);

      const toolVerb = page.locator(".wb-tool-row .wb-tool-verb").first();
      const toolRest = page.locator(".wb-tool-row .wb-tool-rest").first();
      await expect(toolVerb).toBeVisible();
      await expect(toolRest).toBeVisible();

      const mutedColor = await page.evaluate(() => {
        const probe = document.createElement("div");
        probe.style.color = "var(--muted)";
        document.body.appendChild(probe);
        const color = window.getComputedStyle(probe).color;
        probe.remove();
        return color;
      });

      await expect(toolVerb).toHaveCSS("color", mutedColor);
      await expect(toolRest).toHaveCSS("color", mutedColor);
    });

    test(`assistant rows use updated vertical padding ${theme}`, async ({ page }) => {
      await openWorkbenchVisualPage(page, transcriptSeed.workspaceId, { theme, viewport: "desktop" });
      await openFirstTaskSession(page);

      const assistantEntry = page.locator(".wb-assistant-entry").first();
      await expect(assistantEntry).toBeVisible();
      await expect(assistantEntry).toHaveCSS("padding-top", "10px");
      await expect(assistantEntry).toHaveCSS("padding-right", "2px");
      await expect(assistantEntry).toHaveCSS("padding-bottom", "10px");
      await expect(assistantEntry).toHaveCSS("padding-left", "2px");
    });

    for (const viewport of TRANSCRIPT_VIEWPORTS) {
      test(`transcript ${theme} ${viewport}`, async ({ page }) => {
        await openWorkbenchVisualPage(page, transcriptSeed.workspaceId, { theme, viewport });
        await openFirstTaskSession(page);
        await expect
          .poll(async () => page.locator(".wb-turn-header-content").count(), { timeout: 20_000 })
          .toBeGreaterThan(0);
        await expect
          .poll(async () => page.locator(".wb-assistant-entry").count(), { timeout: 20_000 })
          .toBeGreaterThan(0);
        await captureVisual(
          page,
          buildVisualName(["workbench-thread", "transcript", theme, visualViewportLabel(viewport)]),
        );
      });
    }

    test(`dense thread ${theme}`, async ({ page }) => {
      await openWorkbenchVisualPage(page, denseSeed.workspaceId, { theme, viewport: "narrow" });
      await openFirstTaskSession(page);
      await ensureToolRows(page, denseSeed.sessionId);
      await captureVisual(
        page,
        buildVisualName(["workbench-thread", "dense", theme, visualViewportLabel("narrow")]),
      );
    });

    test(`running ${theme}`, async ({ page }) => {
      await page.setViewportSize({ width: 1400, height: 900 });
      await setupRunningSession(page, theme);
      await captureVisual(
        page,
        buildVisualName(["workbench-thread", "running", theme, visualViewportLabel("desktop")]),
      );
    });

    test(`queued ${theme}`, async ({ page }) => {
      await page.setViewportSize({ width: 1400, height: 900 });
      await setupRunningSession(page, theme, { queuedMessages: true });
      const sessionId = await readVisibleSessionId(page);
      expect(sessionId).not.toBe("");
      const firstQueuedText = `slow-diff-test visual-queued-primary-${theme}`;
      const secondQueuedText = `visual-queued-secondary-${theme}`;
      await queueMessageViaApi(page, sessionId, firstQueuedText);
      await queueMessageViaApi(page, sessionId, secondQueuedText);
      const queuePanel = page.locator(".wb-session .queue-panel");
      await expect(queuePanel).toBeVisible({ timeout: 20_000 });
      await expect(queuePanel).toContainText(secondQueuedText, { timeout: 20_000 });
      await captureVisual(
        page,
        buildVisualName(["workbench-thread", "queued-panel", theme, visualViewportLabel("desktop")]),
        { ready: queuePanel },
      );
    });

    test(`interrupt pending ${theme}`, async ({ page }) => {
      await page.setViewportSize({ width: 1400, height: 900 });
      await setupRunningSession(page, theme);
      const sessionId = await readVisibleSessionId(page);
      expect(sessionId).not.toBe("");

      let releaseInterrupt!: () => void;
      const interruptHeld = new Promise<void>((resolve) => {
        releaseInterrupt = resolve;
      });
      const interruptRoute = `**/api/sessions/${sessionId}/interrupt`;
      let stalledInterruptRequest = false;
      await page.route(interruptRoute, async (route) => {
        if (stalledInterruptRequest) {
          await route.continue();
          return;
        }
        stalledInterruptRequest = true;
        await interruptHeld;
        try {
          await route.fulfill({
            status: 200,
            contentType: "application/json",
            body: "{}",
          });
        } catch (error) {
          if (!(error instanceof Error) || !error.message.includes("Route is already handled")) {
            throw error;
          }
        }
      });

      const stopButton = page.getByRole("button", { name: "Stop" });
      await expect(stopButton).toBeVisible({ timeout: 20_000 });
      await stopButton.click();
      const stoppingButton = page.getByRole("button", { name: "Stopping..." });
      await expect(stoppingButton).toBeVisible({ timeout: 20_000 });
      await captureVisual(
        page,
        buildVisualName(["workbench-thread", "interrupt-pending", theme, visualViewportLabel("desktop")]),
        { ready: stoppingButton },
      );

      releaseInterrupt();
      await page.unroute(interruptRoute);
    });
  }
});
