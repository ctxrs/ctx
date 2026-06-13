import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { execSync } from "child_process";
import { tmpdir } from "os";
import path from "path";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";
import { clearDiagnostics, expectNoUnexpectedDiagnostics, getDiagnostics } from "./utils/diagnostics";
import { expectWsPathOnCanonicalOrigin } from "./utils/wsUrls";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

type E2EWindow = Window & {
  __ctxE2E?: {
    workspaceStream?: {
      getConnectionState?: () => string | null;
    };
  };
  __sendClickAt?: number;
  __optimisticHeaderSeen?: boolean;
  __optimisticHeaderDisappeared?: boolean;
  __optimisticHeaderDuplicated?: boolean;
  __optimisticHeaderItemId?: string | null;
  __emptyPlaceholderSeen?: boolean;
  __emptyPlaceholderObserver?: MutationObserver;
};

test("workbench: first user message renders from stream when head is stale", async ({ page, request }) => {
  test.setTimeout(120000);
  await page.setViewportSize({ width: 1400, height: 900 });

  const seed = await seedDummyWorkspace(request, {
    tasks: 1,
    sessionsPerTask: 1,
    turnsPerSession: 0,
  });
  const sessionId = seed.sessionIdsByTask[seed.taskIds[0]][0];
  const prompt = `first-message-${Date.now()}`;
  let forceStaleHead = true;

  await page.route("**/api/sessions/*/snapshot**", async (route) => {
    const url = route.request().url();
    if (!url.includes(sessionId)) {
      await route.continue();
      return;
    }

    const response = await route.fetch();
    if (!forceStaleHead) {
      await route.fulfill({ response });
      return;
    }

    const snapshot = asRecord(await response.json());
    const snapshotHead = asRecord(snapshot.head);
    const staleHead = {
      ...snapshotHead,
      turns: [],
      messages: [],
      events: [],
      tool_summaries: [],
      has_more_turns: false,
      last_event_seq: 0,
    };
    await route.fulfill({
      response,
      body: JSON.stringify({ ...snapshot, head: staleHead }),
    });
  });

  await page.goto(`/workspaces/${seed.workspaceId}`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(1);
  await rows.first().click();
  await page.waitForTimeout(400);

  const composer = page.locator(".wb-session-slot textarea.wb-active-textarea");
  await expect(composer).toBeVisible({ timeout: 20000 });

  await expect
    .poll(async () =>
      page.evaluate(() => typeof (window as E2EWindow).__ctxE2E?.workspaceStream?.getConnectionState === "function"),
    )
    .toBe(true);

  await expect
    .poll(async () => page.evaluate(() => (window as E2EWindow).__ctxE2E?.workspaceStream?.getConnectionState?.()))
    .toBe("connected");
  await clearDiagnostics(page);

  await composer.fill(prompt);
  await page.locator(".wb-session-slot button[aria-label=\"Send\"]").click();

  const header = page.locator(".wb-turn-header-content").filter({ hasText: prompt });
  await expect(header).toBeVisible({ timeout: 20000 });
  await expectWsPathOnCanonicalOrigin(page, "/api/workspaces/");
  await expectNoUnexpectedDiagnostics(page);
  const streamWarnings = (await getDiagnostics(page)).filter((event) =>
    ["workspace.stream_connect_failed", "workspace.stream_connection_missing"].includes(event.code),
  );
  expect(streamWarnings).toEqual([]);

  forceStaleHead = false;
});

test("workbench: optimistic first user message survives new-task handoff while the first head is stale", async ({ page }) => {
  test.setTimeout(120000);
  await page.setViewportSize({ width: 1400, height: 900 });

  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceName = `ws-${Date.now()}`;
  await createWorkspaceAndOpenWorkbench({ page, request: page.request, repo, workspaceName });
  await selectHarnessBySearch(page, "fake", /fake/i);

  let allowFirstCreateTask: (() => void) | null = null;
  const firstCreateTaskGate = new Promise<void>((resolve) => {
    allowFirstCreateTask = resolve;
  });
  let stalledCreateTask = true;
  await page.route("**/api/workspaces/*/tasks", async (route) => {
    if (stalledCreateTask && route.request().method() === "POST") {
      stalledCreateTask = false;
      await firstCreateTaskGate;
    }
    await route.continue();
  });

  let forceStaleFirstSnapshot = true;
  await page.route("**/api/sessions/*/snapshot**", async (route) => {
    if (!forceStaleFirstSnapshot) {
      await route.continue();
      return;
    }

    forceStaleFirstSnapshot = false;
    const response = await route.fetch();
    const snapshot = asRecord(await response.json());
    const snapshotHead = asRecord(snapshot.head);
    const staleHead = {
      ...snapshotHead,
      turns: [],
      messages: [],
      events: [],
      tool_summaries: [],
      has_more_turns: false,
      last_event_seq: 0,
    };
    await route.fulfill({
      response,
      body: JSON.stringify({ ...snapshot, head: staleHead }),
    });
  });

  const prompt = `optimistic-handoff-${Date.now()}`;
  const composer = page.locator("textarea.wb-composer-textarea").first();
  await expect(composer).toBeVisible({ timeout: 20000 });
  await composer.fill(prompt);

  await page.evaluate((promptText: string) => {
    const w = window as E2EWindow;
    w.__sendClickAt = performance.now();
    w.__optimisticHeaderSeen = false;
    w.__optimisticHeaderDisappeared = false;
    w.__optimisticHeaderDuplicated = false;
    w.__optimisticHeaderItemId = null;
    w.__emptyPlaceholderSeen = false;
    w.__emptyPlaceholderObserver?.disconnect?.();

    const selector = ".wb-session-slot .wb-turn-header-content";
    const monitorWindowMs = 2000;
    const startAt = w.__sendClickAt;

    const getMatches = () =>
      Array.from(document.querySelectorAll(selector))
        .filter((node) => (node.textContent ?? "").includes(promptText))
        .map((node) => ({
          itemId: node.closest("[data-thread-item-id]")?.getAttribute("data-thread-item-id") ?? null,
        }));

    const hasEmptyPlaceholder = () =>
      Array.from(document.querySelectorAll(".wb-session-slot .wb-muted")).some(
        (node) => (node.textContent ?? "").trim() === "Empty",
      );

    const emptyObserver = new MutationObserver(() => {
      if (hasEmptyPlaceholder()) {
        w.__emptyPlaceholderSeen = true;
      }
    });
    emptyObserver.observe(document.body, { childList: true, subtree: true, attributes: true, characterData: true });
    w.__emptyPlaceholderObserver = emptyObserver;

    const tick = () => {
      const elapsed = performance.now() - startAt;
      const matches = getMatches();
      const itemIds = matches.map((match) => match.itemId).filter(Boolean) as string[];
      if (!w.__optimisticHeaderSeen && itemIds.length > 0) {
        w.__optimisticHeaderSeen = true;
        w.__optimisticHeaderItemId = itemIds[0];
      }
      if (w.__optimisticHeaderSeen && w.__optimisticHeaderItemId && !itemIds.includes(w.__optimisticHeaderItemId)) {
        w.__optimisticHeaderDisappeared = true;
      }
      if (new Set(itemIds).size > 1) w.__optimisticHeaderDuplicated = true;
      if (elapsed < monitorWindowMs) requestAnimationFrame(tick);
    };

    requestAnimationFrame(tick);
  }, prompt);

  await page.getByRole("button", { name: "Send" }).click();

  const header = page
    .locator(".wb-session-slot .wb-turn-header-content")
    .filter({ hasText: prompt })
    .first();
  await expect(header).toBeVisible({ timeout: 2000 });
  const headerItemId = await header.evaluate((node) =>
    node.closest("[data-thread-item-id]")?.getAttribute("data-thread-item-id"),
  );
  expect(headerItemId).toBeTruthy();

  allowFirstCreateTask?.();
  await page.waitForTimeout(1000);

  if (headerItemId) {
    await expect(
      page.locator(`[data-thread-item-id="${headerItemId}"] .wb-turn-header-content`),
    ).toBeVisible();
  } else {
    await expect(header).toBeVisible();
  }

  const { headerDisappeared, headerDuplicated, emptyPlaceholderSeen } = await page.evaluate(() => {
    const w = window as E2EWindow;
    return {
      headerDisappeared: Boolean(w.__optimisticHeaderDisappeared),
      headerDuplicated: Boolean(w.__optimisticHeaderDuplicated),
      emptyPlaceholderSeen: Boolean(w.__emptyPlaceholderSeen),
    };
  });

  expect(headerDisappeared).toBe(false);
  expect(headerDuplicated).toBe(false);
  expect(emptyPlaceholderSeen).toBe(false);
});
