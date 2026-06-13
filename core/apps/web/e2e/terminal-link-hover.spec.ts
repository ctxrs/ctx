import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import WebSocket from "ws";
import type { RawData } from "ws";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";
import type { Page } from "playwright/test";

const AUTH_TOKEN = process.env.CTX_E2E_AUTH_TOKEN ?? "ctx-e2e-auth-token";
const LINK_TEXT = "https://example.com";
const TERMINAL_DONE = "TERM_LINK_DONE";

type WorkspaceRecord = {
  id: string;
};

type TerminalLookupRecord = {
  id: unknown;
  stream_path?: unknown;
};

type TerminalCellDimensions = {
  width: number;
  height: number;
};

type TerminalBufferLine = {
  translateToString?: (trimRight?: boolean) => string;
};

type TerminalBufferActive = {
  length?: number;
  ydisp?: number;
  getLine?: (index: number) => TerminalBufferLine | undefined;
};

type TerminalLinkState = {
  decorations?: {
    underline?: boolean;
  };
};

type TerminalCurrentLink = {
  link?: {
    text?: string;
  };
  state?: TerminalLinkState;
};

type TerminalEntry = {
  element?: HTMLElement;
  rows?: number;
  _core?: {
    screenElement?: HTMLElement;
    linkifier?: { currentLink?: TerminalCurrentLink };
    _renderService?: { dimensions?: { css?: { cell?: TerminalCellDimensions } } };
  };
  buffer?: { active?: TerminalBufferActive };
};

type TerminalRegistryWindow = Window & {
  __ctxE2ETerminals?: Map<string, TerminalEntry>;
};

const toBuffer = (data: RawData): Buffer => {
  if (typeof data === "string") return Buffer.from(data);
  if (data instanceof ArrayBuffer) return Buffer.from(data);
  if (Array.isArray(data)) {
    return Buffer.concat(
      data.map((chunk) => (typeof chunk === "string" ? Buffer.from(chunk) : Buffer.from(chunk))),
    );
  }
  return Buffer.from(data);
};

test("terminal links underline on modifier hover", async ({ page }) => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-terminal-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceName = `ws-terminal-${Date.now()}`;
  await createWorkspaceAndOpenWorkbench({
    page,
    request: page.request,
    repo,
    workspaceName,
    token: AUTH_TOKEN,
  });

  await selectHarnessBySearch(page, "fake", /fake/i);
  await page.locator("textarea.wb-composer-textarea").first().fill("terminal link check");
  await page.getByRole("button", { name: "Send" }).click();

  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1, { timeout: 20_000 });
  await rows.first().click();
  await expect(page.locator(".wb-session-slot[aria-hidden=\"false\"] textarea.wb-active-textarea")).toBeVisible({ timeout: 20_000 });

  await openTerminalPanel(page);
  await ensureTerminalVisible(page);
  const terminalId = await waitForVisibleTerminalId(page);

  const baseURL = new URL(page.url()).origin;
  await seedTerminalOutput(baseURL, AUTH_TOKEN, terminalId, LINK_TEXT);

  const modifierKey = process.platform === "darwin" ? "Meta" : "Control";

  await hoverLinkAndConfirm(page, terminalId, LINK_TEXT);

  await page.keyboard.down(modifierKey);

  await expect
    .poll(() => getHoveredLinkState(page, terminalId))
    .toMatchObject({ underline: true });

  await page.keyboard.up(modifierKey);
});

async function openTerminalPanel(page: Page) {
  const terminalToggle = page.getByRole("button", { name: "Toggle terminal panel" }).first();
  await expect(terminalToggle).toBeVisible({ timeout: 20_000 });
  const panel = page.locator(".wb-terminal-panel-inner");
  if (!(await panel.isVisible())) {
    await terminalToggle.click();
  }
  await expect(panel).toBeVisible({ timeout: 20_000 });
  await panel
    .getByRole("button", { name: "Workspace" })
    .click()
    .catch(() => {});
}

async function ensureTerminalVisible(page: Page) {
  const panel = page.locator(".wb-terminal-panel-inner");
  const xterm = page.locator(".xterm");
  if (await xterm.count()) {
    await expect(xterm.first()).toBeVisible({ timeout: 30_000 });
    return;
  }
  const openWorktree = page.getByRole("button", { name: "Open worktree terminal" });
  if (await openWorktree.isEnabled()) {
    await openWorktree.click();
  } else {
    await panel.getByRole("button", { name: "New terminal" }).click();
    const tabs = panel.locator(".wb-terminal-tab");
    await expect(tabs.first()).toBeVisible({ timeout: 20_000 });
    await tabs.first().click();
  }
  await expect(xterm.first()).toBeVisible({ timeout: 30_000 });
}

async function waitForVisibleTerminalId(page: Page): Promise<string> {
  let terminalId = "";
  await expect
    .poll(
      async () => {
        terminalId = await page.evaluate(() => {
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
        return terminalId;
      },
      { timeout: 20_000 },
    )
    .not.toBe("");
  return terminalId;
}

async function terminalWsUrl(baseURL: string, token: string, terminalId: string): Promise<string> {
  const workspacesResp = await fetch(new URL("/api/workspaces", baseURL), {
    headers: {
      Authorization: `Bearer ${token}`,
    },
  });
  if (!workspacesResp.ok) {
    throw new Error(`Failed to list workspaces for terminal lookup: ${workspacesResp.status}`);
  }
  const workspaces = (await workspacesResp.json()) as WorkspaceRecord[];
  for (const workspace of workspaces) {
    const terminalsResp = await fetch(new URL(`/api/workspaces/${workspace.id}/terminals`, baseURL), {
      headers: {
        Authorization: `Bearer ${token}`,
      },
    });
    if (!terminalsResp.ok) continue;
    const terminals = (await terminalsResp.json()) as TerminalLookupRecord[];
    const terminal = terminals.find((candidate) => candidate.id === terminalId);
    if (terminal && typeof terminal.stream_path === "string" && terminal.stream_path) {
      const streamResp = await fetch(
        new URL(`/api/terminals/${terminal.id}/stream_token`, baseURL),
        {
          method: "POST",
          headers: {
            Authorization: `Bearer ${token}`,
          },
        },
      );
      if (!streamResp.ok) continue;
      const streamInfo = (await streamResp.json()) as { stream_path?: string };
      if (!streamInfo.stream_path) continue;
      const url = new URL(streamInfo.stream_path, baseURL);
      url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
      return url.toString();
    }
  }
  throw new Error(`Failed to resolve stream path for terminal ${terminalId}`);
}

async function seedTerminalOutput(baseURL: string, token: string, terminalId: string, link: string) {
  const wsUrl = await terminalWsUrl(baseURL, token, terminalId);
  const linkLine = JSON.stringify(`${link}\n`);
  const command = [
    "export PS1='$ ' PROMPT_COMMAND=''",
    "printf '\\033[2J\\033[H'",
    `printf ${linkLine}`,
    `echo ${TERMINAL_DONE}`,
  ].join(" && ");

  await new Promise<void>((resolve, reject) => {
    const ws = new WebSocket(wsUrl);
    const deadline = setTimeout(() => {
      try {
        ws.close();
      } catch {}
      reject(new Error("Timed out seeding terminal output"));
    }, 20_000);

    const finish = () => {
      clearTimeout(deadline);
      try {
        ws.close();
      } catch {}
      resolve();
    };

    ws.on("open", () => {
      ws.send(JSON.stringify({ type: "resize", cols: 120, rows: 30 }));
      ws.send(`${command}\n`);
    });

    ws.on("message", (data: RawData) => {
      const buf = toBuffer(data);
      if (buf.includes(Buffer.from(TERMINAL_DONE))) {
        finish();
      }
    });

    ws.on("error", (err) => {
      clearTimeout(deadline);
      reject(err);
    });
  });
}

async function getTerminalMetrics(
  page: Page,
  terminalId: string,
): Promise<{
  left: number;
  top: number;
  cellWidth: number;
  cellHeight: number;
}> {
  await page.waitForFunction((id: string) => {
    const reg = (window as TerminalRegistryWindow).__ctxE2ETerminals;
    if (!reg) return false;
    const term = reg.get(id);
    const screen = term?._core?.screenElement ?? term?.element?.querySelector?.(".xterm-screen");
    const dims = term?._core?._renderService?.dimensions?.css?.cell;
    if (!screen || !dims) return false;
    const rect = screen.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0 && dims.width > 0 && dims.height > 0;
  }, terminalId);

  const metrics = await page.evaluate((id: string) => {
    const reg = (window as TerminalRegistryWindow).__ctxE2ETerminals;
    if (!reg) return null;
    const term = reg.get(id);
    const screen = term?._core?.screenElement ?? term?.element?.querySelector?.(".xterm-screen");
    const dims = term?._core?._renderService?.dimensions?.css?.cell;
    if (!screen || !dims) return null;
    const rect = screen.getBoundingClientRect();
    const style = window.getComputedStyle(screen);
    const padLeft = parseFloat(style.paddingLeft || "0") || 0;
    const padTop = parseFloat(style.paddingTop || "0") || 0;
    return {
      left: rect.left + padLeft,
      top: rect.top + padTop,
      cellWidth: dims.width,
      cellHeight: dims.height,
    };
  }, terminalId);

  if (!metrics) {
    throw new Error("Failed to read terminal metrics");
  }
  return metrics;
}

async function hoverLinkAndConfirm(page: Page, terminalId: string, linkText: string) {
  const metrics = await getTerminalMetrics(page, terminalId);
  const modifierKey = process.platform === "darwin" ? "Meta" : "Control";
  await page.keyboard.up(modifierKey).catch(() => {});

  for (let attempt = 0; attempt < 3; attempt += 1) {
    const linkCell = await waitForLinkCell(page, terminalId, linkText);
    const hoverX = metrics.left + metrics.cellWidth * (linkCell.col + 0.5);
    const hoverY = metrics.top + metrics.cellHeight * (linkCell.row + 0.5);
    await page.mouse.move(hoverX, hoverY);
    const hovered = await getHoveredLinkState(page, terminalId);
    if (hovered.hasCurrent && hovered.text === linkText) {
      return;
    }
    await page.waitForTimeout(100);
  }

  await expect
    .poll(() => getHoveredLinkState(page, terminalId), { timeout: 5_000 })
    .toMatchObject({
      present: true,
      hasCurrent: true,
      text: linkText,
    });
}

async function getHoveredLinkState(
  page: Page,
  terminalId: string,
): Promise<{
  present: boolean;
  hasCurrent: boolean;
  text: string;
  underline: boolean | null;
}> {
  return await page.evaluate((id: string) => {
    const reg = (window as TerminalRegistryWindow).__ctxE2ETerminals;
    const term = reg?.get(id);
    const linkifier = term?._core?.linkifier;
    const current = linkifier?.currentLink;
    const state = current?.state;
    return {
      present: !!term,
      hasCurrent: !!current,
      text: typeof current?.link?.text === "string" ? current.link.text : "",
      underline:
        typeof state?.decorations?.underline === "boolean"
          ? state.decorations.underline
          : null,
    };
  }, terminalId);
}

async function getLinkCellPosition(
  page: Page,
  terminalId: string,
  text: string,
): Promise<{ row: number; col: number } | null> {
  return await page.evaluate(
    ({ id, needle }: { id: string; needle: string }) => {
      const reg = (window as TerminalRegistryWindow).__ctxE2ETerminals;
      const term = reg?.get(id);
      const buf = term?.buffer?.active;
      if (!term || !buf || typeof buf.length !== "number") return null;
      const ydisp = typeof buf.ydisp === "number" ? buf.ydisp : 0;
      const start = Math.max(0, ydisp);
      const end = Math.min(buf.length - 1, ydisp + term.rows + 5);
      for (let i = start; i <= end; i += 1) {
        const line = buf.getLine?.(i);
        const lineText = line?.translateToString?.(true) ?? "";
        const col = lineText.indexOf(needle);
        if (col === -1) continue;
        const row = i - ydisp;
        if (row < 0 || row >= term.rows) continue;
        return { row, col };
      }
      return null;
    },
    { id: terminalId, needle: text },
  );
}

async function waitForLinkCell(
  page: Page,
  terminalId: string,
  text: string,
): Promise<{ row: number; col: number }> {
  await expect
    .poll(() => getLinkCellPosition(page, terminalId, text), { timeout: 20_000 })
    .not.toBeNull();
  const cell = await getLinkCellPosition(page, terminalId, text);
  if (!cell) {
    throw new Error("Failed to locate link in terminal buffer");
  }
  return cell;
}
