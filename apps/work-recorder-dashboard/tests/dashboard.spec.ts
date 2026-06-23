import { expect, test, type Page } from "@playwright/test";
import path from "node:path";

const artifactDir = path.resolve(process.cwd(), "../../target/ctx-artifacts/dashboard-react");

test("desktop light populated dashboard", async ({ page }, testInfo) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Work Records" })).toBeVisible();
  await expect(page.getByText("Finish dashboard React export")).toBeVisible();
  await expect(page.getByText("Share-safe export")).toBeVisible();
  await expect(page.getByText("1 failing command")).toBeVisible();
  await expect(page.locator(".metric").filter({ hasText: "Linked PRs" }).getByText("2")).toBeVisible();
  await assertNonBlank(page);
  await screenshot(page, testInfo.project.name, "desktop-light-overview");
});

test("desktop dark session transcript and commands", async ({ page }, testInfo) => {
  await page.goto("/");
  await page.getByTitle("Use dark theme").click();
  await page.getByRole("tab", { name: "Session" }).click();
  await expect(page.getByRole("heading", { name: "Transcript, Messages, and Tool Calls" })).toBeVisible();
  await expect(page.getByText("exec_command npm run build")).toBeVisible();
  await expect(page.getByText("cargo test -p work-record-report")).toBeVisible();
  await assertNonBlank(page);
  await screenshot(page, testInfo.project.name, "desktop-dark-session");
});

test("mobile evidence failure state", async ({ page }, testInfo) => {
  await page.goto("/");
  await page.getByRole("tab", { name: "PR/Evidence" }).click();
  await expect(page.getByRole("heading", { name: "Evidence Previews" })).toBeVisible();
  await expect(page.getByText("buildkite-agent pipeline upload")).toBeVisible();
  await expect(page.getByText("missing BUILDKITE_AGENT_TOKEN")).toBeVisible();
  await expect(page.locator(".badge-danger").filter({ hasText: "Exit 1" })).toBeVisible();
  await expect(page.locator(".row-card-danger").filter({ hasText: "failed" })).toBeVisible();
  await expect(page.locator(".link-row")).toHaveCount(2);
  await expectActiveTabSettled(page, "PR/Evidence");
  await assertNonBlank(page);
  await screenshot(page, testInfo.project.name, "mobile-evidence-failure");
});

test("mobile status and search", async ({ page }, testInfo) => {
  await page.goto("/");
  await page.getByRole("tab", { name: "Search" }).click();
  await page.getByPlaceholder("Search records, commands, transcript previews, artifacts").fill("provider");
  await expect(page.getByText("Import provider fixture sessions")).toBeVisible();
  await page.getByRole("tab", { name: "Status" }).click();
  await expect(page.getByRole("heading", { name: "Settings / Status" })).toBeVisible();
  await expect(page.getByText("Work Recorder dashboard export v1")).toBeVisible();
  await expectActiveTabSettled(page, "Status");
  await expectVisibleActiveTabOnly(page, "Status");
  await assertNonBlank(page);
  await screenshot(page, testInfo.project.name, "mobile-status-search");
});

async function screenshot(page: Page, project: string, name: string) {
  await page.waitForTimeout(150);
  await page.screenshot({
    path: path.join(artifactDir, `${project}-${name}.png`),
    fullPage: true
  });
}

async function expectActiveTabSettled(page: Page, label: string) {
  await page.waitForFunction((expectedLabel) => {
    const active = document.querySelector<HTMLElement>("[role='tab'][data-state='active']");
    if (!active || !active.textContent?.includes(String(expectedLabel))) return false;
    const rect = active.getBoundingClientRect();
    return rect.left >= 0 && rect.right <= window.innerWidth;
  }, label);
}

async function expectVisibleActiveTabOnly(page: Page, label: string) {
  await page.waitForFunction((expectedLabel) => {
    const tabs = Array.from(document.querySelectorAll<HTMLElement>("[role='tab']"));
    const visibleTabs = tabs.filter((tab) => {
      const rect = tab.getBoundingClientRect();
      return rect.right > 0 && rect.left < window.innerWidth;
    });
    return visibleTabs.some((tab) => tab.dataset.state === "active" && tab.textContent?.includes(String(expectedLabel)));
  }, label);
}

async function assertNonBlank(page: Page) {
  const sample = await page.evaluate(() => {
    const rect = document.body.getBoundingClientRect();
    const textLength = document.body.innerText.trim().length;
    const elements = document.querySelectorAll("section, article, table, [role='tab']").length;
    return { width: rect.width, height: rect.height, textLength, elements };
  });
  expect(sample.width).toBeGreaterThan(300);
  expect(sample.height).toBeGreaterThan(500);
  expect(sample.textLength).toBeGreaterThan(400);
  expect(sample.elements).toBeGreaterThan(6);
}
