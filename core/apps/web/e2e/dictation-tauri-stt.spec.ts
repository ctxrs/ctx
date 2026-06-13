import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import type { APIRequestContext } from "playwright/test";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";

const initRepo = (): string => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });
  return repo;
};

const enableTauriDictation = async (request: APIRequestContext) => {
  const resp = await request.post("/api/settings", {
    data: {
      dictation: {
        enabled: true,
        provider: "tauri_stt",
      },
    },
  });
  expect(resp.ok()).toBeTruthy();
};

const runTauriStt = process.env.CTX_E2E_TAURI_STT === "1";

test.describe("dictation tauri stt", () => {
  test.skip(!runTauriStt, "Requires the Tauri desktop app with tauri-plugin-stt available.");

  test("dictation tauri stt: install model and start", async ({ page, request }) => {
    test.setTimeout(10 * 60 * 1000);

    const repo = initRepo();
    await enableTauriDictation(request);

    const workspaceName = `ws-${Date.now()}`;
    const workspaceId = await createWorkspaceAndOpenWorkbench({ page, request, repo, workspaceName });

    await page.goto("/settings#dictation");

    const modelRow = page.locator(".settings-row", { hasText: "Desktop models" });
    await expect(modelRow).toBeVisible();

    const statusPill = modelRow.locator(".settings-pill");
    const downloadButton = modelRow.getByRole("button", { name: /Download model|Retry download|Downloading/ });
    const downloadCount = await downloadButton.count();

    if (downloadCount > 0) {
      const button = downloadButton.first();
      if (await button.isEnabled()) {
        await button.click();
      }
      await expect(statusPill).toContainText(/Downloading|Installed|Checking/, { timeout: 60_000 });
      await expect(statusPill).toContainText("Installed", { timeout: 9 * 60 * 1000 });
    } else {
      await expect(statusPill).toContainText("Installed", { timeout: 20_000 });
    }

    await page.goto(`/workspaces/${workspaceId}`);
    const recordButton = page.getByRole("button", { name: "Record" });
    await recordButton.click();
    await expect(recordButton).toHaveClass(/wb-icon-active/);

    await recordButton.click();
    await expect(recordButton).not.toHaveClass(/wb-icon-active/);
  });
});
