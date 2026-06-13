import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import type { Locator, Page } from "playwright/test";

const AUTH_TOKEN = process.env.CTX_E2E_AUTH_TOKEN ?? "ctx-e2e-auth-token";

type TerminalBufferSnapshot = {
  baseY?: number;
  viewportY?: number;
  ydisp?: number;
  length?: number;
};

type TerminalEntry = {
  element?: HTMLElement;
  rows?: number;
  scrollToBottom?: () => void;
  buffer?: { active?: TerminalBufferSnapshot };
};

type TerminalRegistryWindow = Window & {
  __ctxE2ETerminals?: Map<string, TerminalEntry>;
};

type TerminalSession = {
  id: unknown;
  status: string;
};

type TaskRecord = {
  id: string;
};

const readTerminalId = (value: unknown): string | undefined => {
  if (typeof value === "string" && value) return value;
  if (Array.isArray(value) && typeof value[0] === "string" && value[0]) return value[0];
  return undefined;
};

test("terminal scroll stays consistent after closing and reopening panel while output streams", async ({ page }) => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-terminal-scroll-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");

  const streamScriptPath = path.join(repo, "ctx-e2e-stream.sh");
  writeFileSync(
    streamScriptPath,
    [
      "#!/bin/sh",
      "i=1",
      "while [ \"$i\" -le 3000 ]; do",
      "  printf 'line %s\\n' \"$i\"",
      "  if [ $((i % 10)) -eq 0 ]; then",
      "    sleep 0.25",
      "  fi",
      "  i=$((i + 1))",
      "done",
      "sleep 5",
      "",
    ].join("\n"),
  );
  execSync("chmod +x ctx-e2e-stream.sh", { cwd: repo });

  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceName = `ws-terminal-scroll-${Date.now()}`;
  const workspaceId = await createWorkspaceAndOpenWorkbench({
    page,
    request: page.request,
    repo,
    workspaceName,
    token: AUTH_TOKEN,
  });

  // Seed a deterministic task/session so this test only validates terminal behavior.
  await createTaskAndSessionForWorkspace(page, workspaceId);

  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1, { timeout: 20_000 });
  await rows.first().click();
  await expect(page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea")).toBeVisible({ timeout: 20_000 });

  const terminalId = await createStreamingWorkspaceTerminal(page, workspaceId, streamScriptPath);

  // TerminalPanel only refreshes terminal list on mount; reload to pick up API-created terminal.
  await page.reload({ waitUntil: "domcontentloaded" });
  await expect(page.locator(".wb-main")).toBeVisible({ timeout: 20_000 });
  await expect(rows).toHaveCount(1, { timeout: 20_000 });
  await rows.first().click();
  await expect(page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea")).toBeVisible({ timeout: 20_000 });

  await openTerminalPanel(page);

  const panel = page.locator(".wb-terminal-panel-inner");
  await ensureWorkspaceScope(panel);
  await selectRunningTerminalTabById(page, panel, terminalId);
  await expect(panel.locator(".wb-terminal-tab-active.wb-terminal-tab-exited")).toHaveCount(0);
  await expect.poll(() => waitForVisibleTerminalId(page), { timeout: 20_000 }).toBe(terminalId);

  await expect
    .poll(() => getScrollState(page, terminalId).then((s) => s.present), { timeout: 20_000 })
    .toBe(true);
  await expect
    .poll(() => isTerminalVisibleById(page, terminalId), { timeout: 30_000 })
    .toBe(true);

  try {
    const startLength = (await getScrollState(page, terminalId)).length;

    await expect
      .poll(() => getScrollState(page, terminalId).then((s) => s.length), { timeout: 20_000 })
      .toBeGreaterThanOrEqual(startLength + 80);
    const beforeCloseLength = (await getScrollState(page, terminalId)).length;

    await closeTerminalPanel(page);

    // Let output accumulate while the panel is closed.
    await page.waitForTimeout(2500);

    await openTerminalPanel(page);
    await ensureWorkspaceScope(panel);
    await selectRunningTerminalTabById(page, panel, terminalId);
    await expect(panel.locator(".wb-terminal-tab-active.wb-terminal-tab-exited")).toHaveCount(0);
    await expect
      .poll(() => isTerminalVisibleById(page, terminalId), { timeout: 30_000 })
      .toBe(true);

    // Ensure new output arrived while the panel was closed.
    await expect
      .poll(() => getScrollState(page, terminalId).then((s) => s.length), { timeout: 20_000 })
      .toBeGreaterThanOrEqual(beforeCloseLength + 80);

    const state = await getScrollState(page, terminalId);
    const bottomTarget = Math.max(0, state.length - state.rows);
    await scrollTerminalViewportToBottom(page, terminalId);
    await page.waitForTimeout(200);
    await scrollTerminalViewportToBottom(page, terminalId);

    await expect
      .poll(
        async () => {
          const current = await getScrollState(page, terminalId);
          const viewportY = current.ydisp ?? current.viewportY;
          if (viewportY === null) return false;
          return viewportY >= bottomTarget;
        },
        { timeout: 20_000 },
      )
      .toBe(true);
  } finally {
    // Ensure we don't leave a runaway terminal behind if the test fails mid-stream.
    await page.request
      .delete(`/api/terminals/${terminalId}`, { headers: { authorization: `Bearer ${AUTH_TOKEN}` } })
      .catch(() => {});
  }
});

async function createStreamingWorkspaceTerminal(
  page: Page,
  workspaceId: string,
  streamScriptPath: string,
): Promise<string> {
  const resp = await page.request.post(`/api/workspaces/${workspaceId}/terminals`, {
    headers: { authorization: `Bearer ${AUTH_TOKEN}` },
    data: { shell: streamScriptPath },
  });
  expect(resp.ok()).toBeTruthy();
  const terminal = (await resp.json()) as { id: unknown };
  const terminalId = readTerminalId(terminal.id);
  if (!terminalId) throw new Error("failed to parse streaming terminal id");

  await expect
    .poll(
      async () => {
        const listResp = await page.request.get(`/api/workspaces/${workspaceId}/terminals`, {
          headers: { authorization: `Bearer ${AUTH_TOKEN}` },
        });
        if (!listResp.ok()) return "missing";
        const terminals = (await listResp.json()) as TerminalSession[];
        return terminals.find((t) => readTerminalId(t.id) === terminalId)?.status ?? "missing";
      },
      { timeout: 20_000 },
    )
    .toBe("running");

  return terminalId;
}

async function createTaskAndSessionForWorkspace(page: Page, workspaceId: string): Promise<void> {
  const taskResp = await page.request.post(`/api/workspaces/${workspaceId}/tasks`, {
    headers: { authorization: `Bearer ${AUTH_TOKEN}` },
    data: {
      title: "terminal scroll test",
      default_session: {
        provider_id: "fake",
        model_id: "fake-model",
        execution_environment: "host",
      },
    },
  });
  expect(taskResp.ok()).toBeTruthy();
  const task = (await taskResp.json()) as TaskRecord;
  if (!task.id) throw new Error("failed to parse seeded task id");
}

async function openTerminalPanel(page: Page) {
  const terminalToggle = page.getByRole("button", { name: "Toggle terminal panel" }).first();
  await expect(terminalToggle).toBeVisible({ timeout: 20_000 });
  const panel = page.locator(".wb-terminal-panel-inner");
  if (!(await panel.isVisible())) {
    await terminalToggle.click();
  }
  await expect(panel).toBeVisible({ timeout: 20_000 });
}

async function closeTerminalPanel(page: Page) {
  const terminalToggle = page.getByRole("button", { name: "Toggle terminal panel" }).first();
  const panel = page.locator(".wb-terminal-panel-inner");
  if (await panel.isVisible()) {
    await terminalToggle.click();
  }
  await expect(panel).toBeHidden({ timeout: 20_000 });
}

async function getScrollState(
  page: Page,
  terminalId: string,
): Promise<{
  present: boolean;
  baseY: number;
  viewportY: number | null;
  ydisp: number | null;
  length: number;
  rows: number;
}> {
  return await page.evaluate((id: string) => {
    const reg = (window as TerminalRegistryWindow).__ctxE2ETerminals;
    const term = reg?.get(id);
    const buf = term?.buffer?.active;
    return {
      present: !!term,
      baseY: typeof buf?.baseY === "number" ? buf.baseY : 0,
      viewportY: typeof buf?.viewportY === "number" ? buf.viewportY : null,
      ydisp: typeof buf?.ydisp === "number" ? buf.ydisp : null,
      length: typeof buf?.length === "number" ? buf.length : 0,
      rows: typeof term?.rows === "number" ? term.rows : 0,
    };
  }, terminalId);
}

async function isTerminalVisibleById(page: Page, terminalId: string): Promise<boolean> {
  return await page.evaluate((id: string) => {
    const reg = (window as TerminalRegistryWindow).__ctxE2ETerminals;
    const term = reg?.get(id);
    const el = term?.element as HTMLElement | undefined;
    if (!el || !el.isConnected) return false;
    if (el.closest(".wb-terminal-group-hidden")) return false;
    const rect = el.getBoundingClientRect();
    if (rect.width <= 5 || rect.height <= 5) return false;
    const style = window.getComputedStyle(el);
    if (style.visibility === "hidden" || style.display === "none") return false;
    return true;
  }, terminalId);
}

async function ensureWorkspaceScope(panel: Locator) {
  const workspaceButton = panel.getByRole("button", { name: "Workspace" });
  if (!(await workspaceButton.evaluate((node) => node.classList.contains("wb-terminal-scope-active")))) {
    await workspaceButton.click();
  }
  await expect(workspaceButton).toHaveClass(/wb-terminal-scope-active/);
}

async function waitForVisibleTerminalId(page: Page): Promise<string> {
  let terminalId = "";
  await expect
    .poll(
      async () => {
        terminalId = await visibleTerminalIdNow(page);
        return terminalId;
      },
      { timeout: 20_000 },
    )
    .not.toBe("");
  return terminalId;
}

async function visibleTerminalIdNow(page: Page): Promise<string> {
  return await page.evaluate(() => {
    const reg = (window as TerminalRegistryWindow).__ctxE2ETerminals;
    if (!reg) return "";
    for (const [id, term] of reg.entries()) {
      const el = term?.element as HTMLElement | undefined;
      if (!el || !el.isConnected) continue;
      if (el.closest(".wb-terminal-group-hidden")) continue;
      const rect = el.getBoundingClientRect();
      if (rect.width <= 5 || rect.height <= 5) continue;
      const style = window.getComputedStyle(el);
      if (style.visibility === "hidden" || style.display === "none") continue;
      return typeof id === "string" ? id : "";
    }
    return "";
  });
}

async function selectRunningTerminalTabById(page: Page, panel: Locator, terminalId: string) {
  await expect
    .poll(async () => panel.locator(".wb-terminal-tab:not(.wb-terminal-tab-exited)").count(), { timeout: 20_000 })
    .toBeGreaterThan(0);

  await expect
    .poll(
      async () => {
        const tabs = panel.locator(".wb-terminal-tab:not(.wb-terminal-tab-exited)");
        const count = await tabs.count();
        for (let i = 0; i < count; i += 1) {
          const tab = tabs.nth(i);
          await tab.click();
          await page.waitForTimeout(50);
          if ((await visibleTerminalIdNow(page)) === terminalId) return true;
        }
        return false;
      },
      { timeout: 20_000 },
    )
    .toBe(true);
}

async function scrollTerminalViewportToBottom(page: Page, terminalId: string) {
  await page.evaluate((id: string) => {
    const reg = (window as TerminalRegistryWindow).__ctxE2ETerminals;
    const term = reg?.get(id);
    term?.scrollToBottom?.();
    const root = term?.element as HTMLElement | undefined;
    const viewport = root?.querySelector(".xterm-viewport") as HTMLElement | null | undefined;
    if (!viewport) return;
    viewport.scrollTop = viewport.scrollHeight;
    viewport.dispatchEvent(new Event("scroll"));
  }, terminalId);
}
