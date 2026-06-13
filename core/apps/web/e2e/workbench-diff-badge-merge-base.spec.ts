import { test, expect } from "./fixtures";
import { existsSync, mkdtempSync, readdirSync, rmSync, statSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import type { APIRequestContext, Page } from "@playwright/test";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

const asArray = (value: unknown): unknown[] => (Array.isArray(value) ? value : []);

const readId = (value: unknown): string => (typeof value === "string" ? value : "");

const readNumber = (value: unknown): number | null => {
  const n = typeof value === "number" ? value : Number(value);
  return Number.isFinite(n) ? n : null;
};

async function createWorkspaceAndStartRun(opts: {
  page: Page;
  request: APIRequestContext;
  repo: string;
  workspaceName: string;
  prompt: string;
  primaryBranch?: string;
}) {
  const { page, repo, workspaceName, prompt, request, primaryBranch } = opts;
  const workspaceId = await createWorkspaceAndOpenWorkbench({
    page,
    request,
    repo,
    workspaceName,
  });
  if (primaryBranch) {
    const resp = await request.post(`/api/workspaces/${workspaceId}/primary_branch`, {
      data: { primary_branch: primaryBranch },
    });
    expect(resp.ok()).toBeTruthy();
  }

  await selectHarnessBySearch(page, "fake", /fake/i);

  await page.locator("textarea.wb-composer-textarea").first().fill(prompt);
  await page.getByRole("button", { name: "Send" }).click();

  const sessionComposer = page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea");
  await expect(sessionComposer).toBeVisible({ timeout: 20_000 });

  let sessionId = "";
  await expect
    .poll(
      async () => {
        const resp = await request.get(`/api/workspaces/${workspaceId}/active_snapshot`);
        if (!resp.ok()) return "";
        const snapshot = asRecord(await resp.json());
        const active = asRecord(snapshot.active);
        const taskSummary = asRecord(asArray(active.tasks)[0]);
        if (!taskSummary) return "";
        const sessions = asArray(taskSummary.sessions);
        const sessionSummary = asRecord(sessions[sessions.length - 1]);
        const primarySessionId = readId(asRecord(taskSummary.task).primary_session_id);
        sessionId = readId(asRecord(sessionSummary.session).id) || primarySessionId;
        return sessionId;
      },
      { timeout: 20_000 },
    )
    .not.toBe("");

  const headResp = await request.get(`/api/sessions/${sessionId}/snapshot?limit=1`);
  expect(headResp.ok()).toBeTruthy();
  const snapshot = asRecord(await headResp.json());
  const session = asRecord(asRecord(snapshot.head).session ?? asRecord(snapshot.summary).session);
  const worktreeId = readId(session.worktree_id);
  expect(worktreeId).toBeTruthy();
  const wtResp = await request.get(`/api/worktrees/${worktreeId}`);
  expect(wtResp.ok()).toBeTruthy();
  const wt = asRecord(await wtResp.json());
  const worktreeRoot = String(wt.root_path ?? "");
  expect(worktreeRoot).toBeTruthy();

  return { sessionId, worktreeRoot, workspaceId, worktreeId };
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
        const resp = await request.get(`/api/workspaces/${workspaceId}/active_snapshot?limit=5`);
        if (!resp.ok()) return null;
        const snapshot = asRecord(await resp.json());
        const entry =
          asArray(snapshot.worktree_vcs_snapshots)
            .map((item) => asRecord(item))
            .find((item) => readId(item.worktree_id) === worktreeId) ?? null;
        if (!entry) return null;
        if (entry.compute_state !== "ready") return null;
        const summary = asRecord(entry.summary);
        const fileCount = readNumber(summary.file_count);
        const lineCount = readNumber(summary.line_count);
        if (fileCount === null && lineCount === null) return null;
        return Number((fileCount ?? lineCount) as number);
      },
      { timeout: timeoutMs },
    )
    .toBeGreaterThan(0);
}

function clearTempWorktreeIndexLocks(repoRoot: string) {
  const gitWorktreesDir = path.join(repoRoot, ".git", "worktrees");
  if (!existsSync(gitWorktreesDir)) return;
  for (const entry of readdirSync(gitWorktreesDir)) {
    const lockPath = path.join(gitWorktreesDir, entry, "index.lock");
    if (!existsSync(lockPath)) continue;
    const stats = statSync(lockPath);
    if (!stats.isFile()) continue;
    rmSync(lockPath, { force: true });
  }
}

test("workbench: merge-base diff badge + pane stay consistent across phases", async ({ page, request }) => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init -b main", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });
  execSync("git branch merge-target", { cwd: repo });

  const workspaceName = `ws-${Date.now()}`;
  const taskTitle = "merge-base-diff-test";

  const { worktreeRoot, workspaceId, worktreeId } = await createWorkspaceAndStartRun({
    page,
    request,
    repo,
    workspaceName,
    prompt: taskTitle,
    primaryBranch: "merge-target",
  });

  const diffButton = page.getByRole("button", { name: "Toggle diff view" });
  const diffBadge = diffButton.locator(".wb-icon-badge");

  writeFileSync(path.join(worktreeRoot, "file.txt"), "hello\nphase1\n");
  await waitForWorktreeSummary({ request, workspaceId, worktreeId });

  await diffButton.click();
  await expect(page.locator(".wb-right-pane.wb-diff")).toBeVisible({ timeout: 10_000 });
  await expect(page.locator(".cursor-diff-file-header").filter({ hasText: "file.txt" })).toBeVisible({
    timeout: 20_000,
  });
  await expect(diffBadge).toHaveText("1", { timeout: 20_000 });
  await expect(
    page.locator(".wb-right-pane.wb-diff").getByText("No changes on this worktree."),
  ).toHaveCount(0);

  clearTempWorktreeIndexLocks(repo);
  execSync("git add file.txt", { cwd: worktreeRoot });
  execSync("git commit -m phase1", { cwd: worktreeRoot });
  const phase1Sha = execSync("git rev-parse HEAD", { cwd: worktreeRoot }).toString().trim();
  execSync(`git branch -f merge-target ${phase1Sha}`, { cwd: repo });
  await diffButton.click();
  await expect(page.locator(".wb-right-pane.wb-diff")).toHaveCount(0, { timeout: 10_000 });

  await diffButton.click();
  await expect(page.locator(".wb-right-pane.wb-diff")).toBeVisible({ timeout: 10_000 });
  await expect(diffBadge).toHaveCount(0, { timeout: 20_000 });

  writeFileSync(path.join(worktreeRoot, "file.txt"), "hello\nphase1\nphase2\n");

  await diffButton.click();
  await expect(page.locator(".wb-right-pane.wb-diff")).toHaveCount(0, { timeout: 10_000 });

  await diffButton.click();
  await expect(page.locator(".wb-right-pane.wb-diff")).toBeVisible({ timeout: 10_000 });
  await expect(page.locator(".cursor-diff-file-header").filter({ hasText: "file.txt" })).toBeVisible({
    timeout: 20_000,
  });
  await expect(diffBadge).toHaveText("1", { timeout: 20_000 });
  await expect(
    page.locator(".wb-right-pane.wb-diff").getByText("No changes on this worktree."),
  ).toHaveCount(0);
});

test("workbench: diff badge updates without opening diff pane", async ({ page, request }) => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init -b main", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceName = `ws-${Date.now()}`;
  const taskTitle = "diff-badge-passive";

  const { worktreeRoot, workspaceId, worktreeId } = await createWorkspaceAndStartRun({
    page,
    request,
    repo,
    workspaceName,
    prompt: taskTitle,
  });

  const diffButton = page.getByRole("button", { name: "Toggle diff view" });
  const diffBadge = diffButton.locator(".wb-icon-badge");

  await expect(page.locator(".wb-right-pane.wb-diff")).toHaveCount(0);
  await expect(diffBadge).toHaveCount(0);

  writeFileSync(path.join(worktreeRoot, "file.txt"), "hello\npassive\n");
  await waitForWorktreeSummary({ request, workspaceId, worktreeId });

  await expect(diffBadge).toHaveText("1", { timeout: 20_000 });
  await expect(page.locator(".wb-right-pane.wb-diff")).toHaveCount(0);
});
