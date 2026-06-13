export type WorkerClientConfig = {
  baseUrl?: string | null;
  authToken?: string | null;
  runId?: string | null;
};

type WorkerClientState = {
  baseUrl: string | null;
  authToken: string | null;
  runId: string | null;
};

let state: WorkerClientState = {
  baseUrl: null,
  authToken: null,
  runId: null,
};

const normalizeBaseUrl = (baseUrl?: string | null): string | null => {
  const trimmed = String(baseUrl ?? "").trim();
  return trimmed ? trimmed : null;
};

export const setWorkerClientConfig = (config: WorkerClientConfig) => {
  state = {
    baseUrl: normalizeBaseUrl(config.baseUrl),
    authToken: config.authToken ?? null,
    runId: config.runId ?? null,
  };
};

export const updateWorkerClientConfig = (config: Partial<WorkerClientConfig>) => {
  state = {
    baseUrl: normalizeBaseUrl(config.baseUrl ?? state.baseUrl),
    authToken: config.authToken !== undefined ? config.authToken ?? null : state.authToken,
    runId: config.runId !== undefined ? config.runId ?? null : state.runId,
  };
};

const buildUrl = (path: string): string => {
  if (!state.baseUrl) return path;
  const base = state.baseUrl.replace(/\/+$/, "");
  if (path.startsWith("http://") || path.startsWith("https://")) return path;
  if (path.startsWith("/")) return `${base}${path}`;
  return `${base}/${path}`;
};

const collectHeaders = (init?: RequestInit): Record<string, string> => {
  const extraHeaders: Record<string, string> = {};
  if (!init?.headers) return extraHeaders;
  if (init.headers instanceof Headers) {
    init.headers.forEach((value, key) => {
      extraHeaders[key] = value;
    });
  } else if (Array.isArray(init.headers)) {
    for (const [key, value] of init.headers) {
      extraHeaders[key] = value;
    }
  } else {
    Object.assign(extraHeaders, init.headers as Record<string, string>);
  }
  return extraHeaders;
};

const toHex = (bytes: Uint8Array): string =>
  Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");

const createTraceparent = (): string | null => {
  if (typeof crypto === "undefined" || !crypto.getRandomValues) return null;
  const traceId = new Uint8Array(16);
  const spanId = new Uint8Array(8);
  crypto.getRandomValues(traceId);
  crypto.getRandomValues(spanId);
  return `00-${toHex(traceId)}-${toHex(spanId)}-01`;
};

const looksLikeHtml = (text: string): boolean => {
  const t = String(text || "").trimStart().toLowerCase();
  return t.startsWith("<!doctype html") || t.startsWith("<html");
};

const trimForError = (text: string): string => {
  const s = String(text || "").trim();
  if (s.length <= 800) return s;
  return `${s.slice(0, 800)}…`;
};

export const workerFetchJson = async <T>(path: string, init?: RequestInit): Promise<T> => {
  const { headers: _headers, ...restInit } = init ?? {};
  const extraHeaders = collectHeaders(init);
  const traceparent = createTraceparent();
  if (traceparent && !extraHeaders.traceparent) {
    extraHeaders.traceparent = traceparent;
  }
  if (state.runId && !extraHeaders["x-ctx-run-id"]) {
    extraHeaders["x-ctx-run-id"] = state.runId;
  }

  const res = await fetch(buildUrl(path), {
    headers: {
      "content-type": "application/json",
      ...(state.authToken ? { authorization: `Bearer ${state.authToken}` } : {}),
      ...extraHeaders,
    },
    ...restInit,
  });

  const text = await res.text();
  const contentType = res.headers.get("content-type") ?? "";
  const htmlResponse = contentType.includes("text/html") || looksLikeHtml(text);

  if (!res.ok) {
    if (htmlResponse && path.startsWith("/api/")) {
      throw new Error(
        `The daemon returned HTML for ${path} (${res.status}). Restart/update the daemon (and ensure Vite is proxying /api to it).`,
      );
    }
    const lowered = String(text || "").toLowerCase();
    if (
      res.status >= 500 &&
      (lowered.includes("econnrefused") ||
        lowered.includes("proxy error") ||
        lowered.includes("connect econnrefused") ||
        lowered.includes("socket hang up"))
    ) {
      throw new Error(
        "Cannot reach the ctx daemon via /api. If you're running the web dev server, start the daemon (default http://127.0.0.1:4399) or set CTX_DAEMON_URL before `pnpm dev`.",
      );
    }
    try {
      const parsed = text ? JSON.parse(text) : null;
      const msg = parsed?.error ?? parsed?.message;
      if (typeof msg === "string" && msg.length > 0) {
        throw new Error(msg);
      }
    } catch {
      // ignore
    }
    throw new Error(trimForError(text) || `${res.status} ${res.statusText}`);
  }

  if (res.status === 204) {
    return undefined as T;
  }
  if (!text) return undefined as T;
  try {
    return JSON.parse(text) as T;
  } catch {
    if (htmlResponse && path.startsWith("/api/")) {
      throw new Error(
        `The daemon returned HTML for ${path}. Restart/update the daemon (and ensure Vite is proxying /api to it).`,
      );
    }
    throw new Error(`Unexpected non-JSON response from ${path}.`);
  }
};
