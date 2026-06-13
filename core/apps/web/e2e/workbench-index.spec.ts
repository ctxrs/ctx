import { test, expect } from "./fixtures";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { execSync } from "child_process";
import { createWorkspaceAndOpenWorkbench } from "./utils/workbench";

test("workbench active snapshot stream keeps network lean", async ({ page }) => {
  const repo = mkdtempSync(path.join(tmpdir(), "ctx-e2e-"));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "file.txt"), "hello\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });

  const workspaceName = `ws-${Date.now()}`;
  const startedRequests: { url: string; method: string }[] = [];
  const requests: { url: string; method: string }[] = [];
  page.on("request", (req) => {
    const url = req.url();
    if (!url.includes("/api/")) return;
    startedRequests.push({ url, method: req.method() });
  });
  page.on("requestfinished", (req) => {
    const url = req.url();
    if (!url.includes("/api/")) return;
    requests.push({ url, method: req.method() });
  });

  const snapshotStreamPromise = page.waitForEvent("websocket", (ws) =>
    /\/api\/workspaces\/[^/]+\/active_snapshot\/stream/.test(ws.url()),
  );
  const workspaceId = await createWorkspaceAndOpenWorkbench({
    page,
    request: page.request,
    repo,
    workspaceName,
  });
  const snapshotStream = await snapshotStreamPromise;
  await expect.poll(() => requests.length).toBeGreaterThan(0);
  expect(workspaceId).not.toEqual("");
  await expect(page.getByRole("button", { name: "fake", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "fake-model", exact: true })).toBeVisible();

  const apiRequests = () => requests.filter((r) => r.url.includes("/api/"));
  expect(apiRequests().length).toBeLessThanOrEqual(18);

  const providerBootstrapPath = `/api/workspaces/${workspaceId}/providers/bootstrap`;
  const providerBootstrapRequests = apiRequests().filter((r) => {
    if (r.method !== "GET") return false;
    const pathname = new URL(r.url).pathname;
    return pathname === providerBootstrapPath;
  });
  // Provider bootstrap can be satisfied from the active snapshot/bootstrap store,
  // so opening the workbench should not require more than one extra bootstrap GET.
  expect(providerBootstrapRequests.length).toBeLessThanOrEqual(1);

  const startupProviderAccounts = apiRequests().filter((r) => {
    if (r.method !== "GET") return false;
    const pathname = new URL(r.url).pathname;
    return /^\/api\/providers\/[^/]+\/accounts(?:\/[^/]+)?$/.test(pathname);
  });
  expect(startupProviderAccounts.length).toBe(0);

  const startupHarnessConfig = apiRequests().filter((r) => {
    if (r.method !== "GET") return false;
    const pathname = new URL(r.url).pathname;
    return /^\/api\/providers\/[^/]+\/harness_config(?:\/.*)?$/.test(pathname);
  });
  expect(startupHarnessConfig.length).toBe(0);

  const startupRuntimeProviderOptions = apiRequests().filter((r) => {
    if (r.method !== "GET") return false;
    const pathname = new URL(r.url).pathname;
    return new RegExp(`^/api/workspaces/${workspaceId}/providers/[^/]+/options$`).test(pathname);
  });
  expect(startupRuntimeProviderOptions.length).toBe(0);

  const updatesRequests = () =>
    startedRequests.filter((r) => {
      if (r.method !== "GET") return false;
      const pathname = new URL(r.url).pathname;
      return pathname.startsWith("/api/updates/");
    });
  await expect.poll(() => updatesRequests().length).toBe(1);
  const updates = updatesRequests();
  expect(updates.every((r) => new URL(r.url).pathname === "/api/updates/check")).toBe(true);
  expect(updates.length).toBe(1);

  const snapshotRequests = apiRequests().filter((r) => {
    if (r.method !== "GET") return false;
    const pathname = new URL(r.url).pathname;
    return pathname === `/api/workspaces/${workspaceId}/active_snapshot`;
  });
  expect(snapshotRequests.length).toBe(0);
  expect(snapshotStream.url()).toContain(`/api/workspaces/${workspaceId}/active_snapshot/stream`);
  const trackRequests = apiRequests().filter(
    (r) => /\/api\/tasks\/[^/]+\/tracks/.test(r.url) || /\/api\/tracks\/[^/]+/.test(r.url),
  );
  expect(trackRequests.length).toBe(0);
});
