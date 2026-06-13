import { test, expect } from "./fixtures";
import type { Locator } from "playwright/test";
import { execSync } from "child_process";
import { mkdtempSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import http from "http";
import type { AddressInfo } from "node:net";

const initRepo = (prefix: string) => {
  const repo = mkdtempSync(path.join(tmpdir(), prefix));
  execSync("git init", { cwd: repo });
  execSync("git config user.email test@example.com", { cwd: repo });
  execSync("git config user.name Test", { cwd: repo });
  writeFileSync(path.join(repo, "README.md"), "fixture\n");
  execSync("git add .", { cwd: repo });
  execSync("git commit -m init", { cwd: repo });
  return repo;
};

const startDocsServer = async () => {
  const server = http.createServer((req, res) => {
    if (!req.url) {
      res.writeHead(404);
      res.end();
      return;
    }
    if (req.url === "/llms.txt" || req.url === "/docs/llms.txt") {
      const baseUrl = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
      const urls = [`${baseUrl}/docs/intro.md`, `${baseUrl}/docs/guide.md`];
      res.writeHead(200, { "content-type": "text/plain" });
      res.end(urls.join("\n"));
      return;
    }
    if (req.url === "/docs/intro.md") {
      res.writeHead(200, { "content-type": "text/markdown" });
      res.end("# Intro\n\nHello docs.\n");
      return;
    }
    if (req.url === "/docs/guide.md") {
      res.writeHead(200, { "content-type": "text/markdown" });
      res.end("# Guide\n\nMore docs.\n");
      return;
    }
    res.writeHead(404);
    res.end();
  });

  await new Promise<void>((resolve) => {
    server.listen(0, "127.0.0.1", () => resolve());
  });
  const port = (server.address() as AddressInfo).port;
  return {
    server,
    docsUrl: `http://127.0.0.1:${port}/docs/`,
  };
};

const waitForReady = async (row: Locator) => {
  await expect(row.locator(".settings-attachments-status-dot-ready")).toBeVisible({ timeout: 30_000 });
  await expect(row.locator(".settings-attachments-indexed-label")).toContainText(/Indexed|Ready/i, { timeout: 30_000 });
};

test("workspace attachments show pending then ready", async ({ page, request }) => {
  const workspaceRoot = initRepo("ctx-e2e-attachments-ws-");
  const refRepoRoot = initRepo("ctx-e2e-attachments-ref-");
  const { server, docsUrl } = await startDocsServer();

  try {
    const workspaceRes = await request.post("/api/workspaces", {
      data: { root_path: workspaceRoot, name: `ws-${Date.now()}` },
    });
    expect(workspaceRes.ok()).toBeTruthy();
    const workspace = (await workspaceRes.json()) as { id: string };

    await page.goto(`/settings?ws=${workspace.id}#workspace_attachments`);
    await page.getByRole("main").locator(".settings-row-title", { hasText: "Reference repos" }).first().waitFor();

    await page.getByRole("button", { name: "Add reference repo" }).click();
    await page.locator("#attachments-source").fill(refRepoRoot);
    await page.locator("#attachments-name").fill("ref-fixture");
    await page.getByRole("button", { name: "Add repo" }).click();

    const repoRow = page.locator(".settings-attachments-list-row", { hasText: "ref-fixture" });
    await expect(repoRow).toBeVisible({ timeout: 10_000 });

    await page.getByRole("button", { name: "Add docs mirror" }).click();
    await page.locator("#attachments-docs-source").fill(docsUrl);
    await page.locator("#attachments-docs-name").fill("docs-fixture");
    await page.getByRole("button", { name: "Add docs", exact: true }).click();

    const docsRow = page.locator(".settings-attachments-list-row", { hasText: "docs-fixture" });
    await expect(docsRow).toBeVisible({ timeout: 10_000 });

    await waitForReady(repoRow);
    await waitForReady(docsRow);
  } finally {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
});
