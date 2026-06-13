import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";

test("workbench: providers bootstrap refreshes on reconnect/foreground", async ({ page, request }) => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init -b main", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "README.md"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const requests: Array<{ method: string; pathname: string }> = [];
  page.on("requestfinished", (req) => {
    const url = req.url();
    if (!url.includes("/api/")) return;
    requests.push({ method: req.method(), pathname: new URL(url).pathname });
  });

  const workspaceId = await createWorkspaceAndOpenWorkbench({
    page,
    request,
    repo,
    workspaceName: `ws-${Date.now()}`,
  });

  const bootstrapResponse = await request.get(`/api/workspaces/${workspaceId}/providers/bootstrap`);
  expect(bootstrapResponse.ok()).toBeTruthy();
  const bootstrapPayload = (await bootstrapResponse.json()) as {
    providers?: Array<{ provider_id?: string }>;
    provider_options?: Record<
      string,
      {
        has_active_auth?: boolean;
        models?: { current_model_id?: string };
      }
    >;
  };
  expect(
    (bootstrapPayload.providers ?? []).some((provider) => provider?.provider_id === "fake"),
  ).toBe(true);
  expect(bootstrapPayload.provider_options?.fake?.has_active_auth).toBe(true);
  expect(bootstrapPayload.provider_options?.fake?.models?.current_model_id).toBe("fake-model");

  const bootstrapPath = `/api/workspaces/${workspaceId}/providers/bootstrap`;
  const bootstrapCount = () =>
    requests.filter((entry) => entry.method === "GET" && entry.pathname === bootstrapPath).length;

  await expect.poll(bootstrapCount).toBe(1);

  await page.evaluate(() => {
    window.dispatchEvent(new Event("online"));
    window.dispatchEvent(new Event("focus"));
    document.dispatchEvent(new Event("visibilitychange"));
  });

  await expect.poll(bootstrapCount).toBeGreaterThanOrEqual(2);
});
