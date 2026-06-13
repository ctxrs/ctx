import { mkdtempSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import type { APIRequestContext, Page, Route } from "playwright/test";
import { test, expect } from "./fixtures";
import { getProviderBootstrapTimeoutMessage } from "../src/utils/providerBootstrapTimeout";

type Deferred<T> = {
  promise: Promise<T>;
  resolve: (value: T | PromiseLike<T>) => void;
  reject: (reason?: unknown) => void;
};

type BootstrapFailureMode =
  | {
      kind: "error";
      expectedMessage: string;
    }
  | {
      kind: "timeout";
      expectedMessage: string;
      release: Deferred<void>;
    };

type BootstrapRouteController = {
  firstInterceptedWorkspaceId: () => string | null;
  releaseTimeout: () => void;
};

type Scenario = {
  slug: string;
  createFailure: () => BootstrapFailureMode;
};

const wizard = (page: Page) => page.getByTestId("workspace-setup");

const createDeferred = <T,>(): Deferred<T> => {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
};

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
        const prototype =
          element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
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

async function openWizard(page: Page) {
  await page.goto("/workspace-setup", { waitUntil: "domcontentloaded" });
  await expect(wizard(page)).toBeVisible({ timeout: 20_000 });
}

async function moveWizardToConfirmForLocalHostNew(
  page: Page,
  opts: { destPath: string; workspaceName: string },
) {
  const stepKey = async () => wizard(page).getAttribute("data-step-key");

  await expect(wizard(page)).toHaveAttribute("data-step-key", "location", { timeout: 20_000 });
  await clickWizardStepOption(page, "wizard-option-location-local");
  await expect(wizard(page)).toHaveAttribute("data-step-key", "container", { timeout: 20_000 });
  await clickWizardStepOption(page, "wizard-option-container-host");

  for (let attempt = 0; attempt < 4; attempt += 1) {
    const current = await stepKey();
    if (current === "harness-downloads") {
      const skip = page.getByTestId("wizard-harness-skip");
      if ((await skip.count()) > 0) {
        await skip.click();
      } else {
        await expect(page.getByTestId("wizard-next")).toBeEnabled({ timeout: 20_000 });
        await page.getByTestId("wizard-next").click();
      }
      continue;
    }
    if (current === "auth-import") {
      await page.getByRole("button", { name: "Skip for now" }).click();
      continue;
    }
    if (current === "session-titling") {
      await page.getByTestId("wizard-titling-skip").click();
      continue;
    }
    break;
  }

  await expect(wizard(page)).toHaveAttribute("data-step-key", "source", { timeout: 20_000 });
  const sourceNew = page.getByTestId("wizard-option-source-new");
  await expect(sourceNew).toBeVisible({ timeout: 20_000 });
  if ((await sourceNew.getAttribute("aria-pressed")) !== "true") {
    await sourceNew.click();
  }
  await setWizardInputValue(page, "wizard-source-path", opts.destPath);
  await setWizardInputValue(page, "wizard-workspace-name", opts.workspaceName);
  await expect(page.getByTestId("wizard-next")).toBeEnabled({ timeout: 20_000 });
  await page.getByTestId("wizard-next").click();

  await expect(wizard(page)).toHaveAttribute("data-step-key", "setup", { timeout: 20_000 });
  await page.getByRole("button", { name: "Skip for now" }).click();

  await expect(wizard(page)).toHaveAttribute("data-step-key", "merge-queue", { timeout: 20_000 });
  await page.getByTestId("wizard-merge-skip").click();

  await expect(wizard(page)).toHaveAttribute("data-step-key", "confirm", { timeout: 20_000 });
}

async function installBootstrapFailureRoute(
  page: Page,
  failure: BootstrapFailureMode,
): Promise<BootstrapRouteController> {
  let consumed = false;
  let firstWorkspaceId: string | null = null;

  await page.route(/\/api\/workspaces\/[^/]+\/providers\/bootstrap(?:\?.*)?$/, async (route: Route) => {
    const url = new URL(route.request().url());
    const match = url.pathname.match(/^\/api\/workspaces\/([^/]+)\/providers\/bootstrap$/);
    if (!match) {
      await route.continue();
      return;
    }

    if (consumed) {
      await route.continue();
      return;
    }

    consumed = true;
    firstWorkspaceId = match[1] ?? null;

    if (failure.kind === "timeout") {
      await failure.release.promise;
      await route.abort("failed");
      return;
    }

    await route.fulfill({
      status: 500,
      contentType: "text/plain",
      body: failure.expectedMessage,
    });
  });

  return {
    firstInterceptedWorkspaceId: () => firstWorkspaceId,
    releaseTimeout: () => {
      if (failure.kind === "timeout") {
        failure.release.resolve();
      }
    },
  };
}

async function expectWorkspacePreservedAndRecoverable(opts: {
  page: Page;
  request: APIRequestContext;
  workspaceId: string;
  rootPath: string;
  recentLabel: string;
}) {
  const { page, request, workspaceId, rootPath, recentLabel } = opts;
  const workspacesResp = await request.get("/api/workspaces");
  expect(workspacesResp.ok()).toBeTruthy();
  const workspaces = (await workspacesResp.json()) as Array<{
    id?: unknown;
    root_path?: unknown;
  }>;
  const persisted = workspaces.find((workspace) => workspace.id === workspaceId);
  expect(persisted).toBeTruthy();
  expect(persisted?.root_path).toBe(realpathSync(rootPath));

  await page.goto("/", { waitUntil: "domcontentloaded" });
  const recent = page.locator(".launcher-recent-item").filter({ hasText: recentLabel }).first();
  await expect(recent).toBeVisible({ timeout: 20_000 });

  await page.goto(`/workspaces/${workspaceId}`, { waitUntil: "domcontentloaded" });
  await expect(page).toHaveURL(new RegExp(`/workspaces/${workspaceId}(\\\\?.*)?$`), { timeout: 20_000 });
  await expect(page.locator(".wb-main")).toBeVisible({ timeout: 20_000 });
}

test.describe("workspace setup: bootstrap failures preserve the created workspace", () => {
  const scenarios: Scenario[] = [
    {
      slug: "timeout",
      createFailure: () => ({
        kind: "timeout",
        expectedMessage: getProviderBootstrapTimeoutMessage(),
        release: createDeferred<void>(),
      }),
    },
    {
      slug: "error",
      createFailure: () => ({
        kind: "error",
        expectedMessage: "bootstrap exploded",
      }),
    },
  ];

  for (const scenario of scenarios) {
    test(`create succeeds, bootstrap ${scenario.slug}, and recovery is preserved`, async ({ page, request }) => {
      const rootPath = mkdtempSync(path.join(tmpdir(), `ctx-e2e-workspace-bootstrap-${scenario.slug}-`));
      const workspaceName = `workspace-bootstrap-${scenario.slug}`;
      const recentLabel = path.basename(rootPath);
      const failure = scenario.createFailure();

      const bootstrapRoute = await installBootstrapFailureRoute(page, failure);

      await openWizard(page);
      await moveWizardToConfirmForLocalHostNew(page, { destPath: rootPath, workspaceName });

      const createResponsePromise = page.waitForResponse((response) =>
        response.request().method() === "POST" && new URL(response.url()).pathname === "/api/workspaces",
      );

      await page.getByTestId("wizard-create").click();

      const createResponse = await createResponsePromise;
      expect(createResponse.ok()).toBeTruthy();
      const created = (await createResponse.json()) as { id?: unknown };
      const workspaceId = typeof created.id === "string" ? created.id : "";
      expect(workspaceId).not.toBe("");

      await expect
        .poll(() => bootstrapRoute.firstInterceptedWorkspaceId(), { timeout: 20_000 })
        .toBe(workspaceId);

      await expect(page).toHaveURL(/\/workspace-setup$/, { timeout: 20_000 });
      await expect(page.locator(".wizard-error")).toContainText(failure.expectedMessage, { timeout: 20_000 });

      bootstrapRoute.releaseTimeout();

      await expectWorkspacePreservedAndRecoverable({
        page,
        request,
        workspaceId,
        rootPath,
        recentLabel,
      });
    });
  }
});
