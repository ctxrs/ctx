import {
  clampString,
  globalAny,
  MAX_STRING_LIGHT,
  nowMs,
  normalizeUrl,
  parseContentLength,
  sanitizeHeaders,
  serializeConsoleArg,
  WAL_ENDPOINT_DEFAULT,
  type WalMode,
  type WalRecorder,
  type WalTrackedXmlHttpRequest,
  type XhrWalMeta,
  WS_IDLE_MS,
  WS_SAMPLE_MS,
} from "./walRecorderShared";
import { randomUuid } from "./randomUuid";

export const installWalRecorderHooks = (recorder: WalRecorder, getMode: () => WalMode) => {
  if (globalAny.__CTX_WAL_HOOKS__) return;
  globalAny.__CTX_WAL_HOOKS__ = true;

  if (typeof window !== "undefined" && window.fetch) {
    const originalFetch = window.fetch.bind(window);
    window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
      const rawUrl = typeof input === "string" ? input : "url" in input ? input.url : String(input);
      const method =
        init?.method ??
        (typeof Request !== "undefined" && input instanceof Request ? input.method : "GET");
      const normalizedUrl = normalizeUrl(rawUrl);
      const skip =
        normalizedUrl.includes(WAL_ENDPOINT_DEFAULT) ||
        Boolean((init?.headers as Record<string, string> | undefined)?.["x-ctx-wal"]);
      if (skip) return originalFetch(input, init);

      const start = nowMs();
      let res: Response | null = null;
      let error: unknown = null;
      try {
        res = await originalFetch(input, init);
        return res;
      } catch (err) {
        error = err;
        throw err;
      } finally {
        const durationMs = nowMs() - start;
        const mode = getMode();
        const requestBytes =
          typeof init?.body === "string"
            ? init.body.length
            : init?.body instanceof ArrayBuffer
              ? init.body.byteLength
              : init?.body instanceof Blob
                ? init.body.size
                : undefined;
        const requestHeaders = mode === "heavy" ? sanitizeHeaders(init?.headers ?? undefined) : undefined;
        recorder.record(
          "fetch",
          {
            url: normalizedUrl,
            method: String(method ?? "GET"),
            status: res?.status ?? null,
            ok: res?.ok ?? false,
            duration_ms: Math.round(durationMs),
            request_bytes: requestBytes,
            response_bytes: parseContentLength(res?.headers.get("content-length") ?? null),
            response_type: res?.type ?? null,
            response_content_type: res?.headers.get("content-type") ?? null,
            error: error instanceof Error ? { name: error.name, message: error.message } : undefined,
            request_headers: requestHeaders,
            response_headers: mode === "heavy" ? sanitizeHeaders(res?.headers ?? undefined) : undefined,
          },
          { level: "light" },
        );
      }
    };
  }

  if (typeof XMLHttpRequest !== "undefined") {
    const originalOpen = XMLHttpRequest.prototype.open;
    const originalSend = XMLHttpRequest.prototype.send;
    XMLHttpRequest.prototype.open = (function (
      this: WalTrackedXmlHttpRequest,
      ...args: [string, string | URL, ...unknown[]]
    ) {
      const [method, url] = args;
      (this as WalTrackedXmlHttpRequest).__ctxWal = {
        method: String(method ?? "GET"),
        url: String(url ?? ""),
        start: 0,
        request_bytes: undefined,
      } satisfies XhrWalMeta;
      Reflect.apply(originalOpen as unknown as (...openArgs: unknown[]) => unknown, this, args);
    } as XMLHttpRequest["open"]);
    XMLHttpRequest.prototype.send = function (
      this: WalTrackedXmlHttpRequest,
      body?: Parameters<XMLHttpRequest["send"]>[0],
    ) {
      const meta = (this as WalTrackedXmlHttpRequest).__ctxWal;
      if (meta) {
        meta.start = nowMs();
        if (typeof body === "string") meta.request_bytes = body.length;
        if (body instanceof ArrayBuffer) meta.request_bytes = body.byteLength;
        if (body instanceof Blob) meta.request_bytes = body.size;
        const onLoadEnd = () => {
          this.removeEventListener("loadend", onLoadEnd);
          const durationMs = nowMs() - meta.start;
          const mode = getMode();
          const normalizedUrl = normalizeUrl(meta.url ?? "");
          if (!normalizedUrl.includes(WAL_ENDPOINT_DEFAULT)) {
            const responseHeaders = mode === "heavy" ? sanitizeHeaders(this.getAllResponseHeaders()) : undefined;
            recorder.record(
              "xhr",
              {
                url: normalizedUrl,
                method: String(meta.method ?? "GET"),
                status: typeof this.status === "number" ? this.status : null,
                ok: typeof this.status === "number" ? this.status >= 200 && this.status < 400 : false,
                duration_ms: Math.round(durationMs),
                request_bytes: meta.request_bytes,
                response_bytes: parseContentLength(this.getResponseHeader("content-length")),
                response_type: this.responseType ?? null,
                response_content_type: this.getResponseHeader("content-type"),
                response_headers: responseHeaders,
              },
              { level: "light" },
            );
          }
        };
        this.addEventListener("loadend", onLoadEnd);
      }
      return originalSend.call(this, body ?? null);
    };
  }

  if (typeof WebSocket !== "undefined") {
    const OriginalWebSocket = WebSocket;
    const socketState = new Map<
      string,
      { lastMessageMs: number; lastIdleMs: number; lastSampleMs: number; messageCount: number; bytes: number }
    >();
    class WrappedWebSocket extends OriginalWebSocket {
      __ctxWalId: string;
      constructor(url: string | URL, protocols?: string | string[]) {
        super(url, protocols);
        const id = randomUuid();
        this.__ctxWalId = id;
        const normalizedUrl = normalizeUrl(String(url));
        recorder.record("ws:open", { id, url: normalizedUrl });
        const now = nowMs();
        socketState.set(id, {
          lastMessageMs: now,
          lastIdleMs: 0,
          lastSampleMs: now,
          messageCount: 0,
          bytes: 0,
        });
        this.addEventListener("message", (event) => {
          const entry = socketState.get(id);
          const now = nowMs();
          if (entry) {
            entry.lastMessageMs = now;
            entry.messageCount += 1;
          }
          const mode = getMode();
          const size =
            typeof event.data === "string"
              ? event.data.length
              : event.data instanceof ArrayBuffer
                ? event.data.byteLength
                : event.data instanceof Blob
                  ? event.data.size
                  : undefined;
          if (entry && typeof size === "number") {
            entry.bytes += size;
          }
          if (mode === "heavy") {
            const preview =
              typeof event.data === "string" ? clampString(event.data, MAX_STRING_LIGHT) : undefined;
            recorder.record(
              "ws:message",
              {
                id,
                url: normalizedUrl,
                size,
                preview,
              },
              { level: "heavy" },
            );
          }
        });
        this.addEventListener("close", (event) => {
          socketState.delete(id);
          recorder.record("ws:close", {
            id,
            url: normalizedUrl,
            code: event.code,
            reason: event.reason,
            was_clean: event.wasClean,
          });
        });
        this.addEventListener("error", () => {
          recorder.record("ws:error", { id, url: normalizedUrl });
        });
      }
    }
    const wsConstants = ["CONNECTING", "OPEN", "CLOSING", "CLOSED"] as const;
    for (const key of wsConstants) {
      if (Object.prototype.hasOwnProperty.call(WrappedWebSocket, key)) continue;
      try {
        Object.defineProperty(WrappedWebSocket, key, {
          value: OriginalWebSocket[key],
          writable: false,
          configurable: true,
        });
      } catch {
        // Ignore if the runtime forbids redefining the static constant.
      }
    }
    (window as unknown as { WebSocket: typeof WebSocket }).WebSocket = WrappedWebSocket;

    window.setInterval(() => {
      const now = nowMs();
      const mode = getMode();
      for (const [id, entry] of socketState.entries()) {
        if (mode !== "heavy" && entry.messageCount > 0 && now - entry.lastSampleMs >= WS_SAMPLE_MS) {
          recorder.record("ws:traffic", {
            id,
            count: entry.messageCount,
            bytes: entry.bytes,
            window_ms: Math.round(now - entry.lastSampleMs),
            last_message_ms: Math.round(entry.lastMessageMs),
          });
          entry.messageCount = 0;
          entry.bytes = 0;
          entry.lastSampleMs = now;
        }
        if (now - entry.lastMessageMs >= WS_IDLE_MS && now - entry.lastIdleMs >= WS_IDLE_MS) {
          entry.lastIdleMs = now;
          recorder.record("ws:idle", { id, idle_ms: Math.round(now - entry.lastMessageMs) });
        }
      }
    }, WS_SAMPLE_MS);
  }

  if (typeof window !== "undefined" && window.console) {
    const levels = ["log", "info", "warn", "error", "debug"] as const;
    for (const level of levels) {
      const original = window.console[level];
      if (typeof original !== "function") continue;
      window.console[level] = (...args: unknown[]) => {
        const mode = getMode();
        recorder.record("console", {
          level,
          args: args.map((arg) => serializeConsoleArg(arg, mode)),
        });
        return Reflect.apply(original, window.console, args);
      };
    }
  }

  if (typeof window !== "undefined") {
    window.addEventListener("error", (event) => {
      const target = event.target as HTMLElement | null;
      const targetTag = target?.tagName?.toLowerCase();
      const targetUrl =
        target instanceof HTMLImageElement
          ? target.currentSrc || target.src
          : target instanceof HTMLScriptElement
            ? target.src
            : target instanceof HTMLLinkElement
              ? target.href
              : undefined;
      if (targetUrl) {
        recorder.record("resource:error", { tag: targetTag, url: normalizeUrl(targetUrl) });
        return;
      }
      const error = event.error as Error | undefined;
      recorder.record("error", {
        message: event.message,
        filename: event.filename,
        lineno: event.lineno,
        colno: event.colno,
        stack: error?.stack ? clampString(error.stack, MAX_STRING_LIGHT) : undefined,
      });
    });

    window.addEventListener("unhandledrejection", (event) => {
      const reason = event.reason;
      recorder.record("unhandledrejection", {
        reason: serializeConsoleArg(reason, getMode()),
      });
    });

    window.addEventListener("online", () => recorder.record("navigator:online"));
    window.addEventListener("offline", () => recorder.record("navigator:offline"));
    window.addEventListener("focus", () => recorder.record("window:focus"));
    window.addEventListener("blur", () => recorder.record("window:blur"));
    window.addEventListener("visibilitychange", () => {
      recorder.record("document:visibility", { state: document.visibilityState });
      if (document.visibilityState === "hidden") recorder.flush("unload");
    });
    window.addEventListener("pagehide", () => recorder.flush("unload"));
  }
};
