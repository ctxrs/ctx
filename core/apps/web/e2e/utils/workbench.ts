import { expect } from "playwright/test";
import fs from "fs";
import path from "path";
import type { APIRequestContext, Page } from "playwright/test";

const AUTH_TOKEN = process.env.CTX_E2E_AUTH_TOKEN ?? "ctx-e2e-auth-token";

type CreateWorkspaceArgs = {
  page: Page;
  request: APIRequestContext;
  repo: string;
  workspaceName: string;
  token?: string;
  debug?: boolean;
};

const readId = (v: unknown): string => (typeof v === "string" ? v : "");

type WorkspaceSummary = {
  id?: unknown;
};

const normalizePath = (value: string): string => {
  if (!value) return "";
  try {
    return fs.realpathSync(value);
  } catch {
    return path.resolve(value);
  }
};

export async function createWorkspaceAndOpenWorkbench(opts: CreateWorkspaceArgs): Promise<string> {
  const { page, request, repo, workspaceName, token, debug = false } = opts;
  const repoPath = normalizePath(repo);
  const authToken = token ?? AUTH_TOKEN;
  const headers = authToken ? { authorization: `Bearer ${authToken}` } : undefined;
  const createResp = await request.post("/api/workspaces", {
    headers,
    data: {
      root_path: repoPath,
      name: workspaceName,
    },
  });
  expect(createResp.ok(), `failed to create workspace for ${repoPath}: ${createResp.status()}`).toBeTruthy();
  const created = (await createResp.json()) as WorkspaceSummary;
  const workspaceId = readId(created.id);
  expect(workspaceId).not.toBe("");

  const query = new URLSearchParams();
  if (debug) query.set("debug", "1");
  const fragment = authToken ? `#token=${encodeURIComponent(authToken)}` : "";
  const workspaceUrl = query.size > 0
    ? `/workspaces/${workspaceId}?${query.toString()}${fragment}`
    : `/workspaces/${workspaceId}${fragment}`;
  await page.goto(workspaceUrl, { waitUntil: "domcontentloaded" });
  await expect(page).toHaveURL(new RegExp(`/workspaces/${workspaceId}(\\?.*)?$`), { timeout: 20_000 });
  await expect(page.locator(".wb-main")).toBeVisible({ timeout: 20_000 });

  return workspaceId;
}

type WorkbenchProjectionDebugEntry = {
  source?: string;
  sessionProjectionReady?: boolean;
  freshness?: string | null;
  lastTurnStatus?: string | null;
};

type WorkbenchDebugWindow = Window & {
  __wbSessionThreadProjectionDebug?: {
    entries?: WorkbenchProjectionDebugEntry[];
  };
};

export async function waitForWorkbenchProjectionReady(
  page: Page,
  {
    requireActiveTurn = false,
    requireAuthoritative = false,
    timeout = 20_000,
  }: {
    requireActiveTurn?: boolean;
    requireAuthoritative?: boolean;
    timeout?: number;
  } = {},
): Promise<void> {
  await expect
    .poll(
      async () =>
        page.evaluate(({ requireActiveTurn: requireActiveTurnValue, requireAuthoritative: requireAuthoritativeValue }) => {
          const store = (window as WorkbenchDebugWindow).__wbSessionThreadProjectionDebug;
          const entries = Array.isArray(store?.entries) ? store.entries : [];
          for (let index = entries.length - 1; index >= 0; index -= 1) {
            const entry = entries[index];
            if (entry?.source !== "supervisor") continue;
            if (!entry?.sessionProjectionReady) continue;
            const freshness = String(entry?.freshness ?? "");
            if (requireAuthoritativeValue) {
              if (freshness !== "authoritative") continue;
            } else if (freshness !== "authoritative" && freshness !== "replica") {
              continue;
            }
            if (requireActiveTurnValue) {
              const status = String(entry?.lastTurnStatus ?? "").toLowerCase();
              if (!/(running|queued)/.test(status)) continue;
            }
            return true;
          }
          return false;
        }, { requireActiveTurn, requireAuthoritative }),
      {
        timeout,
        intervals: [200, 400, 800, 1200],
      },
    )
    .toBe(true);
}
