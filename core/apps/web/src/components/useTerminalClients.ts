import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import type { TerminalSession } from "@ctx/types";
import { useEffect, useRef, useState, type Dispatch, type SetStateAction } from "react";
import { idToString } from "../api/client";
import { mintTerminalStreamPath } from "../api/clientWorkspaces";
import { getDaemonWsUrl } from "../api/daemonConnection";
import { useThemeVariant, type ThemeVariant } from "../utils/theme";
import { terminalFontFamily, terminalTheme } from "./terminalClientTheme";
import { installWebLinksAddon } from "./terminalWebLinks";

export type TerminalConnectionStatus = "connected" | "reconnecting" | "disconnected";

type E2ETerminalClientHandle = {
  close: () => void;
  getConnectionStatus: () => TerminalConnectionStatus;
};

type WindowWithE2ETerminalHooks = Window & {
  __ctxE2ETerminalClients?: Map<string, E2ETerminalClientHandle>;
  __ctxE2ETerminals?: Map<string, Terminal>;
};

function getE2ETerminalClientRegistry(): Map<string, E2ETerminalClientHandle> | null {
  if (typeof window === "undefined") return null;
  try {
    if (window.sessionStorage.getItem("ctxE2E") !== "1") return null;
  } catch {
    return null;
  }
  const w = window as WindowWithE2ETerminalHooks;
  if (!w.__ctxE2ETerminalClients) {
    w.__ctxE2ETerminalClients = new Map<string, E2ETerminalClientHandle>();
  }
  return w.__ctxE2ETerminalClients;
}

function getE2ETerminalRegistry(): Map<string, Terminal> | null {
  // E2E-only hook: allow Playwright tests to introspect xterm state (buffer ydisp/baseY)
  // without relying on renderer-specific DOM text.
  if (typeof window === "undefined") return null;
  try {
    if (window.sessionStorage.getItem("ctxE2E") !== "1") return null;
  } catch {
    return null;
  }
  const w = window as WindowWithE2ETerminalHooks;
  if (!w.__ctxE2ETerminals) {
    w.__ctxE2ETerminals = new Map<string, Terminal>();
  }
  return w.__ctxE2ETerminals;
}

export type TerminalClient = {
  id: string;
  terminal: Terminal;
  fitAddon: FitAddon;
  element: HTMLElement | null;
  status: TerminalSession["status"];
  exitCode: number | null;
  connectionStatus: TerminalConnectionStatus;
  attach: (el: HTMLElement) => void;
  fit: () => void;
  focus: () => void;
  dispose: () => void;
  reconnect: () => void;
};

type TerminalStatusMessage = {
  type: "status";
  status: TerminalSession["status"];
  exit_code?: number | null;
};

type TerminalPongMessage = {
  type: "pong";
};

type TerminalControlMessage = TerminalStatusMessage | TerminalPongMessage;

export function useTerminalClients(
  terminals: TerminalSession[],
  setTerminals: Dispatch<SetStateAction<TerminalSession[]>>,
  workspaceId: string,
) {
  const clientsRef = useRef<Map<string, TerminalClient>>(new Map());
  const [, setConnectionVersion] = useState(0);
  const themeVariant = useThemeVariant();

  useEffect(() => {
    const map = clientsRef.current;
    const seen = new Set<string>();
    for (const terminal of terminals) {
      const id = idToString(terminal.id);
      if (!id) continue;
      seen.add(id);
      if (map.has(id)) {
        const client = map.get(id)!;
        client.status = terminal.status;
        client.exitCode = terminal.exit_code ?? null;
        continue;
      }
      const client = createClient(terminal, themeVariant, (status, exitCode) => {
        setTerminals((prev) =>
          prev.map((t) =>
            idToString(t.id) === id
              ? {
                  ...t,
                  status,
                  exit_code: exitCode ?? null,
                }
              : t,
          ),
        );
      }, () => {
        setConnectionVersion((prev) => prev + 1);
      });
      map.set(id, client);
    }
    for (const id of Array.from(map.keys())) {
      if (!seen.has(id)) {
        const client = map.get(id);
        client?.dispose();
        map.delete(id);
      }
    }
  }, [setTerminals, terminals, themeVariant]);

  useEffect(() => {
    return () => {
      for (const client of clientsRef.current.values()) {
        client.dispose();
      }
      clientsRef.current.clear();
    };
  }, [workspaceId]);

  useEffect(() => {
    const theme = terminalTheme(themeVariant);
    for (const client of clientsRef.current.values()) {
      client.terminal.options.theme = theme;
    }
  }, [themeVariant]);

  return clientsRef;
}

async function buildTerminalWsUrl(terminal: TerminalSession): Promise<string> {
  const { stream_path } = await mintTerminalStreamPath(idToString(terminal.id));
  return getDaemonWsUrl(stream_path);
}

const RECONNECT_BASE_MS = 500;
const RECONNECT_MAX_MS = 10_000;
const DISCONNECTED_AFTER_ATTEMPTS = 3;
const KEEPALIVE_INTERVAL_MS = 25_000;
const KEEPALIVE_TIMEOUT_MS = 75_000;

// When the terminal is hidden/collapsed (height 0), xterm can get into a bad scroll state
// if we keep feeding it output. Buffer a bounded amount while unfittable and flush on the
// next successful fit.
const PENDING_OUTPUT_MAX_BYTES = 512 * 1024;

function createClient(
  terminal: TerminalSession,
  themeVariant: ThemeVariant,
  onStatus: (status: TerminalSession["status"], exitCode: number | null) => void,
  onConnectionChange: () => void,
): TerminalClient {
  const id = idToString(terminal.id);
  const term = new Terminal({
    fontFamily: terminalFontFamily(),
    fontSize: 12,
    lineHeight: 1.15,
    scrollback: 2000,
    cursorBlink: true,
    theme: terminalTheme(themeVariant),
  });
  const fitAddon = new FitAddon();
  term.loadAddon(fitAddon);
  const cleanupLinks = installWebLinksAddon(term);
  getE2ETerminalRegistry()?.set(id, term);

  let client: TerminalClient;
  let socket: WebSocket | null = null;
  let element: HTMLElement | null = null;
  let reconnectTimer: number | null = null;
  let keepaliveTimer: number | null = null;
  let connectInFlight: Promise<void> | null = null;
  let reconnectAttempts = 0;
  let disposed = false;
  let connectionStatus: TerminalConnectionStatus = "disconnected";
  const e2eClientRegistry = getE2ETerminalClientRegistry();
  e2eClientRegistry?.set(id, {
    close: () => {
      try {
        socket?.close();
      } catch {
        // ignore
      }
    },
    getConnectionStatus: () => connectionStatus,
  });
  let status: TerminalSession["status"] = terminal.status;
  let exitCode: number | null = terminal.exit_code ?? null;
  let lastServerMessageAt = Date.now();
  let scrollToBottomOnNextFit = true;
  let pendingOutput: Array<string | Uint8Array | null> = [];
  let pendingOutputHead = 0;
  let pendingOutputBytes = 0;

  const pendingSizeOf = (chunk: string | Uint8Array) =>
    typeof chunk === "string" ? chunk.length : chunk.byteLength;
  const hasPendingOutput = () => pendingOutputHead < pendingOutput.length;

  const maybeCompactPendingOutput = () => {
    // Avoid O(n) shifting on drops while still keeping memory bounded.
    if (pendingOutputHead < 128) return;
    if (pendingOutputHead * 2 < pendingOutput.length) return;
    pendingOutput = pendingOutput.slice(pendingOutputHead);
    pendingOutputHead = 0;
  };

  const enqueuePendingOutput = (chunk: string | Uint8Array) => {
    pendingOutput.push(chunk);
    pendingOutputBytes += pendingSizeOf(chunk);
    while (pendingOutputBytes > PENDING_OUTPUT_MAX_BYTES && hasPendingOutput()) {
      const dropped = pendingOutput[pendingOutputHead];
      // Release dropped chunks immediately to keep actual memory usage bounded even if
      // compaction hasn't run yet (e.g. a few large WS messages while the panel is hidden).
      pendingOutput[pendingOutputHead] = null;
      pendingOutputHead += 1;
      if (dropped !== null) {
        pendingOutputBytes -= pendingSizeOf(dropped);
      }
    }
    maybeCompactPendingOutput();
  };

  const flushPendingOutput = (): boolean => {
    if (!hasPendingOutput()) return false;
    const chunks = pendingOutput;
    const start = pendingOutputHead;
    const end = chunks.length;
    pendingOutputHead = 0;
    pendingOutput = [];
    pendingOutputBytes = 0;
    for (let i = start; i < end; i += 1) {
      const chunk = chunks[i];
      if (chunk === null) continue;
      term.write(chunk);
    }
    return true;
  };

  const scrollToBottomIfRequested = () => {
    if (!scrollToBottomOnNextFit) return;
    scrollToBottomOnNextFit = false;
    term.scrollToBottom();
  };

  const canFit = () => {
    if (!element || !element.isConnected) return false;
    if (element.closest(".wb-terminal-group-hidden")) return false;
    return element.clientWidth > 0 && element.clientHeight > 0;
  };

  const sendResize = () => {
    if (!socket || socket.readyState !== WebSocket.OPEN) return;
    const cols = term.cols;
    const rows = term.rows;
    socket.send(JSON.stringify({ type: "resize", cols, rows }));
  };

  const fitNow = () => {
    if (!canFit()) return;
    fitAddon.fit();
    sendResize();
    // xterm can get a stale viewport after being hidden/collapsed; force a repaint.
    term.refresh(0, Math.max(0, term.rows - 1));
    // If we buffered output while hidden, flush after we've established correct cols/rows.
    if (flushPendingOutput()) {
      term.refresh(0, Math.max(0, term.rows - 1));
    }
    scrollToBottomIfRequested();
  };

  const writeOrBufferOutput = (chunk: string | Uint8Array) => {
    if (!canFit()) {
      enqueuePendingOutput(chunk);
      return;
    }

    // Preserve stream ordering across hidden->visible transitions. Otherwise, new
    // chunks can be written while older buffered chunks flush later in `fitNow()`.
    if (hasPendingOutput()) {
      fitNow();
      // If we couldn't flush (visibility flipped again), keep buffering to avoid reordering.
      if (hasPendingOutput()) {
        enqueuePendingOutput(chunk);
        return;
      }
    }

    term.write(chunk);
  };

  const updateInputState = () => {
    term.options.disableStdin = connectionStatus !== "connected" || status === "exited";
  };

  const clearReconnectTimer = () => {
    if (reconnectTimer === null) return;
    window.clearTimeout(reconnectTimer);
    reconnectTimer = null;
  };

  const clearKeepaliveTimer = () => {
    if (keepaliveTimer === null) return;
    window.clearInterval(keepaliveTimer);
    keepaliveTimer = null;
  };

  const startKeepalive = () => {
    if (keepaliveTimer !== null) return;
    keepaliveTimer = window.setInterval(() => {
      if (disposed) return;
      if (!socket || socket.readyState !== WebSocket.OPEN) return;
      const now = Date.now();
      if (now - lastServerMessageAt > KEEPALIVE_TIMEOUT_MS) {
        socket.close();
        return;
      }
      socket.send(JSON.stringify({ type: "ping" }));
    }, KEEPALIVE_INTERVAL_MS);
  };

  const setConnectionStatus = (next: TerminalConnectionStatus) => {
    if (connectionStatus === next) return;
    connectionStatus = next;
    client.connectionStatus = next;
    updateInputState();
    onConnectionChange();
  };

  const scheduleReconnect = () => {
    if (disposed) return;
    if (status === "exited") {
      setConnectionStatus("disconnected");
      return;
    }
    clearReconnectTimer();
    reconnectAttempts += 1;
    const backoff = Math.min(RECONNECT_MAX_MS, RECONNECT_BASE_MS * 2 ** (reconnectAttempts - 1));
    const jitter = 0.7 + Math.random() * 0.6;
    const delay = Math.round(backoff * jitter);
    const nextState =
      reconnectAttempts > DISCONNECTED_AFTER_ATTEMPTS ? "disconnected" : "reconnecting";
    setConnectionStatus(nextState);
    reconnectTimer = window.setTimeout(() => {
      reconnectTimer = null;
      void connect();
    }, delay);
  };

  const connect = async () => {
    if (disposed) return;
    if (connectInFlight) return;
    if (socket && (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING)) {
      return;
    }
    connectInFlight = (async () => {
      const nextState =
        reconnectAttempts > DISCONNECTED_AFTER_ATTEMPTS ? "disconnected" : "reconnecting";
      setConnectionStatus(nextState);
      let wsUrl = "";
      try {
        wsUrl = await buildTerminalWsUrl(terminal);
      } catch {
        scheduleReconnect();
        return;
      }
      if (disposed) return;
      socket = new WebSocket(wsUrl);
      socket.binaryType = "arraybuffer";
      socket.addEventListener("open", () => {
        reconnectAttempts = 0;
        clearReconnectTimer();
        lastServerMessageAt = Date.now();
        setConnectionStatus("connected");
        sendResize();
        startKeepalive();
      });
      socket.addEventListener("close", () => {
        socket = null;
        clearKeepaliveTimer();
        scheduleReconnect();
      });
      socket.addEventListener("message", (ev) => {
        lastServerMessageAt = Date.now();
        if (typeof ev.data === "string") {
          try {
            const msg = JSON.parse(ev.data) as TerminalControlMessage;
            if (msg.type === "status") {
              status = msg.status;
              exitCode = msg.exit_code ?? null;
              client.status = status;
              client.exitCode = exitCode;
              updateInputState();
              onStatus(status, exitCode);
              return;
            }
            if (msg.type === "pong") {
              return;
            }
          } catch {
            // ignore
          }
          writeOrBufferOutput(ev.data);
          return;
        }
        if (ev.data instanceof ArrayBuffer) {
          writeOrBufferOutput(new Uint8Array(ev.data));
          return;
        }
        if (ev.data instanceof Blob) {
          void ev.data.arrayBuffer().then((buf) => {
            writeOrBufferOutput(new Uint8Array(buf));
          });
        }
      });
    })();
    try {
      await connectInFlight;
    } finally {
      connectInFlight = null;
    }
  };

  term.onData((data) => {
    if (!socket || socket.readyState !== WebSocket.OPEN) return;
    if (connectionStatus !== "connected" || status === "exited") return;
    socket.send(data);
  });

  client = {
    id,
    terminal: term,
    fitAddon,
    element,
    status: terminal.status,
    exitCode: terminal.exit_code ?? null,
    connectionStatus,
    attach: (el) => {
      if (element === el) return;
      element = el;
      if (term.element) {
        el.replaceChildren(term.element);
      } else {
        term.open(el);
      }
      requestAnimationFrame(fitNow);
    },
    fit: () => {
      fitNow();
    },
    focus: () => {
      term.focus();
    },
    dispose: () => {
      disposed = true;
      clearReconnectTimer();
      clearKeepaliveTimer();
      cleanupLinks();
      socket?.close();
      socket = null;
      e2eClientRegistry?.delete(id);
      getE2ETerminalRegistry()?.delete(id);
      term.dispose();
    },
    reconnect: () => {
      reconnectAttempts = 0;
      socket?.close();
      if (!socket) {
        scheduleReconnect();
      }
    },
  };

  updateInputState();
  void connect();

  return client;
}
