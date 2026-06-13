import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import type { APIRequestContext, Locator, Page } from "playwright/test";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import {
  asArray,
  asRecord,
  ensureProviderInstalledAndHealthy,
  readString,
  resolveWorkspaceProviderModelId,
  verifyProviderForWorkspace,
  waitForSessionWorkspaceFileContents,
  waitForTerminalState,
} from "../src/testing/providerRuntime";

const REQUEST_TIMEOUT_MS = 60_000;
const INSTALL_TARGET = "host";
const GEMINI_DEFAULT_MODEL_ID = "gemini-2.0-flash-lite";
const GEMINI_ENDPOINT_NAME = "Gemini API key E2E";

const selectedGeminiEndpointForConfig = (
  config: Record<string, unknown>,
): Record<string, unknown> | null => {
  const selectedEndpointId = readString(config.selected_endpoint_id);
  if (!selectedEndpointId) return null;
  return asArray(config.endpoints)
    .map((entry) => asRecord(entry))
    .find((entry) => readString(entry.id) === selectedEndpointId) ?? null;
};

async function readGeminiHarnessConfig(request: APIRequestContext): Promise<Record<string, unknown>> {
  const response = await request.get("/api/providers/gemini/harness_config", {
    timeout: REQUEST_TIMEOUT_MS,
  });
  expect(response.ok(), `gemini harness config read failed (${response.status()})`).toBe(true);
  return asRecord(await response.json());
}

async function ensureGeminiEndpointSelected(
  request: APIRequestContext,
  apiKey: string,
  modelId: string,
): Promise<Record<string, unknown>> {
  let config = await readGeminiHarnessConfig(request);
  let selectedEndpoint = selectedGeminiEndpointForConfig(config);
  const endpointMatches = selectedEndpoint !== null
    && readString(selectedEndpoint.name) === GEMINI_ENDPOINT_NAME
    && readString(selectedEndpoint.auth_type) === "gemini_api_key"
    && readString(selectedEndpoint.model_override) === modelId;
  if (readString(config.selected_source_kind) === "endpoint" && endpointMatches) {
    return config;
  }

  const existingEndpoint =
    asArray(config.endpoints)
      .map((entry) => asRecord(entry))
      .find((entry) => readString(entry.name) === GEMINI_ENDPOINT_NAME)
    ?? selectedEndpoint;
  const endpointId = readString(existingEndpoint?.id) || null;

  const upsertResp = await request.post("/api/providers/gemini/harness_config/endpoints", {
    data: {
      endpoint_id: endpointId,
      name: GEMINI_ENDPOINT_NAME,
      auth_type: "gemini_api_key",
      api_key: apiKey,
      model_override: modelId,
    },
    timeout: REQUEST_TIMEOUT_MS,
  });
  expect(upsertResp.ok(), `gemini endpoint upsert failed (${upsertResp.status()})`).toBe(true);
  config = asRecord(await upsertResp.json());
  selectedEndpoint =
    asArray(config.endpoints)
      .map((entry) => asRecord(entry))
      .find((entry) =>
        readString(entry.name) === GEMINI_ENDPOINT_NAME
        && readString(entry.auth_type) === "gemini_api_key"
        && readString(entry.model_override) === modelId,
      ) ?? selectedGeminiEndpointForConfig(config);
  const selectedEndpointId = readString(selectedEndpoint?.id);
  expect(selectedEndpointId).not.toBe("");

  const selectResp = await request.post("/api/providers/gemini/harness_config/select", {
    data: {
      source_kind: "endpoint",
      endpoint_id: selectedEndpointId,
    },
    timeout: REQUEST_TIMEOUT_MS,
  });
  expect(selectResp.ok(), `gemini endpoint select failed (${selectResp.status()})`).toBe(true);
  return asRecord(await selectResp.json());
}

async function openHarnessMenu(page: Page): Promise<Locator> {
  const harnessButton = page
    .locator(
      ".wb-new-composer-stack .wb-switcher-harness, .wb-new-composer-stack button[title='Harness'], button[title='Agents']",
    )
    .first();
  await expect(harnessButton).toBeVisible({ timeout: 20_000 });
  const menu = page.locator(".wb-harness-menu");
  if (!(await menu.isVisible().catch(() => false))) {
    await harnessButton.click();
    await expect(menu).toBeVisible({ timeout: 10_000 });
  }
  return menu;
}

async function configureGeminiApiKeyViaModal(page: Page, apiKey: string): Promise<void> {
  console.warn("[gemini-provider-api-key-real] opening harness menu");
  const menu = await openHarnessMenu(page);
  await menu.getByLabel("Search agents").fill("gemini", { timeout: 10_000 });
  const geminiRowButton = menu
    .locator(".wb-harness-row .wb-harness-row-main")
    .filter({ hasText: /Gemini/i })
    .first();
  await expect(geminiRowButton).toBeVisible({ timeout: 20_000 });
  await geminiRowButton.click({ timeout: 10_000 });
  console.warn("[gemini-provider-api-key-real] gemini row selected");

  const modal = page.locator(".settings-harness-modal");
  const modalVisible = await modal.waitFor({ state: "visible", timeout: 3_000 }).then(() => true).catch(() => false);
  if (!modalVisible) {
    console.warn("[gemini-provider-api-key-real] modal not shown; assuming existing auth");
    return;
  }
  console.warn("[gemini-provider-api-key-real] auth modal visible");

  console.warn("[gemini-provider-api-key-real] selecting API Key auth mode");
  await modal.getByRole("button", { name: "API Key" }).click({ timeout: 10_000 });
  console.warn("[gemini-provider-api-key-real] API Key mode selected");
  await expect(modal.getByRole("link", { name: "Google AI Studio" })).toHaveAttribute(
    "href",
    "https://aistudio.google.com/app/apikey",
  );
  console.warn("[gemini-provider-api-key-real] AI Studio link verified");
  await expect(modal.getByRole("combobox", { name: "Gemini auth mode" })).toContainText("Gemini API Key");
  console.warn("[gemini-provider-api-key-real] auth mode verified");

  const apiKeyInput = modal.locator("input[type='password']").first();
  await expect(apiKeyInput).toBeVisible({ timeout: 10_000 });
  await apiKeyInput.fill(apiKey, { timeout: 10_000 });
  console.warn("[gemini-provider-api-key-real] API key field filled");
  const labelInput = modal
    .locator("label.settings-harness-modal-label")
    .filter({ hasText: "Label (optional)" })
    .locator("input")
    .first();
  if ((await labelInput.count()) > 0) {
    await labelInput.fill("Gemini API key E2E", { timeout: 10_000 });
  }
  console.warn("[gemini-provider-api-key-real] label field filled");

  console.warn("[gemini-provider-api-key-real] submitting API key");
  await modal.getByRole("button", { name: "Add API key" }).click({ timeout: 10_000 });
  console.warn("[gemini-provider-api-key-real] submit clicked");
  const modalClosed = await modal.waitFor({ state: "hidden", timeout: 30_000 }).then(() => true).catch(() => false);
  if (!modalClosed) {
    console.warn("[gemini-provider-api-key-real] modal still open after submit");
    const errorLocator = page.locator(".settings-row-error").first();
    const errorText = (await errorLocator.count()) > 0
      ? await errorLocator.textContent({ timeout: 1_000 }).catch(() => null)
      : null;
    if (errorText && errorText.trim().length > 0) {
      console.warn(`[gemini-provider-api-key-real] modal warning: ${errorText.trim()}`);
    }
    const closeButton = modal.getByRole("button", { name: "Close" });
    if ((await closeButton.count()) > 0) {
      await closeButton.click({ timeout: 10_000 });
    }
    await expect(modal).toBeHidden({ timeout: 10_000 });
  }
  console.warn("[gemini-provider-api-key-real] modal closed");
}

test("workbench: gemini provider API key auth can run a real task", async ({ page, request }) => {
  test.setTimeout(10 * 60_000);

  if ((process.env.CTX_E2E_TIER ?? "") !== "provider-api-auth") {
    test.skip(true, "set CTX_E2E_TIER=provider-api-auth to run gemini provider API-key e2e");
  }

  const geminiApiKey = (process.env.CTX_E2E_GEMINI_API_KEY ?? "").trim();
  if (!geminiApiKey) {
    test.skip(true, "missing CTX_E2E_GEMINI_API_KEY");
  }
  const geminiModelId = (process.env.CTX_E2E_GEMINI_MODEL_ID ?? GEMINI_DEFAULT_MODEL_ID).trim();

  const providerStatus = await ensureProviderInstalledAndHealthy(request, "gemini", INSTALL_TARGET, {
    timeoutMs: 10 * 60_000,
    pollMs: 2_000,
    requestTimeoutMs: REQUEST_TIMEOUT_MS,
  });
  console.warn(
    `[gemini-provider-api-key-real] provider ready: target=${providerStatus.details.install_target || INSTALL_TARGET} health=${providerStatus.health}`,
  );

  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-gemini-"));
  execSync("git init -b main", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "README.md"), "gemini provider auth e2e\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceId = await createWorkspaceAndOpenWorkbench({
    page,
    request,
    repo,
    workspaceName: `gemini-auth-${Date.now()}`,
  });
  console.warn(`[gemini-provider-api-key-real] workspace ready: ${workspaceId}`);

  await configureGeminiApiKeyViaModal(page, geminiApiKey);
  console.warn("[gemini-provider-api-key-real] api key submitted");

  const config = await ensureGeminiEndpointSelected(request, geminiApiKey, geminiModelId);
  expect(readString(config.selected_source_kind)).toBe("endpoint");
  const selectedEndpoint = selectedGeminiEndpointForConfig(config);
  expect(selectedEndpoint).not.toBeNull();
  expect(readString(selectedEndpoint?.auth_type)).toBe("gemini_api_key");
  expect(readString(selectedEndpoint?.name)).toBe(GEMINI_ENDPOINT_NAME);
  expect(readString(selectedEndpoint?.model_override)).toBe(geminiModelId);

  await verifyProviderForWorkspace(request, workspaceId, "gemini", {
    timeoutMs: 90_000,
    pollMs: 3_000,
    requestTimeoutMs: REQUEST_TIMEOUT_MS,
  });
  console.warn("[gemini-provider-api-key-real] verify step done");

  const modelId = await resolveWorkspaceProviderModelId(request, workspaceId, "gemini", {
    timeoutMs: 90_000,
    pollMs: 3_000,
    requestTimeoutMs: REQUEST_TIMEOUT_MS,
  });
  console.warn(`[gemini-provider-api-key-real] using model: ${modelId}`);

  const promptMarker = `gemini-provider-auth-${Date.now()}`;
  const prompt = [
    "This is an end to end test, so it is very important that you do exactly what I ask.",
    "Make a new file in the workspace root called hello.md and put exactly this text in it: hi. The file must contain exactly those two characters with no trailing newline or extra whitespace. If you use a shell command to write the file, use printf rather than echo -n, because echo -n is not portable and may write the literal text -n.",
    "Use only the current worktree root as the target directory. Do not write in a parent directory, and if your first attempt adds a trailing newline or uses the wrong directory, fix the file before replying.",
    "That is all. Do it now without further deliberation.",
    "After writing the file, reply with exactly: hi",
  ].join(" ");

  const createTaskResp = await request.post(`/api/workspaces/${workspaceId}/tasks`, {
    data: {
      title: promptMarker,
      default_session: {
        provider_id: "gemini",
        model_id: modelId,
        execution_environment: "host",
      },
    },
    timeout: REQUEST_TIMEOUT_MS,
  });
  expect(createTaskResp.ok(), `task create failed (${createTaskResp.status()})`).toBe(true);
  const task = asRecord(await createTaskResp.json());
  const taskId = readString(task.id);
  expect(taskId).not.toBe("");
  console.warn(`[gemini-provider-api-key-real] task created: ${taskId}`);
  const sessionId = readString(task.primary_session_id);
  expect(sessionId).not.toBe("");
  console.warn(`[gemini-provider-api-key-real] session created: ${sessionId}`);

  const messageResp = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: {
      content: prompt,
      delivery: "immediate",
    },
    timeout: REQUEST_TIMEOUT_MS,
  });
  expect(messageResp.ok(), `message send failed (${messageResp.status()})`).toBe(true);
  console.warn("[gemini-provider-api-key-real] prompt sent");

  const terminal = await waitForTerminalState(request, sessionId, {
    timeoutMs: 180_000,
    pollMs: 3_000,
    requestTimeoutMs: REQUEST_TIMEOUT_MS,
  });
  console.warn(`[gemini-provider-api-key-real] terminal status: ${terminal.terminalStatus ?? "unknown"}`);
  expect(terminal.terminalStatus, terminal.errorMessage ?? "gemini run did not complete").toBe("completed");
  expect(terminal.assistantMessages).toBeGreaterThan(0);
  await waitForSessionWorkspaceFileContents(request, sessionId, "hello.md", "hi", {
    timeoutMs: 30_000,
    pollMs: 1_000,
  });
});
