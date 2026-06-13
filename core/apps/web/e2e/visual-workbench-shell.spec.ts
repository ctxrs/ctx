import { test, expect } from "./fixtures";
import type { Page } from "@playwright/test";
import { seedDummyWorkspace } from "./utils/seedDummyWorkspace";
import {
  buildVisualName,
  captureVisual,
  visualViewportLabel,
  type VisualTheme,
  type VisualViewportName,
} from "./utils/visual";
import {
  newTaskComposer,
  openHarnessMenu,
  openFirstTaskSession,
  openWorkbenchVisualPage,
  selectFakeHarness,
} from "./utils/visualWorkbench";

const THEMES = ["dark", "light"] as const satisfies VisualTheme[];
const EMPTY_VIEWPORTS = ["desktop", "narrow"] as const satisfies VisualViewportName[];

test.describe.serial("visual: workbench shell", () => {
  test.describe.configure({ timeout: 180_000 });
  let emptyWorkspaceId = "";
  let archivedWorkspaceId = "";
  let activeWorkspaceId = "";
  let activeSessionId = "";

  test.beforeAll(async ({ request }) => {
    test.setTimeout(180_000);
    const empty = await seedDummyWorkspace(request, {
      tasks: 0,
      sessionsPerTask: 0,
      turnsPerSession: 0,
    });
    emptyWorkspaceId = empty.workspaceId;

    const archived = await seedDummyWorkspace(request, {
      tasks: 12,
      sessionsPerTask: 0,
      turnsPerSession: 0,
      throttleMs: 0,
    });
    archivedWorkspaceId = archived.workspaceId;
    for (const taskId of archived.taskIds.slice(-8)) {
      const response = await request.post(`/api/tasks/${taskId}/archive`, {});
      expect(response.ok()).toBeTruthy();
    }

    const active = await seedDummyWorkspace(request, {
      tasks: 1,
      sessionsPerTask: 1,
      turnsPerSession: 1,
      throttleMs: 0,
    });
    activeWorkspaceId = active.workspaceId;
    activeSessionId = active.sessionIdsByTask[active.taskIds[0] ?? ""]?.[0] ?? "";
  });

  const seedActiveSession = async (page: Page) => {
    await openFirstTaskSession(page);
    await expect
      .poll(async () => page.locator(".wb-turn-header-content").count(), { timeout: 20_000 })
      .toBeGreaterThan(0);
  };

  const attachComposerImage = async (page: Page) => {
    await newTaskComposer(page).evaluate((el) => {
      const base64Png =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+lmZYAAAAASUVORK5CYII=";
      const bytes = Uint8Array.from(atob(base64Png), (c) => c.charCodeAt(0));
      const file = new File([bytes], "drop.png", { type: "image/png" });
      const dataTransfer = new DataTransfer();
      dataTransfer.items.add(file);

      const rect = el.getBoundingClientRect();
      const clientX = Math.floor(rect.left + rect.width / 2);
      const clientY = Math.floor(rect.top + rect.height / 2);

      el.dispatchEvent(
        new DragEvent("dragover", {
          bubbles: true,
          cancelable: true,
          clientX,
          clientY,
          dataTransfer,
        }),
      );
      el.dispatchEvent(
        new DragEvent("drop", {
          bubbles: true,
          cancelable: true,
          clientX,
          clientY,
          dataTransfer,
        }),
      );
    });
  };

  for (const theme of THEMES) {
    for (const viewport of EMPTY_VIEWPORTS) {
      test(`empty workbench ${theme} ${viewport}`, async ({ page }) => {
        await openWorkbenchVisualPage(page, emptyWorkspaceId, { theme, viewport });
        await expect(newTaskComposer(page)).toBeVisible({ timeout: 20_000 });
        await captureVisual(
          page,
          buildVisualName(["workbench-shell", "empty", theme, visualViewportLabel(viewport)]),
        );
      });
    }

    test(`harness menu ${theme}`, async ({ page }) => {
      await openWorkbenchVisualPage(page, emptyWorkspaceId, { theme, viewport: "desktop-tight" });
      const menu = await openHarnessMenu(page);
      await captureVisual(
        page,
        buildVisualName(["workbench-shell", "harness-menu-open", theme, visualViewportLabel("desktop-tight")]),
        { ready: menu },
      );
    });

    test(`slash commands ${theme}`, async ({ page }) => {
      await openWorkbenchVisualPage(page, emptyWorkspaceId, { theme, viewport: "narrow" });
      const composer = newTaskComposer(page);
      await composer.fill("/");
      const autocomplete = page.locator(".composer-ac");
      await expect(autocomplete).toBeVisible({ timeout: 20_000 });
      await captureVisual(
        page,
        buildVisualName(["workbench-shell", "slash-commands-open", theme, visualViewportLabel("narrow")]),
        { ready: autocomplete },
      );
    });

    test(`archived tasks ${theme}`, async ({ page }) => {
      await openWorkbenchVisualPage(page, archivedWorkspaceId, { theme, viewport: "desktop-tight" });
      await page.getByRole("button", { name: "Archived Tasks" }).click();
      const archivedRows = page.getByRole("listitem").filter({ hasText: /fixture task/i }).first();
      await expect(archivedRows).toBeVisible({ timeout: 20_000 });
      await captureVisual(
        page,
        buildVisualName(["workbench-shell", "archived-open", theme, visualViewportLabel("desktop-tight")]),
        { ready: archivedRows },
      );
    });

    test(`mixed task list ${theme}`, async ({ page, request }) => {
      const mixed = await seedDummyWorkspace(request, {
        tasks: 3,
        sessionsPerTask: 1,
        turnsPerSession: 1,
        throttleMs: 0,
      });
      await openWorkbenchVisualPage(page, mixed.workspaceId, { theme, viewport: "desktop" });
      await expect(page.locator(".wb-task-row")).toHaveCount(3, { timeout: 20_000 });
      await selectFakeHarness(page);
      const composer = newTaskComposer(page);
      await composer.fill(`slow-diff-test visual-shell-running-${theme}`);
      await page.getByRole("button", { name: "Send" }).click();
      await expect(page.locator('.wb-session-slot button[aria-label="Stop"]')).toBeVisible({
        timeout: 20_000,
      });
      await expect(page.locator(".wb-task-row")).toHaveCount(4, { timeout: 20_000 });
      await captureVisual(
        page,
        buildVisualName(["workbench-shell", "mixed-task-list", theme, visualViewportLabel("desktop")]),
      );
    });

    test(`warm switch end state ${theme}`, async ({ page, request }) => {
      const switched = await seedDummyWorkspace(request, {
        tasks: 2,
        sessionsPerTask: 1,
        turnsPerSession: 2,
        throttleMs: 0,
      });
      await openWorkbenchVisualPage(page, switched.workspaceId, { theme, viewport: "desktop-tight" });
      const rows = page.locator(".wb-task-row");
      await expect(rows).toHaveCount(2, { timeout: 20_000 });
      await rows.nth(0).click();
      await expect(page.locator(".wb-session-slot .wb-turn-header-content").first()).toBeVisible({
        timeout: 20_000,
      });
      await rows.nth(1).click();
      await expect(page.locator(".wb-session-slot .wb-turn-header-content").first()).toBeVisible({
        timeout: 20_000,
      });
      await captureVisual(
        page,
        buildVisualName(["workbench-shell", "warm-switch-end-state", theme, visualViewportLabel("desktop-tight")]),
      );
    });

    test(`composer image attachment ${theme}`, async ({ page }) => {
      await openWorkbenchVisualPage(page, activeWorkspaceId, { theme, viewport: "desktop-tight" });
      await openFirstTaskSession(page);
      await page.getByRole("button", { name: "New Task" }).click();
      await expect(newTaskComposer(page)).toBeVisible({ timeout: 20_000 });
      await attachComposerImage(page);
      const attachments = page.locator(".wb-composer-attachments");
      await expect(attachments.locator(".wb-attach-thumb-img")).toHaveCount(1, { timeout: 20_000 });
      await captureVisual(
        page,
        buildVisualName(["workbench-shell", "composer-image-attachment", theme, visualViewportLabel("desktop-tight")]),
        { ready: attachments },
      );
    });

    test(`terminal panel ${theme}`, async ({ page }) => {
      await openWorkbenchVisualPage(page, activeWorkspaceId, { theme, viewport: "desktop-tight" });
      await seedActiveSession(page);
      await page.getByRole("button", { name: "Toggle terminal panel" }).click();
      const terminalPane = page.locator(".wb-terminal-panel-inner");
      await expect(terminalPane).toBeVisible({ timeout: 20_000 });
      await captureVisual(
        page,
        buildVisualName(["workbench-shell", "terminal-panel-open", theme, visualViewportLabel("desktop-tight")]),
        { ready: terminalPane },
      );
    });

    test(`artifacts pane ${theme}`, async ({ page }) => {
      let legacyArtifactListRequests = 0;
      let stateRequests = 0;
      const stateRequestSessionIds = new Set<string>();
      await page.route(/\/api\/sessions\/[^/]+\/artifacts$/, async (route) => {
        legacyArtifactListRequests += 1;
        await route.fulfill({
          status: 500,
          contentType: "application/json",
          body: JSON.stringify({ error: "legacy artifact list route should not be used" }),
        });
      });
      await page.route(/\/api\/sessions\/[^/]+\/state$/, async (route) => {
        stateRequests += 1;
        const requestUrl = new URL(route.request().url());
        const requestMatch = requestUrl.pathname.match(/\/api\/sessions\/([^/]+)\/state$/);
        if (requestMatch) {
          stateRequestSessionIds.add(requestMatch[1] ?? "");
        }
        await route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({
            artifacts: [
              {
                id: "artifact-1",
                session_id: activeSessionId,
                task_id: "task-1",
                workspace_id: activeWorkspaceId,
                worktree_id: "worktree-1",
                name: "state-artifact.bin",
                absolute_path: "/tmp/state-artifact.bin",
                mime_type: "application/octet-stream",
                bytes: 32,
                created_at: "2024-01-01T00:00:00.000Z",
              },
            ],
            git_status: null,
          }),
        });
      });
      await openWorkbenchVisualPage(page, activeWorkspaceId, { theme, viewport: "desktop-tight" });
      await seedActiveSession(page);
      await page.getByRole("button", { name: "Toggle artifacts" }).click();
      const artifactsPane = page.locator(".wb-artifacts");
      await expect(artifactsPane).toContainText("state-artifact.bin", { timeout: 20_000 });
      expect(legacyArtifactListRequests).toBe(0);
      expect(stateRequests).toBeGreaterThan(0);
      expect(stateRequestSessionIds.has(activeSessionId)).toBe(true);
      await captureVisual(
        page,
        buildVisualName(["workbench-shell", "artifacts-pane-open", theme, visualViewportLabel("desktop-tight")]),
        { ready: artifactsPane },
      );
    });
  }
});
