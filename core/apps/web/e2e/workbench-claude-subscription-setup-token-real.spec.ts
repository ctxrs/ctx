import { execSync } from "child_process";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import type { APIRequestContext } from "playwright/test";
import { test, expect } from "./fixtures";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";
import {
  asArray,
  asRecord,
  ensureProviderInstalledAndHealthy,
  readString,
  verifyProviderForWorkspace,
  waitForSessionWorkspaceFileContents,
  waitForTerminalState,
} from "../src/testing/providerRuntime";
import {
  completeClaudeManagedSetupTokenWithGoogleBrowserCredentials,
} from "./utils/providerBrowserAuth";
import { selectHarnessBySearch } from "./utils/harnessEndpointAuth";

const REQUEST_TIMEOUT_MS = 60_000;
const INSTALL_TARGET = "host" as const;
const LOGIN_POLL_MS = 1_000;
const LOGIN_TIMEOUT_MS = 10 * 60_000;

const getClaudeLoginStatus = async (
  request: APIRequestContext,
  loginId: string,
): Promise<Record<string, unknown>> => {
  const response = await request.get(`/api/providers/claude-crp/accounts/login/${encodeURIComponent(loginId)}`, {
    timeout: REQUEST_TIMEOUT_MS,
  });
  expect(response.ok(), `failed to read Claude login ${loginId} (${response.status()})`).toBe(true);
  return asRecord(await response.json());
};

const claudeAuthUrlLooksComplete = (rawAuthUrl: string): boolean => {
  try {
    const authUrl = new URL(rawAuthUrl);
    const redirectUri = authUrl.searchParams.get("redirect_uri");
    if (!redirectUri) {
      return true;
    }
    const redirect = new URL(redirectUri);
    const hostname = redirect.hostname.toLowerCase();
    if (redirect.protocol !== "http:") {
      return false;
    }
    if (hostname !== "localhost" && hostname !== "127.0.0.1" && hostname !== "::1" && hostname !== "[::1]") {
      return false;
    }
    return redirect.port.length > 0;
  } catch {
    return false;
  }
};

const waitForClaudeLoginAuthUrl = async (
  request: APIRequestContext,
  loginId: string,
): Promise<string> => {
  const startedAt = Date.now();
  while (Date.now() - startedAt <= LOGIN_TIMEOUT_MS) {
    const status = await getClaudeLoginStatus(request, loginId);
    const authUrl = readString(status.auth_url);
    if (authUrl && claudeAuthUrlLooksComplete(authUrl)) {
      return authUrl;
    }
    const normalizedStatus = readString(status.status).toLowerCase();
    if (normalizedStatus === "failed" || normalizedStatus === "timeout") {
      throw new Error(`Claude login ${normalizedStatus}: ${readString(status.error) || JSON.stringify(status)}`);
    }
    await pageWait(LOGIN_POLL_MS);
  }
  throw new Error(`timed out waiting for Claude auth url for login ${loginId}`);
};

const waitForClaudeLoginSuccess = async (
  request: APIRequestContext,
  loginId: string,
): Promise<void> => {
  const startedAt = Date.now();
  while (Date.now() - startedAt <= LOGIN_TIMEOUT_MS) {
    const status = await getClaudeLoginStatus(request, loginId);
    const normalizedStatus = readString(status.status).toLowerCase();
    if (normalizedStatus === "success") {
      return;
    }
    if (normalizedStatus === "failed" || normalizedStatus === "timeout") {
      throw new Error(`Claude login ${normalizedStatus}: ${readString(status.error) || JSON.stringify(status)}`);
    }
    await pageWait(LOGIN_POLL_MS);
  }
  throw new Error(`timed out waiting for Claude login success for ${loginId}`);
};

const pageWait = async (ms: number): Promise<void> => {
  await new Promise((resolve) => setTimeout(resolve, ms));
};

const listClaudeAccounts = async (request: APIRequestContext): Promise<Record<string, unknown>> => {
  const response = await request.get("/api/providers/claude-crp/accounts", { timeout: REQUEST_TIMEOUT_MS });
  expect(response.ok(), `failed to list Claude accounts (${response.status()})`).toBe(true);
  return asRecord(await response.json());
};

const clearClaudeAccounts = async (request: APIRequestContext): Promise<void> => {
  const payload = await listClaudeAccounts(request);
  for (const account of asArray(payload.accounts).map((entry) => asRecord(entry))) {
    const accountId = readString(account.id);
    if (!accountId) continue;
    const response = await request.delete(`/api/providers/claude-crp/accounts/${encodeURIComponent(accountId)}`, {
      timeout: REQUEST_TIMEOUT_MS,
    });
    expect(response.ok(), `failed to delete Claude account ${accountId} (${response.status()})`).toBe(true);
  }
};

test("workbench: claude setup-token subscription auth can run a real task", async ({ page, request }) => {
  test.setTimeout(20 * 60_000);

  if ((process.env.CTX_E2E_TIER ?? "") !== "provider-browser-auth") {
    test.skip(true, "set CTX_E2E_TIER=provider-browser-auth to run claude setup-token e2e");
  }
  const googleEmail = (process.env.GOOGLE_TEST_EMAIL ?? "").trim();
  const googlePassword = process.env.GOOGLE_TEST_PASSWORD ?? "";
  if (!googleEmail || !googlePassword) {
    test.skip(true, "missing GOOGLE_TEST_EMAIL / GOOGLE_TEST_PASSWORD");
  }

  await ensureProviderInstalledAndHealthy(request, "claude-crp", INSTALL_TARGET, {
    timeoutMs: 10 * 60_000,
    pollMs: 2_000,
    requestTimeoutMs: REQUEST_TIMEOUT_MS,
  });

  await clearClaudeAccounts(request);

  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-claude-setup-token-"));
  execSync("git init -b main", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "README.md"), "claude setup token e2e\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceId = await createWorkspaceAndOpenWorkbench({
    page,
    request,
    repo,
    workspaceName: `claude-setup-token-${Date.now()}`,
  });

  const harnessButton = page
    .locator(
      ".wb-new-composer-stack .wb-switcher-harness, .wb-new-composer-stack button[title='Agents'], .wb-new-composer-stack button[title='Harness']",
    )
    .first();
  await expect(harnessButton).toBeVisible({ timeout: 20_000 });
  await harnessButton.click();
  const menu = page.locator(".wb-harness-menu");
  await expect(menu).toBeVisible({ timeout: 10_000 });
  await menu.getByLabel("Search agents").fill("claude");
  const rowButton = menu
    .locator(".wb-harness-row .wb-harness-row-main")
    .filter({ hasText: /claude code/i })
    .first();
  await expect(rowButton).toBeVisible({ timeout: 20_000 });
  const loginStartResponsePromise = page.waitForResponse((response) =>
    response.request().method() === "POST"
    && response.url().endsWith("/api/providers/claude-crp/accounts/login/start"),
  );
  await rowButton.click();

  const modal = page.locator(".settings-harness-modal");
  await expect(modal).toBeVisible({ timeout: 10_000 });
  const subscriptionChoice = modal.getByRole("button", { name: /^Subscription$/i }).first();
  const subscriptionChoiceVisible = await subscriptionChoice
    .isVisible()
    .catch(() => false);
  if (subscriptionChoiceVisible) {
    await subscriptionChoice.click();
  }
  const loginStartResponse = await loginStartResponsePromise;
  expect(loginStartResponse.ok(), `Claude login start failed (${loginStartResponse.status()})`).toBe(true);
  const loginStartPayload = asRecord(await loginStartResponse.json());
  const loginId = readString(loginStartPayload.login_id);
  expect(loginId).not.toBe("");
  await expect(modal.getByText("Setup token (recommended)")).toBeVisible({ timeout: 10_000 });

  const authUrl = await waitForClaudeLoginAuthUrl(request, loginId);
  await completeClaudeManagedSetupTokenWithGoogleBrowserCredentials({
    context: page.context(),
    authUrl,
    email: googleEmail,
    password: googlePassword,
    timeoutMs: 10 * 60_000,
  });
  await waitForClaudeLoginSuccess(request, loginId);
  await expect(modal).toBeHidden({ timeout: 20_000 });

  const accountsPayload = await listClaudeAccounts(request);
  const activeAccountId = readString(accountsPayload.active_account_id);
  expect(activeAccountId).not.toBe("");
  const activeAccount = asArray(accountsPayload.accounts)
    .map((entry) => asRecord(entry))
    .find((entry) => readString(entry.id) === activeAccountId);
  expect(readString(activeAccount?.kind)).toBe("setup_token");
  await verifyProviderForWorkspace(request, workspaceId, "claude-crp", {
    timeoutMs: 90_000,
    pollMs: 3_000,
    requestTimeoutMs: REQUEST_TIMEOUT_MS,
  });

  const prompt = [
    "This is an end to end test, so it is very important that you do exactly what I ask.",
    "Make a new file in the workspace root called hello.md and put exactly this text in it: hi. The file must contain exactly those two characters with no trailing newline or extra whitespace. If you use a shell command to write the file, use printf rather than echo -n, because echo -n is not portable and may write the literal text -n.",
    "Use only the current worktree root as the target directory. Do not write in a parent directory, and if your first attempt adds a trailing newline or uses the wrong directory, fix the file before replying.",
    "That is all. Do it now without further deliberation.",
    "After writing the file, reply with exactly: hi",
  ].join(" ");

  await selectHarnessBySearch(page, "claude", /claude code/i);
  await page.locator("textarea.wb-composer-textarea").first().fill(prompt);

  const createTaskResponsePromise = page.waitForResponse((response) =>
    response.request().method() === "POST"
    && /\/api\/workspaces\/[^/]+\/tasks$/.test(response.url()),
  );
  await page.getByRole("button", { name: "Send" }).click();

  const createTaskResp = await createTaskResponsePromise;
  expect(createTaskResp.ok(), `task create failed (${createTaskResp.status()})`).toBe(true);
  const sessionId = String(asRecord(await createTaskResp.json()).primary_session_id ?? "");
  expect(sessionId).not.toBe("");

  await expect(page.getByText("Failed to start")).toHaveCount(0);
  await expect(page.getByText("Session not found in workspace snapshot")).toHaveCount(0);
  await expect(page.locator(".wb-session-slot textarea.wb-active-textarea")).toBeVisible({ timeout: 30_000 });

  const terminal = await waitForTerminalState(request, sessionId, {
    timeoutMs: 180_000,
    pollMs: 3_000,
    requestTimeoutMs: REQUEST_TIMEOUT_MS,
  });
  expect(terminal.terminalStatus, terminal.errorMessage ?? "claude run did not complete").toBe("completed");
  expect(terminal.assistantMessages).toBeGreaterThan(0);
  await expect(page.locator(".wb-session-slot .wb-assistant-entry").filter({ hasText: "hi" })).toBeVisible({
    timeout: 180_000,
  });
  await waitForSessionWorkspaceFileContents(request, sessionId, "hello.md", "hi", {
    timeoutMs: 30_000,
    pollMs: 1_000,
  });
});
