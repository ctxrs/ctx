import { rmSync, writeFileSync } from "fs";
import path from "path";
import type { APIRequestContext, Page } from "@playwright/test";
import { test, expect } from "./fixtures";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { createTempGitRepo } from "./utils/testRepo";
import {
  buildVisualName,
  captureVisual,
  setVisualTheme,
  visualViewportLabel,
  waitForVisualSettled,
  type VisualTheme,
} from "./utils/visual";
import { newTaskComposer, selectFakeHarness } from "./utils/visualWorkbench";

const THEMES = ["dark", "light"] as const satisfies VisualTheme[];

const readId = (value: unknown): string => (typeof value === "string" ? value : "");

const asRecord = (value: unknown): Record<string, unknown> =>
  value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};

const asArray = (value: unknown): unknown[] => (Array.isArray(value) ? value : []);

async function createWorkspaceAndStartRun(opts: {
  page: Page;
  request: APIRequestContext;
  repo: string;
  workspaceName: string;
  prompt: string;
  theme: VisualTheme;
}) {
  const { page, repo, workspaceName, prompt, request, theme } = opts;
  await setVisualTheme(page, theme);
  const workspaceId = await createWorkspaceAndOpenWorkbench({
    page,
    request,
    repo,
    workspaceName,
  });
  await setVisualTheme(page, theme);
  await selectFakeHarness(page);
  await newTaskComposer(page).fill(prompt);
  await page.getByRole("button", { name: "Send" }).click();

  const sessionComposer = page.locator(".wb-session-slot textarea.wb-active-textarea");
  await expect(sessionComposer).toBeVisible({ timeout: 20_000 });

  let sessionId = "";
  let worktreeId = "";
  let worktreeRoot = "";
  await expect
    .poll(
      async () => {
        const response = await request.get(`/api/workspaces/${workspaceId}/active_snapshot`);
        if (!response.ok()) return false;
        const snapshot = asRecord(await response.json());
        const active = asRecord(snapshot.active);
        const taskSummary = asRecord(asArray(active.tasks)[0]);
        const sessions = asArray(taskSummary.sessions).map((entry) => asRecord(entry));
        const sessionSummary = sessions.length > 0 ? sessions[sessions.length - 1] : {};
        const primarySessionId = readId(asRecord(taskSummary.task).primary_session_id);
        sessionId = readId(asRecord(sessionSummary.session).id) || primarySessionId;
        if (!sessionId) return false;
        const headResponse = await request.get(`/api/sessions/${sessionId}/snapshot?limit=1`);
        if (!headResponse.ok()) return false;
        const headSnapshot = asRecord(await headResponse.json());
        const head = asRecord(headSnapshot.head);
        const summary = asRecord(headSnapshot.summary);
        const session = asRecord(head.session ?? summary.session);
        worktreeId = readId(session.worktree_id);
        if (!worktreeId) return false;
        const worktreeResponse = await request.get(`/api/worktrees/${worktreeId}`);
        if (!worktreeResponse.ok()) return false;
        const worktree = asRecord(await worktreeResponse.json());
        worktreeRoot = String(worktree.root_path ?? "");
        return Boolean(worktreeRoot);
      },
      { timeout: 20_000 },
    )
    .toBeTruthy();

  return { sessionId, worktreeId, worktreeRoot, workspaceId };
}

async function waitForWorktreeSummary(opts: {
  request: APIRequestContext;
  workspaceId: string;
  worktreeId: string;
  timeoutMs?: number;
}) {
  const { request, workspaceId, worktreeId, timeoutMs = 20_000 } = opts;
  await expect
    .poll(
      async () => {
        const response = await request.get(`/api/workspaces/${workspaceId}/active_snapshot?limit=5`);
        if (!response.ok()) return null;
        const snapshot = (await response.json()) as {
          worktree_vcs_snapshots?: Array<{
            worktree_id?: string;
            compute_state?: string;
            summary?: { file_count?: number | null; line_count?: number | null };
          }>;
        };
        const entry = snapshot.worktree_vcs_snapshots?.find((item) => item.worktree_id === worktreeId);
        if (!entry || entry.compute_state !== "ready") return null;
        return Number(entry.summary?.file_count ?? entry.summary?.line_count ?? 0);
      },
      { timeout: timeoutMs },
    )
    .toBeGreaterThan(0);
}

async function waitForNoRepoUnavailable(opts: {
  request: APIRequestContext;
  workspaceId: string;
  worktreeId: string;
}) {
  await expect
    .poll(
      async () => {
        const response = await opts.request.get(`/api/workspaces/${opts.workspaceId}/active_snapshot?limit=5`);
        if (!response.ok()) return false;
        const snapshot = (await response.json()) as {
          worktree_vcs_snapshots?: Array<{
            worktree_id?: string;
            available?: boolean;
            unavailable_reason?: string;
          }>;
        };
        const entry = snapshot.worktree_vcs_snapshots?.find((item) => item.worktree_id === opts.worktreeId);
        return entry?.available === false && entry?.unavailable_reason === "no_repo";
      },
      { timeout: 20_000 },
    )
    .toBeTruthy();
}

test.describe.serial("visual: diff pane", () => {
  for (const theme of THEMES) {
    test(`diff ready ${theme}`, async ({ page, request }) => {
      await page.setViewportSize({ width: 1600, height: 900 });
      const repo = createTempGitRepo({
        prefix: "ctx-e2e-visual-diff-",
        files: [{ path: "file.txt", content: "hello\n" }],
      });
      const { workspaceId, worktreeId, worktreeRoot } = await createWorkspaceAndStartRun({
        page,
        request,
        repo,
        workspaceName: `ws-visual-diff-ready-${Date.now()}`,
        prompt: "visual diff ready",
        theme,
      });
      writeFileSync(path.join(worktreeRoot, "file.txt"), "hello\nvisual diff\n");
      await waitForWorktreeSummary({ request, workspaceId, worktreeId });
      await page.getByRole("button", { name: "Toggle diff view" }).click();
      const diffPane = page.locator(".wb-right-pane.wb-diff");
      await expect(diffPane).toBeVisible({ timeout: 20_000 });
      await waitForVisualSettled(page, { ready: diffPane });
      await captureVisual(
        page,
        buildVisualName(["diff-pane", "ready", theme, visualViewportLabel("diff-wide")]),
        { ready: diffPane },
      );
    });

    test(`diff fetch failure ${theme}`, async ({ page, request }) => {
      await page.setViewportSize({ width: 1600, height: 900 });
      const repo = createTempGitRepo({
        prefix: "ctx-e2e-visual-diff-",
        files: [{ path: "file.txt", content: "hello\n" }],
      });
      const { workspaceId, worktreeId, worktreeRoot } = await createWorkspaceAndStartRun({
        page,
        request,
        repo,
        workspaceName: `ws-visual-diff-failure-${Date.now()}`,
        prompt: "visual diff fetch failure",
        theme,
      });
      writeFileSync(path.join(worktreeRoot, "file.txt"), "hello\nbroken diff\n");
      await waitForWorktreeSummary({ request, workspaceId, worktreeId });
      await page.route(/\/api\/sessions\/[^/]+\/diff$/, async (route) => {
        await route.fulfill({
          status: 500,
          contentType: "application/json",
          body: JSON.stringify({ error: "simulated diff fetch failure" }),
        });
      });
      await page.getByRole("button", { name: "Toggle diff view" }).click();
      const diffPane = page.locator(".wb-right-pane.wb-diff");
      await expect(diffPane).toContainText("Failed to load diff content", { timeout: 20_000 });
      await captureVisual(
        page,
        buildVisualName(["diff-pane", "fetch-failure", theme, visualViewportLabel("diff-wide")]),
        { ready: diffPane },
      );
    });

    test(`diff no repo ${theme}`, async ({ page, request }) => {
      await page.setViewportSize({ width: 1600, height: 900 });
      const repo = createTempGitRepo({
        prefix: "ctx-e2e-visual-diff-",
        files: [{ path: "file.txt", content: "hello\n" }],
      });
      const { workspaceId, worktreeId, worktreeRoot } = await createWorkspaceAndStartRun({
        page,
        request,
        repo,
        workspaceName: `ws-visual-diff-no-repo-${Date.now()}`,
        prompt: "visual diff no repo",
        theme,
      });
      rmSync(path.join(worktreeRoot, ".git"), { recursive: true, force: true });
      await waitForNoRepoUnavailable({ request, workspaceId, worktreeId });
      await page.getByRole("button", { name: "Toggle diff view" }).click();
      const diffPane = page.locator(".wb-right-pane.wb-diff");
      await expect(diffPane).toContainText("No git repo detected for this workspace yet.", { timeout: 20_000 });
      await captureVisual(
        page,
        buildVisualName(["diff-pane", "no-repo", theme, visualViewportLabel("diff-wide")]),
        { ready: diffPane },
      );
    });
  }
});
