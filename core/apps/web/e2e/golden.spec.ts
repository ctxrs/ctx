import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";

type GoldenE2EWindow = Window & {
  __ctxE2E?: {
    getActiveTask?: () => { taskId: string | null; sessionId: string | null } | null;
    getSessionHeadMessages?: (sessionId: string) => string[];
    getSessionLastEventSeq?: (sessionId: string) => number | null;
    getSessionProjectionRev?: (sessionId: string) => number | null;
    getVisibleSessionEntryDebug?: () => {
      sessionId: string | null;
      lastEventSeq: number | null;
      messageContents: string[];
    } | null;
    getVisibleSessionThreadDebug?: () => {
      sessionId: string | null;
      projectionRev: number;
      turnsStamp: string;
      messagesStamp: string;
      assistantContents: string[];
      listItemIds: string[];
    } | null;
    getWorkspaceSnapshot?: () => {
      connection?: string;
    } | null;
  };
};

async function captureAssistantState(page: Parameters<typeof test>[0]["page"], expectedText: string) {
  return page.evaluate((text) => {
    const win = window as GoldenE2EWindow;
    const bridge = win.__ctxE2E;
    const activeTask = bridge?.getActiveTask?.() ?? null;
    const sessionId = activeTask?.sessionId ?? null;
    const assistantEntries = Array.from(document.querySelectorAll(".wb-session-slot .wb-assistant-entry"));
    const visibleAssistantText = assistantEntries
      .map((entry) => entry.textContent ?? "")
      .filter((content) => content.length > 0);
    const visible = visibleAssistantText.some((content) => content.includes(text));
    const headMessages = sessionId ? (bridge?.getSessionHeadMessages?.(sessionId) ?? []) : [];
    const visibleEntry = bridge?.getVisibleSessionEntryDebug?.() ?? null;
    const visibleThread = bridge?.getVisibleSessionThreadDebug?.() ?? null;

    return {
      visible,
      activeTask,
      workspaceConnection: bridge?.getWorkspaceSnapshot?.()?.connection ?? null,
      workspaceHeadHasExpected: headMessages.some((content) => content.includes(text)),
      workspaceHeadTail: headMessages.slice(-4),
      workspaceHeadLastEventSeq: sessionId ? (bridge?.getSessionLastEventSeq?.(sessionId) ?? null) : null,
      workspaceHeadProjectionRev: sessionId ? (bridge?.getSessionProjectionRev?.(sessionId) ?? null) : null,
      visibleEntrySessionId: visibleEntry?.sessionId ?? null,
      visibleEntryLastEventSeq: visibleEntry?.lastEventSeq ?? null,
      visibleEntryHasExpected:
        visibleEntry?.messageContents?.some((content) => content.includes(text)) ?? false,
      visibleEntryTail: visibleEntry?.messageContents?.slice(-4) ?? [],
      visibleThreadSessionId: visibleThread?.sessionId ?? null,
      visibleThreadProjectionRev: visibleThread?.projectionRev ?? null,
      visibleThreadHasExpected:
        visibleThread?.assistantContents?.some((content) => content.includes(text)) ?? false,
      visibleThreadTail: visibleThread?.assistantContents?.slice(-4) ?? [],
      visibleThreadListTail: visibleThread?.listItemIds?.slice(-8) ?? [],
      visibleAssistantText,
    };
  }, expectedText);
}

test("golden path: workspace → task → session → message", async ({ page }) => {
  test.setTimeout(120000);
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceName = `ws-${Date.now()}`;

  await createWorkspaceAndOpenWorkbench({ page, request: page.request, repo, workspaceName });

  // Workbench UI: choose Fake harness so the test doesn't depend on external agents.
  await selectHarnessBySearch(page, "fake", /fake/i);

  await page.locator("textarea.wb-composer-textarea").first().fill("hello");
  await page.getByRole("button", { name: "Send" }).click();

  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1, { timeout: 20000 });
  await rows.first().click();

  await expect(page.locator(".wb-session-slot textarea.wb-active-textarea")).toBeVisible({
    timeout: 20000,
  });
  try {
    await expect
      .poll(async () => captureAssistantState(page, "done: hello"), {
        timeout: 60000,
        intervals: [250, 500, 1000, 2000],
      })
      .toMatchObject({ visible: true });
  } catch (error) {
    const state = await captureAssistantState(page, "done: hello");
    throw new Error(
      [
        "golden assistant completion did not become visible",
        `final state: ${JSON.stringify(state, null, 2)}`,
        error instanceof Error ? error.message : String(error),
      ].join("\n"),
    );
  }
});
