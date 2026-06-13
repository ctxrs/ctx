import type { APIRequestContext } from "playwright/test";
import { execSync } from "child_process";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { test, expect } from "./fixtures";
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
const INSTALL_TARGET = "host" as const;

async function upsertCopilotAccount(request: APIRequestContext, token: string): Promise<void> {
  const upsertResp = await request.post("/api/providers/copilot/accounts", {
    data: {
      token,
      label: "Copilot subscription E2E",
      email: "copilot-e2e@example.com",
    },
    timeout: REQUEST_TIMEOUT_MS,
  });
  expect(upsertResp.ok(), `copilot account upsert failed (${upsertResp.status()})`).toBe(true);
  const payload = asRecord(await upsertResp.json());
  expect(readString(payload.active_account_id)).not.toBe("");
}

async function readCopilotOptions(
  request: APIRequestContext,
  workspaceId: string,
): Promise<Record<string, unknown>> {
  const optionsResp = await request.get(`/api/workspaces/${workspaceId}/providers/copilot/options`, {
    timeout: REQUEST_TIMEOUT_MS,
  });
  expect(optionsResp.ok(), `failed to read copilot options (${optionsResp.status()})`).toBe(true);
  return asRecord(await optionsResp.json());
}

async function waitForCopilotOptionsReady(
  request: APIRequestContext,
  workspaceId: string,
  timeoutMs = 90_000,
  pollMs = 3_000,
): Promise<Record<string, unknown>> {
  const startedAt = Date.now();
  let lastDetail = "copilot options were never populated";

  while (Date.now() - startedAt < timeoutMs) {
    const options = await readCopilotOptions(request, workspaceId);
    const models = asRecord(options.models);
    const catalogSource = readString(models.catalog_source);
    const currentModelId = readString(models.current_model_id);
    const defaultModelId = readString(models.default_model_id);
    const modelIds = asArray(models.models)
      .map((entry) => asRecord(entry))
      .map((entry) => readString(entry.id))
      .filter((entry) => entry.length > 0);

    if (currentModelId && modelIds.includes(currentModelId)) {
      return options;
    }

    lastDetail =
      `catalog_source=${catalogSource || "<empty>"} current_model_id=${currentModelId || "<empty>"} ` +
      `default_model_id=${defaultModelId || "<empty>"} model_count=${modelIds.length}`;
    await new Promise((resolve) => setTimeout(resolve, pollMs));
  }

  throw new Error(`copilot options did not become ready: ${lastDetail}`);
}

test("workbench: copilot subscription token auth can run a real task", async ({ page, request }) => {
  test.setTimeout(10 * 60_000);

  if ((process.env.CTX_E2E_TIER ?? "") !== "provider-api-auth") {
    test.skip(true, "set CTX_E2E_TIER=provider-api-auth to run copilot subscription token e2e");
  }

  const copilotToken = (process.env.CTX_E2E_COPILOT_TOKEN ?? "").trim();
  if (!copilotToken) {
    test.skip(true, "missing CTX_E2E_COPILOT_TOKEN");
  }

  await ensureProviderInstalledAndHealthy(request, "copilot", INSTALL_TARGET, {
    timeoutMs: 10 * 60_000,
    pollMs: 2_000,
    requestTimeoutMs: REQUEST_TIMEOUT_MS,
  });

  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-copilot-"));
  execSync("git init -b main", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "README.md"), "copilot subscription token e2e\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceId = await createWorkspaceAndOpenWorkbench({
    page,
    request,
    repo,
    workspaceName: `copilot-subscription-auth-${Date.now()}`,
  });

  await upsertCopilotAccount(request, copilotToken);

  await verifyProviderForWorkspace(request, workspaceId, "copilot", {
    timeoutMs: 90_000,
    pollMs: 3_000,
    requestTimeoutMs: REQUEST_TIMEOUT_MS,
  });

  const options = await waitForCopilotOptionsReady(request, workspaceId);
  const models = asRecord(options.models);
  const currentModelId = readString(models.current_model_id);
  const defaultModelId = readString(models.default_model_id);
  const modelIds = asArray(models.models)
    .map((entry) => asRecord(entry))
    .map((entry) => readString(entry.id))
    .filter((entry) => entry.length > 0);
  expect(currentModelId).not.toBe("");
  expect(modelIds).toContain(currentModelId);
  if (defaultModelId) {
    expect(modelIds).toContain(defaultModelId);
  }

  const modelId = await resolveWorkspaceProviderModelId(request, workspaceId, "copilot", {
    timeoutMs: 90_000,
    pollMs: 3_000,
    requestTimeoutMs: REQUEST_TIMEOUT_MS,
  });
  expect(modelId).toBe(currentModelId);

  const promptMarker = `copilot-subscription-token-${Date.now()}`;
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
        provider_id: "copilot",
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

  const terminal = await waitForTerminalState(request, sessionId, {
    timeoutMs: 180_000,
    pollMs: 3_000,
    requestTimeoutMs: REQUEST_TIMEOUT_MS,
  });
  expect(terminal.terminalStatus, terminal.errorMessage ?? "copilot run did not complete").toBe("completed");
  expect(terminal.assistantMessages).toBeGreaterThan(0);
  expect(terminal.modelId).toBe(modelId);
  await waitForSessionWorkspaceFileContents(request, sessionId, "hello.md", "hi", {
    timeoutMs: 30_000,
    pollMs: 1_000,
  });
});
