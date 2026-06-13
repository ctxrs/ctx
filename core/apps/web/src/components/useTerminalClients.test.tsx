import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useState } from "react";
import type { TerminalSession } from "@ctx/types";
import * as clientWorkspaces from "../api/clientWorkspaces";

vi.mock("@xterm/xterm", () => {
  class MockTerminal {
    cols = 80;
    rows = 24;
    element: HTMLElement | null = null;
    options = { disableStdin: false };
    writes: Array<string | Uint8Array> = [];

    loadAddon() {}
    setOption(key: string, value: unknown) {
      (this.options as Record<string, unknown>)[key] = value;
    }
    open(el: HTMLElement) {
      this.element = el;
    }
    write(data: string | Uint8Array) {
      this.writes.push(data);
    }
    refresh() {}
    scrollToBottom() {}
    dispose() {}
    focus() {}
    onData() {
      return { dispose() {} };
    }
  }

  return { Terminal: MockTerminal };
});

vi.mock("@xterm/addon-fit", () => {
  class MockFitAddon {
    fit() {}
  }

  return { FitAddon: MockFitAddon };
});

import { useTerminalClients, type TerminalClient } from "./useTerminalClients";

vi.mock("../api/clientWorkspaces", async () => {
  const actual = await vi.importActual<typeof import("../api/clientWorkspaces")>(
    "../api/clientWorkspaces",
  );
  return {
    ...actual,
    mintTerminalStreamPath: vi.fn(),
  };
});

type MockWebSocketListener = (event: unknown) => void;

class MockWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  static instances: MockWebSocket[] = [];

  readyState = MockWebSocket.CONNECTING;
  binaryType = "arraybuffer";
  url: string;
  sent: unknown[] = [];
  private listeners: Record<string, MockWebSocketListener[]> = {};

  constructor(url: string) {
    this.url = url;
    MockWebSocket.instances.push(this);
  }

  addEventListener(type: string, cb: MockWebSocketListener) {
    if (!this.listeners[type]) {
      this.listeners[type] = [];
    }
    this.listeners[type].push(cb);
  }

  send(data: unknown) {
    this.sent.push(data);
  }

  close() {
    this.readyState = MockWebSocket.CLOSED;
    this.emit("close", {});
  }

  open() {
    this.readyState = MockWebSocket.OPEN;
    this.emit("open", {});
  }

  message(data: unknown) {
    this.emit("message", { data });
  }

  private emit(type: string, event: unknown) {
    const handlers = this.listeners[type] ?? [];
    for (const handler of handlers) {
      handler(event);
    }
  }
}

let originalWebSocket: typeof window.WebSocket | undefined;
let lastClient: TerminalClient | null = null;

const baseTerminal = (): TerminalSession => ({
  id: "terminal-1",
  workspace_id: "workspace-1",
  task_id: null,
  session_id: null,
  worktree_id: null,
  cwd: "/",
  shell: "/bin/bash",
  title: "bash",
  status: "running",
  exit_code: null,
  stream_path: "/api/terminals/terminal-1/stream",
  created_at: new Date().toISOString(),
  updated_at: new Date().toISOString(),
});

const Harness = () => {
  const [terminals, setTerminals] = useState<TerminalSession[]>([baseTerminal()]);
  const clientsRef = useTerminalClients(terminals, setTerminals, "workspace-1");
  const client = clientsRef.current.get("terminal-1");
  lastClient = client ?? null;
  return <div data-testid="status">{client?.connectionStatus ?? "missing"}</div>;
};

async function renderHarness() {
  const view = render(<Harness />);
  await act(async () => {
    await Promise.resolve();
  });
  return view;
}

beforeEach(() => {
  lastClient = null;
  MockWebSocket.instances = [];
  vi.useFakeTimers();
  vi.spyOn(Math, "random").mockReturnValue(0);
  vi.mocked(clientWorkspaces.mintTerminalStreamPath).mockResolvedValue({
    stream_path: "/api/terminals/terminal-1/stream?token=terminal-secret",
    expires_at: new Date().toISOString(),
  });
  originalWebSocket = window.WebSocket;
  Object.defineProperty(window, "WebSocket", {
    value: MockWebSocket,
    configurable: true,
    writable: true,
  });
});

afterEach(() => {
  lastClient = null;
  vi.restoreAllMocks();
  vi.useRealTimers();
  Object.defineProperty(window, "WebSocket", {
    value: originalWebSocket,
    configurable: true,
    writable: true,
  });
});

describe("useTerminalClients", () => {
  it("reconnects after close and updates connection state", async () => {
    await renderHarness();
    expect(MockWebSocket.instances).toHaveLength(1);

    const first = MockWebSocket.instances[0];
    await act(async () => {
      first.open();
    });
    expect(screen.getByTestId("status")).toHaveTextContent("connected");

    await act(async () => {
      first.close();
    });
    expect(screen.getByTestId("status")).toHaveTextContent("reconnecting");

    await act(async () => {
      vi.advanceTimersByTime(500);
    });

    expect(MockWebSocket.instances).toHaveLength(2);
    const second = MockWebSocket.instances[1];
    await act(async () => {
      second.open();
    });
    expect(screen.getByTestId("status")).toHaveTextContent("connected");
  });

  it("surfaces disconnected after repeated failures", async () => {
    await renderHarness();
    expect(MockWebSocket.instances).toHaveLength(1);

    for (let i = 0; i < 4; i += 1) {
      const socket = MockWebSocket.instances[i];
      await act(async () => {
        socket.close();
      });
      await act(async () => {
        vi.advanceTimersByTime(5000);
      });
    }

    expect(screen.getByTestId("status")).toHaveTextContent("disconnected");
  });

  it("reconnects when keepalive stalls", async () => {
    await renderHarness();
    expect(MockWebSocket.instances).toHaveLength(1);

    const first = MockWebSocket.instances[0];
    await act(async () => {
      first.open();
    });
    expect(screen.getByTestId("status")).toHaveTextContent("connected");

    await act(async () => {
      vi.advanceTimersByTime(110_000);
    });

    expect(MockWebSocket.instances.length).toBeGreaterThan(1);
    expect(screen.getByTestId("status")).toHaveTextContent(/reconnecting|disconnected/);
  });

  it("flushes buffered output before writing new chunks once fittable (preserves ordering)", async () => {
    const rafCallbacks: FrameRequestCallback[] = [];
    const originalRaf = window.requestAnimationFrame;
    // Control rAF so we can reproduce the window where `canFit()` becomes true
    // before the scheduled `fitNow()` has a chance to flush `pendingOutput`.
    Object.defineProperty(window, "requestAnimationFrame", {
      value: (cb: FrameRequestCallback) => {
        rafCallbacks.push(cb);
        return 1;
      },
      configurable: true,
      writable: true,
    });

    const el = document.createElement("div");
    Object.defineProperty(el, "clientWidth", { value: 800, configurable: true });
    Object.defineProperty(el, "clientHeight", { value: 200, configurable: true });
    document.body.appendChild(el);

    try {
      await renderHarness();
      expect(MockWebSocket.instances).toHaveLength(1);
      const socket = MockWebSocket.instances[0];
      expect(lastClient).toBeTruthy();
      if (!lastClient) throw new Error("Expected terminal client");
      const client = lastClient;

      const term = client.terminal as unknown as { writes: Array<string | Uint8Array> };

      await act(async () => {
        socket.message("old");
      });
      expect(term.writes).toEqual([]);

      await act(async () => {
        client.attach(el);
      });

      // If we write "new" while "old" is still buffered, we must flush "old" first.
      await act(async () => {
        socket.message("new");
      });
      expect(term.writes[0]).toBe("old");
      expect(term.writes[1]).toBe("new");

      await act(async () => {
        for (const cb of rafCallbacks) cb(0);
      });
      expect(term.writes[0]).toBe("old");
      expect(term.writes[1]).toBe("new");
    } finally {
      el.remove();
      Object.defineProperty(window, "requestAnimationFrame", {
        value: originalRaf,
        configurable: true,
        writable: true,
      });
    }
  });
});
