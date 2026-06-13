import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";

test("workbench: unauthed harness opens auth modal and API key flow readies harness", async ({ page, request }) => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init -b main", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "README.md"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  let cursorAuthed = false;
  let cursorAccounts = {
    active_account_id: null as string | null,
    accounts: [] as Array<{
      id: string;
      label: string;
      kind: string;
      email: string | null;
      created_at: string;
      last_used_at: string | null;
    }>,
  };

  await page.route("**/api/workspaces/*/providers/bootstrap", async (route) => {
    if (route.request().method() !== "GET") {
      await route.continue();
      return;
    }
    const url = new URL(route.request().url());
    const match = url.pathname.match(/^\/api\/workspaces\/([^/]+)\/providers\/bootstrap$/);
    const workspaceId = match ? decodeURIComponent(match[1]) : "";
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
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
          cursor: {
            provider_id: "cursor",
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
      }),
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
      cursorAuthed = true;
      cursorAccounts = {
        active_account_id: "cursor-1",
        accounts: [
          {
            id: "cursor-1",
            label: "Cursor API",
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

  await createWorkspaceAndOpenWorkbench({
    page,
    request,
    repo,
    workspaceName: `ws-${Date.now()}`,
  });

  const harnessButton = page.locator('button[title="Agents"]');
  await expect(harnessButton).toBeVisible({ timeout: 15_000 });

  await harnessButton.click();
  const menu = page.locator(".wb-harness-menu");
  await expect(menu).toBeVisible();

  await menu.getByRole("button", { name: /Cursor/ }).click();
  const modal = page.locator(".settings-harness-modal");
  await expect(modal).toBeVisible({ timeout: 10_000 });
  const apiKeyButton = modal.getByRole("button", { name: "API Key" });
  if (await apiKeyButton.count()) {
    await expect(apiKeyButton).toBeVisible();
    await apiKeyButton.click();
  }
  await expect(modal.getByRole("link", { name: "Cursor Integrations" })).toHaveAttribute(
    "href",
    "https://cursor.com/dashboard?tab=integrations",
  );
  await expect(modal.getByText("Label (optional)")).toBeVisible();
  await expect(modal.getByText("Manual model slugs (optional)")).toHaveCount(0);
  await expect(modal.getByText("Base URL")).toHaveCount(0);
  await modal.locator("input[type='password']").fill("cursor-test-token");
  await modal.getByRole("button", { name: "Add API key" }).click();
  await expect(modal).toBeHidden({ timeout: 10_000 });

  const cursorHarnessButton = page.getByRole("button", { name: "Cursor" });
  await expect(cursorHarnessButton).toBeVisible({ timeout: 10_000 });
  await cursorHarnessButton.click();
  await expect(
    page
      .locator(".wb-harness-row")
      .filter({ hasText: "Cursor" })
      .locator(".wb-harness-auth-dot-active"),
  ).toBeVisible();
});
