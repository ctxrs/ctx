import { test, expect } from "./fixtures";
import { seedDummyWorkspace, startStreamingMessages } from "./utils/seedDummyWorkspace";

type SubscribeMessage = {
  at: number;
  includeActiveHeads: boolean;
  foregroundTaskId: string | null;
  sessionCount: number;
};

type E2EWindow = Window & {
  __ctxE2E?: {
    getDiagnostics?: () => Array<{ code?: string }>;
    workspaceStream?: {
      getConnectionState?: () => string | null;
    };
  };
  __ctxSubscribeProbe?: {
    reset: () => void;
    snapshot: () => {
      subscribeCount: number;
      subscribeMessages: SubscribeMessage[];
    };
  };
};

test("workbench: live activity does not resubscribe on the same workspace stream connection", async ({
  page,
  request,
}) => {
  test.setTimeout(120_000);

  await page.addInitScript(() => {
    window.sessionStorage.setItem("ctxE2E", "1");
    const subscribeMessages: SubscribeMessage[] = [];
    const OriginalWebSocket = window.WebSocket;
    class TrackedWebSocket extends OriginalWebSocket {
      private __ctxUrl: string;

      constructor(url: string | URL, protocols?: string | string[]) {
        super(url, protocols);
        this.__ctxUrl = String(url);
      }

      send(data: string | ArrayBufferLike | Blob | ArrayBufferView) {
        try {
          const text = typeof data === "string" ? data : "";
          const parsed = text ? (JSON.parse(text) as Record<string, unknown>) : null;
          if (parsed?.type === "subscribe" && this.__ctxUrl.includes("/active_snapshot/stream")) {
            subscribeMessages.push({
              at: performance.now(),
              includeActiveHeads: Boolean(parsed.include_active_heads),
              foregroundTaskId:
                typeof parsed.foreground_task_id === "string" ? parsed.foreground_task_id : null,
              sessionCount: Array.isArray(parsed.sessions) ? parsed.sessions.length : 0,
            });
          }
        } catch {
          // Ignore non-JSON frames.
        }
        super.send(data);
      }
    }

    Object.defineProperty(window, "WebSocket", {
      value: TrackedWebSocket,
      configurable: true,
      writable: true,
    });

    (window as E2EWindow).__ctxSubscribeProbe = {
      reset() {
        subscribeMessages.length = 0;
      },
      snapshot() {
        return {
          subscribeCount: subscribeMessages.length,
          subscribeMessages: subscribeMessages.slice(),
        };
      },
    };
  });

  const seed = await seedDummyWorkspace(request, {
    tasks: 12,
    sessionsPerTask: 1,
    turnsPerSession: 2,
    throttleMs: 2,
    includeToolSummaries: true,
    toolSummariesPerTurn: 2,
    messageBytes: 1400,
  });

  await page.goto(`/workspaces/${seed.workspaceId}?ctxE2E=1`, { waitUntil: "domcontentloaded" });
  const rows = page.locator(".wb-task-row");
  await expect(rows).toHaveCount(12, { timeout: 30_000 });

  await expect
    .poll(async () => page.evaluate(() => (window as E2EWindow).__ctxE2E?.workspaceStream?.getConnectionState?.()))
    .toBe("connected");

  await page.waitForTimeout(2_500);
  await page.evaluate(() => (window as E2EWindow).__ctxSubscribeProbe?.reset());

  const sessionIds = Object.values(seed.sessionIdsByTask).flat();
  const streamer = startStreamingMessages(request, {
    sessionIds,
    intervalMs: 250,
    durationMs: 3_000,
    messageBytes: 1400,
    includeToolSummaries: true,
    toolSummariesPerTurn: 2,
  });

  await page.waitForTimeout(3_500);
  await streamer.stop();
  await expect
    .poll(async () => page.evaluate(() => (window as E2EWindow).__ctxE2E?.workspaceStream?.getConnectionState?.()), {
      timeout: 10_000,
    })
    .toBe("connected");

  const result = await page.evaluate(() => {
    const win = window as E2EWindow;
    return {
      probe: win.__ctxSubscribeProbe?.snapshot() ?? { subscribeCount: -1, subscribeMessages: [] },
      diagnostics: win.__ctxE2E?.getDiagnostics?.() ?? [],
      connectionState: win.__ctxE2E?.workspaceStream?.getConnectionState?.() ?? null,
    };
  });

  expect(result.connectionState).toBe("connected");
  expect(result.probe.subscribeCount, JSON.stringify(result.probe.subscribeMessages, null, 2)).toBe(0);
  expect(
    result.diagnostics.filter((entry) =>
      /workspace\\.stream_connection_missing|workspace\\.snapshot_wait_timeout|stream_seq_gap|stream_seq_reset|snapshot_rev_reset/.test(
        String(entry?.code ?? ""),
      ),
    ),
  ).toEqual([]);
});
