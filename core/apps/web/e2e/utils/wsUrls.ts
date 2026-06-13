import { expect } from "../fixtures";
import type { Page } from "playwright/test";

type E2EWindow = Window & {
  __ctxE2E?: {
    clearOpenedWebSocketUrls?: () => void;
    getOpenedWebSocketUrls?: () => unknown[];
    workspaceStream?: {
      getCanonicalUrl?: () => string | null;
    };
  };
};

export async function clearOpenedWebSocketUrls(page: Page): Promise<void> {
  await page.evaluate(() => {
    (window as E2EWindow).__ctxE2E?.clearOpenedWebSocketUrls?.();
  });
}

export async function getOpenedWebSocketUrls(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const urls = (window as E2EWindow).__ctxE2E?.getOpenedWebSocketUrls?.();
    return Array.isArray(urls) ? urls.map((value) => String(value)) : [];
  });
}

const readCanonicalWsBaseUrl = async (page: Page): Promise<string | null> => {
  return page.evaluate(() => {
    const readOrigin = (value: unknown): string | null => {
      if (typeof value !== "string") return null;
      const trimmed = value.trim();
      if (!trimmed) return null;
      try {
        return new URL(trimmed).origin;
      } catch {
        return null;
      }
    };
    try {
      const raw = window.sessionStorage.getItem("ctxDaemonConnectionV1");
      if (raw) {
        const parsed = JSON.parse(raw) as { wsBaseUrl?: unknown };
        const sessionOrigin = readOrigin(parsed.wsBaseUrl);
        if (sessionOrigin) return sessionOrigin;
      }
    } catch {
      // ignore session storage parsing and fall back to stream URL
    }
    const streamCanonicalUrl = (window as E2EWindow).__ctxE2E?.workspaceStream?.getCanonicalUrl?.() ?? null;
    return readOrigin(streamCanonicalUrl);
  });
};

export async function expectWsPathOnCanonicalOrigin(page: Page, pathFragment: string): Promise<void> {
  const expectedOrigin = await readCanonicalWsBaseUrl(page);
  expect(expectedOrigin, "Missing canonical websocket origin from session storage and stream state.").toBeTruthy();
  const urls = await getOpenedWebSocketUrls(page);
  let matching = urls.filter((url) => url.includes(pathFragment));
  if (matching.length === 0 && pathFragment.includes("/api/workspaces/")) {
    const workspaceStreamUrl = await page.evaluate(() => {
      return (window as E2EWindow).__ctxE2E?.workspaceStream?.getCanonicalUrl?.() ?? null;
    });
    if (typeof workspaceStreamUrl === "string" && workspaceStreamUrl.includes(pathFragment)) {
      matching = [workspaceStreamUrl];
    }
  }
  expect(matching.length, `Expected at least one websocket URL containing ${pathFragment}. URLs: ${JSON.stringify(urls, null, 2)}`)
    .toBeGreaterThan(0);
  for (const url of matching) {
    expect(new URL(url).origin).toBe(expectedOrigin);
  }
}
