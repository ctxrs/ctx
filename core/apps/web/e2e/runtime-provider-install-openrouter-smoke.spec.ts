import { mkdirSync, mkdtempSync, writeFileSync } from "fs";
import { execSync } from "child_process";
import { tmpdir } from "os";
import path from "path";
import type { APIRequestContext, TestInfo } from "playwright/test";
import { test, expect } from "./fixtures";
import { ensureLocalLinuxSandboxPrepared } from "./utils/workspaceExecution";
import {
  classifyInstallSmokeFailureCategory,
  envTruthy,
  parseCsv,
  shouldRetryInstallSmokeFirstTurnFailure,
  shouldSkipBundledOnlyInstall,
  type RuntimeInstallSmokeFailureCategory,
} from "./runtimeInstallSmoke";

type ProviderStatus = {
  installed: boolean;
  health: string;
  diagnostics: string[];
  details: Record<string, string>;
};

type ProviderListRow = ProviderStatus & {
  provider_id: string;
};

type TerminalState = {
  done: boolean;
  terminalStatus: string | null;
  assistantMessages: number;
  errorMessage: string | null;
  modelId: string | null;
};

type ExecutionEnvironment = "host" | "sandbox";
type InstallTarget = "host" | "container";
type NetworkMode = "llm_only" | "allowlist" | "all";
type ResultOutcome = "pass" | "fail";
type ProviderResult = {
  provider_id: string;
  install_target: InstallTarget;
  environment: ExecutionEnvironment;
  network_mode: NetworkMode;
  model_override: string;
  install_id: string | null;
  session_id: string | null;
  model_id: string | null;
  terminal_status: string | null;
  assistant_messages: number;
  stage: string;
  error_code: string | null;
  category: RuntimeInstallSmokeFailureCategory | null;
  reason: string;
  result: ResultOutcome;
  elapsed_ms: number;
};

type StageError = Error & {
  stage?: string;
  errorCode?: string;
};

const DEFAULT_OPENROUTER_BASE_URL = "https://openrouter.ai/api/v1";
const DEFAULT_PROVIDER_ID = "codex";
const DEFAULT_OPENAI_MODEL_OVERRIDE = "openai/gpt-4.1-mini";
const DEFAULT_QWEN_MODEL_OVERRIDE = "qwen/qwen3-coder";
const DEFAULT_GEMINI_MODEL_OVERRIDE = "google/gemini-3-flash-preview";
const DEFAULT_TERMINAL_TIMEOUT_MS = 180_000;
const FIRST_TURN_MAX_ATTEMPTS = 3;
const FIRST_TURN_RETRY_BASE_DELAY_MS = 10_000;
const TERMINAL_TURN_STATUSES = new Set(["completed", "failed", "interrupted"]);

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

const providerStatusPath = (providerId: string, target: InstallTarget): string => {
  const resolvedTarget = target === "container" ? "container" : "host";
  return `/api/providers/${encodeURIComponent(providerId)}?target=${resolvedTarget}`;
};

const providersListPath = (target: InstallTarget): string => {
  const resolvedTarget = target === "container" ? "container" : "host";
  return `/api/providers?target=${resolvedTarget}`;
};

const readStringMap = (value: unknown): Record<string, string> => {
  const out: Record<string, string> = {};
  for (const [key, rawValue] of Object.entries(asRecord(value))) {
    if (typeof rawValue === "string") {
      out[key] = rawValue;
      continue;
    }
    if (typeof rawValue === "number" || typeof rawValue === "boolean") {
      out[key] = String(rawValue);
    }
  }
  return out;
};

const envInt = (value: string | undefined, fallback: number): number => {
  const parsed = Number.parseInt(String(value || "").trim(), 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
};

const providerOverrideEnvVar = (providerId: string): string =>
  `CTX_E2E_${providerId.toUpperCase().replace(/[^A-Z0-9]+/g, "_")}_OPENROUTER_MODEL_OVERRIDE`;

const providerDefaultOpenRouterModelOverride = (providerId: string): string => {
  if (providerId === "qwen") {
    return DEFAULT_QWEN_MODEL_OVERRIDE;
  }
  if (providerId === "pi") {
    return DEFAULT_GEMINI_MODEL_OVERRIDE;
  }
  return DEFAULT_OPENAI_MODEL_OVERRIDE;
};

const createStageError = (stage: string, message: string, errorCode?: string): StageError => {
  const error = new Error(message) as StageError;
  error.stage = stage;
  if (errorCode) {
    error.errorCode = errorCode;
  }
  return error;
};

const toStageError = (stage: string, error: unknown): StageError => {
  if (error instanceof Error) {
    const typed = error as StageError;
    if (!typed.stage) typed.stage = stage;
    return typed;
  }
  return createStageError(stage, normalizeErrorMessage(String(error)));
};

const initRepo = (): string => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init -b main", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "README.md"), "runtime provider install openrouter smoke e2e\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });
  return repo;
};

async function getProviderStatus(
  request: APIRequestContext,
  providerId: string,
  target: InstallTarget,
): Promise<ProviderStatus> {
  const response = await request.get(providerStatusPath(providerId, target));
  expect(response.ok(), `failed to read providers (${response.status()})`).toBeTruthy();
  const row = asRecord(await response.json());
  if (!row) {
    return {
      installed: false,
      health: "missing",
      diagnostics: [`provider not returned by ${providerStatusPath(providerId, target)}`],
      details: {},
    };
  }
  return {
    installed: row.installed === true,
    health: firstText(row.health, "unknown"),
    diagnostics: asArray(row.diagnostics).map((entry) => readString(entry)).filter(Boolean),
    details: readStringMap(row.details),
  };
}

async function listProviderStatuses(
  request: APIRequestContext,
  target: InstallTarget,
): Promise<ProviderListRow[]> {
  const pathForTarget = providersListPath(target);
  const response = await request.get(pathForTarget);
  expect(response.ok(), `failed to list providers (${response.status()})`).toBeTruthy();
  return asArray(await response.json())
    .map((entry) => {
      const row = asRecord(entry);
      const providerId = firstText(row.provider_id);
      if (!providerId) return null;
      return {
        provider_id: providerId,
        installed: row.installed === true,
        health: firstText(row.health, "unknown"),
        diagnostics: asArray(row.diagnostics).map((item) => readString(item)).filter(Boolean),
        details: readStringMap(row.details),
      };
    })
    .filter((row): row is ProviderListRow => row !== null);
}

const installSupportedProviderIds = (providers: ProviderListRow[]): string[] =>
  providers
    .filter((row) => row.details.install_supported === "true")
    .map((row) => row.provider_id)
    .sort();

async function installProviderAndWait(
  request: APIRequestContext,
  providerId: string,
  target: InstallTarget,
): Promise<{ installId: string }> {
  const start = await request.post(`/api/providers/${providerId}/install?target=${target}`, { data: {} });
  if (!start.ok()) {
    const body = normalizeErrorMessage(await start.text().catch(() => ""));
    throw createStageError("install", `provider install start failed (${start.status()}): ${body}`);
  }
  const payload = asRecord(await start.json());
  const installId = readString(payload.install_id);
  if (!installId) {
    throw createStageError("install", `provider install response missing install_id: ${JSON.stringify(payload)}`);
  }

  await expect
    .poll(
      async () => {
        const poll = await request.get(`/api/providers/install/${installId}`);
        if (!poll.ok()) {
          throw createStageError("install", `provider install poll failed (${poll.status()})`);
        }
        const info = asRecord(await poll.json());
        const state = firstText(info.state).toLowerCase();
        if (state === "failed" || state === "cancelled") {
          const lastEvent = asRecord(info.last_event);
          const errorCode = firstText(info.error_code, lastEvent.error_code) || undefined;
          const detail = normalizeErrorMessage(
            firstText(
              lastEvent.message,
              lastEvent.stage,
              info.error,
              JSON.stringify(info),
            ),
          );
          throw createStageError("install", `provider install ${state}: ${detail}`, errorCode);
        }
        return state;
      },
      { timeout: 10 * 60_000, intervals: [1_000, 2_000, 3_000] },
    )
    .toBe("succeeded");

  return { installId };
}

async function configureWorkspaceExecution(
  request: APIRequestContext,
  workspaceId: string,
  environment: ExecutionEnvironment,
  networkMode: NetworkMode,
  allowlist: string[],
): Promise<void> {
  if (environment === "host") return;

  const payload: Record<string, unknown> = {
    environment,
    network_mode: networkMode,
  };
  if (networkMode === "allowlist") {
    payload.allowlist = allowlist;
  }

  const update = await request.post(`/api/workspaces/${workspaceId}/execution_config`, {
    data: payload,
  });
  if (!update.ok()) {
    const body = normalizeErrorMessage(await update.text().catch(() => ""));
    throw createStageError(
      "execution_config",
      `workspace execution config update failed (${update.status()}): ${body}`,
    );
  }

  await expect
    .poll(
      async () => {
        const current = await request.get(`/api/workspaces/${workspaceId}/execution_config`);
        if (!current.ok()) return "";
        const config = asRecord(await current.json());
        const currentEnvironment = firstText(config.environment);
        const currentNetworkMode = firstText(config.network_mode);
        if (currentEnvironment !== environment) return "";
        if (networkMode && currentNetworkMode !== networkMode) return "";
        return "ok";
      },
      { timeout: 60_000, intervals: [1_000, 2_000, 3_000] },
    )
    .toBe("ok");
}

async function ensureWorkspaceExecutionLaunched(
  request: APIRequestContext,
  workspaceId: string,
  environment: ExecutionEnvironment,
): Promise<void> {
  if (environment === "host") return;

  await ensureLocalLinuxSandboxPrepared(request);

  const start = await request.post("/api/execution/launch/start", {
    data: {
      kind: "workspace_launch",
      workspace_id: workspaceId,
    },
  });
  if (!start.ok()) {
    const body = normalizeErrorMessage(await start.text().catch(() => ""));
    throw createStageError(
      "execution_launch",
      `workspace execution launch start failed (${start.status()}): ${body}`,
    );
  }

  const started = asRecord(await start.json());
  const jobId = firstText(started.job_id);
  if (!jobId) {
    throw createStageError(
      "execution_launch",
      `workspace execution launch response missing job_id: ${JSON.stringify(started)}`,
    );
  }

  let state = firstText(started.state).toLowerCase();
  let launchError = firstText(started.error);
  const deadline = Date.now() + 10 * 60_000;
  while (true) {
    if (state === "ready") return;
    if (state === "error") {
      const detail = normalizeErrorMessage(launchError || "unknown execution launch error");
      throw createStageError("execution_launch", `workspace execution launch failed: ${detail}`);
    }
    if (Date.now() >= deadline) {
      throw createStageError(
        "execution_launch",
        `workspace execution launch timed out after 600000ms (last_state=${state || "unknown"})`,
      );
    }

    await new Promise((resolve) => setTimeout(resolve, 2_000));
    const status = await request.get(`/api/execution/launch/status?job_id=${encodeURIComponent(jobId)}`);
    if (!status.ok()) {
      const body = normalizeErrorMessage(await status.text().catch(() => ""));
      throw createStageError(
        "execution_launch",
        `workspace execution launch status failed (${status.status()}): ${body}`,
      );
    }
    const snapshot = asRecord(await status.json());
    state = firstText(snapshot.state).toLowerCase();
    launchError = firstText(snapshot.error);
  }
}

async function configureOpenRouterEndpoint(
  request: APIRequestContext,
  providerId: string,
  baseUrl: string,
  apiKey: string,
  modelOverride: string,
): Promise<void> {
  const endpointName = `${providerId}-openrouter-smoke`;
  const upsert = await request.post(`/api/providers/${providerId}/harness_config/endpoints`, {
    data: {
      name: endpointName,
      base_url: baseUrl,
      auth_type: "api_key",
      api_key: apiKey,
      model_override: modelOverride,
    },
  });
  if (!upsert.ok()) {
    const body = normalizeErrorMessage(await upsert.text().catch(() => ""));
    throw createStageError("endpoint_config", `endpoint upsert failed (${upsert.status()}): ${body}`);
  }
  const config = asRecord(await upsert.json());
  const endpoints = asArray(config.endpoints).map((entry) => asRecord(entry));
  const chosen =
    endpoints.find((entry) => readString(entry.name) === endpointName)
    ?? endpoints.find((entry) => readString(entry.id) === readString(config.selected_endpoint_id))
    ?? asRecord({});
  const endpointId = firstText(chosen.id, config.selected_endpoint_id);
  if (!endpointId) {
    throw createStageError("endpoint_config", `endpoint upsert returned no endpoint id: ${JSON.stringify(config)}`);
  }
  const select = await request.post(`/api/providers/${providerId}/harness_config/select`, {
    data: {
      source_kind: "endpoint",
      endpoint_id: endpointId,
    },
  });
  if (!select.ok()) {
    const body = normalizeErrorMessage(await select.text().catch(() => ""));
    throw createStageError("endpoint_config", `endpoint select failed (${select.status()}): ${body}`);
  }
}

async function verifyProviderForWorkspace(
  request: APIRequestContext,
  workspaceId: string,
  providerId: string,
): Promise<void> {
  const response = await request.post(`/api/workspaces/${workspaceId}/providers/${providerId}/verify`, {
    data: {},
    timeout: 30_000,
  });
  if (!response.ok()) {
    const body = normalizeErrorMessage(await response.text().catch(() => ""));
    throw createStageError("verify", `provider verify request failed (${response.status()}): ${body}`);
  }
  const payload = asRecord(await response.json());
  const status = firstText(payload.status).toLowerCase();
  if (status !== "ok") {
    const detail = normalizeErrorMessage(firstText(payload.message, JSON.stringify(payload)));
    throw createStageError("verify", `provider verify failed (status=${status}): ${detail}`);
  }
}

async function resolveWorkspaceProviderModelId(
  request: APIRequestContext,
  workspaceId: string,
  providerId: string,
): Promise<string> {
  let resolved = "";
  await expect
    .poll(
      async () => {
        const response = await request.get(`/api/workspaces/${workspaceId}/providers/${providerId}/options`);
        if (!response.ok()) {
          return "";
        }
        const payload = asRecord(await response.json());
        const models = asRecord(payload.models);
        const currentModel = firstText(models.current_model_id, models.currentModelId);
        if (currentModel) {
          resolved = currentModel;
          return currentModel;
        }
        const firstModel =
          asArray(models.models)
            .map((entry) => asRecord(entry))
            .map((entry) => firstText(entry.id, entry.model_id, entry.modelId, entry.name))
            .find(Boolean) || "";
        resolved = firstModel;
        return firstModel;
      },
      { timeout: 60_000, intervals: [1_000, 2_000, 3_000] },
    )
    .not.toBe("");
  return resolved;
}

function extractErrorMessage(snapshot: Record<string, unknown>): string {
  const head = asRecord(snapshot.head);
  const turns = asArray(head.turns).map((entry) => asRecord(entry));
  const events = asArray(head.events).map((entry) => asRecord(entry));
  for (let i = events.length - 1; i >= 0; i -= 1) {
    const event = events[i];
    const payload = asRecord(event.payload_json);
    const message = firstText(payload.message, payload.error, payload.reason, payload.detail, payload.stderr, payload.stdout);
    if (message) {
      const eventType = firstText(event.event_type);
      return normalizeErrorMessage(eventType ? `[${eventType}] ${message}` : message);
    }
  }
  const lastTurn = turns.length > 0 ? turns[turns.length - 1] : {};
  return normalizeErrorMessage(firstText(lastTurn.status, "no explicit error payload"));
}

function toTerminalState(snapshot: Record<string, unknown>): TerminalState {
  const head = asRecord(snapshot.head);
  const activity = asRecord(head.activity);
  const turns = asArray(head.turns).map((entry) => asRecord(entry));
  const messages = asArray(head.messages).map((entry) => asRecord(entry));
  const lastTurn = turns.length > 0 ? turns[turns.length - 1] : {};
  const terminalStatus = firstText(lastTurn.status, activity.last_turn_status).toLowerCase() || null;
  const assistantMessages = messages.filter((message) => {
    if (firstText(message.role) !== "assistant") return false;
    return firstText(message.content).length > 0;
  }).length;
  const headWorking = activity.is_working === true;
  const done = terminalStatus ? TERMINAL_TURN_STATUSES.has(terminalStatus) : !headWorking && assistantMessages > 0;
  const modelId = firstText(asRecord(head.session).model_id) || null;
  const failed = terminalStatus === "failed" || terminalStatus === "interrupted";
  return {
    done,
    terminalStatus,
    assistantMessages,
    errorMessage: failed ? extractErrorMessage(snapshot) : null,
    modelId,
  };
}

async function waitForTerminalState(
  request: APIRequestContext,
  sessionId: string,
  timeoutMs: number,
): Promise<TerminalState> {
  let resolved: TerminalState | null = null;
  await expect
    .poll(
      async () => {
        const response = await request.get(`/api/sessions/${sessionId}/head?include_events=1&limit=80`);
        if (!response.ok()) return "";
        const state = toTerminalState({ head: asRecord(await response.json()) });
        if (!state.done) return "";
        resolved = state;
        return "done";
      },
      { timeout: timeoutMs, intervals: [1_000, 2_000, 3_000] },
    )
    .toBe("done");
  if (!resolved) {
    throw createStageError("first_turn", `session ${sessionId} did not reach terminal state`);
  }
  return resolved;
}

async function runFirstTurnAttempt(
  request: APIRequestContext,
  workspaceId: string,
  providerId: string,
  environment: ExecutionEnvironment,
  modelId: string,
  timeoutMs: number,
): Promise<{ sessionId: string; terminal: TerminalState }> {
  let stage = "task_create";
  const taskResp = await request.post(`/api/workspaces/${workspaceId}/tasks`, {
    data: {
      title: `runtime-install-smoke-${providerId}-${Date.now()}`,
      default_session: {
        provider_id: providerId,
        model_id: modelId,
        execution_environment: environment,
      },
    },
  });
  if (!taskResp.ok()) {
    throw createStageError(stage, `task create failed (${taskResp.status()}): ${normalizeErrorMessage(await taskResp.text().catch(() => ""))}`);
  }
  const task = asRecord(await taskResp.json());
  const taskId = firstText(task.id);
  if (!taskId) {
    throw createStageError(stage, "task create returned empty task id");
  }

  stage = "session_create";
  const sessionId = firstText(task.primary_session_id);
  if (!sessionId) {
    throw createStageError(stage, "task create returned empty default session id");
  }

  stage = "first_turn_request";
  const prompt = `runtime-install-smoke-${providerId}-${Date.now()}: reply with exactly the word pong`;
  const messageResp = await request.post(`/api/sessions/${sessionId}/messages`, {
    data: {
      content: prompt,
      delivery: "immediate",
    },
  });
  if (!messageResp.ok()) {
    throw createStageError(stage, `session message failed (${messageResp.status()}): ${normalizeErrorMessage(await messageResp.text().catch(() => ""))}`);
  }

  stage = "first_turn";
  const terminal = await waitForTerminalState(request, sessionId, timeoutMs);
  if (terminal.terminalStatus !== "completed" || terminal.assistantMessages <= 0) {
    const detail = normalizeErrorMessage(
      firstText(terminal.errorMessage, `terminal_status=${terminal.terminalStatus}`, "assistant completion missing"),
    );
    throw createStageError(stage, `runtime install smoke session failed: ${detail}`);
  }

  return { sessionId, terminal };
}

function summarizeDistribution(results: ProviderResult[]): Record<string, number> {
  const out: Record<string, number> = {};
  for (const row of results) {
    out[row.result] = (out[row.result] || 0) + 1;
  }
  return out;
}

function summarizeFailureCategories(results: ProviderResult[]): Record<string, number> {
  const out: Record<string, number> = {};
  for (const row of results) {
    if (row.result !== "fail") continue;
    const key = row.category || "unknown";
    out[key] = (out[key] || 0) + 1;
  }
  return out;
}

async function runProvider(
  request: APIRequestContext,
  workspaceId: string,
  providerId: string,
  installTarget: InstallTarget,
  environment: ExecutionEnvironment,
  networkMode: NetworkMode,
  baseUrl: string,
  apiKey: string,
  modelOverride: string,
  terminalTimeoutMs: number,
  installOnly: boolean,
): Promise<ProviderResult> {
  const started = Date.now();
  let stage = "status_before_install";
  let installId: string | null = null;
  let sessionId: string | null = null;
  let modelId: string | null = null;

  try {
    const providerBefore = await getProviderStatus(request, providerId, installTarget);
    console.log(`runtime install smoke: provider=${providerId} before installed=${providerBefore.installed} health=${providerBefore.health}`);

    stage = "install";
    if (shouldSkipBundledOnlyInstall(providerId)) {
      if (!providerBefore.installed || providerBefore.health !== "ok") {
        const detail = normalizeErrorMessage(
          firstText(providerBefore.diagnostics[0], `installed=${providerBefore.installed} health=${providerBefore.health}`),
        );
        throw createStageError(stage, `bundled-only provider unavailable before install: ${detail}`);
      }
      console.log(`runtime install smoke: provider=${providerId} install skipped bundled_only=true`);
    } else {
      installId = (await installProviderAndWait(request, providerId, installTarget)).installId;
      console.log(`runtime install smoke: provider=${providerId} install_id=${installId} completed`);
    }

    stage = "status_after_install";
    const providerAfter = await getProviderStatus(request, providerId, installTarget);
    if (!providerAfter.installed || providerAfter.health !== "ok") {
      const detail = normalizeErrorMessage(firstText(providerAfter.diagnostics[0], `installed=${providerAfter.installed} health=${providerAfter.health}`));
      throw createStageError(stage, `provider unhealthy after install: ${detail}`);
    }

    if (installOnly) {
      return {
        provider_id: providerId,
        install_target: installTarget,
        environment,
        network_mode: networkMode,
        model_override: modelOverride,
        install_id: installId,
        session_id: null,
        model_id: null,
        terminal_status: null,
        assistant_messages: 0,
        stage,
        error_code: null,
        category: null,
        reason: "provider installed and reported healthy",
        result: "pass",
        elapsed_ms: Date.now() - started,
      };
    }

    stage = "endpoint_config";
    await configureOpenRouterEndpoint(request, providerId, baseUrl, apiKey, modelOverride);

    stage = "verify";
    await verifyProviderForWorkspace(request, workspaceId, providerId);

    stage = "model_resolve";
    modelId = await resolveWorkspaceProviderModelId(request, workspaceId, providerId);
    expect(modelId).not.toBe("");

    let terminal: TerminalState | null = null;
    let lastError: StageError | null = null;
    for (let attempt = 1; attempt <= FIRST_TURN_MAX_ATTEMPTS; attempt += 1) {
      try {
        const attemptResult = await runFirstTurnAttempt(
          request,
          workspaceId,
          providerId,
          environment,
          modelId,
          terminalTimeoutMs,
        );
        sessionId = attemptResult.sessionId;
        terminal = attemptResult.terminal;
        break;
      } catch (rawError) {
        const error = toStageError("first_turn", rawError);
        const failureStage = error.stage || "first_turn";
        const reason = normalizeErrorMessage(error.message || String(error));
        const category = classifyInstallSmokeFailureCategory(failureStage, reason, error.errorCode || null);
        if (sessionId) {
          console.log(
            `runtime install smoke: provider=${providerId} attempt=${attempt} session_id=${sessionId} failed stage=${failureStage} category=${category} reason=${reason}`,
          );
        }
        if (shouldRetryInstallSmokeFirstTurnFailure(failureStage, category, attempt, FIRST_TURN_MAX_ATTEMPTS)) {
          const delayMs = FIRST_TURN_RETRY_BASE_DELAY_MS * attempt;
          console.log(
            `runtime install smoke: provider=${providerId} retrying first turn after external outage attempt=${attempt}/${FIRST_TURN_MAX_ATTEMPTS} wait_ms=${delayMs}`,
          );
          await new Promise((resolve) => setTimeout(resolve, delayMs));
          continue;
        }
        lastError = error;
        break;
      }
    }
    if (!terminal) {
      throw lastError || createStageError("first_turn", `runtime install smoke session failed for provider ${providerId}`);
    }

    return {
      provider_id: providerId,
      install_target: installTarget,
      environment,
      network_mode: networkMode,
      model_override: modelOverride,
      install_id: installId,
      session_id: sessionId,
      model_id: terminal.modelId || modelId,
      terminal_status: terminal.terminalStatus,
      assistant_messages: terminal.assistantMessages,
      stage,
      error_code: null,
      category: null,
      reason: "assistant completion observed",
      result: "pass",
      elapsed_ms: Date.now() - started,
    };
  } catch (rawError) {
    const error = toStageError(stage, rawError);
    const reason = normalizeErrorMessage(error.message || String(rawError));
    const category = classifyInstallSmokeFailureCategory(error.stage || stage, reason, error.errorCode || null);

    return {
      provider_id: providerId,
      install_target: installTarget,
      environment,
      network_mode: networkMode,
      model_override: modelOverride,
      install_id: installId,
      session_id: sessionId,
      model_id: modelId,
      terminal_status: null,
      assistant_messages: 0,
      stage: error.stage || stage,
      error_code: error.errorCode || null,
      category,
      reason,
      result: "fail",
      elapsed_ms: Date.now() - started,
    };
  }
}

async function persistReport(testInfo: TestInfo, report: Record<string, unknown>, reportPath: string): Promise<void> {
  const serialized = `${JSON.stringify(report, null, 2)}\n`;
  await testInfo.attach("runtime-install-openrouter-smoke-report", {
    body: serialized,
    contentType: "application/json",
  });
  if (!reportPath) return;
  const absolutePath = path.isAbsolute(reportPath) ? reportPath : path.resolve(process.cwd(), reportPath);
  mkdirSync(path.dirname(absolutePath), { recursive: true });
  writeFileSync(absolutePath, serialized, "utf8");
  console.log(`runtime install smoke report written: ${absolutePath}`);
}

test("runtime install smoke: provider matrix install/probe/first-turn via OpenRouter", async ({ request }, testInfo) => {
  test.setTimeout(35 * 60_000);

  if ((process.env.CTX_E2E_TIER ?? "") !== "endpoint-ui") {
    test.skip(true, "set CTX_E2E_TIER=endpoint-ui to run runtime install OpenRouter smoke");
  }

  const apiKey = (process.env.OPENROUTER_API_KEY ?? "").trim();
  const installOnly = envTruthy(process.env.CTX_E2E_INSTALL_SMOKE_INSTALL_ONLY);
  if (!installOnly && !apiKey) {
    test.skip(true, "missing OPENROUTER_API_KEY");
  }

  const singleProvider = (process.env.CTX_E2E_INSTALL_SMOKE_PROVIDER ?? DEFAULT_PROVIDER_ID).trim() || DEFAULT_PROVIDER_ID;
  let providerIds = parseCsv(process.env.CTX_E2E_INSTALL_SMOKE_PROVIDERS);

  const defaultModelOverride =
    (process.env.CTX_E2E_INSTALL_SMOKE_MODEL_OVERRIDE ?? "").trim();
  const baseUrl = (process.env.OPENROUTER_BASE_URL ?? "").trim() || DEFAULT_OPENROUTER_BASE_URL;
  const executionEnvironment =
    ((process.env.CTX_E2E_INSTALL_SMOKE_ENVIRONMENT ?? "host").trim() as ExecutionEnvironment) || "host";
  const networkMode =
    ((process.env.CTX_E2E_INSTALL_SMOKE_NETWORK_MODE ?? "llm_only").trim() as NetworkMode) || "llm_only";
  const allowlist = parseCsv(process.env.CTX_E2E_INSTALL_SMOKE_ALLOWLIST);
  const installTarget: InstallTarget = executionEnvironment === "host" ? "host" : "container";
  const terminalTimeoutMs = envInt(process.env.CTX_E2E_INSTALL_SMOKE_TERMINAL_TIMEOUT_MS, DEFAULT_TERMINAL_TIMEOUT_MS);
  const allowFailures = envTruthy(process.env.CTX_E2E_INSTALL_SMOKE_ALLOW_FAILURES);
  const reportPath = (process.env.CTX_E2E_INSTALL_SMOKE_REPORT_PATH ?? "").trim();

  const repo = initRepo();
  const workspaceResp = await request.post("/api/workspaces", {
    data: {
      root_path: repo,
      name: `runtime-install-smoke-${Date.now()}`,
    },
  });
  expect(workspaceResp.ok(), `workspace create failed (${workspaceResp.status()})`).toBeTruthy();
  const workspaceId = firstText(asRecord(await workspaceResp.json()).id);
  expect(workspaceId).not.toBe("");

  await configureWorkspaceExecution(request, workspaceId, executionEnvironment, networkMode, allowlist);
  await ensureWorkspaceExecutionLaunched(request, workspaceId, executionEnvironment);

  if (providerIds.length === 0) {
    providerIds = [singleProvider];
  }
  if (providerIds.includes("all")) {
    if (providerIds.length !== 1) {
      throw new Error("CTX_E2E_INSTALL_SMOKE_PROVIDERS=all must not be combined with explicit provider ids");
    }
    providerIds = installSupportedProviderIds(await listProviderStatuses(request, installTarget));
  }
  expect(providerIds, `no install-supported providers found for target=${installTarget}`).not.toEqual([]);

  const results: ProviderResult[] = [];
  for (const providerId of providerIds) {
    const perProviderModelOverride =
      (process.env[providerOverrideEnvVar(providerId)] ?? "").trim()
      || defaultModelOverride
      || providerDefaultOpenRouterModelOverride(providerId);
    const result = await runProvider(
      request,
      workspaceId,
      providerId,
      installTarget,
      executionEnvironment,
      networkMode,
      baseUrl,
      apiKey,
      perProviderModelOverride,
      terminalTimeoutMs,
      installOnly,
    );
    results.push(result);
    console.log(
      `runtime install smoke: provider=${providerId} result=${result.result} stage=${result.stage} error_code=${result.error_code || ""} reason=${result.reason}`,
    );
  }

  const report = {
    generated_at: new Date().toISOString(),
    workspace_id: workspaceId,
    provider_ids: providerIds,
    install_target: installTarget,
    execution_environment: executionEnvironment,
    network_mode: networkMode,
    allow_failures: allowFailures,
    install_only: installOnly,
    terminal_timeout_ms: terminalTimeoutMs,
    distribution: summarizeDistribution(results),
    failure_categories: summarizeFailureCategories(results),
    results,
  };

  await persistReport(testInfo, report, reportPath);

  const failures = results.filter((row) => row.result === "fail");
  const summaryLines = results.map(
    (row) => `${row.provider_id} | ${row.result} | ${row.stage} | ${row.error_code || "-"} | ${row.reason}`,
  );
  console.log(["provider | result | stage | error_code | reason", ...summaryLines].join("\n"));

  if (!allowFailures) {
    expect(failures, `runtime install smoke failures detected\n${summaryLines.join("\n")}`).toEqual([]);
  }
});
