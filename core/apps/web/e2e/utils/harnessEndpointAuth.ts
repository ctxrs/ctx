import { expect, type Locator, type Page } from "playwright/test";
import type { EndpointHarnessMatrixEntry } from "./harnessEndpointMatrix";

export type HarnessAuthConfigResult =
  | { ok: true; detail: string }
  | { ok: false; detail: string };

export type EndpointAuthModalOptions = {
  providerPresetLabel?: string;
  allowGenericProviderFallback?: boolean;
  geminiAuthMode?: "gemini_api_key" | "vertex_ai";
  endpointName?: string;
  serviceAccountJson?: string;
  projectId?: string;
  location?: string;
  expectedDocsLink?: {
    name: string;
    href: string;
  };
};

const normalizeText = (value: string | null | undefined): string => (value ?? "").replace(/\s+/g, " ").trim();
const escapeRegex = (value: string): string => value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

async function readOptionalVisibleText(locator: Locator): Promise<string> {
  const count = await locator.count().catch(() => 0);
  if (count === 0) return "";
  const visible = await locator.isVisible().catch(() => false);
  if (!visible) return "";
  const text = await locator.textContent({ timeout: 250 }).catch(() => "");
  return normalizeText(text);
}

async function visibleHarnessMenuNames(menu: Locator): Promise<string[]> {
  const rows = menu.locator(".wb-harness-row .wb-harness-name");
  const count = await rows.count().catch(() => 0);
  const out: string[] = [];
  for (let idx = 0; idx < count; idx += 1) {
    const text = normalizeText(await rows.nth(idx).textContent().catch(() => ""));
    if (text) out.push(text);
  }
  return out;
}

async function modalDebugSummary(modal: Locator): Promise<string> {
  const text = normalizeText(await modal.textContent().catch(() => ""));
  const hasApiKeyButton = (
    await modal.getByRole("button", { name: /^API Key$/i }).count().catch(() => 0)
  ) > 0;
  const hasPasswordInput = (await modal.locator("input[type='password']").count().catch(() => 0)) > 0;
  const hasProviderSelect = (await modal.getByRole("combobox").count().catch(() => 0)) > 0;
  return `modal_text=${JSON.stringify(text)} api_key_button=${hasApiKeyButton} password_input=${hasPasswordInput} combobox=${hasProviderSelect}`;
}

const harnessTriggerLabel = (page: Page) =>
  page
    .locator(
      ".wb-new-composer-stack .wb-switcher-harness .wb-switcher-label, .wb-new-composer-stack button[title='Agents'] .wb-switcher-label, .wb-new-composer-stack button[title='Harness'] .wb-switcher-label",
    )
    .first();

const isHarnessLabelSelected = (labelText: string, entry: EndpointHarnessMatrixEntry): boolean => {
  const selected = labelText.toLowerCase();
  return selected.includes(entry.menuLabel.toLowerCase()) || selected.includes(entry.providerId.toLowerCase());
};

async function openHarnessMenu(page: Page) {
  const harnessButton = page
    .locator(
      ".wb-new-composer-stack .wb-switcher-harness, .wb-new-composer-stack button[title='Agents'], .wb-new-composer-stack button[title='Harness']",
    )
    .first();
  await expect(harnessButton).toBeVisible({ timeout: 20_000 });
  const menu = page.locator(".wb-harness-menu");
  if (!(await menu.isVisible().catch(() => false))) {
    await harnessButton.click();
    await expect(menu).toBeVisible({ timeout: 10_000 });
  }
  return menu;
}

async function resolveHarnessMenuButton(
  menu: Locator,
  entry: EndpointHarnessMatrixEntry,
) {
  const labelPattern = new RegExp(`${escapeRegex(entry.menuLabel)}|${escapeRegex(entry.providerId)}`, "i");
  return menu
    .locator(".wb-harness-row .wb-harness-row-main")
    .filter({ hasText: labelPattern })
    .first();
}

async function waitForHarnessRowReady(rowButton: Locator, timeout: number) {
  await expect(rowButton).toBeVisible({ timeout });
  await expect(rowButton).toBeEnabled({ timeout });
}

async function chooseEndpointPreset(
  page: Page,
  modal: Locator,
  label: string,
  allowGenericFallback: boolean,
) {
  const providerSelect = modal
    .locator("label.settings-harness-modal-label")
    .filter({ hasText: /^Provider$/i })
    .locator('[role="combobox"]')
    .first();
  if ((await providerSelect.count()) === 0) return;
  await providerSelect.click();
  const roleOption = page.getByRole("option", {
    name: new RegExp(`^${escapeRegex(label)}$`, "i"),
  }).first();
  if ((await roleOption.count()) > 0) {
    await roleOption.click();
    return;
  }
  const textOption = page.locator(".tw-z-\\[1101\\]").getByText(label, { exact: true }).first();
  if ((await textOption.count()) > 0) {
    await textOption.click();
    return;
  }
  if (allowGenericFallback) {
    const genericOption = page
      .getByRole("option", { name: /(custom|endpoint|api key)/i })
      .first();
    if ((await genericOption.count()) > 0) {
      await genericOption.click();
      return;
    }
  }
  await page.keyboard.press("Escape").catch(() => {});
  throw new Error(`provider preset option not found: ${label}`);
}

async function chooseGeminiAuthMode(
  page: Page,
  modal: Locator,
  mode: "gemini_api_key" | "vertex_ai",
) {
  const trigger = modal
    .locator("label.settings-harness-modal-label")
    .filter({ hasText: /Gemini auth mode/i })
    .locator('[role="combobox"]')
    .first();
  if ((await trigger.count()) === 0) return;
  await trigger.click();
  const optionLabel = mode === "vertex_ai" ? "Vertex AI" : "Gemini API Key";
  const option = page.getByRole("option", { name: new RegExp(`^${escapeRegex(optionLabel)}$`, "i") }).first();
  if ((await option.count()) > 0) {
    await option.click();
    return;
  }
  const textOption = page.locator(".tw-z-\\[1101\\]").getByText(optionLabel, { exact: true }).first();
  if ((await textOption.count()) > 0) {
    await textOption.click();
    return;
  }
  await page.keyboard.press("Escape").catch(() => {});
  throw new Error(`gemini auth mode option not found: ${optionLabel}`);
}

async function dismissAuthModalIfOpen(page: Page): Promise<void> {
  const modal = page.locator(".settings-harness-modal");
  if (!(await modal.isVisible().catch(() => false))) return;
  const closeButton = modal.getByRole("button", { name: "Close" }).first();
  if ((await closeButton.count()) > 0) {
    await closeButton.click().catch(() => {});
  }
  if (await modal.isVisible().catch(() => false)) {
    const backButton = modal.getByRole("button", { name: "Back" }).first();
    if ((await backButton.count()) > 0) {
      await backButton.click().catch(() => {});
    }
  }
  if (await modal.isVisible().catch(() => false)) {
    await page.locator(".modal-overlay").first().click({ position: { x: 4, y: 4 } }).catch(() => {});
  }
  if (await modal.isVisible().catch(() => false)) {
    await page.keyboard.press("Escape").catch(() => {});
  }
  await modal.waitFor({ state: "hidden", timeout: 4_000 }).catch(() => {});
}

async function advanceToApiKeyStageIfPresent(modal: Locator): Promise<void> {
  const apiKeyButton = modal.getByRole("button", { name: /^API Key$/i }).first();
  if ((await apiKeyButton.count()) === 0) return;
  if (!(await apiKeyButton.isVisible().catch(() => false))) return;
  await apiKeyButton.click();
}

export async function configureHarnessEndpointAuthViaModal(
  page: Page,
  entry: EndpointHarnessMatrixEntry,
  apiKey: string,
  baseUrl: string,
  modelOverride = "",
  options: EndpointAuthModalOptions = {},
): Promise<HarnessAuthConfigResult> {
  await dismissAuthModalIfOpen(page);
  const menu = await openHarnessMenu(page);
  const searchInput = menu.getByLabel("Search agents");
  await searchInput.fill(entry.searchTerm);
  await expect(searchInput).toHaveValue(entry.searchTerm);
  const rowButton = await resolveHarnessMenuButton(menu, entry);
  try {
    await expect(rowButton).toBeVisible({ timeout: 10_000 });
  } catch {
    const visibleNames = await visibleHarnessMenuNames(menu);
    return {
      ok: false,
      detail: `harness menu row not found; visible=${visibleNames.join(", ") || "none"}`,
    };
  }
  await rowButton.click();

  const modal = page.locator(".settings-harness-modal");
  try {
    await expect(modal).toBeVisible({ timeout: 10_000 });
  } catch {
    return { ok: false, detail: "auth modal did not open (provider may already be configured)" };
  }

  await advanceToApiKeyStageIfPresent(modal);
  if (options.geminiAuthMode) {
    await chooseGeminiAuthMode(page, modal, options.geminiAuthMode);
  }
  const providerPresetLabel = options.providerPresetLabel ?? "OpenRouter";
  await chooseEndpointPreset(page, modal, providerPresetLabel, options.allowGenericProviderFallback ?? true);

  if (options.expectedDocsLink) {
    await expect(modal.getByRole("link", { name: options.expectedDocsLink.name })).toHaveAttribute(
      "href",
      options.expectedDocsLink.href,
    );
  }

  if (options.geminiAuthMode === "vertex_ai") {
    const serviceAccountInput = modal
      .locator("label.settings-harness-modal-label")
      .filter({ hasText: /Service account JSON/i })
      .locator("textarea")
      .first();
    await expect(serviceAccountInput).toBeVisible({ timeout: 10_000 });
    await serviceAccountInput.fill(options.serviceAccountJson ?? apiKey);
    const projectIdInput = modal
      .locator("label.settings-harness-modal-label")
      .filter({ hasText: /^Project ID \(optional\)$/i })
      .locator("input")
      .first();
    if ((await projectIdInput.count()) > 0 && options.projectId !== undefined) {
      await projectIdInput.fill(options.projectId);
    }
    const locationInput = modal
      .locator("label.settings-harness-modal-label")
      .filter({ hasText: /^Location \(optional\)$/i })
      .locator("input")
      .first();
    if ((await locationInput.count()) > 0 && options.location !== undefined) {
      await locationInput.fill(options.location);
    }
  } else {
    const passwordInput = modal.locator("input[type='password']").first();
    try {
      await expect(passwordInput).toBeVisible({ timeout: 10_000 });
    } catch {
      throw new Error(`password input not visible; ${await modalDebugSummary(modal)}`);
    }
    await passwordInput.fill(apiKey);
  }

  const endpointName = options.endpointName
    ?? `${entry.providerId}-${providerPresetLabel.toLowerCase().replace(/[^a-z0-9]+/g, "-")}`;
  const nameInput = modal
    .locator("label.settings-harness-modal-label")
    .filter({ hasText: /Name \(optional\)|Label \(optional\)/i })
    .locator("input")
    .first();
  if ((await nameInput.count()) > 0) {
    await nameInput.fill(endpointName);
  }

  const baseUrlInput = modal
    .locator("label.settings-harness-modal-label")
    .filter({ hasText: "Base URL" })
    .locator("input")
    .first();
  if ((await baseUrlInput.count()) > 0) {
    await baseUrlInput.fill(baseUrl);
  }

  const targetModel = modelOverride.trim();
  if (targetModel) {
    const modelOverrideInput = modal
      .locator("label.settings-harness-modal-label")
      .filter({ hasText: "Model override" })
      .locator("input")
      .first();
    if ((await modelOverrideInput.count()) > 0) {
      await modelOverrideInput.fill(targetModel);
    } else {
      const modelInput = modal
        .locator("label.settings-harness-modal-label")
        .filter({ hasText: "Model" })
        .locator("input")
        .first();
      if ((await modelInput.count()) > 0) {
        await modelInput.fill(targetModel);
      }
    }
  }

  await modal.getByRole("button", { name: /Add API key|Save/i }).click();
  const providerError = page.locator(".settings-banner.settings-banner-error").first();
  const startedAt = Date.now();
  const timeoutMs = 20_000;
  while (Date.now() - startedAt < timeoutMs) {
    const modalVisible = await modal.isVisible().catch(() => false);
    if (!modalVisible) {
      return { ok: true, detail: "endpoint auth saved via modal" };
    }

    const errorText = await readOptionalVisibleText(providerError);
    if (errorText) {
      const errorLower = errorText.toLowerCase();
      if (
        errorLower.includes("endpoint verification failed")
        || errorLower.includes("models.list probe timed out")
      ) {
        await dismissAuthModalIfOpen(page);
        return { ok: true, detail: `endpoint auth saved with verify warning: ${errorText}` };
      }
      await dismissAuthModalIfOpen(page).catch(() => {});
      return { ok: false, detail: errorText };
    }

    await page.waitForTimeout(200);
  }

  await dismissAuthModalIfOpen(page).catch(() => {});
  return { ok: false, detail: "auth modal did not close after Add API key" };
}

export async function selectHarnessForComposer(
  page: Page,
  entry: EndpointHarnessMatrixEntry,
  options: { requireAuthDot?: boolean } = {},
): Promise<HarnessAuthConfigResult> {
  const triggerLabel = harnessTriggerLabel(page);
  await expect(triggerLabel).toBeVisible({ timeout: 10_000 });
  const currentLabel = ((await triggerLabel.textContent()) ?? "").trim();
  if (isHarnessLabelSelected(currentLabel, entry)) {
    return { ok: true, detail: `selected harness '${currentLabel}'` };
  }

  const menu = await openHarnessMenu(page);
  await menu.getByLabel("Search agents").fill(entry.searchTerm);
  const rowButton = await resolveHarnessMenuButton(menu, entry);
  if ((await rowButton.count()) === 0) {
    return { ok: false, detail: "harness menu row not found" };
  }
  await waitForHarnessRowReady(rowButton, 20_000);
  if (options.requireAuthDot !== false) {
    await expect(rowButton.locator(".wb-harness-auth-dot-active")).toBeVisible({ timeout: 20_000 });
  }
  await rowButton.click();

  await expect
    .poll(
      async () => {
        const label = ((await triggerLabel.textContent()) ?? "").trim();
        return isHarnessLabelSelected(label, entry);
      },
      { timeout: 10_000, intervals: [200, 400, 800] },
    )
    .toBe(true);

  const selected = ((await triggerLabel.textContent()) ?? "").trim();
  return { ok: true, detail: `selected harness '${selected}'` };
}

export async function selectHarnessBySearch(
  page: Page,
  searchTerm: string,
  optionPattern: RegExp,
): Promise<void> {
  const triggerLabel = harnessTriggerLabel(page);
  const isSelected = async () => optionPattern.test(((await triggerLabel.textContent()) ?? "").trim());
  if (await isSelected()) return;

  for (let attempt = 0; attempt < 3; attempt += 1) {
    const menu = await openHarnessMenu(page);
    await menu.getByLabel("Search agents").fill(searchTerm);

    const rowButton = menu
      .locator(".wb-harness-row .wb-harness-row-main")
      .filter({ hasText: optionPattern })
      .first();
    await expect(rowButton).toBeVisible({ timeout: 20_000 });
    await expect(rowButton).toBeEnabled({ timeout: 20_000 });
    await rowButton.click();

    const selectedAfterClick = await expect
      .poll(
        async () => isSelected(),
        { timeout: 1_500, intervals: [100, 200, 400] },
      )
      .toBe(true)
      .then(() => true)
      .catch(() => false);
    if (selectedAfterClick) return;

    const modal = page.locator(".settings-harness-modal");
    const modalVisible = await modal
      .waitFor({ state: "visible", timeout: 1_500 })
      .then(() => true)
      .catch(() => false);
    if (modalVisible) {
      const subscriptionButton = modal.getByRole("button", { name: "Subscription" }).first();
      const hasSubscriptionChoice =
        (await subscriptionButton.count()) > 0 && (await subscriptionButton.isEnabled().catch(() => false));
      if (hasSubscriptionChoice) {
        await subscriptionButton.click();
        const closedAfterChoose = await modal
          .waitFor({ state: "hidden", timeout: 1_500 })
          .then(() => true)
          .catch(() => false);
        if (closedAfterChoose) {
          if (await isSelected()) return;
          await page.waitForTimeout(300);
          continue;
        }
      }

      const saveSubscriptionButton = modal.getByRole("button", { name: /Save subscription/i }).first();
      if (
        (await saveSubscriptionButton.count()) > 0
        && (await saveSubscriptionButton.isEnabled().catch(() => false))
      ) {
        await saveSubscriptionButton.click();
        await expect(modal).toBeHidden({ timeout: 10_000 });
      } else if (!hasSubscriptionChoice) {
        throw new Error(`harness auth modal blocked '${searchTerm}' selection`);
      } else {
        throw new Error(`harness auth modal requires extra input for '${searchTerm}'`);
      }
    }

    if (await isSelected()) return;
    await page.waitForTimeout(300);
  }

  throw new Error(`failed to select harness matching ${optionPattern}`);
}
