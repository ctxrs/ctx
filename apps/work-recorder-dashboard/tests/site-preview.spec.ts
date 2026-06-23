import { expect, test, type Page } from "@playwright/test";
import path from "node:path";

const artifactDir = path.resolve(process.cwd(), "../../target/ctx-artifacts/dashboard-react");

test("desktop site preview positions ctx work records honestly", async ({ page }, testInfo) => {
  await page.goto("/site-preview.html");
  await expect(page.getByRole("heading", { name: "ctx records agent work" })).toBeVisible();
  await expect(page.getByText("0.1.0 candidate", { exact: true })).toBeVisible();
  await expect(page.getByText("This preview covers the public ctx CLI only.")).toBeVisible();
  await page.getByRole("tab", { name: "Providers" }).click();
  await expect(page.getByRole("heading", { name: "Current 0.1.0 candidate matrix" })).toBeVisible();
  await expect(page.getByText("supported-import").first()).toBeVisible();
  await expect(page.getByText("fixture-only").first()).toBeVisible();
  await assertNonBlank(page);
  await screenshot(page, testInfo.project.name, "site-preview-desktop-overview");
});

test("mobile site preview shows install and boundaries tabs", async ({ page }, testInfo) => {
  await page.goto("/site-preview.html");
  await page.getByRole("tab", { name: "Install" }).click();
  await expect(page.getByRole("heading", { name: "Release and install posture" })).toBeVisible();
  await expect(page.getByText("No curl-pipe-shell instructions")).toBeVisible();
  await page.getByRole("tab", { name: "Boundaries" }).click();
  await expect(page.getByText(/^Not the ctx ADE,/)).toBeVisible();
  await expect(page.getByRole("heading", { name: "Troubleshooting and wording discipline" })).toBeVisible();
  await assertNonBlank(page);
  await screenshot(page, testInfo.project.name, "site-preview-mobile-boundaries");
});

async function screenshot(page: Page, project: string, name: string) {
  await page.waitForTimeout(150);
  await page.screenshot({
    path: path.join(artifactDir, `${project}-${name}.png`),
    fullPage: true
  });
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
  expect(sample.textLength).toBeGreaterThan(700);
  expect(sample.elements).toBeGreaterThan(10);
}
