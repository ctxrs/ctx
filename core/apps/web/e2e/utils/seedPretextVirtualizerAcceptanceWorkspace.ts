import { execSync } from "child_process";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import type { APIRequestContext } from "playwright/test";

type AcceptanceTaskKey = "warm" | "long" | "short" | "rehit";

type AcceptanceTaskTitles = Record<AcceptanceTaskKey, string>;

type AcceptanceTaskSeed = {
  title: string;
  turns: number;
  messageBytes: number;
  messagePrefix: string;
  includeToolSummaries: boolean;
  toolSummariesPerTurn: number;
  throttleMs: number;
  firstMessage?: string;
};

type AcceptanceWorkspaceTarget = {
  mode: "manual" | "seeded";
  workspacePath: string;
  earliestText: string;
  titles: AcceptanceTaskTitles;
};

const DEFAULT_EARLIEST_TEXT = "Yeah, I got this notice";
const DEFAULT_TITLES: AcceptanceTaskTitles = {
  warm: "Virtualization Scrolling Performance",
  long: "Security Notice Mac",
  short: "Greeting",
  rehit: "Debug Scroll Jitter",
};

const DEFAULT_TOOL_FIXTURES = [
  { kind: "execute", title: "Run pwd", input: { command: "pwd" } },
  { kind: "search", title: "Searched context", input: { query: "pretextVirtualizer" } },
  { kind: "execute", title: "Inspect thread list", input: { command: "rg -n pretextVirtualizer core/apps/web/src" } },
  { kind: "read", title: "Read session view", input: { path: "core/apps/web/src/pages/SessionPage.view.tsx" } },
];

let cachedWorkspace: Promise<AcceptanceWorkspaceTarget> | null = null;
const SESSION_COMPLETION_TIMEOUT_MS = 180_000;
const SESSION_COMPLETION_POLL_MS = 250;

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

const parseTitle = (value: string | undefined, fallback: string) => {
  const normalized = String(value ?? "").trim();
  return normalized.length > 0 ? normalized : fallback;
};

const resolveTitles = (): AcceptanceTaskTitles => ({
  warm: parseTitle(process.env.PRETEXT_VIRTUALIZER_SWITCH_WARM_TASK_TITLE, DEFAULT_TITLES.warm),
  long: parseTitle(
    process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_TASK_TITLE ?? process.env.PRETEXT_VIRTUALIZER_SWITCH_TARGET_TASK_TITLE,
    DEFAULT_TITLES.long,
  ),
  short: parseTitle(process.env.PRETEXT_VIRTUALIZER_SHORT_THREAD_TASK_TITLE, DEFAULT_TITLES.short),
  rehit: parseTitle(process.env.PRETEXT_VIRTUALIZER_BOTTOM_REHIT_TASK_TITLE, DEFAULT_TITLES.rehit),
});

const resolveEarliestText = () => {
  const normalized = String(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_EARLIEST_TEXT ?? "").trim();
  return normalized.length > 0 ? normalized : DEFAULT_EARLIEST_TEXT;
};

const buildToolMarker = (fixtures: Array<{ kind: string; title: string; input: unknown }>) => {
  if (fixtures.length === 0) return "";
  return `\n[[tool_calls]]\n${JSON.stringify(fixtures)}\n[[/tool_calls]]`;
};

const buildPaddedMessage = (base: string, targetBytes: number) => {
  if (targetBytes <= base.length) return base;
  const padding = targetBytes - base.length;
  if (padding === 1) return `${base} `;
  return `${base} ${"x".repeat(padding - 1)}`;
};

const chunkFixtures = (count: number, offset: number) => {
  if (count <= 0) return [] as typeof DEFAULT_TOOL_FIXTURES;
  const fixtures = [];
  for (let index = 0; index < count; index += 1) {
    fixtures.push(DEFAULT_TOOL_FIXTURES[(offset + index) % DEFAULT_TOOL_FIXTURES.length]);
  }
  return fixtures;
};

const initRepo = () => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-pretext-virtualizer-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "README.md"), "pretextVirtualizer acceptance fixture\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });
  return repo;
};

async function apiPost<T>(request: APIRequestContext, url: string, data: unknown): Promise<T> {
  const response = await request.post(url, { data });
  if (!response.ok()) {
    throw new Error(`pretextVirtualizer acceptance seed failed: POST ${url} (${response.status()})`);
  }
  return (await response.json()) as T;
}

const asRecord = (value: unknown): Record<string, unknown> =>
  value && typeof value === "object" ? (value as Record<string, unknown>) : {};

const asArray = (value: unknown): unknown[] => (Array.isArray(value) ? value : []);

const isTerminalStatus = (value: unknown) => {
  const normalized = String(value ?? "").trim().toLowerCase();
  return normalized.length > 0 && !["running", "queued"].includes(normalized);
};

async function waitForSessionCompletion(request: APIRequestContext, sessionId: string): Promise<void> {
  const startedAt = Date.now();
  let lastObservedStatus = "unknown";
  while (Date.now() - startedAt < SESSION_COMPLETION_TIMEOUT_MS) {
    const response = await request.get(`/api/sessions/${sessionId}/snapshot?include_events=1&limit=60`);
    if (response.ok()) {
      const data = asRecord(await response.json());
      const summary = asRecord(data.summary);
      const summaryActivity = asRecord(summary.activity);
      const head = asRecord(data.head);
      const headActivity = asRecord(head.activity);
      const turns = asArray(head.turns).map((turn) => asRecord(turn));
      const lastTurn = turns[turns.length - 1] ?? {};
      const statuses = [
        summaryActivity.last_turn_status,
        headActivity.last_turn_status,
        (lastTurn as Record<string, unknown>).status,
      ];
      const terminalStatus = statuses.find(isTerminalStatus);
      if (terminalStatus != null) {
        return;
      }
      lastObservedStatus = statuses
        .map((status) => String(status ?? "").trim())
        .filter((status) => status.length > 0)
        .join(" / ") || "pending";
    } else {
      lastObservedStatus = `snapshot ${response.status()}`;
    }
    await sleep(SESSION_COMPLETION_POLL_MS);
  }
  throw new Error(
    `pretextVirtualizer acceptance seed timeout waiting for session ${sessionId} completion (last status: ${lastObservedStatus})`,
  );
}

async function seedTask(
  request: APIRequestContext,
  workspaceId: string,
  seed: AcceptanceTaskSeed,
) {
  const task = await apiPost<{ id: string; primary_session_id?: string | null }>(request, `/api/workspaces/${workspaceId}/tasks`, {
    title: seed.title,
    default_session: {
      provider_id: "fake",
      model_id: "fake-model",
      execution_environment: "host",
    },
  });
  const session = { id: task.primary_session_id };
  if (!session.id) throw new Error(`seeded task ${task.id} did not include a primary session`);

  for (let turnIndex = 0; turnIndex < seed.turns; turnIndex += 1) {
    const toolFixtures = seed.includeToolSummaries
      ? chunkFixtures(seed.toolSummariesPerTurn, turnIndex * seed.toolSummariesPerTurn)
      : [];
    const toolMarker = seed.includeToolSummaries ? buildToolMarker(toolFixtures) : "";
    const baseMessage =
      turnIndex === 0 && seed.firstMessage
        ? seed.firstMessage
        : `${seed.messagePrefix} ${turnIndex + 1}`;
    const paddedMessage = buildPaddedMessage(baseMessage, seed.messageBytes);
    await apiPost(request, `/api/sessions/${session.id}/messages`, {
      content: `${paddedMessage}${toolMarker}`,
      delivery: "immediate",
    });
    if (seed.throttleMs > 0) {
      await sleep(seed.throttleMs);
    }
  }
  await waitForSessionCompletion(request, session.id);
}

async function createSeededWorkspace(request: APIRequestContext): Promise<AcceptanceWorkspaceTarget> {
  const titles = resolveTitles();
  const earliestText = resolveEarliestText();
  const repoRoot = initRepo();
  const workspace = await apiPost<{ id: string }>(request, "/api/workspaces", {
    root_path: repoRoot,
    name: `pretext-virtualizer-acceptance-${Date.now()}`,
  });

  const taskSeeds: Record<AcceptanceTaskKey, AcceptanceTaskSeed> = {
    warm: {
      title: titles.warm,
      turns: 18,
      messageBytes: 768,
      messagePrefix: "warm transcript",
      includeToolSummaries: true,
      toolSummariesPerTurn: 2,
      throttleMs: 15,
    },
    long: {
      title: titles.long,
      turns: 28,
      messageBytes: 1024,
      messagePrefix: "security notice transcript",
      includeToolSummaries: true,
      toolSummariesPerTurn: 2,
      throttleMs: 15,
      firstMessage: `${earliestText} about the security notice thread`,
    },
    short: {
      title: titles.short,
      turns: 4,
      messageBytes: 96,
      messagePrefix: "greeting thread",
      includeToolSummaries: false,
      toolSummariesPerTurn: 0,
      throttleMs: 10,
      firstMessage: "Hello there from the short fixture thread",
    },
    rehit: {
      title: titles.rehit,
      turns: 36,
      messageBytes: 1024,
      messagePrefix: "bottom rehit transcript",
      includeToolSummaries: true,
      toolSummariesPerTurn: 2,
      throttleMs: 15,
    },
  };

  for (const key of ["warm", "long", "short", "rehit"] as const) {
    await seedTask(request, workspace.id, taskSeeds[key]);
  }

  return {
    mode: "seeded",
    workspacePath: `/workspaces/${workspace.id}`,
    earliestText,
    titles,
  };
}

export async function resolveAnchorstreamAcceptanceWorkspace(
  request: APIRequestContext,
): Promise<AcceptanceWorkspaceTarget> {
  const manualWorkspacePath = String(process.env.PRETEXT_VIRTUALIZER_ACCEPTANCE_WORKSPACE_PATH ?? "").trim();
  if (manualWorkspacePath.length > 0) {
    return {
      mode: "manual",
      workspacePath: manualWorkspacePath,
      earliestText: resolveEarliestText(),
      titles: resolveTitles(),
    };
  }

  cachedWorkspace ??= createSeededWorkspace(request);
  return cachedWorkspace;
}
