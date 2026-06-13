import { writeFile } from "node:fs/promises";
import type { Locator, Page, TestInfo } from "playwright/test";

export const visibleSessionSelector = '.wb-session-slot[aria-hidden="false"] [data-testid="session-view"]';
export const visibleScrollerSelector = `${visibleSessionSelector} .wb-thread-scroller`;
export const visibleStatusSelector = `${visibleSessionSelector} .wb-turn-status-label`;

export type ThreadOverlap = {
  previousId: string;
  nextId: string;
  previousBottom: number;
  nextTop: number;
  previousText: string;
  nextText: string;
};

export type ThreadRowGeometry = {
  id: string;
  top: number;
  bottom: number;
  height: number;
  text: string;
  style: string | null;
  className: string;
  knownSize: string | null;
  dataIndex: string | null;
  parent: {
    tagName: string;
    className: string;
    style: string | null;
    dataIndex: string | null;
    knownSize: string | null;
    top: number;
    bottom: number;
    height: number;
  } | null;
};

export type ThreadGeometrySnapshot = {
  overlaps: ThreadOverlap[];
  visibleRows: ThreadRowGeometry[];
  scroller: {
    top: number;
    bottom: number;
    height: number;
    scrollTop: number;
    scrollHeight: number;
    clientHeight: number;
    overflow: number;
  } | null;
  sessionId: string | null;
};

export type StreamingEvidence = {
  assistantRowTexts: string[];
  pendingAssistantIds: string[];
  sessionId: string | null;
  statusTexts: string[];
};

type DiagnosticOptions = {
  debugLogs?: string[];
  expectedSessionId: string;
  extra?: Record<string, unknown>;
  prefix: string;
  step: string;
  testInfo: TestInfo;
};

type MessageListDebugWindow = Window & {
  __wbSessionMessageListDebug?: {
    entries: Array<{ sessionId?: string | null } & Record<string, unknown>>;
  };
};

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

const cssAttrValue = (value: string) => value.replace(/\\/gu, "\\\\").replace(/"/gu, '\\"');

const visibleSessionViewLocator = (page: Page, sessionId: string): Locator =>
  page.locator(`${visibleSessionSelector}[data-session-id="${cssAttrValue(sessionId)}"]`).first();

const readMountedSessionSummary = async (page: Page) =>
  page.locator(".wb-session-slot").evaluateAll((nodes) =>
    nodes.map((node) => {
      const slot = node as HTMLElement;
      const sessionView = slot.querySelector('[data-testid="session-view"]') as HTMLElement | null;
      const rect = slot.getBoundingClientRect();
      return {
        slotAriaHidden: slot.getAttribute("aria-hidden"),
        slotClassName: slot.className,
        slotStyle: slot.getAttribute("style"),
        slotRect: {
          top: rect.top,
          bottom: rect.bottom,
          height: rect.height,
          width: rect.width,
        },
        sessionId: sessionView?.getAttribute("data-session-id") ?? null,
      };
    }),
  );

const readSessionMessageListDebug = async (page: Page, sessionId: string) =>
  page.evaluate((targetSessionId) => {
    const store = (window as MessageListDebugWindow).__wbSessionMessageListDebug;
    if (!store) return [];
    return store.entries.filter((entry) => entry.sessionId === targetSessionId).slice(-60);
  }, sessionId);

export async function readThreadGeometryFromLocator(sessionView: Locator): Promise<ThreadGeometrySnapshot> {
  return sessionView.evaluate((root) => {
    const scroller = root.querySelector(".wb-thread-scroller") as HTMLElement | null;
    const scrollerRect = scroller?.getBoundingClientRect() ?? null;
    const visibleRows = Array.from(
      root.querySelectorAll('.wb-thread-scroller [role="listitem"][data-thread-item-id]'),
    )
      .map((node) => {
        const el = node as HTMLElement;
        const rect = el.getBoundingClientRect();
        const parent = el.parentElement as HTMLElement | null;
        const parentRect = parent?.getBoundingClientRect() ?? null;
        return {
          id: el.getAttribute("data-thread-item-id") ?? "",
          top: rect.top,
          bottom: rect.bottom,
          height: rect.height,
          text: (el.innerText || "").slice(0, 180),
          style: el.getAttribute("style"),
          className: el.className,
          knownSize: parent?.getAttribute("data-known-size") ?? null,
          dataIndex: parent?.getAttribute("data-index") ?? null,
          parent: parentRect
            ? {
                tagName: parent?.tagName ?? "",
                className: parent?.className ?? "",
                style: parent?.getAttribute("style") ?? null,
                dataIndex: parent?.getAttribute("data-index") ?? null,
                knownSize: parent?.getAttribute("data-known-size") ?? null,
                top: parentRect.top,
                bottom: parentRect.bottom,
                height: parentRect.height,
              }
            : null,
        };
      })
      .filter((row) => {
        if (!scrollerRect) return row.height > 1;
        return row.height > 1 && row.bottom > scrollerRect.top + 1 && row.top < scrollerRect.bottom - 1;
      })
      .sort((left, right) => left.top - right.top);

    const overlaps: ThreadOverlap[] = [];
    for (let index = 1; index < visibleRows.length; index += 1) {
      const previous = visibleRows[index - 1];
      const next = visibleRows[index];
      if (next.top < previous.bottom - 1) {
        overlaps.push({
          previousId: previous.id,
          nextId: next.id,
          previousBottom: previous.bottom,
          nextTop: next.top,
          previousText: previous.text,
          nextText: next.text,
        });
      }
    }

    return {
      overlaps,
      visibleRows,
      scroller: scrollerRect
        ? {
            top: scrollerRect.top,
            bottom: scrollerRect.bottom,
            height: scrollerRect.height,
            scrollTop: scroller?.scrollTop ?? 0,
            scrollHeight: scroller?.scrollHeight ?? 0,
            clientHeight: scroller?.clientHeight ?? 0,
            overflow: Math.max(0, (scroller?.scrollHeight ?? 0) - (scroller?.clientHeight ?? 0)),
          }
        : null,
      sessionId: root.getAttribute("data-session-id"),
    };
  });
}

export async function readVisibleThreadGeometry(page: Page, sessionId: string): Promise<ThreadGeometrySnapshot> {
  return readThreadGeometryFromLocator(visibleSessionViewLocator(page, sessionId));
}

export async function readThreadGeometryBySession(
  page: Page,
  sessionId: string,
): Promise<ThreadGeometrySnapshot | null> {
  const sessionView = page.locator(`[data-testid="session-view"][data-session-id="${cssAttrValue(sessionId)}"]`).first();
  if ((await sessionView.count()) === 0) return null;
  return readThreadGeometryFromLocator(sessionView);
}

export async function readStreamingEvidence(page: Page, sessionId: string): Promise<StreamingEvidence | null> {
  const sessionView = visibleSessionViewLocator(page, sessionId);
  if ((await sessionView.count()) === 0 || !(await sessionView.isVisible())) return null;
  return sessionView.evaluate((root) => {
    const isVisible = (el: Element) => {
      const node = el as HTMLElement;
      const rect = node.getBoundingClientRect();
      const style = window.getComputedStyle(node);
      return rect.width > 0 && rect.height > 0 && style.display !== "none" && style.visibility !== "hidden";
    };
    const assistantRows = Array.from(
      root.querySelectorAll('[data-thread-item-id^="assistant-"]'),
    ).filter(isVisible) as HTMLElement[];
    const pendingAssistantRows = assistantRows.filter((row) =>
      (row.getAttribute("data-thread-item-id") ?? "").endsWith("-pending"),
    );
    const statusTexts = Array.from(root.querySelectorAll(".wb-turn-status-label"))
      .filter(isVisible)
      .map((node) => ((node as HTMLElement).innerText || "").trim())
      .filter(Boolean);
    return {
      assistantRowTexts: assistantRows.map((row) => (row.innerText || "").slice(0, 180)),
      pendingAssistantIds: pendingAssistantRows.map((row) => row.getAttribute("data-thread-item-id") ?? ""),
      sessionId: root.getAttribute("data-session-id"),
      statusTexts,
    };
  });
}

export async function attachThreadDiagnostics(
  page: Page,
  {
    debugLogs = [],
    expectedSessionId,
    extra = {},
    prefix,
    step,
    testInfo,
  }: DiagnosticOptions,
): Promise<void> {
  let geometry: ThreadGeometrySnapshot | null = null;
  let streamingEvidence: StreamingEvidence | null = null;
  let geometryError = "";
  try {
    geometry = await readVisibleThreadGeometry(page, expectedSessionId);
  } catch (error) {
    geometryError = error instanceof Error ? error.message : String(error);
  }
  try {
    streamingEvidence = await readStreamingEvidence(page, expectedSessionId);
  } catch {
    streamingEvidence = null;
  }
  const mountedSessions = await readMountedSessionSummary(page);
  const sessionMessageListDebug = await readSessionMessageListDebug(page, expectedSessionId);
  const screenshotPath = testInfo.outputPath(`${prefix}-${step}.png`);
  const detailsPath = testInfo.outputPath(`${prefix}-${step}.json`);
  await page.screenshot({ path: screenshotPath, fullPage: true });
  await writeFile(
    detailsPath,
    JSON.stringify(
      {
        debugLogs,
        expectedSessionId,
        extra,
        geometry,
        geometryError,
        mountedSessions,
        sessionMessageListDebug,
        streamingEvidence,
      },
      null,
      2,
    ),
    "utf8",
  );
  await testInfo.attach(`${prefix}-${step}.png`, {
    path: screenshotPath,
    contentType: "image/png",
  });
  await testInfo.attach(`${prefix}-${step}.json`, {
    path: detailsPath,
    contentType: "application/json",
  });
}

export async function failWithThreadDiagnostics(
  page: Page,
  options: DiagnosticOptions & { message: string },
): Promise<never> {
  await attachThreadDiagnostics(page, options);
  throw new Error(options.message);
}

export async function requireVisibleSession(
  page: Page,
  options: DiagnosticOptions & { timeoutMs?: number },
): Promise<void> {
  const timeoutMs = options.timeoutMs ?? 20_000;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const sessionView = visibleSessionViewLocator(page, options.expectedSessionId);
    if ((await sessionView.count()) > 0 && (await sessionView.isVisible())) {
      const scroller = sessionView.locator(".wb-thread-scroller").first();
      if ((await scroller.count()) > 0 && (await scroller.isVisible())) {
        return;
      }
    }
    await sleep(100);
  }
  await failWithThreadDiagnostics(page, {
    ...options,
    message: `visible session setup failed at ${options.step}: expected visible session ${options.expectedSessionId}`,
  });
}

export async function requireThreadOverflow(
  page: Page,
  options: DiagnosticOptions & { minOverflow?: number; timeoutMs?: number },
): Promise<ThreadGeometrySnapshot> {
  const minOverflow = options.minOverflow ?? 300;
  const timeoutMs = options.timeoutMs ?? 20_000;
  const deadline = Date.now() + timeoutMs;
  let lastGeometry: ThreadGeometrySnapshot | null = null;
  let lastError = "";
  while (Date.now() < deadline) {
    try {
      const geometry = await readVisibleThreadGeometry(page, options.expectedSessionId);
      lastGeometry = geometry;
      if (geometry.scroller && geometry.scroller.overflow > minOverflow) return geometry;
    } catch (error) {
      lastError = error instanceof Error ? error.message : String(error);
    }
    await sleep(100);
  }
  await failWithThreadDiagnostics(page, {
    ...options,
    extra: {
      ...(options.extra ?? {}),
      lastError,
      lastGeometry,
      minOverflow,
    },
    message: `thread overflow setup failed at ${options.step}: expected overflow > ${minOverflow}`,
  });
}

export async function scrollThreadToFraction(
  page: Page,
  options: DiagnosticOptions & { fraction: number; minOverflow?: number; timeoutMs?: number },
): Promise<number> {
  const geometry = await requireThreadOverflow(page, options);
  const targetTop = await visibleSessionViewLocator(page, options.expectedSessionId).evaluate((root, targetFraction) => {
    const scroller = root.querySelector(".wb-thread-scroller") as HTMLElement | null;
    if (!scroller) return 0;
    const maxTop = Math.max(0, scroller.scrollHeight - scroller.clientHeight);
    const desiredTop = Math.round(maxTop * targetFraction);
    scroller.scrollTop = desiredTop;
    scroller.dispatchEvent(new Event("scroll"));
    return desiredTop;
  }, options.fraction);
  const timeoutMs = options.timeoutMs ?? 20_000;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const currentTop = await visibleSessionViewLocator(page, options.expectedSessionId).evaluate((root, desiredTop) => {
      const scroller = root.querySelector(".wb-thread-scroller") as HTMLElement | null;
      if (!scroller) return 0;
      if (scroller.scrollTop < desiredTop - 16) {
        scroller.scrollTop = desiredTop;
        scroller.dispatchEvent(new Event("scroll"));
      }
      return scroller.scrollTop;
    }, targetTop);
    if (currentTop > Math.min(targetTop, 100) || Math.abs(currentTop - targetTop) <= 16) {
      return currentTop;
    }
    await sleep(100);
  }
  await failWithThreadDiagnostics(page, {
    ...options,
    extra: {
      ...(options.extra ?? {}),
      initialGeometry: geometry,
      targetTop,
    },
    message: `thread scroll setup failed at ${options.step}: targetTop=${targetTop}`,
  });
}

export async function assertNoVisibleRowOverlap(
  page: Page,
  options: DiagnosticOptions,
): Promise<void> {
  let geometry: ThreadGeometrySnapshot;
  try {
    geometry = await readVisibleThreadGeometry(page, options.expectedSessionId);
  } catch (error) {
    await failWithThreadDiagnostics(page, {
      ...options,
      extra: {
        ...(options.extra ?? {}),
        readError: error instanceof Error ? error.message : String(error),
        setupIssue: true,
      },
      message: `${options.prefix} setup failure at ${options.step}: expected visible session ${options.expectedSessionId}`,
    });
  }
  if (
    geometry.sessionId === options.expectedSessionId
    && geometry.visibleRows.length > 0
    && geometry.overlaps.length === 0
  ) {
    return;
  }
  const setupIssue = geometry.sessionId !== options.expectedSessionId || geometry.visibleRows.length === 0;
  await failWithThreadDiagnostics(page, {
    ...options,
    extra: {
      ...(options.extra ?? {}),
      geometry,
      setupIssue,
    },
    message: `${options.prefix} ${setupIssue ? "setup failure" : "row overlap"} at ${options.step}: expectedSession=${options.expectedSessionId} actualSession=${geometry.sessionId} visibleRows=${geometry.visibleRows.length} visibleOverlaps=${geometry.overlaps.length}`,
  });
}

export async function waitForStreamingObserved(
  page: Page,
  options: DiagnosticOptions & { timeoutMs?: number },
): Promise<StreamingEvidence> {
  const timeoutMs = options.timeoutMs ?? 20_000;
  const deadline = Date.now() + timeoutMs;
  let lastEvidence: StreamingEvidence | null = null;
  while (Date.now() < deadline) {
    const evidence = await readStreamingEvidence(page, options.expectedSessionId);
    lastEvidence = evidence;
    const statusText = evidence?.statusTexts.join(" ") ?? "";
    const hasRunningStatus = /\b(Working|Queued)\b/u.test(statusText);
    if (evidence && (evidence.pendingAssistantIds.length > 0 || hasRunningStatus)) {
      return evidence;
    }
    await sleep(100);
  }
  await failWithThreadDiagnostics(page, {
    ...options,
    extra: {
      ...(options.extra ?? {}),
      lastEvidence,
    },
    message: `streaming setup failed at ${options.step}: no pending assistant row or running status observed for ${options.expectedSessionId}`,
  });
}
