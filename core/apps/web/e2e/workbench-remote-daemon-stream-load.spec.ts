import { spawn, type ChildProcessWithoutNullStreams } from "child_process";
import fs from "fs/promises";
import type { APIRequestContext, Page, Request, TestInfo } from "playwright/test";
import { test, expect } from "./fixtures";
import { clearDiagnostics, getDiagnostics } from "./utils/diagnostics";
import {
  postImmediateMessageAndWaitForCompletion,
  seedDummyWorkspace,
} from "./utils/seedDummyWorkspace";
import {
  minSamplesForDistinctPercentile,
  percentile,
  percentileSelectsMaximum,
} from "../src/utils/perfPercentile";

const ENABLED = process.env.CTX_REMOTE_DAEMON_STREAM_SOAK === "1";
const FAULT_MODE = process.env.CTX_REMOTE_DAEMON_STREAM_SOAK_FAULT ?? "";
const REMOTE_MODE =
  process.env.CTX_REMOTE_DAEMON_STREAM_SOAK_REMOTE === "1" ||
  Boolean(process.env.CTX_E2E_BASE_URL && !process.env.CTX_E2E_BASE_URL.includes("127.0.0.1"));
const SCENARIO = process.env.CTX_REMOTE_DAEMON_STREAM_SOAK_SCENARIO ?? "multi-session";
const LONG_FOREGROUND_RECOVERY = SCENARIO === "long-foreground-recovery";

const envNumber = (name: string, fallback: number): number => {
  const raw = process.env[name];
  if (!raw) return fallback;
  const parsed = Number(raw);
  return Number.isFinite(parsed) ? parsed : fallback;
};

const TASK_COUNT = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_TASKS",
  LONG_FOREGROUND_RECOVERY ? 1 : 16,
);
const TURNS_PER_SESSION = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_TURNS",
  LONG_FOREGROUND_RECOVERY ? 13_000 : 3,
);
const DIRECT_SEED_BATCH_SIZE = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_DIRECT_SEED_BATCH_SIZE",
  LONG_FOREGROUND_RECOVERY ? 2000 : Number.MAX_SAFE_INTEGER,
);
const DIRECT_SEED_MATERIALIZED_TAIL_TURNS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_DIRECT_SEED_MATERIALIZED_TAIL_TURNS",
  LONG_FOREGROUND_RECOVERY ? 200 : Number.MAX_SAFE_INTEGER,
);
const MESSAGE_BYTES = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_MESSAGE_BYTES",
  LONG_FOREGROUND_RECOVERY ? 220 : 1800,
);
const STREAMERS = envNumber("CTX_REMOTE_DAEMON_STREAM_SOAK_STREAMERS", 6);
const STREAM_INTERVAL_MS = envNumber("CTX_REMOTE_DAEMON_STREAM_SOAK_STREAM_INTERVAL_MS", 5);
const STREAM_TIMEOUT_MS = envNumber("CTX_REMOTE_DAEMON_STREAM_SOAK_STREAM_TIMEOUT_MS", 75_000);
const DAEMON_REPO_ROOT =
  process.env.CTX_REMOTE_DAEMON_STREAM_SOAK_REPO_ROOT?.trim() || undefined;
const MIN_SESSION_HEAD_PROGRESS_EVENTS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_MIN_SESSION_HEAD_PROGRESS_EVENTS",
  envNumber(
    "CTX_REMOTE_DAEMON_STREAM_SOAK_MIN_SESSION_HEAD_DELTAS",
    LONG_FOREGROUND_RECOVERY ? 12 : 250,
  ),
);
const MIN_STREAM_EVENTS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_MIN_EVENTS",
  LONG_FOREGROUND_RECOVERY ? 20 : 900,
);
const PROBE_COUNT = envNumber("CTX_REMOTE_DAEMON_STREAM_SOAK_PROBES", 4);
const PROBE_TIMEOUT_MS = envNumber("CTX_REMOTE_DAEMON_STREAM_SOAK_PROBE_TIMEOUT_MS", 35_000);
const HEAD_POLL_MS = envNumber("CTX_REMOTE_DAEMON_STREAM_SOAK_HEAD_POLL_MS", REMOTE_MODE ? 250 : 75);
const MAX_INITIAL_UI_READY_MS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_MAX_INITIAL_UI_READY_MS",
  REMOTE_MODE ? 60_000 : 30_000,
);
const CLOCK_SAMPLES = envNumber("CTX_REMOTE_DAEMON_STREAM_SOAK_CLOCK_SAMPLES", 7);
const MAX_CLOCK_UNCERTAINTY_MS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_MAX_CLOCK_UNCERTAINTY_MS",
  REMOTE_MODE ? 1500 : 750,
);

const MAX_VISIBLE_SILENCE_MS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_MAX_STALENESS_MS",
  REMOTE_MODE ? 8000 : 5000,
);
const MAX_HARD_VISIBLE_SILENCE_MS = Math.max(
  MAX_VISIBLE_SILENCE_MS,
  envNumber(
    "CTX_REMOTE_DAEMON_STREAM_SOAK_MAX_HARD_STALENESS_MS",
    REMOTE_MODE ? 12_000 : MAX_VISIBLE_SILENCE_MS,
  ),
);
const MAX_BACKEND_TO_DOM_P95_MS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_MAX_BACKEND_TO_DOM_P95_MS",
  REMOTE_MODE ? 5000 : 2500,
);
const MAX_BACKEND_TO_DOM_MS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_MAX_BACKEND_TO_DOM_MS",
  REMOTE_MODE ? 10_000 : 5000,
);
const MAX_SEND_TO_VISIBLE_MS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_MAX_SEND_TO_VISIBLE_MS",
  REMOTE_MODE ? 8000 : 5000,
);
const MAX_FOREGROUND_CLIENT_RECEIVE_LAG_MS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_MAX_CLIENT_RECEIVE_LAG_MS",
  10_000,
);
const MAX_ALL_CLIENT_RECEIVE_LAG_MS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_MAX_ALL_CLIENT_RECEIVE_LAG_MS",
  REMOTE_MODE ? 30_000 : 15_000,
);
const MAX_REPLICA_APPLY_LAG_MS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_MAX_REPLICA_APPLY_LAG_MS",
  REMOTE_MODE ? 20_000 : 10_000,
);
const MAX_CLICK_TO_PENDING_MS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_MAX_CLICK_TO_PENDING_MS",
  REMOTE_MODE ? 1500 : 1000,
);
const MAX_CLICK_TO_TERMINAL_MS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_MAX_CLICK_TO_TERMINAL_MS",
  REMOTE_MODE ? 25_000 : 15_000,
);
const VCS_CHURN_ENABLED = process.env.CTX_REMOTE_DAEMON_STREAM_SOAK_VCS_CHURN === "1";
const VCS_CHURN_UPDATES = envNumber("CTX_REMOTE_DAEMON_STREAM_SOAK_VCS_CHURN_UPDATES", 1400);
const VCS_CHURN_INTERVAL_MS = envNumber("CTX_REMOTE_DAEMON_STREAM_SOAK_VCS_CHURN_INTERVAL_MS", 60);
const MIN_VCS_SNAPSHOTS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_MIN_VCS_SNAPSHOTS",
  VCS_CHURN_ENABLED ? 25 : 0,
);
const MAX_VCS_RECEIVE_LAG_MS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_MAX_VCS_RECEIVE_LAG_MS",
  REMOTE_MODE ? 20_000 : 10_000,
);
const MAX_VCS_GIT_PANE_OPEN_MS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_MAX_VCS_GIT_PANE_OPEN_MS",
  REMOTE_MODE ? 20_000 : 10_000,
);
const MAX_VCS_TASK_SWITCH_MS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_MAX_VCS_TASK_SWITCH_MS",
  REMOTE_MODE ? 8000 : 5000,
);
const DEFAULT_TEST_SETUP_TIMEOUT_MS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_SETUP_TIMEOUT_MS",
  REMOTE_MODE ? 300_000 : 120_000,
);
const DEFAULT_VCS_CHURN_TIMEOUT_MS = VCS_CHURN_ENABLED
  ? VCS_CHURN_UPDATES * VCS_CHURN_INTERVAL_MS +
    MAX_VCS_GIT_PANE_OPEN_MS +
    2 * MAX_VCS_TASK_SWITCH_MS
  : 0;
const DEFAULT_FORCED_GAP_RECOVERY_TIMEOUT_MS = LONG_FOREGROUND_RECOVERY
  ? 3 * (PROBE_TIMEOUT_MS + 1_000)
  : 0;
const DEFAULT_TEST_TIMEOUT_MS = Math.max(
  480_000,
  DEFAULT_TEST_SETUP_TIMEOUT_MS +
    DEFAULT_FORCED_GAP_RECOVERY_TIMEOUT_MS +
    PROBE_COUNT * (PROBE_TIMEOUT_MS + 1_000) +
    STREAM_TIMEOUT_MS +
    MAX_CLICK_TO_PENDING_MS +
    MAX_CLICK_TO_TERMINAL_MS +
    DEFAULT_VCS_CHURN_TIMEOUT_MS +
    120_000,
);
const TEST_TIMEOUT_MS = envNumber(
  "CTX_REMOTE_DAEMON_STREAM_SOAK_TEST_TIMEOUT_MS",
  DEFAULT_TEST_TIMEOUT_MS,
);
const REMOTE_CHURN_HOST = process.env.CTX_REMOTE_DAEMON_STREAM_SOAK_SSH_HOST?.trim() ?? "";
const REMOTE_CHURN_KEY_PATH = process.env.CTX_REMOTE_DAEMON_STREAM_SOAK_SSH_KEY_PATH?.trim() ?? "";

type SessionHeadResponse = {
  session?: {
    worktree_id?: string | null;
  };
  turns?: Array<{
    turn_id?: string;
    user_message_id?: string;
    status?: string;
  }>;
  messages?: Array<{
    id?: string;
    turn_id?: string;
    role?: string;
    content?: string;
  }>;
};

type PostedMessage = {
  id?: string;
  turn_id?: string | null;
};

type SessionEventRecord = {
  seq?: number;
  event_type?: string;
  turn_id?: string | null;
  payload_json?: unknown;
  created_at?: string | null;
};

type SessionEventsPage = {
  events?: SessionEventRecord[];
  next_cursor?: number | null;
  has_more?: boolean;
};

type TurnTerminalObservation = {
  observedAtMs: number;
  status: string | null;
  terminalAtMs: number | null;
};

type TelemetryMetricSummary = {
  name?: string;
  count?: number;
  sum?: number;
  min?: number | null;
  max?: number | null;
  p50?: number | null;
  p95?: number | null;
  p99?: number | null;
  labels?: Record<string, string>;
};

type TelemetrySummaryResponse = {
  metrics?: TelemetryMetricSummary[];
  window_ms?: number | null;
};

type MetricRollup = {
  count: number;
  sum: number;
  min: number | null;
  max: number | null;
  p50: number | null;
  p95: number | null;
  p99: number | null;
};

type WorktreeResponse = {
  id?: string;
  root_path?: string;
};

type WorktreeVcsSnapshotLike = {
  worktree_id?: string | null;
  compute_state?: string | null;
  summary?: {
    file_count?: number | null;
    line_count?: number | null;
  } | null;
  git_status?: {
    entries?: Array<{ path?: string | null }> | null;
  } | null;
  touched_files?: {
    items?: Array<{ path?: string | null }> | null;
    total_count?: number | null;
  } | null;
  touched_files_state?: string | null;
};

type VcsChurnSummary = {
  enabled: boolean;
  worktreeId: string | null;
  worktreeRoot: string | null;
  updates: number;
  intervalMs: number;
  startedAtMs: number | null;
  stoppedAtMs: number | null;
  exitCode: number | null;
  signal: string | null;
  error: string | null;
  stdoutTail: string;
  stderrTail: string;
};

type VcsTaskSwitchSummary = {
  toBackgroundMs: number | null;
  backToForegroundMs: number | null;
  error: string | null;
};

type VcsGitPaneSummary = {
  openMs: number | null;
  firstFileVisibleMs: number | null;
  error: string | null;
};

type ProbeOutcome = {
  marker: string;
  turnId: string | null;
  sentAtMs: number | null;
  firstVisibleAtMs: number | null;
  sendToFirstVisibleMs: number | null;
  backendReadyAtMs: number | null;
  domVisibleAtMs: number | null;
  backendToDomMs: number | null;
  timedOut: boolean;
  error: string | null;
};

type ClockSample = {
  startedAtMs: number;
  endedAtMs: number;
  rttMs: number;
  daemonUnixMs: number;
  offsetMs: number;
  uncertaintyMs: number;
};

type ClockCalibration = {
  samples: ClockSample[];
  offsetMs: number;
  uncertaintyMs: number;
  minRttMs: number;
  p95RttMs: number | null;
};

type VisibleProgressSnapshot = {
  startedAtMs: number;
  endedAtMs: number;
  samples: Array<{
    at_ms: number;
    text_length: number;
    signature: string;
  }>;
};

type BoundedWriter = {
  stop: () => Promise<void>;
  getStats: () => { sent: number; failures: string[]; backpressure: number };
};

type WorkspaceStreamTelemetrySample = {
  lane: "foreground" | "workspace";
  eventType: string;
  sessionId: string | null;
  emittedAtMs: number | null;
  receivedAtMs: number;
  streamSource?: "live" | "replay";
};

type RemoteDaemonLoadWindow = Window & {
  __ctxVisibleProgressProbe?: {
    getSnapshot: () => VisibleProgressSnapshot;
    stop: () => VisibleProgressSnapshot;
  };
  __ctxWorkspaceStreamTelemetrySamples?: WorkspaceStreamTelemetrySample[];
  __ctxE2E?: {
    workspaceStream?: {
      getConnectionState?: () => string | null;
      setDropMessages?: (drop: boolean) => void;
    };
    getVcsSnapshot?: (worktreeId: string) => WorktreeVcsSnapshotLike | null;
    refreshVcsDetails?: (worktreeId: string) => boolean;
  };
};

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

const metricRollupEmpty = (): MetricRollup => ({
  count: 0,
  sum: 0,
  min: null,
  max: null,
  p50: null,
  p95: null,
  p99: null,
});

const formatUnknownError = (error: unknown): string => {
  if (error instanceof Error && error.message) return error.message;
  return String(error);
};

const asRecord = (value: unknown): Record<string, unknown> =>
  value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};

const appendTail = (current: string, chunk: Buffer, maxLength = 12_000): string => {
  const next = `${current}${chunk.toString("utf8")}`;
  return next.length <= maxLength ? next : next.slice(next.length - maxLength);
};

const shellQuote = (value: string): string => `'${value.replace(/'/g, `'\\''`)}'`;

const emptyVcsChurnSummary = (error: string | null = null): VcsChurnSummary => ({
  enabled: VCS_CHURN_ENABLED,
  worktreeId: null,
  worktreeRoot: null,
  updates: VCS_CHURN_UPDATES,
  intervalMs: VCS_CHURN_INTERVAL_MS,
  startedAtMs: null,
  stoppedAtMs: null,
  exitCode: null,
  signal: null,
  error,
  stdoutTail: "",
  stderrTail: "",
});

async function getSessionWorktreeRoot(
  request: APIRequestContext,
  sessionId: string,
): Promise<{ worktreeId: string; rootPath: string }> {
  const headResponse = await request.get(`/api/sessions/${sessionId}/head`);
  expect(headResponse.ok(), `head request failed: ${headResponse.url()}`).toBeTruthy();
  const head = (await headResponse.json()) as SessionHeadResponse;
  const worktreeId = String(head.session?.worktree_id ?? "").trim();
  if (!worktreeId) {
    throw new Error(`session ${sessionId} head did not include a worktree id`);
  }
  const worktreeResponse = await request.get(`/api/worktrees/${worktreeId}`);
  expect(worktreeResponse.ok(), `worktree request failed: ${worktreeResponse.url()}`).toBeTruthy();
  const worktree = (await worktreeResponse.json()) as WorktreeResponse;
  const rootPath = String(worktree.root_path ?? "").trim();
  if (!rootPath) {
    throw new Error(`worktree ${worktreeId} response did not include a root path`);
  }
  return { worktreeId, rootPath };
}

async function waitForBrowserWorktreeVcsSummary(
  page: Page,
  worktreeId: string,
  timeoutMs: number,
): Promise<void> {
  await expect
    .poll(
      async () => {
        return page.evaluate((id) => {
          const bridge = (window as RemoteDaemonLoadWindow).__ctxE2E;
          bridge?.refreshVcsDetails?.(id);
          const snapshot = bridge?.getVcsSnapshot?.(id) ?? null;
          if (!snapshot || snapshot.compute_state !== "ready") return null;
          const fileCount = snapshot.summary?.file_count ?? null;
          const lineCount = snapshot.summary?.line_count ?? null;
          const inventoryCount =
            (snapshot.git_status?.entries ?? []).length + (snapshot.touched_files?.items ?? []).length;
          return Math.max(Number(fileCount ?? 0), Number(lineCount ?? 0), inventoryCount);
        }, worktreeId);
      },
      { timeout: timeoutMs },
    )
    .toBeGreaterThan(0);
}

async function waitForBrowserWorktreeVcsInventoryPath(
  page: Page,
  worktreeId: string,
  expectedPath: string,
  timeoutMs: number,
): Promise<void> {
  await expect
    .poll(
      async () => {
        return page.evaluate(
          ({ id, path }) => {
            const bridge = (window as RemoteDaemonLoadWindow).__ctxE2E;
            bridge?.refreshVcsDetails?.(id);
            const snapshot = bridge?.getVcsSnapshot?.(id) ?? null;
            if (!snapshot || snapshot.compute_state !== "ready") return false;
            const inventoryPaths = [
              ...(snapshot.git_status?.entries ?? []),
              ...(snapshot.touched_files?.items ?? []),
            ].map((item) => String(item.path ?? ""));
            return inventoryPaths.includes(path);
          },
          { id: worktreeId, path: expectedPath },
        );
      },
      { timeout: timeoutMs },
    )
    .toBe(true);
}

async function seedRemoteVcsInitialChange(
  request: APIRequestContext,
  sessionId: string,
): Promise<{ worktreeId: string; rootPath: string }> {
  if (!REMOTE_CHURN_HOST || !REMOTE_CHURN_KEY_PATH) {
    throw new Error(
      "VCS seed requires CTX_REMOTE_DAEMON_STREAM_SOAK_SSH_HOST and CTX_REMOTE_DAEMON_STREAM_SOAK_SSH_KEY_PATH",
    );
  }

  const { worktreeId, rootPath } = await getSessionWorktreeRoot(request, sessionId);
  let stdoutTail = "";
  let stderrTail = "";
  const child: ChildProcessWithoutNullStreams = spawn(
    "ssh",
    [
      "-o",
      "StrictHostKeyChecking=no",
      "-o",
      "UserKnownHostsFile=/dev/null",
      "-o",
      "LogLevel=ERROR",
      "-i",
      REMOTE_CHURN_KEY_PATH,
      `root@${REMOTE_CHURN_HOST}`,
      "bash",
      "-s",
      "--",
      rootPath,
    ],
    { stdio: "pipe" },
  );

  child.stdout.on("data", (chunk: Buffer) => {
    stdoutTail = appendTail(stdoutTail, chunk);
  });
  child.stderr.on("data", (chunk: Buffer) => {
    stderrTail = appendTail(stderrTail, chunk);
  });

  const result = new Promise<void>((resolve, reject) => {
    let settled = false;
    let timeout: ReturnType<typeof setTimeout>;
    const finish = (error: string | null) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      if (error) {
        reject(new Error(error));
        return;
      }
      resolve();
    };
    timeout = setTimeout(() => {
      if (settled) return;
      child.kill("SIGTERM");
      const details = [stdoutTail.trim(), stderrTail.trim()].filter(Boolean).join("\n");
      finish(`remote VCS initial seed timed out after 10000ms${details ? `: ${details}` : ""}`);
    }, 10_000);
    child.on("error", (error) => {
      finish(formatUnknownError(error));
    });
    child.on("exit", (code, signal) => {
      if (code === 0) {
        finish(null);
        return;
      }
      const details = [stdoutTail.trim(), stderrTail.trim()].filter(Boolean).join("\n");
      finish(
        `remote VCS initial seed exited with code ${code ?? "null"} signal ${signal ?? "null"}${
          details ? `: ${details}` : ""
        }`,
      );
    });
  });

  child.stdin.end(`#!/usr/bin/env bash
set -euo pipefail
worktree_root="$1"
if [[ ! -d "$worktree_root" ]]; then
  echo "worktree root missing: $worktree_root" >&2
  exit 11
fi
if [[ ! -d "$worktree_root/.git" && ! -f "$worktree_root/.git" ]]; then
  echo "not a git worktree: $worktree_root" >&2
  exit 12
fi
mkdir -p "$worktree_root/.ctx-vcs-soak"
rm -f "$worktree_root/.ctx-vcs-soak/stop-requested"
seed_marker="$(date +%s%3N)"
if ! git -C "$worktree_root" ls-files --error-unmatch vcs-soak-tracked.txt >/dev/null 2>&1; then
  printf 'remote vcs soak baseline\\n' >"$worktree_root/vcs-soak-tracked.txt"
  git -C "$worktree_root" add vcs-soak-tracked.txt
  git -C "$worktree_root" -c user.name='ctx e2e' -c user.email='ctx-e2e@example.invalid' commit --no-gpg-sign --no-verify -m 'seed remote vcs soak file' >/dev/null
fi
printf 'remote vcs soak seed %s\\n' "$seed_marker" >"$worktree_root/vcs-soak-tracked.txt"
printf 'remote vcs soak inventory seed %s\\n' "$seed_marker" >"$worktree_root/vcs-soak-initial.txt"
git -C "$worktree_root" status --short >/dev/null
`);

  await result;
  return { worktreeId, rootPath };
}

async function requestRemoteVcsChurnStop(rootPath: string): Promise<string | null> {
  const stopDirectory = `${rootPath}/.ctx-vcs-soak`;
  const stopFile = `${stopDirectory}/stop-requested`;
  let stdoutTail = "";
  let stderrTail = "";
  const child = spawn(
    "ssh",
    [
      "-o",
      "StrictHostKeyChecking=no",
      "-o",
      "UserKnownHostsFile=/dev/null",
      "-o",
      "LogLevel=ERROR",
      "-i",
      REMOTE_CHURN_KEY_PATH,
      `root@${REMOTE_CHURN_HOST}`,
      `bash -s -- ${shellQuote(stopDirectory)} ${shellQuote(stopFile)}`,
    ],
    { stdio: "pipe" },
  );
  child.stdout.on("data", (chunk: Buffer) => {
    stdoutTail = appendTail(stdoutTail, chunk);
  });
  child.stderr.on("data", (chunk: Buffer) => {
    stderrTail = appendTail(stderrTail, chunk);
  });
  const stopResult = new Promise<string | null>((resolve) => {
    let settled = false;
    const timeout = setTimeout(() => {
      if (settled) return;
      settled = true;
      child.kill("SIGTERM");
      resolve("remote VCS churn stop request timed out");
    }, 5000);
    const finish = (error: string | null) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      resolve(error);
    };
    child.on("error", (error) => {
      finish(formatUnknownError(error));
    });
    child.on("exit", (code, signal) => {
      if (code === 0) {
        finish(null);
        return;
      }
      const details = [stdoutTail.trim(), stderrTail.trim()].filter(Boolean).join("\n");
      finish(
        `remote VCS churn stop request exited with code ${code ?? "null"} signal ${
          signal ?? "null"
        }${details ? `: ${details}` : ""}`,
      );
    });
  });
  child.stdin.end(`#!/usr/bin/env bash
set -euo pipefail
stop_directory="$1"
stop_file="$2"
mkdir -p "$stop_directory"
touch "$stop_file"
`);
  return await stopResult;
}

async function startRemoteVcsChurn(
  request: APIRequestContext,
  sessionId: string,
): Promise<{ summary: VcsChurnSummary; stop: () => Promise<VcsChurnSummary> } | null> {
  if (!VCS_CHURN_ENABLED) return null;
  if (!REMOTE_CHURN_HOST || !REMOTE_CHURN_KEY_PATH) {
    throw new Error(
      "VCS churn requires CTX_REMOTE_DAEMON_STREAM_SOAK_SSH_HOST and CTX_REMOTE_DAEMON_STREAM_SOAK_SSH_KEY_PATH",
    );
  }

  const { worktreeId, rootPath } = await getSessionWorktreeRoot(request, sessionId);
  let stdoutTail = "";
  let stderrTail = "";
  let completed: VcsChurnSummary | null = null;
  const startedAtMs = Date.now();
  const child: ChildProcessWithoutNullStreams = spawn(
    "ssh",
    [
      "-o",
      "StrictHostKeyChecking=no",
      "-o",
      "UserKnownHostsFile=/dev/null",
      "-o",
      "LogLevel=ERROR",
      "-i",
      REMOTE_CHURN_KEY_PATH,
      `root@${REMOTE_CHURN_HOST}`,
      "bash",
      "-s",
      "--",
      rootPath,
      String(VCS_CHURN_UPDATES),
      String(VCS_CHURN_INTERVAL_MS),
    ],
    { stdio: "pipe" },
  );

  child.stdout.on("data", (chunk: Buffer) => {
    stdoutTail = appendTail(stdoutTail, chunk);
  });
  child.stderr.on("data", (chunk: Buffer) => {
    stderrTail = appendTail(stderrTail, chunk);
  });
  const completion = new Promise<VcsChurnSummary>((resolve) => {
    const finish = (summary: VcsChurnSummary) => {
      completed = summary;
      resolve(summary);
    };
    child.on("error", (error) => {
      finish({
        enabled: true,
        worktreeId,
        worktreeRoot: rootPath,
        updates: VCS_CHURN_UPDATES,
        intervalMs: VCS_CHURN_INTERVAL_MS,
        startedAtMs,
        stoppedAtMs: Date.now(),
        exitCode: null,
        signal: null,
        error: formatUnknownError(error),
        stdoutTail,
        stderrTail,
      });
    });
    child.on("exit", (code, signal) => {
      finish({
        enabled: true,
        worktreeId,
        worktreeRoot: rootPath,
        updates: VCS_CHURN_UPDATES,
        intervalMs: VCS_CHURN_INTERVAL_MS,
        startedAtMs,
        stoppedAtMs: Date.now(),
        exitCode: code,
        signal,
        error: code === 0 ? null : `remote VCS churn exited with code ${code ?? "null"} signal ${signal ?? "null"}`,
        stdoutTail,
        stderrTail,
      });
    });
  });

  child.stdin.end(`#!/usr/bin/env bash
set -euo pipefail
worktree_root="$1"
updates="$2"
interval_ms="$3"
if [[ ! -d "$worktree_root" ]]; then
  echo "worktree root missing: $worktree_root" >&2
  exit 11
fi
if [[ ! -d "$worktree_root/.git" && ! -f "$worktree_root/.git" ]]; then
  echo "not a git worktree: $worktree_root" >&2
  exit 12
fi
sleep_seconds="$(awk -v ms="$interval_ms" 'BEGIN { printf "%.3f", ms / 1000 }')"
mkdir -p "$worktree_root/.ctx-vcs-soak"
stop_file="$worktree_root/.ctx-vcs-soak/stop-requested"
rm -f "$stop_file"
stop_requested=0
trap 'stop_requested=1' TERM INT
for ((i=1; i<=updates; i++)); do
  if [[ "$stop_requested" == "1" || -f "$stop_file" ]]; then
    echo "stopped after $((i - 1)) updates"
    exit 0
  fi
  printf 'remote vcs soak update %06d %s\\n' "$i" "$(date +%s%3N)" >"$worktree_root/vcs-soak-tracked.txt"
  printf 'rotating vcs soak file %06d\\n' "$i" >"$worktree_root/.ctx-vcs-soak/rotating-$((i % 64)).txt"
  if (( i % 100 == 0 )); then
    git -C "$worktree_root" status --short >/dev/null || true
  fi
  sleep "$sleep_seconds"
done
echo "completed $updates updates"
`);

  await sleep(1000);
  if (completed?.error) {
    throw new Error(completed.error);
  }

  return {
    summary: {
      enabled: true,
      worktreeId,
      worktreeRoot: rootPath,
      updates: VCS_CHURN_UPDATES,
      intervalMs: VCS_CHURN_INTERVAL_MS,
      startedAtMs,
      stoppedAtMs: null,
      exitCode: null,
      signal: null,
      error: null,
      stdoutTail,
      stderrTail,
    },
    stop: async () => {
      let stopRequestError: string | null = null;
      if (!completed) {
        stopRequestError = await requestRemoteVcsChurnStop(rootPath);
      }
      const timeout = new Promise<VcsChurnSummary>((resolve) => {
        setTimeout(() => {
          if (!completed) {
            child.kill("SIGTERM");
            setTimeout(() => {
              if (!completed) child.kill("SIGKILL");
            }, 1000).unref();
          }
          resolve({
            enabled: true,
            worktreeId,
            worktreeRoot: rootPath,
            updates: VCS_CHURN_UPDATES,
            intervalMs: VCS_CHURN_INTERVAL_MS,
            startedAtMs,
            stoppedAtMs: Date.now(),
            exitCode: null,
            signal: null,
            error: stopRequestError
              ? `remote VCS churn did not stop within 10000ms after stop request failed: ${stopRequestError}`
              : "remote VCS churn did not stop within 10000ms",
            stdoutTail,
            stderrTail,
          });
        }, 10_000);
      });
      return Promise.race([completion, timeout]);
    },
  };
}

const buildSlowPrompt = (
  marker: string,
  opts?: { toolCount?: number; bodyLines?: number },
): string => {
  const toolCount = opts?.toolCount ?? 5;
  const bodyLines = opts?.bodyLines ?? 80;
  const tools = Array.from({ length: toolCount }, (_, index) => ({
    kind: "execute",
    title: `remote stream tool ${index + 1}`,
    input: { command: `printf '${marker}-${index + 1}'` },
    output_text: `${marker} output ${index + 1}`,
  }));
  const body = Array.from(
    { length: bodyLines },
    (_, index) => `remote daemon stream visible progress ${marker} line ${index + 1}`,
  ).join("\n");
  return `slow-diff-test stream-assistant-partials emit-thought ${marker}
${body}
[[tool_calls]]
${JSON.stringify(tools)}
[[/tool_calls]]`;
};

const buildBackgroundPrompt = (label: string): string => {
  const tools = Array.from({ length: 4 }, (_, index) => ({
    kind: "execute",
    title: `background stream tool ${index + 1}`,
    input: { command: `printf '${label}-${index + 1}'` },
    output_text: `${label} output ${index + 1}`,
  }));
  const base = `remote-daemon-load-background ${label}`;
  const padding = MESSAGE_BYTES > base.length ? `\n${"x".repeat(MESSAGE_BYTES - base.length)}` : "";
  return `${base}${padding}
[[tool_calls]]
${JSON.stringify(tools)}
[[/tool_calls]]`;
};

function startBoundedBackgroundWriters(
  request: APIRequestContext,
  sessionIds: readonly string[],
): BoundedWriter[] {
  return sessionIds.map((sessionId, sessionIndex) => {
    let stopped = false;
    let sent = 0;
    let backpressure = 0;
    const failures: string[] = [];
    const loop = (async () => {
      while (!stopped) {
        const label = `s${sessionIndex + 1}-${sent + 1}-${Date.now()}`;
        try {
          await postImmediateMessageAndWaitForCompletion(
            request,
            sessionId,
            buildBackgroundPrompt(label),
            { timeoutMs: 30_000, pollMs: 250 },
          );
          sent += 1;
        } catch (error) {
          const message = formatUnknownError(error);
          if (
            message.includes("A turn is already running") ||
            message.includes("turn completion timeout")
          ) {
            backpressure += 1;
          } else {
            failures.push(message);
          }
          await sleep(250);
        }
        await sleep(Math.max(STREAM_INTERVAL_MS, 100));
      }
    })();
    return {
      stop: async () => {
        stopped = true;
        await loop;
      },
      getStats: () => ({ sent, failures: failures.slice(), backpressure }),
    };
  });
}

async function sendSessionMessage(
  request: APIRequestContext,
  sessionId: string,
  content: string,
  opts?: { retryBusyForMs?: number },
): Promise<{ messageId: string; turnId: string; afterSeq: number }> {
  const afterSeq = await currentSessionEventSeq(request, sessionId);
  const deadline = Date.now() + Math.max(0, opts?.retryBusyForMs ?? 0);
  while (true) {
    const response = await request.post(`/api/sessions/${sessionId}/messages`, {
      data: {
        content,
        delivery: "immediate",
      },
    });
    if (response.ok()) {
      const payload = (await response.json()) as PostedMessage;
      const messageId = String(payload.id ?? "");
      const turnId = String(payload.turn_id ?? "");
      if (!messageId || !turnId) {
        throw new Error(`message send did not return message id and turn id for session ${sessionId}`);
      }
      return { messageId, turnId, afterSeq };
    }
    const body = await response.text().catch(() => "");
    const retryableBusy =
      response.status() === 409 &&
      body.toLowerCase().includes("turn") &&
      body.toLowerCase().includes("running") &&
      Date.now() < deadline;
    if (retryableBusy) {
      await sleep(150);
      continue;
    }
    const suffix = body.trim() ? `: ${body.trim()}` : "";
    throw new Error(`message send failed: ${response.url()} (${response.status()})${suffix}`);
  }
}

function terminalStatusFromEvent(event: SessionEventRecord): string | null {
  if (event.event_type === "turn_interrupted") return "interrupted";
  if (event.event_type !== "turn_finished") return null;
  const status = asRecord(event.payload_json).status;
  return typeof status === "string" ? status.toLowerCase() : null;
}

function eventCreatedAtMs(event: SessionEventRecord): number | null {
  if (!event.created_at) return null;
  const parsed = Date.parse(event.created_at);
  return Number.isFinite(parsed) ? parsed : null;
}

function pageNextSeq(page: SessionEventsPage, events: SessionEventRecord[]): number | null {
  if (typeof page.next_cursor === "number" && Number.isFinite(page.next_cursor)) {
    return page.next_cursor;
  }
  const seqs = events
    .map((event) => event.seq)
    .filter((seq): seq is number => typeof seq === "number" && Number.isFinite(seq));
  return seqs.length > 0 ? Math.max(...seqs) : null;
}

async function currentSessionEventSeq(
  request: APIRequestContext,
  sessionId: string,
): Promise<number> {
  const response = await request.get(`/api/sessions/${sessionId}/events?tail=1&include_transient=1`);
  expect(response.ok(), `events cursor request failed: ${response.url()}`).toBeTruthy();
  const payload = (await response.json()) as SessionEventsPage;
  const events = Array.isArray(payload.events) ? payload.events : [];
  return pageNextSeq(payload, events) ?? 0;
}

async function waitForTurnTerminalEvent(
  request: APIRequestContext,
  sessionId: string,
  turnId: string,
  statuses: ReadonlySet<string>,
  timeoutMs: number,
  startAfterSeq?: number,
): Promise<TurnTerminalObservation> {
  const deadline = Date.now() + timeoutMs;
  let afterSeq: number | null = startAfterSeq ?? null;
  let useTail = afterSeq === null;
  while (Date.now() < deadline) {
    const eventsUrl = useTail
      ? `/api/sessions/${sessionId}/events?tail=1000&include_transient=1`
      : `/api/sessions/${sessionId}/events?after_seq=${afterSeq ?? 0}&limit=1000&include_transient=1`;
    const response = await request.get(eventsUrl);
    expect(response.ok(), `events request failed: ${response.url()}`).toBeTruthy();
    const payload = (await response.json()) as SessionEventsPage;
    const wasTail = useTail;
    useTail = false;
    const events = Array.isArray(payload.events) ? payload.events : [];
    for (const event of events) {
      if (String(event.turn_id ?? "") !== turnId) continue;
      const status = terminalStatusFromEvent(event);
      if (status && statuses.has(status)) {
        return {
          observedAtMs: Date.now(),
          status,
          terminalAtMs: eventCreatedAtMs(event),
        };
      }
    }
    const nextSeq = pageNextSeq(payload, events);
    if (nextSeq !== null) {
      afterSeq = Math.max(afterSeq ?? nextSeq, nextSeq);
    }
    if (!wasTail && payload.has_more) {
      continue;
    }
    await sleep(HEAD_POLL_MS);
  }
  return { observedAtMs: Date.now(), status: null, terminalAtMs: null };
}

async function waitForForegroundTurnCompletion(
  request: APIRequestContext,
  sessionId: string,
  turnId: string,
  startAfterSeq: number,
  timeoutMs: number,
): Promise<{ backendReadyAtMs: number; turnId: string }> {
  const terminal = await waitForTurnTerminalEvent(
    request,
    sessionId,
    turnId,
    new Set(["completed", "done"]),
    timeoutMs,
    startAfterSeq,
  );
  if (terminal.status) {
    return {
      backendReadyAtMs: terminal.terminalAtMs ?? terminal.observedAtMs,
      turnId,
    };
  }
  throw new Error(`foreground turn did not complete for turn ${turnId}`);
}

async function waitForVisibleMarker(
  page: Page,
  marker: string,
  timeoutMs: number,
): Promise<number> {
  const handle = await page.waitForFunction(
    ({ text }) => {
      const session = document.querySelector('.wb-session-slot[aria-hidden="false"]');
      return session?.textContent?.includes(text) ? Date.now() : null;
    },
    { text: marker },
    { timeout: timeoutMs },
  );
  const value = await handle.jsonValue();
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new Error(`marker ${marker} did not expose a visible timestamp`);
  }
  return value;
}

async function waitForVisibleTerminalStatus(
  page: Page,
  marker: string,
  timeoutMs: number,
): Promise<number> {
  const handle = await page.waitForFunction(
    ({ text }) => {
      const session = document.querySelector('.wb-session-slot[aria-hidden="false"]');
      const content = session?.textContent ?? "";
      return content.includes(text) && /Interrupted|Cancelled|Canceled/.test(content) ? Date.now() : null;
    },
    { text: marker },
    { timeout: timeoutMs },
  );
  const value = await handle.jsonValue();
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new Error(`marker ${marker} did not expose a visible terminal status`);
  }
  return value;
}

async function runForegroundProbe(
  page: Page,
  request: APIRequestContext,
  sessionId: string,
  marker: string,
): Promise<ProbeOutcome> {
  let sentAtMs: number | null = null;
  try {
    const sent = await sendSessionMessage(
      request,
      sessionId,
      buildSlowPrompt(marker, { bodyLines: 24, toolCount: 2 }),
      { retryBusyForMs: 5000 },
    );
    sentAtMs = Date.now();
    const [firstVisibleAtMs, completion] = await Promise.all([
      waitForVisibleMarker(page, marker, PROBE_TIMEOUT_MS),
      waitForForegroundTurnCompletion(
        request,
        sessionId,
        sent.turnId,
        sent.afterSeq,
        PROBE_TIMEOUT_MS,
      ),
    ]);
    const { backendReadyAtMs, turnId } = completion;
    const domVisibleAtMs = firstVisibleAtMs;
    return {
      marker,
      turnId,
      sentAtMs,
      firstVisibleAtMs,
      sendToFirstVisibleMs: firstVisibleAtMs - sentAtMs,
      backendReadyAtMs,
      domVisibleAtMs,
      backendToDomMs: Math.max(0, domVisibleAtMs - backendReadyAtMs),
      timedOut: false,
      error: null,
    };
  } catch (error) {
    return {
      marker,
      turnId: null,
      sentAtMs,
      firstVisibleAtMs: null,
      sendToFirstVisibleMs: null,
      backendReadyAtMs: null,
      domVisibleAtMs: null,
      backendToDomMs: null,
      timedOut: true,
      error: formatUnknownError(error),
    };
  }
}

async function waitForTerminalInterrupted(
  page: Page,
  request: APIRequestContext,
  sessionId: string,
  turnId: string,
  startAfterSeq: number,
  marker: string,
  timeoutMs: number,
): Promise<{
  terminalAtMs: number | null;
  status: string | null;
  terminalEventAtMs: number | null;
  terminalEventObservedAtMs: number | null;
}> {
  const [terminalEvent, visibleAtMs] = await Promise.all([
    waitForTurnTerminalEvent(
      request,
      sessionId,
      turnId,
      new Set(["interrupted", "cancelled", "canceled"]),
      timeoutMs,
      startAfterSeq,
    ),
    waitForVisibleTerminalStatus(page, marker, timeoutMs),
  ]);
  return {
    terminalAtMs: visibleAtMs,
    status: terminalEvent.status,
    terminalEventAtMs: terminalEvent.terminalAtMs,
    terminalEventObservedAtMs: terminalEvent.observedAtMs,
  };
}

async function readTelemetryMetric(
  request: APIRequestContext,
  metric: string,
  windowMs: number,
): Promise<MetricRollup> {
  const response = await request.get(
    `/api/telemetry/summary?metric=${encodeURIComponent(metric)}&window_ms=${windowMs}`,
  );
  expect(response.ok(), `telemetry summary failed: ${response.url()}`).toBeTruthy();
  const payload = (await response.json()) as TelemetrySummaryResponse;
  const metrics = Array.isArray(payload.metrics) ? payload.metrics : [];
  if (metrics.length === 0) return metricRollupEmpty();
  const numericValues = (key: keyof MetricRollup) =>
    metrics
      .map((entry) => entry[key as keyof TelemetryMetricSummary])
      .filter((value): value is number => typeof value === "number" && Number.isFinite(value));
  const counts = numericValues("count");
  const sums = numericValues("sum");
  const mins = numericValues("min");
  const maxes = numericValues("max");
  const p50s = numericValues("p50");
  const p95s = numericValues("p95");
  const p99s = numericValues("p99");
  return {
    count: counts.reduce((total, value) => total + value, 0),
    sum: sums.reduce((total, value) => total + value, 0),
    min: mins.length > 0 ? Math.min(...mins) : null,
    max: maxes.length > 0 ? Math.max(...maxes) : null,
    p50: p50s.length > 0 ? Math.max(...p50s) : null,
    p95: p95s.length > 0 ? Math.max(...p95s) : null,
    p99: p99s.length > 0 ? Math.max(...p99s) : null,
  };
}

async function readTelemetryMetricEntries(
  request: APIRequestContext,
  metric: string,
  windowMs: number,
): Promise<TelemetryMetricSummary[]> {
  const response = await request.get(
    `/api/telemetry/summary?metric=${encodeURIComponent(metric)}&window_ms=${windowMs}`,
  );
  expect(response.ok(), `telemetry summary failed: ${response.url()}`).toBeTruthy();
  const payload = (await response.json()) as TelemetrySummaryResponse;
  return Array.isArray(payload.metrics) ? payload.metrics : [];
}

function sumMetricEntries(entries: readonly TelemetryMetricSummary[]): number {
  return entries.reduce(
    (total, entry) =>
      total + (typeof entry.sum === "number" && Number.isFinite(entry.sum) ? entry.sum : 0),
    0,
  );
}

function sumMetricEntriesByLabel(
  entries: readonly TelemetryMetricSummary[],
  labelName: string,
): Record<string, number> {
  const out: Record<string, number> = {};
  for (const entry of entries) {
    const labelValue = entry.labels?.[labelName] ?? "unknown";
    const value = typeof entry.sum === "number" && Number.isFinite(entry.sum) ? entry.sum : 0;
    out[labelValue] = (out[labelValue] ?? 0) + value;
  }
  return out;
}

function sessionHeadProgressEventCount(countsByType: Record<string, number>): number {
  return (countsByType.session_head_delta ?? 0) + (countsByType.session_head_seed ?? 0);
}

async function waitForTelemetryMetricSum(
  request: APIRequestContext,
  metric: string,
  minimum: number,
  timeoutMs: number,
): Promise<number> {
  const deadline = Date.now() + timeoutMs;
  let current = 0;
  while (Date.now() < deadline) {
    current = sumMetricEntries(await readTelemetryMetricEntries(request, metric, 180_000));
    if (current >= minimum) return current;
    await sleep(500);
  }
  return current;
}

async function readTelemetryMetrics(
  request: APIRequestContext,
  names: readonly string[],
  windowMs: number,
): Promise<Record<string, MetricRollup>> {
  const entries = await Promise.all(
    names.map(async (name) => [name, await readTelemetryMetric(request, name, windowMs)] as const),
  );
  return Object.fromEntries(entries);
}

async function attachPartialRemoteDaemonStreamLoadMetrics(
  testInfo: TestInfo,
  request: APIRequestContext,
  reason: string,
  context: Record<string, unknown>,
): Promise<void> {
  const captureErrors: string[] = [];
  const streamEventMetricEntries = await readTelemetryMetricEntries(
    request,
    "workbench.workspace_stream_event_count",
    180_000,
  ).catch((error: unknown): TelemetryMetricSummary[] => {
    captureErrors.push(`stream events: ${formatUnknownError(error)}`);
    return [];
  });
  const metrics = await readTelemetryMetrics(
    request,
    [
      "workbench.workspace_stream_event_count",
      "workbench.client_receive_lag_ms",
      "workbench.workspace_event_age_ms",
      "workbench.session_replica_apply_lag_ms",
      "workbench.session_replica_event_age_ms",
      "workbench.final_ws_to_dom_ms",
      "workbench.final_ingress_to_dom_ms",
      "workbench.foreground_queue_age_ms",
      "workspace.stream.receiver_drain_event_count",
      "workspace.vcs_stream.snapshot_count",
      "workspace.vcs_stream.receive_lag_ms",
    ],
    180_000,
  ).catch((error: unknown): Record<string, MetricRollup> => {
    captureErrors.push(`metrics: ${formatUnknownError(error)}`);
    return {};
  });
  const summary = {
    status: "partial",
    reason,
    capturedAtMs: Date.now(),
    context,
    load: {
      streamEventCount: sumMetricEntries(streamEventMetricEntries),
      streamEventCountsByType: sumMetricEntriesByLabel(streamEventMetricEntries, "event_type"),
      streamEventCountsByLane: sumMetricEntriesByLabel(streamEventMetricEntries, "lane"),
    },
    metrics,
    captureErrors,
  };
  const body = JSON.stringify(summary, null, 2);
  await testInfo.attach("remote-daemon-stream-load-partial-metrics.json", {
    body,
    contentType: "application/json",
  });
  await fs.writeFile(
    testInfo.outputPath("remote-daemon-stream-load-partial-metrics.json"),
    body,
    "utf8",
  );
}

async function calibrateClock(request: APIRequestContext): Promise<ClockCalibration> {
  const samples: ClockSample[] = [];
  for (let index = 0; index < CLOCK_SAMPLES; index += 1) {
    const startedAtMs = Date.now();
    const response = await request.get("/api/dev/clock");
    const endedAtMs = Date.now();
    expect(response.ok(), `clock calibration failed: ${response.url()}`).toBeTruthy();
    const payload = (await response.json()) as { daemon_unix_ms?: number };
    const daemonUnixMs =
      typeof payload.daemon_unix_ms === "number" && Number.isFinite(payload.daemon_unix_ms)
        ? payload.daemon_unix_ms
        : NaN;
    expect(Number.isFinite(daemonUnixMs), "clock calibration returned a daemon timestamp").toBeTruthy();
    const rttMs = endedAtMs - startedAtMs;
    const midpointMs = startedAtMs + rttMs / 2;
    samples.push({
      startedAtMs,
      endedAtMs,
      rttMs,
      daemonUnixMs,
      offsetMs: daemonUnixMs - midpointMs,
      uncertaintyMs: rttMs / 2,
    });
    await sleep(25);
  }
  const best = samples.slice().sort((left, right) => left.rttMs - right.rttMs)[0];
  if (!best) throw new Error("clock calibration produced no samples");
  return {
    samples,
    offsetMs: best.offsetMs,
    uncertaintyMs: best.uncertaintyMs,
    minRttMs: best.rttMs,
    p95RttMs: percentile(samples.map((sample) => sample.rttMs), 0.95),
  };
}

async function startVisibleProgressProbe(page: Page, faultMode: string): Promise<void> {
  await page.evaluate((mode) => {
    const win = window as RemoteDaemonLoadWindow;
    const samples: VisibleProgressSnapshot["samples"] = [];
    const startedAtMs = Date.now();
    let endedAtMs = startedAtMs;
    let lastSignature = "";
    let stopped = false;
    let observer: MutationObserver | null = null;
    let timer: number | null = null;

    const isVisible = (element: HTMLElement): boolean => {
      const style = window.getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      return (
        style.display !== "none" &&
        style.visibility !== "hidden" &&
        Number(style.opacity || "1") > 0 &&
        rect.width > 0 &&
        rect.height > 0
      );
    };
    const activeTarget = (): HTMLElement | null => {
      const thread =
        document.querySelector<HTMLElement>(
          '.wb-session-slot[aria-hidden="false"] .wb-thread-scroller',
        ) ??
        document.querySelector<HTMLElement>(
          '.wb-session-slot[aria-hidden="false"] .wb-thread-live-tail',
        ) ??
        document.querySelector<HTMLElement>('.wb-session-slot[aria-hidden="false"]');
      return thread && isVisible(thread) ? thread : null;
    };
    const sample = () => {
      if (stopped) return;
      const target = activeTarget();
      if (!target) return;
      const text = target.innerText || target.textContent || "";
      const signature = `${text.length}:${text.slice(-320)}`;
      if (signature === lastSignature) return;
      lastSignature = signature;
      samples.push({
        at_ms: Date.now(),
        text_length: text.length,
        signature,
      });
    };
    if (mode === "pause-foreground-visible-progress") {
      const style = document.createElement("style");
      style.id = "ctx-remote-daemon-stream-load-fault";
      style.textContent = `
        .wb-session-slot[aria-hidden="false"] .wb-thread-scroller,
        .wb-session-slot[aria-hidden="false"] .wb-thread-live-tail {
          visibility: hidden !important;
        }
      `;
      document.head.appendChild(style);
    }
    observer = new MutationObserver(sample);
    observer.observe(document.body, {
      childList: true,
      characterData: true,
      subtree: true,
    });
    timer = window.setInterval(sample, 250);
    sample();
    const snapshot = (): VisibleProgressSnapshot => ({
      startedAtMs,
      endedAtMs,
      samples: samples.slice(),
    });
    win.__ctxVisibleProgressProbe = {
      getSnapshot: snapshot,
      stop: () => {
        stopped = true;
        endedAtMs = Date.now();
        observer?.disconnect();
        observer = null;
        if (timer !== null) {
          window.clearInterval(timer);
          timer = null;
        }
        return snapshot();
      },
    };
  }, faultMode);
}

async function stopVisibleProgressProbe(page: Page): Promise<VisibleProgressSnapshot> {
  const snapshot = await page.evaluate(() => {
    const win = window as RemoteDaemonLoadWindow;
    return win.__ctxVisibleProgressProbe?.stop() ?? null;
  });
  if (!snapshot) throw new Error("visible progress probe was not installed");
  return snapshot;
}

async function readWorkspaceStreamTelemetrySamples(
  page: Page,
): Promise<WorkspaceStreamTelemetrySample[]> {
  return page.evaluate(() => {
    const win = window as RemoteDaemonLoadWindow;
    return win.__ctxWorkspaceStreamTelemetrySamples?.slice() ?? [];
  });
}

async function waitForWorkspaceStreamConnected(page: Page): Promise<void> {
  await expect
    .poll(async () =>
      page.evaluate(() => {
        const win = window as RemoteDaemonLoadWindow;
        return win.__ctxE2E?.workspaceStream?.getConnectionState?.() ?? null;
      }),
    )
    .toBe("connected");
}

async function setWorkspaceStreamDrop(page: Page, drop: boolean): Promise<void> {
  await page.evaluate((nextDrop) => {
    const win = window as RemoteDaemonLoadWindow;
    win.__ctxE2E?.workspaceStream?.setDropMessages?.(nextDrop);
  }, drop);
}

async function runForcedForegroundGapRecovery(
  page: Page,
  request: APIRequestContext,
  sessionId: string,
): Promise<{
  missedMarker: string;
  triggerProbe: ProbeOutcome;
  missedBackendReadyAtMs: number;
  missedVisibleAtMs: number;
  missedBackendToVisibleMs: number;
}> {
  await waitForWorkspaceStreamConnected(page);
  const missedMarker = `remote-ui-gap-missed-${Date.now()}`;
  await setWorkspaceStreamDrop(page, true);
  const missed = await sendSessionMessage(
    request,
    sessionId,
    buildSlowPrompt(missedMarker, { bodyLines: 4, toolCount: 0 }),
    { retryBusyForMs: 5000 },
  );
  const missedCompletion = await waitForForegroundTurnCompletion(
    request,
    sessionId,
    missed.turnId,
    missed.afterSeq,
    PROBE_TIMEOUT_MS,
  );
  await setWorkspaceStreamDrop(page, false);
  const missedVisiblePromise = waitForVisibleMarker(page, missedMarker, PROBE_TIMEOUT_MS);
  const triggerMarker = `remote-ui-gap-trigger-${Date.now()}`;
  const triggerProbe = await runForegroundProbe(page, request, sessionId, triggerMarker);
  const missedVisibleAtMs = await missedVisiblePromise;
  return {
    missedMarker,
    triggerProbe,
    missedBackendReadyAtMs: missedCompletion.backendReadyAtMs,
    missedVisibleAtMs,
    missedBackendToVisibleMs: missedVisibleAtMs - missedCompletion.backendReadyAtMs,
  };
}

function summarizeVisibleCadence(snapshot: VisibleProgressSnapshot, activeUntilMs?: number | null): {
  sampleCount: number;
  maxVisibleSilenceMs: number;
  p95VisibleSilenceMs: number | null;
  gapsMs: number[];
} {
  const endedAtMs =
    typeof activeUntilMs === "number" && Number.isFinite(activeUntilMs)
      ? Math.max(snapshot.startedAtMs, Math.min(activeUntilMs, snapshot.endedAtMs))
      : snapshot.endedAtMs;
  const times = snapshot.samples
    .map((sample) => sample.at_ms)
    .filter((value) => Number.isFinite(value) && value <= endedAtMs)
    .sort((left, right) => left - right);
  const gaps: number[] = [];
  let cursor = snapshot.startedAtMs;
  for (const atMs of times) {
    if (atMs >= cursor) {
      gaps.push(atMs - cursor);
      cursor = atMs;
    }
  }
  gaps.push(Math.max(0, endedAtMs - cursor));
  return {
    sampleCount: times.length,
    maxVisibleSilenceMs: gaps.length > 0 ? Math.max(...gaps) : endedAtMs - snapshot.startedAtMs,
    p95VisibleSilenceMs: percentile(gaps, 0.95),
    gapsMs: gaps,
  };
}

function sanitizeDiagnosticContext(value: Record<string, unknown> | undefined): Record<string, unknown> | undefined {
  if (!value) return undefined;
  const sanitized: Record<string, unknown> = {};
  for (const key of ["filename", "lineno", "colno", "stack"] as const) {
    const entry = value[key];
    if (entry !== undefined) sanitized[key] = entry;
  }
  return Object.keys(sanitized).length > 0 ? sanitized : undefined;
}

function summarizeCorrectedReceiveLag(
  samples: readonly WorkspaceStreamTelemetrySample[],
  clockOffsetMs: number,
  opts?: {
    minimumEmittedAtMs?: number | null;
    streamSource?: "live" | "replay";
  },
): {
  count: number;
  filteredHistoricalCount: number;
  p50: number | null;
  p95: number | null;
  max: number | null;
} {
  const emittedSamples = samples
    .filter((sample) => typeof sample.emittedAtMs === "number")
    .filter((sample) => {
      const streamSource = opts?.streamSource;
      return !streamSource || sample.streamSource === streamSource;
    })
    .filter((sample) => {
      const minimum = opts?.minimumEmittedAtMs;
      return typeof minimum !== "number" || Number(sample.emittedAtMs) >= minimum;
    });
  const corrected = emittedSamples
    .map((sample) => sample.receivedAtMs - Number(sample.emittedAtMs) + clockOffsetMs)
    .filter((value) => Number.isFinite(value) && value >= 0);
  return {
    count: corrected.length,
    filteredHistoricalCount:
      samples.filter((sample) => {
        if (typeof sample.emittedAtMs !== "number") return false;
        const streamSource = opts?.streamSource;
        return !streamSource || sample.streamSource === streamSource;
      }).length - emittedSamples.length,
    p50: percentile(corrected, 0.5),
    p95: percentile(corrected, 0.95),
    max: corrected.length > 0 ? Math.max(...corrected) : null,
  };
}

async function stopStreamers(
  streamers: readonly BoundedWriter[],
): Promise<{ sent: number; failures: string[]; backpressure: number; stopErrors: string[] }> {
  const stopErrors: string[] = [];
  for (const streamer of streamers) {
    try {
      await streamer.stop();
    } catch (error) {
      stopErrors.push(formatUnknownError(error));
    }
  }
  const stats = streamers.map((streamer) => streamer.getStats());
  return {
    sent: stats.reduce((total, entry) => total + entry.sent, 0),
    failures: stats.flatMap((entry) => entry.failures),
    backpressure: stats.reduce((total, entry) => total + entry.backpressure, 0),
    stopErrors,
  };
}

async function installStopClickTimestampProbe(page: Page): Promise<void> {
  await page.evaluate(() => {
    const win = window as Window & {
      __ctxRemoteSoakStopClickAtMs?: number | null;
    };
    win.__ctxRemoteSoakStopClickAtMs = null;
    const listener = (event: MouseEvent): void => {
      const target = event.target;
      if (!(target instanceof Element)) return;
      const stopButton = target.closest('button[aria-label="Stop"], button[title="Stop"]');
      if (!stopButton) return;
      win.__ctxRemoteSoakStopClickAtMs = Date.now();
      document.removeEventListener("click", listener, true);
    };
    document.addEventListener("click", listener, true);
  });
}

async function readStopClickTimestamp(page: Page): Promise<number> {
  await page.waitForFunction(() => {
    const win = window as Window & {
      __ctxRemoteSoakStopClickAtMs?: number | null;
    };
    return typeof win.__ctxRemoteSoakStopClickAtMs === "number";
  });
  const clickAtMs = await page.evaluate(() => {
    const win = window as Window & {
      __ctxRemoteSoakStopClickAtMs?: number | null;
    };
    return win.__ctxRemoteSoakStopClickAtMs ?? null;
  });
  if (typeof clickAtMs !== "number" || !Number.isFinite(clickAtMs)) {
    throw new Error("Stop click timestamp probe did not observe the browser click event");
  }
  return clickAtMs;
}

test("workbench: remote daemon stream load keeps UI progress fresh", async ({
  page,
  request,
}, testInfo) => {
  test.skip(!ENABLED, "Set CTX_REMOTE_DAEMON_STREAM_SOAK=1 to run the remote daemon stream load proof.");
  test.setTimeout(TEST_TIMEOUT_MS);

  let partialMetricsCapture: Promise<void> | null = null;
  const warnPartialMetricsFailure = (error: unknown): void => {
    console.warn(
      `failed to attach remote daemon stream load partial metrics: ${formatUnknownError(error)}`,
    );
  };
  const attachPartialMetricsOnce = async (
    reason: string,
    context: Record<string, unknown>,
  ): Promise<void> => {
    if (!partialMetricsCapture) {
      partialMetricsCapture = attachPartialRemoteDaemonStreamLoadMetrics(
        testInfo,
        request,
        reason,
        context,
      ).catch((error: unknown) => {
        partialMetricsCapture = null;
        throw error;
      });
    }
    await partialMetricsCapture;
  };

  const clock = await calibrateClock(request);

  const seed = await seedDummyWorkspace(request, {
    repoRoot: DAEMON_REPO_ROOT,
    tasks: TASK_COUNT,
    sessionsPerTask: 1,
    turnsPerSession: TURNS_PER_SESSION,
    throttleMs: 1,
    messageBytes: MESSAGE_BYTES,
    messagePrefix: "remote stream fixture msg",
    includeToolSummaries: !LONG_FOREGROUND_RECOVERY,
    toolSummariesPerTurn: LONG_FOREGROUND_RECOVERY ? 0 : 3,
    seedTranscriptDirect: true,
    directSeedBatchSize: DIRECT_SEED_BATCH_SIZE,
    directSeedMaterializedTailTurns: DIRECT_SEED_MATERIALIZED_TAIL_TURNS,
  });

  const foregroundTaskId = seed.taskIds[0] ?? "";
  const foregroundSessionId = seed.sessionIdsByTask[foregroundTaskId]?.[0] ?? "";
  expect(foregroundSessionId).not.toBe("");
  const backgroundSessionIds = seed.taskIds
    .slice(1)
    .map((taskId) => seed.sessionIdsByTask[taskId]?.[0] ?? "")
    .filter((sessionId): sessionId is string => Boolean(sessionId));
  if (!LONG_FOREGROUND_RECOVERY) {
    expect(backgroundSessionIds.length).toBeGreaterThan(0);
  }

  let pageCrashed = false;
  page.on("crash", () => {
    pageCrashed = true;
  });

  await page.setViewportSize({ width: 1440, height: 960 });
  const authFragment = process.env.CTX_E2E_AUTH_TOKEN
    ? `#token=${encodeURIComponent(process.env.CTX_E2E_AUTH_TOKEN)}`
    : "";
  await page.goto(`/workspaces/${seed.workspaceId}?ctxE2E=1&loadtest=1${authFragment}`, {
    waitUntil: "domcontentloaded",
  });

  const rows = page.locator(".wb-task-row");
  const sessionView = page.locator('.wb-session-slot[aria-hidden="false"]');
  await expect(rows).toHaveCount(TASK_COUNT, { timeout: MAX_INITIAL_UI_READY_MS });
  const focused = await page.evaluate(
    ({ taskId, sessionId }) => window.__ctxE2E?.focusTask?.(taskId, sessionId) ?? false,
    { taskId: foregroundTaskId, sessionId: foregroundSessionId },
  );
  expect(focused).toBe(true);
  await expect(sessionView).toContainText(/remote stream fixture msg 1\.1\./i, {
    timeout: MAX_INITIAL_UI_READY_MS,
  });
  await expect(page.locator(".wb-session-slot textarea.wb-active-textarea")).toBeVisible({
    timeout: MAX_INITIAL_UI_READY_MS,
  });
  await clearDiagnostics(page);

  let vcsChurnController: Awaited<ReturnType<typeof startRemoteVcsChurn>> = null;
  let vcsChurnSummary = vcsChurnController?.summary ?? emptyVcsChurnSummary();
  let vcsTaskSwitch: VcsTaskSwitchSummary | null = null;
  let vcsGitPane: VcsGitPaneSummary | null = null;
  if (VCS_CHURN_ENABLED) {
    vcsGitPane = {
      openMs: null,
      firstFileVisibleMs: null,
      error: null,
    };
    try {
      const snapshotBaseline = sumMetricEntries(
        await readTelemetryMetricEntries(request, "workspace.vcs_stream.snapshot_count", 180_000),
      );
      const seededWorktree = await seedRemoteVcsInitialChange(request, foregroundSessionId);
      const snapshotCountAfterSeed = await waitForTelemetryMetricSum(
        request,
        "workspace.vcs_stream.snapshot_count",
        snapshotBaseline + 1,
        MAX_VCS_GIT_PANE_OPEN_MS,
      );
      expect(snapshotCountAfterSeed).toBeGreaterThan(snapshotBaseline);
      const openStartedAt = Date.now();
      const opened = await page.evaluate(() => window.__ctxE2E?.toggleDiffPane?.() ?? false);
      expect(opened).toBe(true);
      await expect(page.locator(".wb-right-pane.wb-diff")).toBeVisible({
        timeout: MAX_VCS_GIT_PANE_OPEN_MS,
      });
      const refreshed = await page.evaluate(
        (worktreeId) => window.__ctxE2E?.refreshVcsDetails?.(worktreeId) ?? false,
        seededWorktree.worktreeId,
      );
      expect(refreshed).toBe(true);
      await waitForBrowserWorktreeVcsSummary(
        page,
        seededWorktree.worktreeId,
        MAX_VCS_GIT_PANE_OPEN_MS,
      );
      await waitForBrowserWorktreeVcsInventoryPath(
        page,
        seededWorktree.worktreeId,
        "vcs-soak-tracked.txt",
        MAX_VCS_GIT_PANE_OPEN_MS,
      );
      await expect(page.locator(".cursor-diff-file-header").filter({ hasText: "vcs-soak-tracked.txt" })).toBeVisible({
        timeout: MAX_VCS_GIT_PANE_OPEN_MS,
      });
      vcsGitPane.openMs = Date.now() - openStartedAt;
      vcsGitPane.firstFileVisibleMs = Date.now() - openStartedAt;
    } catch (error) {
      vcsGitPane.error = formatUnknownError(error);
    }

    vcsChurnController = await startRemoteVcsChurn(request, foregroundSessionId);
    vcsChurnSummary = vcsChurnController?.summary ?? emptyVcsChurnSummary();
    vcsTaskSwitch = {
      toBackgroundMs: null,
      backToForegroundMs: null,
      error: null,
    };
    try {
      const backgroundTaskId = seed.taskIds[1] ?? "";
      const backgroundSessionId = backgroundTaskId ? seed.sessionIdsByTask[backgroundTaskId]?.[0] ?? "" : "";
      expect(backgroundTaskId).not.toBe("");
      expect(backgroundSessionId).not.toBe("");
      const toBackgroundStartedAt = Date.now();
      const switchedToBackground = await page.evaluate(
        ({ taskId, sessionId }) => window.__ctxE2E?.focusTask?.(taskId, sessionId) ?? false,
        { taskId: backgroundTaskId, sessionId: backgroundSessionId },
      );
      expect(switchedToBackground).toBe(true);
      await expect(sessionView).toContainText(/remote stream fixture msg 2\.1\./i, {
        timeout: MAX_VCS_TASK_SWITCH_MS,
      });
      vcsTaskSwitch.toBackgroundMs = Date.now() - toBackgroundStartedAt;
      const backStartedAt = Date.now();
      const switchedBack = await page.evaluate(
        ({ taskId, sessionId }) => window.__ctxE2E?.focusTask?.(taskId, sessionId) ?? false,
        { taskId: foregroundTaskId, sessionId: foregroundSessionId },
      );
      expect(switchedBack).toBe(true);
      await expect(sessionView).toContainText(/remote stream fixture msg 1\.1\./i, {
        timeout: MAX_VCS_TASK_SWITCH_MS,
      });
      vcsTaskSwitch.backToForegroundMs = Date.now() - backStartedAt;
    } catch (error) {
      vcsTaskSwitch.error = formatUnknownError(error);
    }
  }

  let visibleProgressProbeStarted = false;
  if (!LONG_FOREGROUND_RECOVERY) {
    await startVisibleProgressProbe(page, FAULT_MODE);
    visibleProgressProbeStarted = true;
  }

  const writerSessionIds = LONG_FOREGROUND_RECOVERY
    ? []
    : backgroundSessionIds.slice(0, Math.max(1, Math.min(STREAMERS, backgroundSessionIds.length)));
  const liveReceiveLagStartedAtMs = Date.now();
  const liveReceiveLagMinimumEmittedAtMs =
    liveReceiveLagStartedAtMs + clock.offsetMs - clock.uncertaintyMs - 1000;
  const streamers = startBoundedBackgroundWriters(request, writerSessionIds);

  const probes: ProbeOutcome[] = [];
  const interruptRequestTimes: number[] = [];
  const requestListener = (req: Request) => {
    if (req.url().includes(`/api/sessions/${foregroundSessionId}/interrupt`)) {
      interruptRequestTimes.push(Date.now());
    }
  };
  page.on("request", requestListener);

  let streamerStats = {
    sent: 0,
    failures: [] as string[],
    backpressure: 0,
    stopErrors: [] as string[],
  };
  let interrupt = {
    marker: "",
    error: null as string | null,
    clickAtMs: null as number | null,
    requestAtMs: null as number | null,
    pendingAtMs: null as number | null,
    terminalAtMs: null as number | null,
    terminalEventAtMs: null as number | null,
    terminalEventObservedAtMs: null as number | null,
    terminalStatus: null as string | null,
    pendingSignal: null as "dom" | "telemetry" | null,
    clickAttemptAtMs: null as number | null,
    clickDispatchLagMs: null as number | null,
    clickToRequestMs: null as number | null,
    clickToPendingMs: null as number | null,
    clickToTerminalMs: null as number | null,
    clickToTerminalEventMs: null as number | null,
    waitFinishedAtMs: null as number | null,
  };

  let forcedGapRecovery: Awaited<ReturnType<typeof runForcedForegroundGapRecovery>> | null = null;
  const partialMetricsWatchdogMs = Math.max(1000, TEST_TIMEOUT_MS - 45_000);
  const partialMetricsWatchdog = setTimeout(() => {
    void attachPartialMetricsOnce("test timeout watchdog", {
      testTimeoutMs: TEST_TIMEOUT_MS,
      watchdogMs: partialMetricsWatchdogMs,
    }).catch(warnPartialMetricsFailure);
  }, partialMetricsWatchdogMs);
  if (typeof partialMetricsWatchdog === "object" && "unref" in partialMetricsWatchdog) {
    partialMetricsWatchdog.unref();
  }
  try {
    if (LONG_FOREGROUND_RECOVERY) {
      forcedGapRecovery = await runForcedForegroundGapRecovery(
        page,
        request,
        foregroundSessionId,
      );
      probes.push(forcedGapRecovery.triggerProbe);
    }
    if (!visibleProgressProbeStarted) {
      await startVisibleProgressProbe(page, FAULT_MODE);
      visibleProgressProbeStarted = true;
    }

    for (let index = 0; index < PROBE_COUNT; index += 1) {
      const marker = `remote-ui-progress-${index + 1}-${Date.now()}`;
      const probe = await runForegroundProbe(page, request, foregroundSessionId, marker);
      probes.push(probe);
      if (probe.timedOut) {
        await attachPartialMetricsOnce("foreground probe timed out", {
          probeIndex: index + 1,
          probeCount: PROBE_COUNT,
          probe,
          probes,
        }).catch(warnPartialMetricsFailure);
        throw new Error(`foreground probe ${index + 1}/${PROBE_COUNT} timed out: ${probe.error}`);
      }
      await sleep(250);
    }

    const interruptMarker = `remote-ui-interrupt-${Date.now()}`;
    interrupt.marker = interruptMarker;
    try {
      const sent = await sendSessionMessage(
        request,
        foregroundSessionId,
        buildSlowPrompt(interruptMarker, { bodyLines: 120, toolCount: 14 }),
        { retryBusyForMs: 5000 },
      );
      const stopButton = page.getByRole("button", { name: "Stop" });
      await expect(stopButton).toBeVisible({ timeout: 20_000 });
      await stopButton.click({ trial: true, timeout: MAX_CLICK_TO_PENDING_MS + 5000 });
      await installStopClickTimestampProbe(page);
      interrupt.clickAttemptAtMs = Date.now();
      await stopButton.click();
      interrupt.clickAtMs = await readStopClickTimestamp(page);
      interrupt.clickDispatchLagMs = interrupt.clickAtMs - interrupt.clickAttemptAtMs;
      const pendingButtonVisible = await page
        .getByRole("button", { name: "Stopping..." })
        .waitFor({
          state: "visible",
          timeout: MAX_CLICK_TO_PENDING_MS + 5000,
        })
        .then(() => true)
        .catch(() => false);
      if (pendingButtonVisible) {
        interrupt.pendingSignal = "dom";
        interrupt.pendingAtMs = Date.now();
      } else {
        interrupt.pendingSignal = "telemetry";
      }
      const terminal = await waitForTerminalInterrupted(
        page,
        request,
        foregroundSessionId,
        sent.turnId,
        sent.afterSeq,
        interruptMarker,
        MAX_CLICK_TO_TERMINAL_MS + 5000,
      );
      interrupt.terminalAtMs = terminal.terminalAtMs;
      interrupt.terminalEventAtMs = terminal.terminalEventAtMs;
      interrupt.terminalEventObservedAtMs = terminal.terminalEventObservedAtMs;
      interrupt.terminalStatus = terminal.status;
    } catch (error) {
      interrupt.error = formatUnknownError(error);
    } finally {
      interrupt.waitFinishedAtMs = Date.now();
    }

    const streamDeadline = Date.now() + STREAM_TIMEOUT_MS;
    while (Date.now() < streamDeadline) {
      const streamEvents = await readTelemetryMetricEntries(
        request,
        "workbench.workspace_stream_event_count",
        180_000,
      );
      const streamEventCount = sumMetricEntries(streamEvents);
      const sessionHeadProgressCount = sessionHeadProgressEventCount(
        sumMetricEntriesByLabel(streamEvents, "event_type"),
      );
      if (
        streamEventCount >= MIN_STREAM_EVENTS &&
        sessionHeadProgressCount >= MIN_SESSION_HEAD_PROGRESS_EVENTS
      ) {
        break;
      }
      await sleep(500);
    }
    if (VCS_CHURN_ENABLED) {
      await waitForTelemetryMetricSum(
        request,
        "workspace.vcs_stream.snapshot_count",
        MIN_VCS_SNAPSHOTS,
        STREAM_TIMEOUT_MS,
      );
    }
  } catch (error) {
    await attachPartialMetricsOnce("stream load section failed before final summary", {
      error: formatUnknownError(error),
      probes,
      interrupt,
    }).catch(warnPartialMetricsFailure);
    clearTimeout(partialMetricsWatchdog);
    throw error;
  } finally {
    streamerStats = await stopStreamers(streamers);
    if (vcsChurnController) {
      vcsChurnSummary = await vcsChurnController.stop();
    }
    page.off("request", requestListener);
  }

  interrupt.requestAtMs = interruptRequestTimes[0] ?? null;
  if (interrupt.clickAtMs !== null && interrupt.requestAtMs !== null) {
    interrupt.clickToRequestMs = interrupt.requestAtMs - interrupt.clickAtMs;
  }
  if (interrupt.clickAtMs !== null && interrupt.pendingAtMs !== null) {
    interrupt.clickToPendingMs = interrupt.pendingAtMs - interrupt.clickAtMs;
  }
  if (interrupt.clickAtMs !== null && interrupt.terminalAtMs !== null) {
    interrupt.clickToTerminalMs = interrupt.terminalAtMs - interrupt.clickAtMs;
  }
  if (interrupt.clickAtMs !== null && interrupt.terminalEventAtMs !== null) {
    interrupt.clickToTerminalEventMs = interrupt.terminalEventAtMs - interrupt.clickAtMs;
  }

  if (interrupt.error === null) {
    await waitForTelemetryMetricSum(
      request,
      "workbench.interrupt_click_to_pending_ms",
      1,
      10_000,
    );
  }

  await sleep(1500);
  const visibleSnapshot = await stopVisibleProgressProbe(page);
  const streamTelemetrySamples = await readWorkspaceStreamTelemetrySamples(page);
  const visibleCadence = summarizeVisibleCadence(
    visibleSnapshot,
    interrupt.terminalAtMs ?? interrupt.waitFinishedAtMs,
  );
  const backendToDomMs = probes
    .map((probe) => probe.backendToDomMs)
    .filter((value): value is number => typeof value === "number" && Number.isFinite(value));
  const backendToDomP95SelectsMax = percentileSelectsMaximum(backendToDomMs.length, 0.95);
  const sendToFirstVisibleMs = probes
    .map((probe) => probe.sendToFirstVisibleMs)
    .filter((value): value is number => typeof value === "number" && Number.isFinite(value));
  const correctedReceiveLag = summarizeCorrectedReceiveLag(
    streamTelemetrySamples,
    clock.offsetMs,
    { minimumEmittedAtMs: liveReceiveLagMinimumEmittedAtMs, streamSource: "live" },
  );
  const correctedForegroundReceiveLag = summarizeCorrectedReceiveLag(
    streamTelemetrySamples.filter((sample) => sample.lane === "foreground"),
    clock.offsetMs,
    { minimumEmittedAtMs: liveReceiveLagMinimumEmittedAtMs, streamSource: "live" },
  );
  const foregroundReplayEventAge = summarizeCorrectedReceiveLag(
    streamTelemetrySamples.filter((sample) => sample.lane === "foreground"),
    clock.offsetMs,
    { minimumEmittedAtMs: liveReceiveLagMinimumEmittedAtMs, streamSource: "replay" },
  );
  const streamEventMetricEntries = await readTelemetryMetricEntries(
    request,
    "workbench.workspace_stream_event_count",
    180_000,
  );
  const streamEventCountsByType = sumMetricEntriesByLabel(streamEventMetricEntries, "event_type");
  const streamEventCountsByLane = sumMetricEntriesByLabel(streamEventMetricEntries, "lane");
  const telemetryMetrics = await readTelemetryMetrics(
    request,
    [
      "workbench.workspace_stream_event_count",
      "workbench.client_receive_lag_ms",
      "workbench.workspace_event_age_ms",
      "workbench.session_replica_apply_lag_ms",
      "workbench.session_replica_event_age_ms",
      "workbench.session_replica_apply_duration_ms",
      "workbench.final_ws_to_dom_ms",
      "workbench.final_ingress_to_dom_ms",
      "workbench.foreground_queue_age_ms",
      "workbench.workspace_backlog_age_ms",
      "workbench.interrupt_click_to_pending_ms",
      "workbench.foreground_gap_recovery_timeout_count",
      "workbench.workspace_stream_reset_count",
      "workbench.late_chunk_after_terminal_count",
      "workbench.projection_or_seq_regression_count",
      "workbench.gap_repair_mismatch_count",
      "workbench.switch_stale_visible_count",
      "workbench.nav_thread_activity_mismatch_count",
      "workspace.vcs_stream.snapshot_count",
      "workspace.vcs_stream.receive_lag_ms",
      "workspace.vcs_stream.server_snapshot_queued_count",
      "workspace.vcs_stream.server_snapshot_coalesced_count",
      "workspace.vcs_stream.server_message_sent_count",
      "workspace.vcs_stream.server_snapshot_sent_count",
      "workspace.stream.receiver_drain_event_count",
    ],
    180_000,
  );
  const diagnostics = await getDiagnostics(page);
  const browserLoadTest = await page.evaluate(() => {
    const win = window as Window & {
      __ctxLoadTestTelemetry?: {
        getSummary?: () => unknown;
      };
    };
    return win.__ctxLoadTestTelemetry?.getSummary?.() ?? null;
  });

  const summary = {
    run: {
      remoteMode: REMOTE_MODE,
      faultMode: FAULT_MODE || null,
      baseUrl: process.env.CTX_E2E_BASE_URL ?? null,
      workspaceId: seed.workspaceId,
      foregroundTaskId,
      foregroundSessionId,
      backgroundSessionCount: backgroundSessionIds.length,
      pageCrashed,
      scenario: SCENARIO,
      turnsPerSession: TURNS_PER_SESSION,
    },
    budgets: {
      minStreamEvents: MIN_STREAM_EVENTS,
      minSessionHeadProgressEvents: MIN_SESSION_HEAD_PROGRESS_EVENTS,
      testTimeoutMs: TEST_TIMEOUT_MS,
      headPollMs: HEAD_POLL_MS,
      maxInitialUiReadyMs: MAX_INITIAL_UI_READY_MS,
      maxClockUncertaintyMs: MAX_CLOCK_UNCERTAINTY_MS,
      maxVisibleSilenceMs: MAX_VISIBLE_SILENCE_MS,
      maxHardVisibleSilenceMs: MAX_HARD_VISIBLE_SILENCE_MS,
      maxBackendToDomP95Ms: MAX_BACKEND_TO_DOM_P95_MS,
      maxBackendToDomMs: MAX_BACKEND_TO_DOM_MS,
      maxSendToVisibleMs: MAX_SEND_TO_VISIBLE_MS,
      maxForegroundClientReceiveLagMs: MAX_FOREGROUND_CLIENT_RECEIVE_LAG_MS,
      maxAllClientReceiveLagMs: MAX_ALL_CLIENT_RECEIVE_LAG_MS,
      maxReplicaApplyLagMs: MAX_REPLICA_APPLY_LAG_MS,
      maxClickToPendingMs: MAX_CLICK_TO_PENDING_MS,
      maxClickToTerminalMs: MAX_CLICK_TO_TERMINAL_MS,
      minVcsSnapshots: MIN_VCS_SNAPSHOTS,
      maxVcsReceiveLagMs: MAX_VCS_RECEIVE_LAG_MS,
      maxVcsGitPaneOpenMs: MAX_VCS_GIT_PANE_OPEN_MS,
      maxVcsTaskSwitchMs: MAX_VCS_TASK_SWITCH_MS,
    },
    clock,
    load: {
      streamers: STREAMERS,
      intervalMs: STREAM_INTERVAL_MS,
      messageBytes: MESSAGE_BYTES,
      backgroundMessagesSent: streamerStats.sent,
      streamerFailures: streamerStats.failures,
      streamerBackpressure: streamerStats.backpressure,
      streamerStopErrors: streamerStats.stopErrors,
      streamEventCount: sumMetricEntries(streamEventMetricEntries),
      streamEventCountsByType,
      streamEventCountsByLane,
      streamTelemetrySampleCount: streamTelemetrySamples.length,
    },
    vcs: {
      churn: vcsChurnSummary,
      taskSwitch: vcsTaskSwitch,
      gitPane: vcsGitPane,
      snapshotCount: telemetryMetrics["workspace.vcs_stream.snapshot_count"]?.sum ?? 0,
      receiveLag: telemetryMetrics["workspace.vcs_stream.receive_lag_ms"] ?? metricRollupEmpty(),
      serverSnapshotQueued:
        telemetryMetrics["workspace.vcs_stream.server_snapshot_queued_count"] ?? metricRollupEmpty(),
      serverSnapshotCoalesced:
        telemetryMetrics["workspace.vcs_stream.server_snapshot_coalesced_count"] ?? metricRollupEmpty(),
      serverSnapshotSent:
        telemetryMetrics["workspace.vcs_stream.server_snapshot_sent_count"] ?? metricRollupEmpty(),
    },
    visibleProgress: {
      cadence: visibleCadence,
      samplePreview: visibleSnapshot.samples.slice(-12),
    },
    probes: {
      outcomes: probes,
      backendToDomMs,
      p50BackendToDomMs: percentile(backendToDomMs, 0.5),
      p95BackendToDomMs: percentile(backendToDomMs, 0.95),
      p95BackendToDomBudgetApplies: !backendToDomP95SelectsMax,
      minBackendToDomP95SampleCount: minSamplesForDistinctPercentile(0.95),
      maxBackendToDomMs: backendToDomMs.length > 0 ? Math.max(...backendToDomMs) : null,
      sendToFirstVisibleMs,
      p95SendToFirstVisibleMs: percentile(sendToFirstVisibleMs, 0.95),
      maxSendToFirstVisibleMs:
        sendToFirstVisibleMs.length > 0 ? Math.max(...sendToFirstVisibleMs) : null,
    },
    forcedGapRecovery,
    receiveLag: {
      liveWindow: {
        browserStartedAtMs: liveReceiveLagStartedAtMs,
        daemonMinimumEmittedAtMs: liveReceiveLagMinimumEmittedAtMs,
      },
      correctedLiveAll: correctedReceiveLag,
      correctedLiveForeground: correctedForegroundReceiveLag,
      foregroundReplayEventAge,
      telemetry: telemetryMetrics["workbench.client_receive_lag_ms"] ?? metricRollupEmpty(),
      eventAgeTelemetry: telemetryMetrics["workbench.workspace_event_age_ms"] ?? metricRollupEmpty(),
    },
    interrupt,
    telemetryMetrics,
    diagnostics: diagnostics.map((entry) => ({
      source: entry.source,
      code: entry.code,
      severity: entry.severity,
      message: entry.message,
      context: sanitizeDiagnosticContext(entry.context),
    })),
    browserLoadTest,
  };

  await testInfo.attach("remote-daemon-stream-load-metrics.json", {
    body: JSON.stringify(summary, null, 2),
    contentType: "application/json",
  });
  await fs.writeFile(
    testInfo.outputPath("remote-daemon-stream-load-metrics.json"),
    JSON.stringify(summary, null, 2),
    "utf8",
  );
  console.log(`remote daemon stream load summary: ${JSON.stringify(summary)}`);
  clearTimeout(partialMetricsWatchdog);

  expect(clock.uncertaintyMs).toBeLessThanOrEqual(MAX_CLOCK_UNCERTAINTY_MS);
  expect(sumMetricEntries(streamEventMetricEntries)).toBeGreaterThanOrEqual(MIN_STREAM_EVENTS);
  expect(sessionHeadProgressEventCount(streamEventCountsByType)).toBeGreaterThanOrEqual(
    MIN_SESSION_HEAD_PROGRESS_EVENTS,
  );
  expect(streamEventCountsByType.worktree_vcs_snapshot ?? 0).toBe(0);
  expect(streamEventCountsByLane.foreground ?? 0).toBeGreaterThan(0);
  expect(streamerStats.failures).toEqual([]);
  expect(streamerStats.stopErrors).toEqual([]);
  expect(pageCrashed).toBe(false);
  const unexpectedDiagnostics = diagnostics.filter((entry) => entry.severity === "error");
  expect(
    unexpectedDiagnostics,
    `Unexpected browser diagnostics:\n${JSON.stringify(unexpectedDiagnostics, null, 2)}`,
  ).toEqual([]);
  expect(visibleCadence.sampleCount).toBeGreaterThan(0);
  expect(visibleCadence.p95VisibleSilenceMs ?? Infinity).toBeLessThanOrEqual(MAX_VISIBLE_SILENCE_MS);
  expect(visibleCadence.maxVisibleSilenceMs).toBeLessThanOrEqual(MAX_HARD_VISIBLE_SILENCE_MS);
  expect(probes.every((probe) => !probe.timedOut)).toBe(true);
  const expectedProbeCount = PROBE_COUNT + (forcedGapRecovery ? 1 : 0);
  expect(backendToDomMs.length).toBe(expectedProbeCount);
  expect(sendToFirstVisibleMs.length).toBe(probes.length);
  expect(sendToFirstVisibleMs.length > 0 ? Math.max(...sendToFirstVisibleMs) : Infinity).toBeLessThanOrEqual(
    MAX_SEND_TO_VISIBLE_MS,
  );
  if (!backendToDomP95SelectsMax) {
    expect(percentile(backendToDomMs, 0.95) ?? Infinity).toBeLessThanOrEqual(
      MAX_BACKEND_TO_DOM_P95_MS,
    );
  }
  expect(backendToDomMs.length > 0 ? Math.max(...backendToDomMs) : Infinity).toBeLessThanOrEqual(
    MAX_BACKEND_TO_DOM_MS,
  );
  if (forcedGapRecovery) {
    expect(forcedGapRecovery.missedBackendToVisibleMs).toBeLessThanOrEqual(MAX_BACKEND_TO_DOM_MS);
  }
  expect(telemetryMetrics["workbench.client_receive_lag_ms"]?.count ?? 0).toBeGreaterThan(0);
  expect(correctedReceiveLag.count).toBeGreaterThan(0);
  expect(correctedReceiveLag.p95 ?? Infinity).toBeLessThanOrEqual(MAX_ALL_CLIENT_RECEIVE_LAG_MS);
  expect(correctedForegroundReceiveLag.count).toBeGreaterThan(0);
  expect(correctedForegroundReceiveLag.p95 ?? Infinity).toBeLessThanOrEqual(
    MAX_FOREGROUND_CLIENT_RECEIVE_LAG_MS,
  );
  expect(telemetryMetrics["workbench.final_ws_to_dom_ms"]?.count ?? 0).toBeGreaterThan(0);
  expect(telemetryMetrics["workbench.final_ws_to_dom_ms"]?.p95 ?? Infinity).toBeLessThanOrEqual(
    MAX_BACKEND_TO_DOM_MS,
  );
  expect(telemetryMetrics["workbench.session_replica_apply_lag_ms"]?.count ?? 0).toBeGreaterThan(0);
  expect(telemetryMetrics["workbench.session_replica_apply_lag_ms"]?.p95 ?? Infinity).toBeLessThanOrEqual(
    MAX_REPLICA_APPLY_LAG_MS,
  );
  expect(telemetryMetrics["workbench.session_replica_apply_duration_ms"]?.count ?? 0).toBeGreaterThan(0);
  if (VCS_CHURN_ENABLED) {
    expect(vcsChurnSummary.error).toBeNull();
    expect(vcsChurnSummary.exitCode).toBe(0);
    expect(vcsTaskSwitch?.error ?? null).toBeNull();
    expect(vcsTaskSwitch?.toBackgroundMs ?? Infinity).toBeLessThanOrEqual(MAX_VCS_TASK_SWITCH_MS);
    expect(vcsTaskSwitch?.backToForegroundMs ?? Infinity).toBeLessThanOrEqual(MAX_VCS_TASK_SWITCH_MS);
    expect(vcsGitPane?.error ?? null).toBeNull();
    expect(vcsGitPane?.openMs ?? Infinity).toBeLessThanOrEqual(MAX_VCS_GIT_PANE_OPEN_MS);
    expect(vcsGitPane?.firstFileVisibleMs ?? Infinity).toBeLessThanOrEqual(MAX_VCS_GIT_PANE_OPEN_MS);
    expect(telemetryMetrics["workspace.vcs_stream.snapshot_count"]?.sum ?? 0).toBeGreaterThanOrEqual(
      MIN_VCS_SNAPSHOTS,
    );
    expect(telemetryMetrics["workspace.vcs_stream.receive_lag_ms"]?.count ?? 0).toBeGreaterThan(0);
    expect(telemetryMetrics["workspace.vcs_stream.receive_lag_ms"]?.p95 ?? Infinity).toBeLessThanOrEqual(
      MAX_VCS_RECEIVE_LAG_MS,
    );
  }
  expect(interrupt.error).toBeNull();
  const interruptPendingMetric =
    telemetryMetrics["workbench.interrupt_click_to_pending_ms"] ?? metricRollupEmpty();
  expect(interruptPendingMetric.count).toBeGreaterThan(0);
  expect(interruptPendingMetric.p95 ?? Infinity).toBeLessThanOrEqual(MAX_CLICK_TO_PENDING_MS);
  expect(interrupt.clickToRequestMs ?? Infinity).toBeLessThanOrEqual(MAX_CLICK_TO_PENDING_MS);
  if (interrupt.clickToPendingMs !== null) {
    expect(interrupt.clickToPendingMs).toBeLessThanOrEqual(MAX_CLICK_TO_PENDING_MS);
  }
  expect(interrupt.clickToTerminalMs ?? Infinity).toBeLessThanOrEqual(MAX_CLICK_TO_TERMINAL_MS);
  expect(
    ["interrupted", "cancelled", "canceled"].includes(String(interrupt.terminalStatus ?? "")),
  ).toBe(true);
  expect(telemetryMetrics["workbench.foreground_gap_recovery_timeout_count"]?.count ?? 0).toBe(0);
  expect(telemetryMetrics["workbench.late_chunk_after_terminal_count"]?.count ?? 0).toBe(0);
  expect(telemetryMetrics["workbench.gap_repair_mismatch_count"]?.count ?? 0).toBe(0);
  expect(telemetryMetrics["workbench.switch_stale_visible_count"]?.count ?? 0).toBe(0);
});
