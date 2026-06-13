import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import { configureHarnessEndpointAuthViaModal } from "./utils/harnessEndpointAuth";
import {
  ensureGeminiEndpointSelected,
  GEMINI_DEFAULT_MODEL_ID,
  selectedGeminiEndpointForConfig,
} from "./utils/geminiProviderAuth";
import {
  asRecord,
  ensureProviderInstalledAndHealthy,
  readString,
  verifyProviderForWorkspace,
  waitForSessionWorkspaceFileContents,
  waitForTerminalState,
} from "../src/testing/providerRuntime";

const REQUEST_TIMEOUT_MS = 60_000;
const INSTALL_TARGET = "host" as const;
const GEMINI_ENTRY = {
  providerId: "gemini",
  menuLabel: "Gemini",
  searchTerm: "gemini",
};
const GEMINI_VERTEX_ENDPOINT_NAME = "Gemini Vertex AI Service Account E2E";
const WRITE_FILE_NAME = "hello.md";
const WRITE_FILE_CONTENTS = "hi";

const authModalAlreadyConfigured = (detail: string): boolean =>
  detail.toLowerCase().includes("auth modal did not open (provider may already be configured)");

test("workbench: gemini Vertex AI service account auth can run a real task", async ({ page, request }) => {
  test.setTimeout(10 * 60_000);

  if ((process.env.CTX_E2E_TIER ?? "") !== "provider-api-auth") {
    test.skip(true, "set CTX_E2E_TIER=provider-api-auth to run gemini vertex provider auth e2e");
  }

  const serviceAccountJson = (process.env.GCP_SERVICE_ACCOUNT_JSON ?? "").trim();
  const projectId = (process.env.GCP_PROJECT_ID ?? "").trim();
  if (!serviceAccountJson || !projectId) {
    test.skip(true, "missing GCP_SERVICE_ACCOUNT_JSON / GCP_PROJECT_ID");
  }
  const geminiModelId = (
    process.env.CTX_E2E_VERTEX_MODEL_ID
    ?? process.env.CTX_E2E_GEMINI_MODEL_ID
    ?? GEMINI_DEFAULT_MODEL_ID
  ).trim();
  const location = (process.env.CTX_E2E_VERTEX_LOCATION ?? "global").trim();

  await ensureProviderInstalledAndHealthy(request, "gemini", INSTALL_TARGET, {
    timeoutMs: 10 * 60_000,
    pollMs: 2_000,
    requestTimeoutMs: REQUEST_TIMEOUT_MS,
  });

  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-gemini-vertex-"));
  execSync("git init -b main", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "README.md"), "gemini vertex provider auth e2e\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceId = await createWorkspaceAndOpenWorkbench({
    page,
    request,
    repo,
    workspaceName: `gemini-vertex-auth-${Date.now()}`,
  });

  const authResult = await configureHarnessEndpointAuthViaModal(
    page,
    GEMINI_ENTRY,
    serviceAccountJson,
    "",
    "",
    {
      geminiAuthMode: "vertex_ai",
      endpointName: GEMINI_VERTEX_ENDPOINT_NAME,
      serviceAccountJson,
      projectId,
      location,
      expectedDocsLink: {
        name: "Google Cloud Credentials",
        href: "https://console.cloud.google.com/apis/credentials",
      },
    },
  );
  expect(authResult.ok || authModalAlreadyConfigured(authResult.detail), authResult.detail).toBe(true);

  const config = await ensureGeminiEndpointSelected(request, {
    authType: "vertex_ai",
    endpointName: GEMINI_VERTEX_ENDPOINT_NAME,
    modelId: geminiModelId,
    serviceAccountJson,
    projectId,
    location,
    requestTimeoutMs: REQUEST_TIMEOUT_MS,
  });
  expect(readString(config.selected_source_kind)).toBe("endpoint");
  const selectedEndpoint = selectedGeminiEndpointForConfig(config);
  expect(selectedEndpoint).not.toBeNull();
  expect(readString(selectedEndpoint?.auth_type)).toBe("vertex_ai");
  expect(readString(selectedEndpoint?.name)).toBe(GEMINI_VERTEX_ENDPOINT_NAME);
  expect(readString(selectedEndpoint?.model_override)).toBe(geminiModelId);

  await verifyProviderForWorkspace(request, workspaceId, "gemini", {
    timeoutMs: 90_000,
    pollMs: 3_000,
    requestTimeoutMs: REQUEST_TIMEOUT_MS,
  });

  const promptMarker = `gemini-vertex-provider-auth-${Date.now()}`;
  const prompt = `${promptMarker}: this is an end-to-end write test. Create a new file in this directory called ${WRITE_FILE_NAME} and put exactly ${WRITE_FILE_CONTENTS} in it. The file must contain exactly those two characters with no trailing newline or extra whitespace. If you use a shell command to write the file, use printf rather than echo -n, because echo -n is not portable and may write the literal text -n. Use only the current worktree root as the target directory. Do not write in a parent directory, and if your first attempt adds a trailing newline or uses the wrong directory, fix the file before replying. Then reply with exactly ${WRITE_FILE_CONTENTS}.`;

  const createTaskResp = await request.post(`/api/workspaces/${workspaceId}/tasks`, {
    data: {
      title: promptMarker,
      default_session: {
        provider_id: "gemini",
        model_id: geminiModelId,
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
  expect(terminal.terminalStatus, terminal.errorMessage ?? "gemini vertex run did not complete").toBe("completed");
  expect(terminal.assistantMessages).toBeGreaterThan(0);
  await waitForSessionWorkspaceFileContents(request, sessionId, WRITE_FILE_NAME, WRITE_FILE_CONTENTS, {
    timeoutMs: 15_000,
    pollMs: 500,
  });
});
