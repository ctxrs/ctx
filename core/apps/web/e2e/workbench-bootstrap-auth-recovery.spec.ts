import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";

type CursorAccountsResponse = {
  active_account_id: string | null;
  accounts: Array<{
    id: string;
    label: string;
    kind: string;
    email: string | null;
    created_at: string;
    last_used_at: string | null;
  }>;
};

const initRepo = () => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init -b main", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "README.md"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });
  return repo;
};

const buildBootstrapPayload = (
  workspaceId: string,
  cursorAuthed: boolean,
  cursorAccounts: CursorAccountsResponse,
) => ({
  providers: [
    {
      provider_id: "codex",
      installed: true,
      health: "ok",
      diagnostics: [],
      details: {},
      usability: {
        usable: true,
        status: "ready",
        reason_code: null,
        reason: null,
        blocking_provider_ids: [],
        recommended_action: "none",
      },
    },
    {
      provider_id: "cursor",
      installed: true,
      health: "ok",
      diagnostics: [],
      details: {},
      usability: {
        usable: true,
        status: "ready",
        reason_code: null,
        reason: null,
        blocking_provider_ids: [],
        recommended_action: "none",
      },
    },
  ],
  provider_options: {
    codex: {
      provider_id: "codex",
      workspace_id: workspaceId,
      supports_load: false,
      auth_required: false,
      has_active_auth: true,
      auth_mode: "subscription",
      probed_at: new Date().toISOString(),
    },
    cursor: {
      provider_id: "cursor",
      workspace_id: workspaceId,
      supports_load: false,
      auth_required: false,
      has_active_auth: cursorAuthed,
      auth_mode: cursorAuthed ? "subscription" : "none",
      probed_at: new Date().toISOString(),
    },
  },
  provider_harness_config: {
    codex: {
      provider_id: "codex",
      selected_source_kind: "subscription",
      selected_endpoint_id: null,
      endpoints: [],
    },
  },
  codex_accounts: { active_account_id: "codex-1", accounts: [], logins: [] },
  claude_accounts: { active_account_id: null, accounts: [] },
  gemini_accounts: { active_account_id: null, accounts: [] },
  qwen_accounts: { active_account_id: null, accounts: [] },
  kimi_accounts: { active_account_id: null, accounts: [] },
  mistral_accounts: { active_account_id: null, accounts: [] },
  copilot_accounts: { active_account_id: null, accounts: [], logins: [] },
  cursor_accounts: cursorAccounts,
  amp_accounts: { active_account_id: null, accounts: [] },
  auggie_accounts: { active_account_id: null, accounts: [] },
});

const readLabel = (value: unknown): string | null => {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
};

test("workbench: bootstrap failure still allows harness auth recovery from settings", async ({
  page,
  request,
}) => {
  const repo = initRepo();

  let bootstrapCalls = 0;
  let cursorAuthed = false;
  let cursorAccounts: CursorAccountsResponse = {
    active_account_id: null,
    accounts: [],
  };

  await page.route("**/api/workspaces/*/providers/bootstrap", async (route) => {
    if (route.request().method() !== "GET") {
      await route.continue();
      return;
    }

    bootstrapCalls += 1;
    if (bootstrapCalls === 1) {
      await route.fulfill({
        status: 500,
        contentType: "application/json",
        body: JSON.stringify({ error: "bootstrap exploded" }),
      });
      return;
    }

    const url = new URL(route.request().url());
    const match = url.pathname.match(/^\/api\/workspaces\/([^/]+)\/providers\/bootstrap$/);
    const workspaceId = match ? decodeURIComponent(match[1]) : "";
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(buildBootstrapPayload(workspaceId, cursorAuthed, cursorAccounts)),
    });
  });

  await page.route("**/api/providers/cursor/accounts", async (route) => {
    const method = route.request().method();
    if (method === "GET") {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify(cursorAccounts),
      });
      return;
    }
    if (method === "POST") {
      const payload = route.request().postDataJSON() as { label?: unknown } | null;
      const label = readLabel(payload?.label) ?? "Cursor API";
      cursorAuthed = true;
      cursorAccounts = {
        active_account_id: "cursor-1",
        accounts: [
          {
            id: "cursor-1",
            label,
            kind: "api_key",
            email: null,
            created_at: new Date().toISOString(),
            last_used_at: null,
          },
        ],
      };
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify(cursorAccounts),
      });
      return;
    }
    await route.continue();
  });

  await page.route("**/api/workspaces/*/providers/*/options", async (route) => {
    const url = new URL(route.request().url());
    const match = url.pathname.match(/^\/api\/workspaces\/([^/]+)\/providers\/([^/]+)\/options$/);
    if (!match) {
      await route.continue();
      return;
    }
    const workspaceId = decodeURIComponent(match[1]);
    const providerId = decodeURIComponent(match[2]);
    if (providerId === "codex") {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          provider_id: "codex",
          workspace_id: workspaceId,
          supports_load: false,
          auth_required: false,
          has_active_auth: true,
          auth_mode: "subscription",
          probed_at: new Date().toISOString(),
        }),
      });
      return;
    }
    if (providerId === "cursor") {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          provider_id: "cursor",
          workspace_id: workspaceId,
          supports_load: false,
          auth_required: false,
          has_active_auth: cursorAuthed,
          auth_mode: cursorAuthed ? "subscription" : "none",
          probed_at: new Date().toISOString(),
        }),
      });
      return;
    }
    await route.continue();
  });

  const workspaceId = await createWorkspaceAndOpenWorkbench({
    page,
    request,
    repo,
    workspaceName: `ws-${Date.now()}`,
  });

  await expect(page.getByText("Failed to load workspace.")).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole("button", { name: "Retry workspace load" })).toBeVisible();

  const settingsLink = page.getByRole("link", { name: "Settings" });
  await expect(settingsLink).toBeVisible();
  await expect(settingsLink).toHaveAttribute("href", `/settings?ws=${workspaceId}`);
  await page.goto(`/settings?ws=${workspaceId}#agent_harnesses`, { waitUntil: "domcontentloaded" });
  await expect(page).toHaveURL(new RegExp(`/settings\\?ws=${workspaceId}#agent_harnesses$`), { timeout: 15_000 });
  await expect(page.locator(".settings-main-title")).toHaveText("Harness Authentication", { timeout: 15_000 });

  const cursorRow = page.locator(".settings-harness-row").filter({ hasText: "Cursor" }).first();
  await expect(cursorRow).toBeVisible();
  await cursorRow.getByRole("button", { name: /Add auth for Cursor/i }).click();

  const modal = page.locator(".settings-harness-modal");
  await expect(modal).toBeVisible({ timeout: 10_000 });
  const apiKeyButton = modal.getByRole("button", { name: /^API Key$/i });
  if (await apiKeyButton.count()) {
    await apiKeyButton.click();
  }
  await expect(modal.getByRole("link", { name: "Cursor Integrations" })).toHaveAttribute(
    "href",
    "https://cursor.com/dashboard?tab=integrations",
  );
  await modal
    .locator("label.settings-harness-modal-label")
    .filter({ hasText: /Name \(optional\)|Label \(optional\)/i })
    .locator("input")
    .first()
    .fill("Bootstrap Recovery");
  await modal.locator("input[type='password']").first().fill("cursor-test-token");
  await modal.getByRole("button", { name: "Add API key" }).click();
  await expect(modal).toBeHidden({ timeout: 10_000 });

  await expect(cursorRow.locator(".settings-harness-auth-row-active")).toContainText("Bootstrap Recovery");
  expect(bootstrapCalls).toBeGreaterThanOrEqual(2);
});
