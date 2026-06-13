import { argosScreenshot } from "@argos-ci/playwright";
import os from "os";
import path from "path";
import { expect, type Locator, type Page } from "playwright/test";

export type VisualTheme = "dark" | "light";
export type VisualViewportName =
  | "desktop"
  | "desktop-tight"
  | "narrow"
  | "fullpage"
  | "diff-wide";

type VisualViewport = {
  width: number;
  height: number;
  label: string;
};

type VisualReadyTarget = Locator | string;

type PrepareVisualPageOptions = {
  theme: VisualTheme;
  viewport: VisualViewportName;
  route?: string;
  ready?: VisualReadyTarget;
  waitMs?: number;
};

type CaptureVisualOptions = {
  fullPage?: boolean;
  ready?: VisualReadyTarget;
  waitMs?: number;
  masks?: Locator[];
};

const THEME_STORAGE_KEY = "ctx.theme.mode";
const VISUAL_STYLE_ID = "ctx-e2e-visual-stability";
const DEFAULT_WAIT_MS = 120;
const ARGOS_ROOT = path.join(process.env.CTX_E2E_DATA_DIR ?? os.tmpdir(), "argos-screenshots");
const VISUAL_STABILITY_CSS = `
  *,
  *::before,
  *::after {
    animation: none !important;
    caret-color: transparent !important;
    scroll-behavior: auto !important;
    transition: none !important;
  }
`;

export const VISUAL_VIEWPORTS: Record<VisualViewportName, VisualViewport> = {
  desktop: { width: 1400, height: 900, label: "desktop" },
  "desktop-tight": { width: 1000, height: 900, label: "desktop-tight" },
  narrow: { width: 760, height: 900, label: "narrow" },
  fullpage: { width: 1280, height: 900, label: "fullpage" },
  "diff-wide": { width: 1600, height: 900, label: "diff-wide" },
};

type VisualThemePayload = {
  css: string;
  storageKey: string;
  styleId: string;
  theme: VisualTheme;
};

const applyVisualThemeInBrowser = (payload: VisualThemePayload) => {
  const existing = document.getElementById(payload.styleId) as HTMLStyleElement | null;
  if (existing) {
    existing.textContent = payload.css;
  } else {
    const style = document.createElement("style");
    style.id = payload.styleId;
    style.textContent = payload.css;
    document.head.appendChild(style);
  }
  try {
    window.localStorage.setItem(payload.storageKey, payload.theme);
  } catch {
    // Ignore storage failures in the E2E harness.
  }
  document.documentElement.setAttribute("data-theme", payload.theme);
};

async function expectVisibleTarget(page: Page, target: VisualReadyTarget): Promise<void> {
  const locator = typeof target === "string" ? page.locator(target) : target;
  await expect(locator).toBeVisible({ timeout: 20_000 });
}

export async function setVisualTheme(page: Page, theme: VisualTheme): Promise<void> {
  const payload: VisualThemePayload = {
    css: VISUAL_STABILITY_CSS,
    storageKey: THEME_STORAGE_KEY,
    styleId: VISUAL_STYLE_ID,
    theme,
  };
  await page.emulateMedia({ colorScheme: theme, reducedMotion: "reduce" });
  await page.addInitScript(applyVisualThemeInBrowser, payload);
  await page.evaluate(applyVisualThemeInBrowser, payload);
  await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
}

export async function waitForVisualSettled(page: Page, opts: { ready?: VisualReadyTarget; waitMs?: number } = {}) {
  if (opts.ready) {
    await expectVisibleTarget(page, opts.ready);
  }
  await page.evaluate(async () => {
    if (document.fonts?.ready) {
      try {
        await document.fonts.ready;
      } catch {
        // Ignore font readiness failures inside the screenshot harness.
      }
    }
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
  });
  await page.waitForTimeout(opts.waitMs ?? DEFAULT_WAIT_MS);
}

export async function prepareVisualPage(page: Page, opts: PrepareVisualPageOptions): Promise<void> {
  await page.setViewportSize(VISUAL_VIEWPORTS[opts.viewport]);
  await setVisualTheme(page, opts.theme);
  if (opts.route) {
    await page.goto(opts.route, { waitUntil: "domcontentloaded" });
  }
  await waitForVisualSettled(page, { ready: opts.ready, waitMs: opts.waitMs });
}

export async function captureVisual(page: Page, name: string, opts: CaptureVisualOptions = {}) {
  await waitForVisualSettled(page, { ready: opts.ready, waitMs: opts.waitMs });
  await argosScreenshot(page, name, {
    root: ARGOS_ROOT,
    fullPage: opts.fullPage ?? true,
    ...(opts.masks && opts.masks.length > 0 ? { mask: opts.masks } : {}),
  });
}

export function buildVisualName(parts: Array<string | null | undefined>): string {
  return parts
    .filter((part): part is string => Boolean(part))
    .map((part) => part.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/(^-|-$)/g, ""))
    .filter(Boolean)
    .join("-");
}

export function visualViewportLabel(viewport: VisualViewportName): string {
  return VISUAL_VIEWPORTS[viewport].label;
}
