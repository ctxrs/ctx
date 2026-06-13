import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import type { APIRequestContext, Locator, Page } from "playwright/test";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { waitForSessionWorkspaceFileContents } from "../src/testing/providerRuntime";

type TerminalState = {
  done: boolean;
  terminalStatus: string | null;
  assistantMessages: number;
  errorMessage: string | null;
};

const TERMINAL_TURN_STATUSES = new Set(["completed", "failed", "interrupted"]);
const REQUEST_TIMEOUT_MS = 60_000;

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

const asArray = (value: unknown): unknown[] => (Array.isArray(value) ? value : []);

const readString = (value: unknown): string => (typeof value === "string" ? value : "");

const firstText = (...values: unknown[]): string => {
  for (const value of values) {
    const text = readString(value).trim();
    if (text) return text;
  }
  return "";
};

const normalizeErrorMessage = (raw: string): string => raw.replace(/\s+/g, " ").trim();

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

async function ensureCursorProviderReady(request: APIRequestContext): Promise<{ ok: true } | { ok: false; reason: string }> {
  const providersResp = await request.get("/api/providers", { timeout: REQUEST_TIMEOUT_MS });
  if (!providersResp.ok()) {
    return { ok: false, reason: `failed to read providers (${providersResp.status()})` };
  }
  const providers = asArray(await providersResp.json()).map((entry) => asRecord(entry));
  const cursor = providers.find((entry) => readString(entry.provider_id) === "cursor");
  if (!cursor) {
    return { ok: false, reason: "cursor provider not listed by /api/providers" };
  }
  if (cursor.installed !== true || readString(cursor.health) !== "ok") {
    const diagnostics = asArray(cursor.diagnostics)
      .map((entry) => readString(entry).trim())
      .filter((entry) => entry.length > 0);
    return {
      ok: false,
      reason: diagnostics[0] ?? `cursor provider unavailable (installed=${String(cursor.installed)}, health=${readString(cursor.health) || "unknown"})`,
    };
  }
  return { ok: true };
}

async function configureCursorApiKeyViaModal(page: Page, apiKey: string): Promise<void> {
  const menu = await openHarnessMenu(page);
  await menu.getByLabel("Search agents").fill("cursor");
  const cursorRowButton = menu
    .locator(".wb-harness-row .wb-harness-row-main")
    .filter({ hasText: /Cursor/i })
    .first();
  await expect(cursorRowButton).toBeVisible({ timeout: 20_000 });
  await cursorRowButton.click();

  const modal = page.locator(".settings-harness-modal");
  const modalVisible = await modal.waitFor({ state: "visible", timeout: 3_000 }).then(() => true).catch(() => false);
  if (!modalVisible) {
    return;
  }

  await modal.getByRole("button", { name: "API Key" }).click();
  await expect(modal.getByRole("link", { name: "Cursor Integrations" })).toHaveAttribute(
    "href",
    "https://cursor.com/dashboard?tab=integrations",
  );
  const apiKeyInput = modal.locator("input[type='password']").first();
  await expect(apiKeyInput).toBeVisible({ timeout: 10_000 });
  await apiKeyInput.fill(apiKey);
  const labelInput = modal
    .locator("label.settings-harness-modal-label")
    .filter({ hasText: "Label (optional)" })
    .locator("input")
    .first();
  if ((await labelInput.count()) > 0) {
    await labelInput.fill("Cursor API key E2E");
  }

  await modal.getByRole("button", { name: "Add API key" }).click();
  await expect(modal).toBeHidden({ timeout: 30_000 });
}

async function verifyCursorProviderForWorkspace(opts: {
  request: APIRequestContext;
  workspaceId: string;
}): Promise<{ ok: true } | { ok: false; reason: string }> {
  const { request, workspaceId } = opts;
  const resp = await request.post(`/api/workspaces/${workspaceId}/providers/cursor/verify`, {
    data: {},
    timeout: 30_000,
  });
  if (!resp.ok()) {
    const payload = asRecord(await resp.json().catch(() => ({})));
    return {
      ok: false,
      reason: normalizeErrorMessage(
        firstText(payload.error, payload.message, `verify request failed (${resp.status()})`) || "verify request failed",
      ),
    };
  }
  const payload = asRecord(await resp.json());
  const status = firstText(payload.status).toLowerCase();
  if (status !== "ok") {
    return {
      ok: false,
      reason: normalizeErrorMessage(firstText(payload.message, `verify status=${status}`)),
    };
  }
  return { ok: true };
}

async function resolveCursorModelId(opts: {
  request: APIRequestContext;
  workspaceId: string;
}): Promise<{ ok: true; modelId: string } | { ok: false; reason: string }> {
  const { request, workspaceId } = opts;
  const optionsResp = await request.get(`/api/workspaces/${workspaceId}/providers/cursor/options`, {
    timeout: REQUEST_TIMEOUT_MS,
  });
  if (!optionsResp.ok()) {
    return { ok: false, reason: `failed to read cursor options (${optionsResp.status()})` };
  }
  const options = asRecord(await optionsResp.json());
  const models = asRecord(options.models);
  const currentModelId = firstText(models.current_model_id, models.currentModelId);
  if (currentModelId) {
    return { ok: true, modelId: currentModelId };
  }

  const firstModelId = asArray(models.models)
    .map((entry) => asRecord(entry))
    .map((entry) => firstText(entry.id, entry.model_id, entry.modelId, entry.name))
    .find((entry) => entry.length > 0);
  if (!firstModelId) {
    return { ok: false, reason: "cursor options did not return a model id" };
  }
  return { ok: true, modelId: firstModelId };
}

function toTerminalState(snapshot: Record<string, unknown>): TerminalState {
  const head = asRecord(snapshot.head);
  const activity = asRecord(head.activity);
  const turns = asArray(head.turns).map((entry) => asRecord(entry));
  const messages = asArray(head.messages).map((entry) => asRecord(entry));

  const lastTurn = turns.length > 0 ? turns[turns.length - 1] : {};
  const terminalStatus = firstText(lastTurn.status, activity.last_turn_status).toLowerCase() || null;
  const assistantMessages = messages.filter((message) => {
    if (readString(message.role) !== "assistant") return false;
    return readString(message.content).trim().length > 0;
  }).length;

  const done = terminalStatus
    ? TERMINAL_TURN_STATUSES.has(terminalStatus)
    : (activity.is_working !== true && assistantMessages > 0);

  const errorMessage = terminalStatus === "failed" || terminalStatus === "interrupted"
    ? normalizeErrorMessage(firstText(lastTurn.status, "cursor run failed"))
    : null;

  return {
    done,
    terminalStatus,
    assistantMessages,
    errorMessage,
  };
}

async function waitForTerminalState(opts: {
  request: APIRequestContext;
  sessionId: string;
}): Promise<TerminalState> {
  const { request, sessionId } = opts;
  let resolved: TerminalState | null = null;

  await expect
    .poll(
      async () => {
        const resp = await request.get(`/api/sessions/${sessionId}/head?include_events=1&limit=80`, {
          timeout: REQUEST_TIMEOUT_MS,
        });
        if (!resp.ok()) return "";
        const state = toTerminalState({ head: asRecord(await resp.json()) });
        if (!state.done) return "";
        resolved = state;
        return "done";
      },
      { timeout: 180_000, intervals: [1_000, 2_000, 3_000] },
    )
    .toBe("done");

  if (!resolved) {
    throw new Error(`cursor session ${sessionId} did not reach terminal state`);
  }
  return resolved;
}

test("workbench: cursor provider API key auth can run a real task", async ({ page, request }) => {
  test.setTimeout(10 * 60_000);

  if ((process.env.CTX_E2E_TIER ?? "") !== "provider-api-auth") {
    test.skip(true, "set CTX_E2E_TIER=provider-api-auth to run cursor provider API-key e2e");
  }

  const cursorApiKey = (process.env.CTX_E2E_CURSOR_API_KEY ?? "").trim();
  if (!cursorApiKey) {
    test.skip(true, "missing CTX_E2E_CURSOR_API_KEY");
  }

  const providerReady = await ensureCursorProviderReady(request);
  expect(providerReady.ok, providerReady.ok ? undefined : providerReady.reason).toBe(true);
  if (!providerReady.ok) return;

  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-cursor-"));
  execSync("git init -b main", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "README.md"), "cursor provider auth e2e\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceId = await createWorkspaceAndOpenWorkbench({
    page,
    request,
    repo,
    workspaceName: `cursor-auth-${Date.now()}`,
  });

  await configureCursorApiKeyViaModal(page, cursorApiKey);

  const verify = await verifyCursorProviderForWorkspace({ request, workspaceId });
  if (!verify.ok) {
    // Verification can intermittently time out before the first real task request.
    // The authoritative signal for this test is successful task completion + assistant output.
    console.warn(`[cursor-provider-api-key-real] verify warning: ${verify.reason}`);
  }

  const modelSelection = await resolveCursorModelId({ request, workspaceId });
  const modelId = modelSelection.ok ? modelSelection.modelId : "gpt-5";
  if (!modelSelection.ok) {
    console.warn(`[cursor-provider-api-key-real] model discovery warning: ${modelSelection.reason}`);
    console.warn(`[cursor-provider-api-key-real] using fallback model id: ${modelId}`);
  }

  const promptMarker = `cursor-provider-auth-${Date.now()}`;
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
        provider_id: "cursor",
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
  const sessionId = readString(task.primary_session_id);
  expect(sessionId).not.toBe("");

  const messageResp = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: {
      content: prompt,
      delivery: "immediate",
    },
    timeout: REQUEST_TIMEOUT_MS,
  });
  expect(messageResp.ok(), `message send failed (${messageResp.status()})`).toBe(true);

  const terminal = await waitForTerminalState({ request, sessionId });
  expect(terminal.terminalStatus, terminal.errorMessage ?? "cursor run did not complete").toBe("completed");
  expect(terminal.assistantMessages).toBeGreaterThan(0);
  await waitForSessionWorkspaceFileContents(request, sessionId, "hello.md", "hi", {
    timeoutMs: 30_000,
    pollMs: 1_000,
  });
});
