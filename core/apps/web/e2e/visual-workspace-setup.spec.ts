import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import type { Page } from "playwright/test";
import { test, expect } from "./fixtures";
import {
  buildVisualName,
  captureVisual,
  prepareVisualPage,
  visualViewportLabel,
  type VisualTheme,
} from "./utils/visual";

const THEMES = ["dark", "light"] as const satisfies VisualTheme[];

const okHealth = {
  version: "1.0.0",
  daemon_version: "1.0.0",
  pid: 1,
  data_root: "/tmp/ctx",
  daemon_url: "http://127.0.0.1:4399",
  auth_required: false,
  compatibility: {
    desktop_exact_version: "1.0.0",
    mobile_api_min: 1,
    mobile_api_max: 1,
  },
};

const noUpdateCheck = {
  channel: "stable",
  base_url: "https://example.test/functions/v1",
  current_version: "1.0.0",
  latest_version: "1.0.0",
  update_available: false,
};

const wizard = (page: Page) => page.getByTestId("workspace-setup");

async function waitForWizardStepChange(page: Page, previousStep: string | null) {
  await expect
    .poll(async () => wizard(page).getAttribute("data-step-key"), { timeout: 20_000 })
    .not.toBe(previousStep);
}

async function clickWizardStepOption(page: Page, testId: string) {
  const currentStep = await wizard(page).getAttribute("data-step-key");
  await page.getByTestId(testId).evaluate((node: HTMLElement) => node.click());
  await waitForWizardStepChange(page, currentStep);
}

async function setWizardInputValue(page: Page, testId: string, value: string) {
  let lastError: unknown = null;
  for (let attempt = 0; attempt < 5; attempt += 1) {
    try {
      const input = page.getByTestId(testId);
      await expect(input).toBeVisible({ timeout: 20_000 });
      await input.evaluate((node, nextValue) => {
        const element = node as HTMLInputElement | HTMLTextAreaElement;
        const prototype = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
        const descriptor = Object.getOwnPropertyDescriptor(prototype, "value");
        descriptor?.set?.call(element, nextValue);
        element.dispatchEvent(new Event("input", { bubbles: true }));
        element.dispatchEvent(new Event("change", { bubbles: true }));
      }, value);
      return;
    } catch (error) {
      lastError = error;
      await page.waitForTimeout(100);
    }
  }
  throw lastError instanceof Error ? lastError : new Error(`failed to set wizard input '${testId}'`);
}

async function installLauncherRoutes(page: Page) {
  await page.route("**/api/health", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(okHealth),
    });
  });
  await page.route("**/api/updates/check**", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(noUpdateCheck),
    });
  });
  await page.route("**/api/desktop/log", async (route) => {
    await route.fulfill({ status: 204, body: "" });
  });
}

async function openWizard(page: Page, theme: VisualTheme) {
  await installLauncherRoutes(page);
  await prepareVisualPage(page, {
    theme,
    viewport: "fullpage",
    route: "/workspace-setup",
    ready: page.getByTestId("workspace-setup"),
  });
}

async function moveWizardToStep(
  page: Page,
  targetStep: string,
  opts: { newSourcePath: string; workspaceName: string },
) {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    const stepKey = await wizard(page).getAttribute("data-step-key");
    if (stepKey === targetStep) return;

    switch (stepKey) {
      case "location":
        await clickWizardStepOption(page, "wizard-option-location-local");
        break;
      case "container":
        await clickWizardStepOption(page, "wizard-option-container-host");
        break;
      case "harness-downloads": {
        const skip = page.getByTestId("wizard-harness-skip");
        if ((await skip.count()) > 0) {
          await skip.click();
        } else {
          const next = page.getByTestId("wizard-next");
          await expect(next).toBeEnabled({ timeout: 20_000 });
          await next.click();
        }
        break;
      }
      case "auth-import":
        await page.getByRole("button", { name: "Skip for now" }).click();
        break;
      case "session-titling":
        await page.getByTestId("wizard-titling-skip").click();
        break;
      case "source":
        if (targetStep === "source") return;
        if ((await page.getByTestId("wizard-option-source-new").getAttribute("aria-pressed")) !== "true") {
          await page.getByTestId("wizard-option-source-new").evaluate((node: HTMLElement) => node.click());
        }
        await setWizardInputValue(page, "wizard-source-path", opts.newSourcePath);
        if ((await page.getByTestId("wizard-workspace-name").count()) > 0) {
          await setWizardInputValue(page, "wizard-workspace-name", opts.workspaceName);
        }
        await page.getByTestId("wizard-next").click();
        break;
      case "setup":
        if (targetStep === "setup") return;
        await page.getByTestId("wizard-next").click();
        break;
      case "merge-queue":
        if (targetStep === "merge-queue") return;
        await setWizardInputValue(page, "wizard-merge-target-branch", "main");
        await expect(page.getByTestId("wizard-next")).toBeEnabled({ timeout: 20_000 });
        await page.getByTestId("wizard-next").click();
        break;
      default: {
        const next = page.getByTestId("wizard-next");
        await expect(next).toBeEnabled({ timeout: 20_000 });
        await next.click();
        break;
      }
    }
  }

  throw new Error(`failed to reach wizard step '${targetStep}'`);
}

test.describe.serial("visual: workspace setup wizard", () => {
  for (const theme of THEMES) {
    test(`location local ${theme}`, async ({ page }) => {
      await openWizard(page, theme);
      await expect(wizard(page)).toHaveAttribute("data-step-key", "location");
      await captureVisual(
        page,
        buildVisualName(["workspace-setup", "location-local", theme, visualViewportLabel("fullpage")]),
        { ready: wizard(page) },
      );
    });

    test(`location remote ${theme}`, async ({ page }) => {
      await openWizard(page, theme);
      await page.getByTestId("wizard-option-location-remote").click();
      await expect(page.getByTestId("wizard-remote-host")).toBeVisible({ timeout: 20_000 });
      await captureVisual(
        page,
        buildVisualName(["workspace-setup", "location-remote", theme, visualViewportLabel("fullpage")]),
        { ready: page.getByTestId("wizard-remote-host") },
      );
    });

    test(`source clone ${theme}`, async ({ page }) => {
      await openWizard(page, theme);
      const tempRoot = mkdtempSync(path.join(tmpdir(), "ctx-e2e-visual-wizard-clone-"));
      await moveWizardToStep(page, "source", {
        newSourcePath: path.join(tempRoot, "workspace"),
        workspaceName: "Visual Clone Workspace",
      });
      await page.getByTestId("wizard-option-source-clone").click();
      await setWizardInputValue(page, "wizard-source-path", `${tempRoot}/`);
      await setWizardInputValue(page, "wizard-repo-url", "https://github.com/contextual-ai/ctx.git");
      await setWizardInputValue(page, "wizard-repo-branch", "main");
      await captureVisual(
        page,
        buildVisualName(["workspace-setup", "source-clone", theme, visualViewportLabel("fullpage")]),
        { ready: page.getByTestId("wizard-repo-url") },
      );
    });

    test(`import init modal ${theme}`, async ({ page }) => {
      await openWizard(page, theme);
      const importDir = mkdtempSync(path.join(tmpdir(), "ctx-e2e-visual-wizard-import-"));
      writeFileSync(path.join(importDir, "README.md"), "import me\n");
      await moveWizardToStep(page, "source", {
        newSourcePath: path.join(importDir, "workspace"),
        workspaceName: "Visual Import Workspace",
      });
      await page.getByTestId("wizard-option-source-import").click();
      await setWizardInputValue(page, "wizard-source-path", importDir);
      await expect(page.getByTestId("wizard-next")).toBeEnabled({ timeout: 20_000 });
      await page.getByTestId("wizard-next").click();
      await expect(wizard(page)).toHaveAttribute("data-step-key", "setup");
      await page.getByTestId("wizard-next").click();
      await expect(wizard(page)).toHaveAttribute("data-step-key", "merge-queue");
      await setWizardInputValue(page, "wizard-merge-target-branch", "main");
      await page.getByTestId("wizard-next").click();
      await expect(wizard(page)).toHaveAttribute("data-step-key", "confirm");
      await page.getByRole("button", { name: "Create workspace" }).click();
      const modal = page.getByTestId("wizard-import-init-modal");
      await expect(modal).toBeVisible({ timeout: 20_000 });
      await captureVisual(
        page,
        buildVisualName(["workspace-setup", "import-init-modal", theme, visualViewportLabel("fullpage")]),
        { ready: modal },
      );
    });

    test(`merge queue advanced ${theme}`, async ({ page }) => {
      await openWizard(page, theme);
      const tempRoot = mkdtempSync(path.join(tmpdir(), "ctx-e2e-visual-wizard-merge-"));
      await moveWizardToStep(page, "source", {
        newSourcePath: path.join(tempRoot, "workspace"),
        workspaceName: "Visual Merge Workspace",
      });
      await page.getByTestId("wizard-option-source-new").evaluate((node: HTMLElement) => node.click());
      await expect(page.getByTestId("wizard-source-path")).toBeVisible({ timeout: 20_000 });
      await setWizardInputValue(page, "wizard-source-path", path.join(tempRoot, "workspace"));
      if ((await page.getByTestId("wizard-workspace-name").count()) > 0) {
        await setWizardInputValue(page, "wizard-workspace-name", "Visual Merge Workspace");
      }
      await expect(page.getByTestId("wizard-next")).toBeEnabled({ timeout: 20_000 });
      await page.getByTestId("wizard-next").click();
      await expect(wizard(page)).toHaveAttribute("data-step-key", "setup");
      await page.getByTestId("wizard-next").click();
      await expect(wizard(page)).toHaveAttribute("data-step-key", "merge-queue");
      await setWizardInputValue(page, "wizard-merge-target-branch", "main");
      await setWizardInputValue(page, "wizard-merge-verify-command", "./verify.sh");
      await page.getByTestId("wizard-merge-advanced-toggle").click();
      await page.getByTestId("wizard-merge-push-on-success").check();
      await setWizardInputValue(page, "wizard-merge-push-remote", "origin");
      await setWizardInputValue(page, "wizard-merge-push-branch", "main");
      await captureVisual(
        page,
        buildVisualName(["workspace-setup", "merge-queue-advanced", theme, visualViewportLabel("fullpage")]),
        { ready: page.getByTestId("wizard-merge-push-branch") },
      );
    });

    test(`confirm summary ${theme}`, async ({ page }) => {
      await openWizard(page, theme);
      const tempRoot = mkdtempSync(path.join(tmpdir(), "ctx-e2e-visual-wizard-confirm-"));
      await moveWizardToStep(page, "source", {
        newSourcePath: path.join(tempRoot, "workspace"),
        workspaceName: "Visual Confirm Workspace",
      });
      await page.getByTestId("wizard-option-source-new").evaluate((node: HTMLElement) => node.click());
      await expect(page.getByTestId("wizard-source-path")).toBeVisible({ timeout: 20_000 });
      await setWizardInputValue(page, "wizard-source-path", path.join(tempRoot, "workspace"));
      if ((await page.getByTestId("wizard-workspace-name").count()) > 0) {
        await setWizardInputValue(page, "wizard-workspace-name", "Visual Confirm Workspace");
      }
      await expect(page.getByTestId("wizard-next")).toBeEnabled({ timeout: 20_000 });
      await page.getByTestId("wizard-next").click();
      await expect(wizard(page)).toHaveAttribute("data-step-key", "setup");
      await page.getByTestId("wizard-next").click();
      await expect(wizard(page)).toHaveAttribute("data-step-key", "merge-queue");
      await setWizardInputValue(page, "wizard-merge-target-branch", "main");
      await expect(page.getByTestId("wizard-next")).toBeEnabled({ timeout: 20_000 });
      await page.getByTestId("wizard-next").click();
      await expect(wizard(page)).toHaveAttribute("data-step-key", "confirm");
      await captureVisual(
        page,
        buildVisualName(["workspace-setup", "confirm-summary", theme, visualViewportLabel("fullpage")]),
        { ready: page.locator(".wizard-summary") },
      );
    });
  }
});
