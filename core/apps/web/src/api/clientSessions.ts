import type {
  Artifact,
  ExecutionEnvironment,
  Message,
  MessageAttachment,
  Session,
  SessionEventsPage,
  SessionHeadSnapshot,
  SessionHistoryPage,
  SessionSnapshot,
  SessionState,
  SessionSummary,
  SessionTurnTool,
  SubagentInvocation,
} from "@ctx/types";
import type { BlobUploadResp } from "../generated/desktop-ipc";
import { apiAny, authToken, daemonFetchRaw } from "./clientBase";
import {
  artifactResourceUrl,
  blobResourceUrl,
  browserResourcePathForScope,
} from "./browserResourceUrls";
import { getDaemonConnection } from "./daemonConnection";
import { desktopUploadBlob, isDesktopApp } from "../utils/desktop";
import {
  trackFeatureUsed,
  trackFirstTurnSubmitted,
  trackProviderSelected,
  trackSessionCreated,
  trackUserMessageSent,
} from "../utils/analytics";
import { resolveImageMimeType } from "../utils/imageMime";
import { composeModelId, parseModelId } from "../utils/modelEffort";

export type WebSessionViewport = {
  width: number;
  height: number;
};

export type WebSessionInfo = {
  id: string;
  kind: string;
  session_id?: string | null;
  worktree_id?: string | null;
  status: string;
  created_at: string;
  updated_at: string;
  last_activity: string;
  url: string;
  viewport: WebSessionViewport;
  fps: number;
  viewers: number;
  stream_path: string;
  stream_url?: string | null;
};

export type WebSessionStreamConnectInfo = {
  stream_path: string;
  stream_url?: string | null;
  expires_at: string;
};

export const createSession = (
  taskId: string,
  provider_id: string,
  model_id: string,
  opts?: {
    id?: string;
    parent_session_id?: string | null;
    relationship?: string | null;
    reasoning_effort?: string | null;
    remember_model_preference?: boolean;
    execution_environment?: ExecutionEnvironment;
    worktree_id?: string | null;
    initial_prompt?: string | null;
    initial_message_id?: string | null;
    initial_turn_id?: string | null;
  },
) => {
  const hasPrompt = Boolean(opts?.initial_prompt);
  const hasIds = Boolean(opts?.initial_message_id && opts?.initial_turn_id);
  if (hasPrompt && !hasIds) {
    throw new Error("createSession requires initial_message_id and initial_turn_id when initial_prompt is provided");
  }
  const parsedModel = parseModelId(model_id);
  const reasoningEffort = opts?.reasoning_effort ?? parsedModel.effort;
  return apiAny<Session>(`/api/tasks/${taskId}/sessions`, {
    method: "POST",
    body: JSON.stringify({
      ...(opts?.id ? { id: opts.id } : {}),
      provider_id,
      model_id,
      ...(opts?.reasoning_effort ? { reasoning_effort: opts.reasoning_effort } : {}),
      ...(opts?.remember_model_preference ? { remember_model_preference: true } : {}),
      ...(opts?.parent_session_id ? { parent_session_id: opts.parent_session_id } : {}),
      ...(opts?.relationship ? { relationship: opts.relationship } : {}),
      ...(opts?.execution_environment ? { execution_environment: opts.execution_environment } : {}),
      ...(opts?.worktree_id ? { worktree_id: opts.worktree_id } : {}),
      ...(opts?.initial_prompt ? { initial_prompt: opts.initial_prompt } : {}),
      ...(opts?.initial_message_id && opts?.initial_turn_id
        ? { initial_message_id: opts.initial_message_id, initial_turn_id: opts.initial_turn_id }
        : {}),
    }),
  }).then((session) => {
    const effectiveModelId = composeModelId(parsedModel.base || model_id, reasoningEffort ?? null) || model_id;
    const connection = getDaemonConnection();
    trackSessionCreated({
      providerId: provider_id,
      modelId: effectiveModelId,
      executionEnvironment: opts?.execution_environment,
      sessionRootKind: "worktree",
      sessionLocation: connection.targetScope?.kind === "desktop_ssh"
        ? "remote"
        : connection.targetScope?.kind === "desktop_local"
          ? "local"
          : daemonBaseUrlLocation(connection.baseUrl),
    });
    trackFeatureUsed("session_created");
    trackProviderSelected({
      providerId: provider_id,
      source: "session_create",
    });
    if (opts?.initial_prompt) {
      const sessionId = String(session.id ?? "").trim();
      if (sessionId) {
        trackUserMessageSent({
          providerId: provider_id,
          modelId: effectiveModelId,
          reasoningEffort,
          executionEnvironment: opts?.execution_environment,
          sessionKind:
            opts?.parent_session_id || opts?.relationship === "sub_agent"
              ? "subagent"
              : "primary",
          isFirstTurn: true,
        });
        trackFirstTurnSubmitted({
          sessionId,
          providerId: provider_id,
          modelId: effectiveModelId,
        });
      }
    }
    return session;
  });
};

function daemonBaseUrlLocation(baseUrl: string | null): "local" | "remote" | undefined {
  if (!baseUrl) return undefined;
  try {
    const url = new URL(baseUrl);
    const hostname = url.hostname.trim().toLowerCase();
    if (!hostname) return undefined;
    if (hostname === "localhost" || hostname === "127.0.0.1" || hostname === "::1" || hostname === "[::1]") {
      return "local";
    }
    return "remote";
  } catch {
    return undefined;
  }
}

export type SessionDiffSummary = {
  base_commit_sha?: string;
  head_commit_sha?: string;
  file_count?: number;
  files?: number;
  line_additions?: number;
  additions?: number;
  line_deletions?: number;
  deletions?: number;
  available?: boolean;
  unavailable_reason?: DiffUnavailableReason | null;
};

export type DiffUnavailableReason = "no_repo" | "no_target_branch";

export type SessionDiffResponse = {
  diff: string;
  available?: boolean;
  unavailable_reason?: DiffUnavailableReason | null;
};

export type GitStatusEntry = {
  path: string;
  orig_path?: string | null;
  index_status: string;
  worktree_status: string;
};

export type GitStatusSummary = {
  raw?: string;
  summary_line?: string;
  summaryLine?: string;
  summary?: string;
  status?: string;
  lines?: string[];
  branch?: string | null;
  upstream?: string | null;
  ahead?: number;
  behind?: number;
  detached?: boolean;
  staged?: number;
  unstaged?: number;
  untracked?: number;
  entries?: GitStatusEntry[];
};

export const getSessionGitStatusSummary = (sessionId: string) =>
  apiAny<GitStatusSummary | string>(`/api/sessions/${sessionId}/git/status`);

export const getSessionDiffSummary = (sessionId: string) =>
  apiAny<SessionDiffSummary>(`/api/sessions/${sessionId}/diff/summary`);

export const getSessionSnapshot = (sessionId: string, limit?: number, includeEvents?: boolean) => {
  const qs = new URLSearchParams();
  if (limit) qs.set("limit", String(limit));
  if (includeEvents !== undefined) qs.set("include_events", includeEvents ? "1" : "0");
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return apiAny<SessionSnapshot>(`/api/sessions/${sessionId}/snapshot${suffix}`);
};

export const getSessionHead = (
  sessionId: string,
  limit?: number,
  includeEvents?: boolean,
  opts?: { minEventSeq?: number },
) => {
  const qs = new URLSearchParams();
  if (limit) qs.set("limit", String(limit));
  if (includeEvents !== undefined) qs.set("include_events", includeEvents ? "1" : "0");
  if (typeof opts?.minEventSeq === "number" && Number.isFinite(opts.minEventSeq)) {
    qs.set("min_event_seq", String(opts.minEventSeq));
  }
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return apiAny<SessionHeadSnapshot>(`/api/sessions/${sessionId}/head${suffix}`);
};

export const getSessionState = (sessionId: string) =>
  apiAny<SessionState>(`/api/sessions/${sessionId}/state`);

export type ArtifactInput = {
  absolute_file_path: string;
  name?: string | null;
  mime_type?: string | null;
};

export const setSessionArtifacts = (sessionId: string, artifacts: ArtifactInput[]) =>
  apiAny<Artifact[]>(`/api/sessions/${sessionId}/artifacts`, {
    method: "POST",
    body: JSON.stringify({ artifacts }),
  });

export const listSessionSubagents = (sessionId: string) =>
  apiAny<SessionSummary[]>(`/api/sessions/${sessionId}/subagents`);

export const listSessionSubagentInvocations = (sessionId: string, opts?: { turnId?: string }) => {
  const qs = new URLSearchParams();
  if (opts?.turnId) qs.set("turn_id", opts.turnId);
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return apiAny<SubagentInvocation[]>(`/api/sessions/${sessionId}/subagent_invocations${suffix}`);
};

export const getSubagentInvocation = (sessionId: string, invocationId: string) =>
  apiAny<SubagentInvocation>(`/api/sessions/${sessionId}/subagent_invocations/${invocationId}`);

export const listWebSessions = () => apiAny<WebSessionInfo[]>("/api/sessions/web");

export const mintWebSessionStreamPath = (sessionId: string) =>
  apiAny<WebSessionStreamConnectInfo>(`/api/sessions/web/${sessionId}/stream_token`, {
    method: "POST",
  });

export const getSessionEvents = (
  sessionId: string,
  opts?: { afterSeq?: number; limit?: number; tail?: number },
) => {
  const qs = new URLSearchParams();
  if (typeof opts?.afterSeq === "number") qs.set("after_seq", String(opts.afterSeq));
  if (opts?.limit) qs.set("limit", String(opts.limit));
  if (opts?.tail) qs.set("tail", String(opts.tail));
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return apiAny<SessionEventsPage>(`/api/sessions/${sessionId}/events${suffix}`);
};

export const getSessionHistory = (sessionId: string, beforeSeq?: number, limit?: number) => {
  const qs = new URLSearchParams();
  if (typeof beforeSeq === "number") qs.set("before_seq", String(beforeSeq));
  if (limit) qs.set("limit", String(limit));
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return apiAny<SessionHistoryPage>(`/api/sessions/${sessionId}/history${suffix}`);
};

export const listTurnTools = (sessionId: string, turnId: string) =>
  apiAny<SessionTurnTool[]>(`/api/sessions/${sessionId}/turns/${turnId}/tools`);

export const listSessionFileCompletions = (
  sessionId: string,
  query: string,
  limit?: number,
  signal?: AbortSignal,
) => {
  const qs = new URLSearchParams();
  qs.set("query", query);
  if (limit) qs.set("limit", String(limit));
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return apiAny<string[]>(`/api/sessions/${sessionId}/completions/files${suffix}`, { signal });
};

export const listWorkspaceFileCompletions = (
  workspaceId: string,
  query: string,
  limit?: number,
  signal?: AbortSignal,
) => {
  const qs = new URLSearchParams();
  qs.set("query", query);
  if (limit) qs.set("limit", String(limit));
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return apiAny<string[]>(`/api/workspaces/${workspaceId}/completions/files${suffix}`, { signal });
};

export const postMessage = (
  sessionId: string,
  content: string,
  delivery?: "immediate" | "queued",
  attachments?: MessageAttachment[],
  opts?: {
    id?: string;
    turn_id?: string;
    analytics?: {
      providerId?: string;
      modelId?: string;
      reasoningEffort?: string | null;
      executionEnvironment?: ExecutionEnvironment;
      sessionKind?: "primary" | "subagent";
      isFirstTurn?: boolean;
    };
  },
) =>
  apiAny<Message>(`/api/sessions/${sessionId}/messages`, {
    method: "POST",
    body: JSON.stringify({
      content,
      delivery,
      attachments: attachments ?? [],
      ...(opts?.id ? { id: opts.id } : {}),
      ...(opts?.turn_id ? { turn_id: opts.turn_id } : {}),
    }),
  }).then((message) => {
    trackUserMessageSent({
      providerId: opts?.analytics?.providerId,
      modelId: opts?.analytics?.modelId,
      reasoningEffort: opts?.analytics?.reasoningEffort,
      executionEnvironment: opts?.analytics?.executionEnvironment,
      sessionKind: opts?.analytics?.sessionKind,
      isFirstTurn: opts?.analytics?.isFirstTurn,
    });
    trackFirstTurnSubmitted({
      sessionId,
      providerId: opts?.analytics?.providerId,
      modelId: opts?.analytics?.modelId,
    });
    if (delivery === "queued") {
      trackFeatureUsed("queued_message_sent");
    }
    if (attachments && attachments.length > 0) {
      trackFeatureUsed("message_with_attachment_sent");
    }
    return message;
  });

export const uploadBlob = async (file: File): Promise<BlobUploadResp> => {
  if (isDesktopApp()) {
    const buf = await file.arrayBuffer();
    const bytes = Array.from(new Uint8Array(buf));
    const mimeType = resolveImageMimeType(file.type, file.name) || "application/octet-stream";
    const resp = await desktopUploadBlob({
      bytes,
      mime_type: mimeType,
      name: file.name,
    });
    return resp;
  }
  const token = authToken();
  const form = new FormData();
  form.append("file", file, file.name);
  const res = await fetch("/api/blobs", {
    method: "POST",
    headers: {
      ...(token ? { authorization: `Bearer ${token}` } : {}),
    },
    body: form,
  });
  if (!res.ok) {
    const text = await res.text();
    let message = text || `${res.status} ${res.statusText}`;
    try {
      const parsed = text ? JSON.parse(text) : null;
      const parsedMessage = parsed?.error ?? parsed?.message;
      if (typeof parsedMessage === "string" && parsedMessage.trim()) {
        message = parsedMessage;
      }
    } catch {
      // ignore
    }
    throw new Error(message);
  }
  const text = await res.text();
  return (text ? JSON.parse(text) : undefined) as BlobUploadResp;
};

export const cancelSession = (sessionId: string) =>
  apiAny(`/api/sessions/${sessionId}/cancel`, { method: "POST" });

export const interruptSession = (sessionId: string) =>
  apiAny(`/api/sessions/${sessionId}/interrupt`, { method: "POST" });

export const setSessionModel = (
  sessionId: string,
  model_id: string,
  reasoning_effort?: string | null,
) =>
  apiAny<Session>(`/api/sessions/${sessionId}/model`, {
    method: "POST",
    body: JSON.stringify({
      model_id,
      ...(reasoning_effort ? { reasoning_effort } : {}),
    }),
  });

export const setSessionMode = (sessionId: string, mode_id: string) =>
  apiAny(`/api/sessions/${sessionId}/mode`, {
    method: "POST",
    body: JSON.stringify({ mode_id }),
  });

export const authenticateSession = (sessionId: string, method_id?: string) =>
  apiAny(`/api/sessions/${sessionId}/authenticate`, {
    method: "POST",
    body: JSON.stringify(method_id ? { method_id } : {}),
  });

export type AskUserQuestionOutcome = "submitted" | "cancelled";

export const submitAskUserQuestion = (
  sessionId: string,
  tool_call_id: string,
  outcome: AskUserQuestionOutcome,
  answers?: Record<string, string>,
) =>
  apiAny(`/api/sessions/${sessionId}/ask_user_question`, {
    method: "POST",
    body: JSON.stringify({ tool_call_id, outcome, answers }),
  });

export const getSessionDiff = (sessionId: string) =>
  apiAny<SessionDiffResponse>(`/api/sessions/${sessionId}/diff`);

export const applySessionDiffPatch = (sessionId: string, action: "accept" | "reject", patch: string) =>
  apiAny<SessionDiffResponse>(`/api/sessions/${sessionId}/diff/apply`, {
    method: "POST",
    body: JSON.stringify({ action, patch }),
  });

export const deleteMessage = (sessionId: string, messageId: string) =>
  apiAny(`/api/sessions/${sessionId}/messages/${messageId}`, { method: "DELETE" });

export const blobUrl = (blobId: string): string => {
  return blobResourceUrl(blobId);
};

export const artifactUrl = (sessionId: string, artifactId: string): string => {
  return artifactResourceUrl(sessionId, artifactId);
};

export const fetchArtifactText = async (
  sessionId: string,
  artifactId: string,
  opts?: { signal?: AbortSignal },
): Promise<string> => {
  const response = await daemonFetchRaw(
    browserResourcePathForScope({
      kind: "session_artifact",
      sessionId: String(sessionId || ""),
      artifactId: String(artifactId || ""),
    }),
    {
      cache: "no-store",
      headers: {
        accept: "text/plain, text/markdown, application/json, application/octet-stream, */*",
      },
      signal: opts?.signal,
    },
  );
  if (response.status < 200 || response.status >= 300) {
    throw new Error(`Failed to load artifact (${response.status}).`);
  }
  return response.body;
};
