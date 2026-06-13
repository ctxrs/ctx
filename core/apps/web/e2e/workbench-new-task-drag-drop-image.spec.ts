import type { Page } from "playwright/test";
import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";

async function dragDropImageIntoComposer(selector: string, page: Page) {
  await page.locator(selector).first().evaluate((el) => {
    const base64Png =
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+lmZYAAAAASUVORK5CYII=";
    const bytes = Uint8Array.from(atob(base64Png), (c) => c.charCodeAt(0));
    const file = new File([bytes], "drop.png", { type: "image/png" });
    const dt = new DataTransfer();
    dt.items.add(file);

    const rect = el.getBoundingClientRect();
    const clientX = Math.floor(rect.left + rect.width / 2);
    const clientY = Math.floor(rect.top + rect.height / 2);

    el.dispatchEvent(
      new DragEvent("dragover", {
        bubbles: true,
        cancelable: true,
        clientX,
        clientY,
        dataTransfer: dt,
      }),
    );
    el.dispatchEvent(
      new DragEvent("drop", {
        bubbles: true,
        cancelable: true,
        clientX,
        clientY,
        dataTransfer: dt,
      }),
    );
  });
}

test("workbench: New Task composer accepts drag-dropped images after switching from a session", async ({ page }) => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceName = `ws-${Date.now()}`;

  await createWorkspaceAndOpenWorkbench({ page, request: page.request, repo, workspaceName });

  // Choose Fake harness so the test doesn't depend on external agents.
  await selectHarnessBySearch(page, "fake", /fake/i);

  await page.locator("textarea.wb-composer-textarea").first().fill("hello 1");
  await page.getByRole("button", { name: "Send" }).click();
  await expect(page.locator(".wb-session-slot textarea.wb-active-textarea")).toBeVisible({
    timeout: 20000,
  });

  // Switch into the New Task composer (this previously caused the drop scope to never register).
  await page.getByRole("button", { name: "New Task" }).click();
  const newTaskTextarea = page.locator("textarea.wb-composer-textarea").first();
  await expect(newTaskTextarea).toBeVisible({ timeout: 20000 });
  await dragDropImageIntoComposer("textarea.wb-composer-textarea", page);

  await expect(page.locator(".wb-composer-attachments .wb-attach-thumb-img")).toHaveCount(1, { timeout: 20000 });
});

test("workbench: Active session composer accepts drag-dropped images", async ({ page }) => {
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

  await page.locator("textarea.wb-composer-textarea").first().fill("hello 1");
  await page.getByRole("button", { name: "Send" }).click();

  const activeTextarea = page.locator(".wb-session-slot textarea.wb-active-textarea");
  await expect(activeTextarea).toBeVisible({ timeout: 20000 });
  await dragDropImageIntoComposer(".wb-session-slot textarea.wb-active-textarea", page);

  await expect(page.locator(".wb-session-slot .wb-composer-attachments .wb-attach-thumb-img")).toHaveCount(1, {
    timeout: 20000,
  });
});
