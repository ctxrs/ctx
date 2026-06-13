import type { Page } from "playwright/test";
import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";

function createRepo(): string {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });
  return repo;
}

async function pasteImageIntoComposer(
  selector: string,
  page: Page,
  options?: { htmlSrc?: string; includeFile?: boolean; text?: string },
) {
  await page.locator(selector).first().evaluate((el, options?: { htmlSrc?: string; includeFile?: boolean; text?: string }) => {
    const base64Png =
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+lmZYAAAAASUVORK5CYII=";
    const bytes = Uint8Array.from(atob(base64Png), (c) => c.charCodeAt(0));
    const transfer = new DataTransfer();
    if (options?.includeFile ?? true) {
      const file = new File([bytes], "paste.png", { type: "image/png" });
      transfer.items.add(file);
    }
    if (options?.htmlSrc) {
      transfer.setData("text/html", `<img src="${options.htmlSrc}" alt="pasted image">`);
    }
    if (options?.text) {
      transfer.setData("text/plain", options.text);
    }
    const event = new Event("paste", { bubbles: true, cancelable: true });
    Object.defineProperty(event, "clipboardData", { value: transfer });
    el.dispatchEvent(event);
  }, options);
}

test("workbench: New Task composer accepts pasted images", async ({ page }) => {
  const repo = createRepo();
  const workspaceName = `ws-${Date.now()}`;

  await createWorkspaceAndOpenWorkbench({ page, request: page.request, repo, workspaceName });
  await selectHarnessBySearch(page, "fake", /fake/i);

  const newTaskTextarea = page.locator("textarea.wb-composer-textarea").first();
  await expect(newTaskTextarea).toBeVisible({ timeout: 20000 });
  await pasteImageIntoComposer("textarea.wb-composer-textarea", page);

  await expect(page.locator(".wb-composer-attachments .wb-attach-thumb-img")).toHaveCount(1, {
    timeout: 20000,
  });
});

test("workbench: Active session composer accepts pasted images", async ({ page }) => {
  const repo = createRepo();
  const workspaceName = `ws-${Date.now()}`;

  await createWorkspaceAndOpenWorkbench({ page, request: page.request, repo, workspaceName });
  await selectHarnessBySearch(page, "fake", /fake/i);

  await page.locator("textarea.wb-composer-textarea").first().fill("hello 1");
  await page.getByRole("button", { name: "Send" }).click();

  const activeTextarea = page.locator(".wb-session-slot textarea.wb-active-textarea");
  await expect(activeTextarea).toBeVisible({ timeout: 20000 });
  await pasteImageIntoComposer(".wb-session-slot textarea.wb-active-textarea", page);

  await expect(page.locator(".wb-session-slot .wb-composer-attachments .wb-attach-thumb-img")).toHaveCount(1, {
    timeout: 20000,
  });
});

test("workbench: Pasting an image file also preserves plain text in the composer", async ({ page }) => {
  const repo = createRepo();
  const workspaceName = `ws-${Date.now()}`;

  await createWorkspaceAndOpenWorkbench({ page, request: page.request, repo, workspaceName });
  await selectHarnessBySearch(page, "fake", /fake/i);

  const newTaskTextarea = page.locator("textarea.wb-composer-textarea").first();
  await expect(newTaskTextarea).toBeVisible({ timeout: 20000 });
  await newTaskTextarea.fill("before ");
  await pasteImageIntoComposer("textarea.wb-composer-textarea", page, { text: "caption" });

  await expect(newTaskTextarea).toHaveValue("before caption");
  await expect(page.locator(".wb-composer-attachments .wb-attach-thumb-img")).toHaveCount(1, {
    timeout: 20000,
  });
});

test("workbench: New Task composer accepts pasted html image sources", async ({ page }) => {
  const repo = createRepo();
  const workspaceName = `ws-${Date.now()}`;

  await createWorkspaceAndOpenWorkbench({ page, request: page.request, repo, workspaceName });
  await selectHarnessBySearch(page, "fake", /fake/i);

  const newTaskTextarea = page.locator("textarea.wb-composer-textarea").first();
  await expect(newTaskTextarea).toBeVisible({ timeout: 20000 });
  await pasteImageIntoComposer("textarea.wb-composer-textarea", page, {
    htmlSrc: "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+lmZYAAAAASUVORK5CYII=",
    includeFile: false,
  });

  await expect(page.locator(".wb-composer-attachments .wb-attach-thumb-img")).toHaveCount(1, {
    timeout: 20000,
  });
});
