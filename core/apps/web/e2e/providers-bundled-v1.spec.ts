import { test, expect } from "./fixtures";
import { providerDetailFlag } from "../src/utils/boolish";

type ProviderSummary = {
  provider_id: string;
  installed: boolean;
  health: string;
  details?: {
    ui_hidden?: string;
  };
};

test("providers: bundled v1 shows ready providers with reinstall controls", async ({ page, request }) => {
  await page.goto("/providers", { waitUntil: "domcontentloaded" });
  await expect(page.getByRole("heading", { name: "Providers" })).toBeVisible();

  await expect
    .poll(async () => {
      const resp = await request.get("/api/providers");
      if (!resp.ok()) return 0;
      const providers = (await resp.json()) as ProviderSummary[];
      return providers.length;
    })
    .toBeGreaterThan(0);

  const providersResp = await request.get("/api/providers");
  expect(providersResp.ok()).toBeTruthy();
  const providers = (await providersResp.json()) as ProviderSummary[];
  const visibleProviders = providers.filter((provider) => !providerDetailFlag(provider.details, "ui_hidden"));
  const readyProviders = visibleProviders.filter((p) => p.installed && p.health === "ok");
  expect(readyProviders.length).toBeGreaterThan(0);

  const escapeRegExp = (value: string) => value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

  for (const provider of readyProviders) {
    const card = page.locator("ul.list > li.card").filter({
      has: page.locator("strong", {
        hasText: new RegExp(`^${escapeRegExp(provider.provider_id)}$`),
      }),
    });
    await expect(card).toBeVisible();
    await expect(card.getByText("Installed")).toBeVisible();
    await expect(card.getByText(provider.health)).toBeVisible();
    await expect(card.getByRole("button", { name: "Reinstall" })).toBeVisible();
    await expect(card.getByRole("button", { name: /^Install$/ })).toHaveCount(0);
    await expect(card.getByRole("button", { name: /^Update$/ })).toHaveCount(0);
  }

  await expect(page.getByRole("button", { name: "Install all" })).toBeVisible();
});
