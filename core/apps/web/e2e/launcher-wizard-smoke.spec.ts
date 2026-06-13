import { test, expect } from "./fixtures";

test("launcher opens workspace wizard and does not expose remote ctx path input", async ({ page }) => {
  await page.goto("/", { waitUntil: "domcontentloaded" });
  const newWorkspace = page.getByRole("button", { name: "New Workspace" });
  await expect(newWorkspace).toBeVisible({ timeout: 20_000 });
  await newWorkspace.click();

  const wizard = page.getByTestId("workspace-setup");
  await expect(wizard).toBeVisible({ timeout: 20_000 });
  await expect(wizard).toHaveAttribute("data-step-key", "location");

  await page.getByTestId("wizard-option-location-remote").click();
  await expect(page.getByTestId("wizard-remote-host")).toBeVisible({ timeout: 20_000 });

  await expect(page.getByTestId("wizard-remote-advanced-toggle")).toHaveCount(0);
  await expect(page.getByText("Remote ctx binary path")).toHaveCount(0);
  await expect(page.getByLabel("Root path")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Add workspace" })).toHaveCount(0);
});
