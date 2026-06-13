import type {
  ExecutionLaunchLogLine,
  ExecutionLaunchSnapshot,
  ExecutionLaunchStreamEvent,
} from "../../api/client";
import {
  buildExecutionLaunchWsUrl,
  getExecutionLaunchStatus,
  startWorkspaceSetupLaunchHandoff as requestWorkspaceSetupLaunchHandoff,
} from "../../api/client";
import {
  launchErrorFromSnapshot as formatLaunchErrorFromSnapshot,
  mergeLaunchLogs,
  type WorkspaceSetupLaunchLogLine,
} from "./launchProgress";
import { messageFromError } from "./wizardTypes";

type LaunchCallbacks = {
  applySnapshot: (snapshot: ExecutionLaunchSnapshot) => void;
  appendLines: (lines: ExecutionLaunchLogLine[]) => void;
};

type LaunchHandoffOptions = {
  reconnectDelayMs?: number;
  maxReconnects?: number;
};

type LaunchLogBatcherOptions = {
  scheduleFlush?: (flush: () => void) => number;
  cancelFlush?: (handle: number) => void;
};

const scheduleLaunchLogFlush = (flush: () => void): number => window.setTimeout(flush, 16);

const cancelLaunchLogFlush = (handle: number) => window.clearTimeout(handle);

export const createLaunchLogBatcher = (
  appendLines: (lines: ExecutionLaunchLogLine[]) => void,
  options: LaunchLogBatcherOptions = {},
) => {
  const scheduleFlush = options.scheduleFlush ?? scheduleLaunchLogFlush;
  const cancelFlush = options.cancelFlush ?? cancelLaunchLogFlush;
  let pending: ExecutionLaunchLogLine[] = [];
  let flushHandle: number | null = null;

  const flushPending = () => {
    if (!pending.length) return;
    const lines = pending;
    pending = [];
    appendLines(lines);
  };

  const flush = () => {
    if (flushHandle !== null) {
      cancelFlush(flushHandle);
      flushHandle = null;
    }
    flushPending();
  };

  const enqueue = (line: ExecutionLaunchLogLine) => {
    pending.push(line);
    if (flushHandle !== null) return;
    flushHandle = scheduleFlush(() => {
      flushHandle = null;
      flushPending();
    });
  };

  const dispose = () => {
    if (flushHandle !== null) {
      cancelFlush(flushHandle);
      flushHandle = null;
    }
    pending = [];
  };

  return {
    enqueue,
    flush,
    dispose,
  };
};

export const startWorkspaceSetupLaunchHandoff = (workspaceId: string) =>
  requestWorkspaceSetupLaunchHandoff(workspaceId);

export const launchErrorFromSnapshot = (snapshot: ExecutionLaunchSnapshot): string =>
  formatLaunchErrorFromSnapshot(snapshot);

export const waitForLaunchHandoffTerminal = async (
  initial: ExecutionLaunchSnapshot,
  callbacks: LaunchCallbacks,
  options: LaunchHandoffOptions = {},
): Promise<void> => {
  callbacks.applySnapshot(initial);

  if (initial.state === "ready") return;
  if (initial.state === "error") throw new Error(launchErrorFromSnapshot(initial));

  await new Promise<void>((resolve, reject) => {
    const reconnectDelayMs = Math.max(0, options.reconnectDelayMs ?? 750);
    const maxReconnects = Math.max(0, options.maxReconnects ?? 4);
    let reconnects = 0;
    let settled = false;
    const logBatcher = createLaunchLogBatcher(callbacks.appendLines);
    let ws: WebSocket | null = null;
    let reconnectHandle: number | null = null;

    const settle = (error?: Error) => {
      if (settled) return;
      settled = true;
      if (reconnectHandle !== null) {
        window.clearTimeout(reconnectHandle);
        reconnectHandle = null;
      }
      logBatcher.flush();
      logBatcher.dispose();
      ws?.close();
      if (error) reject(error);
      else resolve();
    };

    const connect = () => {
      void buildExecutionLaunchWsUrl(initial.job_id).then((wsUrl) => {
        if (settled) return;
        ws = new WebSocket(wsUrl);

        ws.onmessage = (event) => {
          let parsed: ExecutionLaunchStreamEvent | null = null;
          try {
            parsed = JSON.parse(String(event.data ?? "")) as ExecutionLaunchStreamEvent;
          } catch {
            return;
          }
          if (!parsed) return;
          if (parsed.type === "launch_log") {
            logBatcher.enqueue(parsed.line);
            return;
          }
          if (parsed.type === "launch_snapshot") {
            logBatcher.flush();
            callbacks.applySnapshot(parsed.snapshot);
            return;
          }
          if (parsed.type === "launch_complete") {
            logBatcher.flush();
            callbacks.applySnapshot(parsed.snapshot);
            settle();
            return;
          }
          if (parsed.type === "launch_error") {
            logBatcher.flush();
            callbacks.applySnapshot(parsed.snapshot);
            settle(new Error(launchErrorFromSnapshot(parsed.snapshot)));
          }
        };

        ws.onclose = () => {
          if (settled) return;
          logBatcher.flush();
          getExecutionLaunchStatus(initial.job_id)
            .then((latest) => {
              callbacks.applySnapshot(latest);
              if (latest.state === "ready") {
                settle();
              } else if (latest.state === "error") {
                settle(new Error(launchErrorFromSnapshot(latest)));
              } else if (reconnects < maxReconnects) {
                reconnects += 1;
                reconnectHandle = window.setTimeout(() => {
                  reconnectHandle = null;
                  connect();
                }, reconnectDelayMs);
              } else {
                settle(new Error("Lost workspace launch stream before setup finished."));
              }
            })
            .catch((error: unknown) => {
              settle(new Error(messageFromError(error)));
            });
        };
      })
      .catch((error: unknown) => {
        settle(new Error(messageFromError(error)));
      });
    };

    connect();
  });
};

export const mergeWorkspaceSetupLaunchLogs = (
  previous: WorkspaceSetupLaunchLogLine[],
  nextLines: ExecutionLaunchLogLine[],
) => mergeLaunchLogs(previous, nextLines);
