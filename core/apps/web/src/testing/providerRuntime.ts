import { existsSync, readFileSync } from "fs";
import path from "path";

export type JsonResponseLike = {
  ok(): boolean;
  status(): number;
  json(): Promise<unknown>;
  text(): Promise<string>;
};

export type JsonRequestLike = {
  get(url: string, options?: { timeout?: number }): Promise<JsonResponseLike>;
  post(url: string, options?: { data?: unknown; timeout?: number }): Promise<JsonResponseLike>;
};

export type ProviderInstallTarget = "host" | "container";

export type ProviderStatus = {
  installed: boolean;
  health: string;
  diagnostics: string[];
  details: Record<string, string>;
  usability: {
    usable: boolean;
    status: string;
    reason_code?: string | null;
    reason?: string | null;
    blocking_provider_ids: string[];
    recommended_action: string;
  };
};

export type TerminalState = {
  done: boolean;
  terminalStatus: string | null;
  assistantMessages: number;
  errorMessage: string | null;
  modelId: string | null;
};

type PollOptions = {
  timeoutMs?: number;
  pollMs?: number;
  requestTimeoutMs?: number;
  sleep?: (ms: number) => Promise<void>;
  now?: () => number;
};

const DEFAULT_REQUEST_TIMEOUT_MS = 30_000;
const DEFAULT_INSTALL_TIMEOUT_MS = 10 * 60_000;
const DEFAULT_VERIFY_TIMEOUT_MS = 90_000;
const DEFAULT_MODEL_TIMEOUT_MS = 90_000;
const DEFAULT_TERMINAL_TIMEOUT_MS = 180_000;
const TERMINAL_TURN_STATUSES = new Set(["completed", "failed", "interrupted"]);
const TRUTHY_ENV_VALUES = new Set(["1", "true", "yes", "on"]);

export const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

export const asArray = (value: unknown): unknown[] => (Array.isArray(value) ? value : []);

export const readString = (value: unknown): string => (typeof value === "string" ? value : "");

export const firstText = (...values: unknown[]): string => {
  for (const value of values) {
    const text = readString(value).trim();
    if (text) return text;
  }
  return "";
};

export const normalizeErrorMessage = (raw: string): string => raw.replace(/\s+/g, " ").trim();
export const normalizeWriteFileContents = (raw: string): string =>
  raw.replace(/\r\n/g, "\n").trimEnd();
export const bundledOnlyModeAppliesToProvider = (
  providerId: string,
  env: NodeJS.ProcessEnv = process.env,
): boolean => {
  if (!TRUTHY_ENV_VALUES.has(String(env.CTX_E2E_BUNDLED_ONLY ?? "").trim().toLowerCase())) {
    return false;
  }

  const providers = String(env.CTX_E2E_BUNDLED_ONLY_PROVIDERS ?? "")
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean);
  if (providers.length === 0) {
    return true;
  }

  return providers.includes(providerId.trim());
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

const defaultSleep = async (ms: number): Promise<void> => {
  await new Promise((resolve) => {
    setTimeout(resolve, Math.max(0, ms));
  });
};

const withInstallTargetParam = (requestPath: string, target: string): string => {
  const normalizedTarget = readString(target).trim();
  if (!normalizedTarget) return requestPath;
  const separator = requestPath.includes("?") ? "&" : "?";
  return `${requestPath}${separator}target=${encodeURIComponent(normalizedTarget)}`;
};

const responseBodyText = async (response: JsonResponseLike): Promise<string> => {
  try {
    return normalizeErrorMessage(await response.text());
  } catch {
    return "";
  }
};

const extractTerminalErrorMessage = (head: Record<string, unknown>): string => {
  const turns = asArray(head.turns).map((entry) => asRecord(entry));
  const events = asArray(head.events).map((entry) => asRecord(entry));
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const event = events[index];
    const payload = asRecord(event.payload_json);
    const message = firstText(payload.message, payload.error, payload.reason, payload.detail, payload.stderr, payload.stdout);
    if (message) {
      const eventType = firstText(event.event_type);
      return normalizeErrorMessage(eventType ? `[${eventType}] ${message}` : message);
    }
  }
  const lastTurn = turns.length > 0 ? turns[turns.length - 1] : {};
  return normalizeErrorMessage(firstText(lastTurn.status, "no explicit error payload"));
};

const extractLatestAssistantMessage = (head: Record<string, unknown>): string => {
  const messages = asArray(head.messages).map((entry) => asRecord(entry));
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (readString(message.role) !== "assistant") continue;
    const content = normalizeErrorMessage(readString(message.content));
    if (content) return content;
  }
  return "";
};

const resolvePollingOptions = (options: PollOptions) => ({
  timeoutMs: options.timeoutMs ?? DEFAULT_VERIFY_TIMEOUT_MS,
  pollMs: options.pollMs ?? 3_000,
  requestTimeoutMs: options.requestTimeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS,
  sleep: options.sleep ?? defaultSleep,
  now: options.now ?? Date.now,
});

export async function getProviderStatus(
  request: JsonRequestLike,
  providerId: string,
  target = "",
  options: { requestTimeoutMs?: number } = {},
): Promise<ProviderStatus> {
  const requestTimeoutMs = options.requestTimeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS;
  const response = await request.get(
    withInstallTargetParam(`/api/providers/${encodeURIComponent(providerId)}`, target),
    { timeout: requestTimeoutMs },
  );
  if (response.status() === 404) {
    return {
      installed: false,
      health: "unknown",
      diagnostics: [`provider not found: ${providerId}`],
      details: {},
      usability: {
        usable: false,
        status: "blocked",
        reason_code: "not_found",
        reason: `provider not found: ${providerId}`,
        blocking_provider_ids: [],
        recommended_action: "none",
      },
    };
  }
  if (!response.ok()) {
    const detail = await responseBodyText(response);
    throw new Error(`failed to read provider '${providerId}' (${response.status()}): ${detail}`);
  }
  const row = asRecord(await response.json());
  return {
    installed: row.installed === true,
    health: firstText(row.health, "unknown"),
    diagnostics: asArray(row.diagnostics).map((entry) => readString(entry)).filter(Boolean),
    details: readStringMap(row.details),
    usability: {
      usable: asRecord(row.usability).usable === true,
      status: firstText(asRecord(row.usability).status, "blocked"),
      reason_code: firstText(asRecord(row.usability).reason_code) || null,
      reason: firstText(asRecord(row.usability).reason) || null,
      blocking_provider_ids: asArray(asRecord(row.usability).blocking_provider_ids)
        .map((entry) => readString(entry))
        .filter(Boolean),
      recommended_action: firstText(asRecord(row.usability).recommended_action, "none"),
    },
  };
}

export async function installProviderAndWait(
  request: JsonRequestLike,
  providerId: string,
  target: ProviderInstallTarget,
  options: PollOptions = {},
): Promise<string> {
  const {
    timeoutMs = DEFAULT_INSTALL_TIMEOUT_MS,
    pollMs = 2_000,
    requestTimeoutMs = DEFAULT_REQUEST_TIMEOUT_MS,
    sleep = defaultSleep,
    now = Date.now,
  } = options;

  const start = await request.post(`/api/providers/${encodeURIComponent(providerId)}/install?target=${target}`, {
    data: {},
    timeout: requestTimeoutMs,
  });
  if (!start.ok()) {
    const detail = await responseBodyText(start);
    throw new Error(`provider install start failed (${start.status()}): ${detail}`);
  }
  const payload = asRecord(await start.json());
  const installId = readString(payload.install_id);
  if (!installId) {
    throw new Error(`provider install response missing install_id: ${JSON.stringify(payload)}`);
  }

  const startedAt = now();
  let lastDetail = "";
  while (now() - startedAt < timeoutMs) {
    const poll = await request.get(`/api/providers/install/${encodeURIComponent(installId)}`, {
      timeout: requestTimeoutMs,
    });
    if (!poll.ok()) {
      const detail = await responseBodyText(poll);
      throw new Error(`provider install poll failed (${poll.status()}): ${detail}`);
    }
    const info = asRecord(await poll.json());
    const state = firstText(info.state).toLowerCase();
    if (state === "succeeded") return installId;
    if (state === "failed" || state === "cancelled") {
      const lastEvent = asRecord(info.last_event);
      const detail = normalizeErrorMessage(
        firstText(lastEvent.message, lastEvent.stage, info.error, JSON.stringify(info)),
      );
      throw new Error(`provider install ${state}: ${detail}`);
    }
    lastDetail = normalizeErrorMessage(firstText(asRecord(info.last_event).message, asRecord(info.last_event).stage));
    await sleep(pollMs);
  }

  const timeoutDetail = lastDetail ? `: ${lastDetail}` : "";
  throw new Error(`provider install timed out for ${providerId} (${installId})${timeoutDetail}`);
}

export async function ensureProviderInstalledAndHealthy(
  request: JsonRequestLike,
  providerId: string,
  target: ProviderInstallTarget,
  options: PollOptions = {},
): Promise<ProviderStatus> {
  const requestTimeoutMs = options.requestTimeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS;
  const initial = await getProviderStatus(request, providerId, target, { requestTimeoutMs });
  if (!initial.installed && !bundledOnlyModeAppliesToProvider(providerId)) {
    await installProviderAndWait(request, providerId, target, options);
  }
  const finalStatus = initial.installed ? initial : await getProviderStatus(request, providerId, target, { requestTimeoutMs });
  if (!finalStatus.usability.usable) {
    const detail = finalStatus.usability.reason
      ?? finalStatus.diagnostics[0]
      ?? `installed=${String(finalStatus.installed)} health=${finalStatus.health || "unknown"} usability=${finalStatus.usability.status}`;
    throw new Error(`provider ${providerId} is not ready for target=${target}: ${detail}`);
  }
  return finalStatus;
}

export async function verifyProviderForWorkspace(
  request: JsonRequestLike,
  workspaceId: string,
  providerId: string,
  options: PollOptions = {},
): Promise<Record<string, unknown>> {
  const {
    timeoutMs,
    pollMs,
    requestTimeoutMs,
    sleep,
    now,
  } = resolvePollingOptions(options);

  const startedAt = now();
  let lastDetail = "";
  while (now() - startedAt < timeoutMs) {
    let response: JsonResponseLike;
    try {
      response = await request.post(`/api/workspaces/${workspaceId}/providers/${encodeURIComponent(providerId)}/verify`, {
        data: {},
        timeout: requestTimeoutMs,
      });
    } catch (error) {
      lastDetail = normalizeErrorMessage(
        `verify request failed for ${providerId}: ${error instanceof Error ? error.message : String(error)}`,
      );
      await sleep(pollMs);
      continue;
    }

    if (!response.ok()) {
      const detail = await responseBodyText(response);
      lastDetail = detail || `verify request failed (${response.status()})`;
      await sleep(pollMs);
      continue;
    }

    const payload = asRecord(await response.json());
    const status = firstText(payload.status).toLowerCase();
    if (status === "ok") return payload;
    lastDetail = normalizeErrorMessage(firstText(payload.message, payload.error, `verify status=${status || "unknown"}`));
    await sleep(pollMs);
  }

  throw new Error(`provider verify failed for ${providerId} in workspace ${workspaceId}: ${lastDetail || "timed out"}`);
}

export async function resolveWorkspaceProviderModelId(
  request: JsonRequestLike,
  workspaceId: string,
  providerId: string,
  options: PollOptions = {},
): Promise<string> {
  const {
    timeoutMs = DEFAULT_MODEL_TIMEOUT_MS,
    pollMs = 3_000,
    requestTimeoutMs = DEFAULT_REQUEST_TIMEOUT_MS,
    sleep = defaultSleep,
    now = Date.now,
  } = options;

  const startedAt = now();
  let lastDetail = "";
  while (now() - startedAt < timeoutMs) {
    const response = await request.get(`/api/workspaces/${workspaceId}/providers/${encodeURIComponent(providerId)}/options`, {
      timeout: requestTimeoutMs,
    });
    if (!response.ok()) {
      lastDetail = `failed to read provider options (${response.status()})`;
      await sleep(pollMs);
      continue;
    }

    const payload = asRecord(await response.json());
    const models = asRecord(payload.models);
    const currentModel = firstText(models.current_model_id, models.currentModelId);
    if (currentModel) return currentModel;

    const firstModel = asArray(models.models)
      .map((entry) => asRecord(entry))
      .map((entry) => firstText(entry.id, entry.model_id, entry.modelId, entry.name))
      .find(Boolean) ?? "";
    if (firstModel) return firstModel;

    lastDetail = "provider options did not return a usable model id";
    await sleep(pollMs);
  }

  throw new Error(`provider model resolution failed for ${providerId} in workspace ${workspaceId}: ${lastDetail || "timed out"}`);
}

export function toTerminalState(head: Record<string, unknown>): TerminalState {
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
  const failed = terminalStatus === "failed" || terminalStatus === "interrupted";
  return {
    done,
    terminalStatus,
    assistantMessages,
    errorMessage: failed ? extractTerminalErrorMessage(head) : null,
    modelId: firstText(asRecord(head.session).model_id) || null,
  };
}

export async function waitForTerminalState(
  request: JsonRequestLike,
  sessionId: string,
  options: PollOptions = {},
): Promise<TerminalState> {
  const {
    timeoutMs = DEFAULT_TERMINAL_TIMEOUT_MS,
    pollMs = 3_000,
    requestTimeoutMs = DEFAULT_REQUEST_TIMEOUT_MS,
    sleep = defaultSleep,
    now = Date.now,
  } = options;

  const startedAt = now();
  let lastDetail = "";
  while (now() - startedAt < timeoutMs) {
    const response = await request.get(`/api/sessions/${sessionId}/head?include_events=1&limit=80`, {
      timeout: requestTimeoutMs,
    });
    if (!response.ok()) {
      lastDetail = `failed to read session head (${response.status()})`;
      await sleep(pollMs);
      continue;
    }

    const state = toTerminalState(asRecord(await response.json()));
    if (state.done) return state;
    lastDetail = normalizeErrorMessage(
      firstText(
        state.errorMessage,
        state.terminalStatus ? `terminal_status=${state.terminalStatus}` : "",
        `assistant_messages=${state.assistantMessages}`,
      ),
    );
    await sleep(pollMs);
  }

  throw new Error(`session ${sessionId} did not reach terminal state: ${lastDetail || "timed out"}`);
}

export async function waitForWorkspaceFileContents(
  workspaceRoot: string,
  relativePath: string,
  expectedContents: string,
  options: Pick<PollOptions, "timeoutMs" | "pollMs" | "sleep" | "now"> = {},
): Promise<string> {
  const {
    timeoutMs = DEFAULT_TERMINAL_TIMEOUT_MS,
    pollMs = 1_000,
    sleep = defaultSleep,
    now = Date.now,
  } = options;

  const trimmedRoot = readString(workspaceRoot).trim();
  const trimmedRelativePath = readString(relativePath).trim();
  if (!trimmedRoot) {
    throw new Error("workspaceRoot is required");
  }
  if (!trimmedRelativePath) {
    throw new Error("relativePath is required");
  }

  const filePath = path.join(trimmedRoot, trimmedRelativePath);
  const startedAt = now();
  let lastDetail = `file not found: ${filePath}`;
  while (now() - startedAt < timeoutMs) {
    if (!existsSync(filePath)) {
      lastDetail = `file not found: ${filePath}`;
      await sleep(pollMs);
      continue;
    }

    const actualContents = readFileSync(filePath, "utf8");
    if (normalizeWriteFileContents(actualContents) === normalizeWriteFileContents(expectedContents)) {
      return filePath;
    }
    lastDetail = `unexpected normalized contents for ${trimmedRelativePath}: ${JSON.stringify(actualContents)}`;
    await sleep(pollMs);
  }

  throw new Error(`workspace file assertion failed: ${lastDetail}`);
}

export async function resolveSessionWorktreeRoot(
  request: JsonRequestLike,
  sessionId: string,
  options: { requestTimeoutMs?: number } = {},
): Promise<string> {
  const requestTimeoutMs = options.requestTimeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS;
  const snapshotResponse = await request.get(`/api/sessions/${sessionId}/snapshot?limit=1`, {
    timeout: requestTimeoutMs,
  });
  if (!snapshotResponse.ok()) {
    const detail = await responseBodyText(snapshotResponse);
    throw new Error(`failed to read session snapshot (${snapshotResponse.status()}): ${detail}`);
  }

  const snapshot = asRecord(await snapshotResponse.json());
  const worktreeId = firstText(
    asRecord(asRecord(snapshot.head).session).worktree_id,
    asRecord(asRecord(snapshot.summary).session).worktree_id,
  );
  if (!worktreeId) {
    throw new Error(`session ${sessionId} snapshot did not include worktree_id`);
  }

  const worktreeResponse = await request.get(`/api/worktrees/${encodeURIComponent(worktreeId)}`, {
    timeout: requestTimeoutMs,
  });
  if (!worktreeResponse.ok()) {
    const detail = await responseBodyText(worktreeResponse);
    throw new Error(`failed to read worktree ${worktreeId} (${worktreeResponse.status()}): ${detail}`);
  }

  const worktree = asRecord(await worktreeResponse.json());
  const rootPath = firstText(worktree.root_path);
  if (!rootPath) {
    throw new Error(`worktree ${worktreeId} response missing root_path`);
  }
  return rootPath;
}

export async function waitForSessionWorkspaceFileContents(
  request: JsonRequestLike,
  sessionId: string,
  relativePath: string,
  expectedContents: string,
  options: PollOptions = {},
): Promise<string> {
  const requestTimeoutMs = options.requestTimeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS;
  const workspaceRoot = await resolveSessionWorktreeRoot(request, sessionId, { requestTimeoutMs });
  try {
    return await waitForWorkspaceFileContents(workspaceRoot, relativePath, expectedContents, options);
  } catch (error) {
    const baseMessage = error instanceof Error ? error.message : String(error);
    try {
      const headResponse = await request.get(`/api/sessions/${sessionId}/head?include_events=1&limit=80`, {
        timeout: requestTimeoutMs,
      });
      if (!headResponse.ok()) {
        throw new Error(baseMessage);
      }
      const head = asRecord(await headResponse.json());
      const assistantMessage = extractLatestAssistantMessage(head);
      const runtimeError = extractTerminalErrorMessage(head);
      const hint = normalizeErrorMessage(firstText(assistantMessage, runtimeError));
      if (!hint) {
        throw new Error(baseMessage);
      }
      throw new Error(`${baseMessage}; session_hint=${hint}`);
    } catch (hintError) {
      throw new Error(hintError instanceof Error ? hintError.message : baseMessage);
    }
  }
}
