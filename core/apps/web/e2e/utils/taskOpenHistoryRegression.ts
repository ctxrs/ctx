import type { APIRequestContext, Page, Response } from "@playwright/test";

type JsonRecord = Record<string, unknown>;

type MessageListDebugTrace = {
  cause?: string;
  maxAbsScrollTopDeltaPx?: number;
  maxAbsFirstItemTopDeltaPx?: number;
  maxAbsScrollHeightDeltaPx?: number;
  sampleCount?: number;
  snapbackDetected?: boolean;
  startedAtMs?: number;
  finishedAtMs?: number;
};

export type MessageListDebugState = {
  seq: number;
  flashSeq: number;
  entries: JsonRecord[];
  flashTraces: MessageListDebugTrace[];
};

export type MessageListWindowSample = {
  atMs: number;
  sessionMounted: boolean;
  threadVisible: boolean;
  scrollerMounted: boolean;
  renderedItemCount: number;
  blankTailPx: number | null;
  scrollTop: number | null;
  clientHeight: number | null;
  scrollHeight: number | null;
  distanceFromMaxScrollPx: number | null;
  firstItemId: string | null;
  lastItemId: string | null;
};

export type SessionHistoryCaptureRecord = {
  beforeSeq: number | null;
  requestAtMs: number;
  responseAtMs: number;
  status: number;
  authorizationHeader: string | null;
  hasMore: boolean;
  nextCursor: number | null;
  turnCount: number;
  turnIds: string[];
  url: string;
};

export type SessionTurnCoverage = {
  initialTurnIds: string[];
  expectedTurnIds: string[];
};

type MessageListDebugWindow = Window & {
  __wbSessionMessageListDebug?: {
    seq: number;
    entries?: Array<unknown>;
    flashSeq?: number;
    flashTraces?: Array<unknown>;
  };
};

function asRecord(value: unknown): JsonRecord {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return {};
  }
  return value as JsonRecord;
}

function asArray(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function toFiniteNumber(value: unknown): number | null {
  if (typeof value !== "number" || !Number.isFinite(value)) return null;
  return value;
}

function toFiniteNumberFromString(value: string | null): number | null {
  if (value == null) return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

function extractTurnId(turn: unknown): string | null {
  const record = asRecord(turn);
  const id = record.turn_id;
  return typeof id === "string" && id.trim() ? id : null;
}

function extractStartSeq(turn: unknown): number | null {
  return toFiniteNumber(asRecord(turn).start_seq);
}

function collectTurnIds(turns: unknown[]): string[] {
  const ids = new Set<string>();
  for (const turn of turns) {
    const id = extractTurnId(turn);
    if (id) ids.add(id);
  }
  return Array.from(ids);
}

export async function clearMessageListDebugStore(page: Page): Promise<void> {
  await page.evaluate(() => {
    const windowRef = window as MessageListDebugWindow;
    windowRef.__wbSessionMessageListDebug = {
      seq: 0,
      entries: [],
      flashSeq: 0,
      flashTraces: [],
    };
  });
}

export async function readMessageListDebugStore(page: Page): Promise<MessageListDebugState> {
  return page.evaluate(() => {
    const windowRef = window as MessageListDebugWindow;
    const store = windowRef.__wbSessionMessageListDebug;
    return {
      seq: store?.seq ?? 0,
      flashSeq: store?.flashSeq ?? 0,
      entries: (Array.isArray(store?.entries) ? store.entries : []).slice(-100) as JsonRecord[],
      flashTraces: (Array.isArray(store?.flashTraces) ? store.flashTraces : []).slice(-40) as MessageListDebugTrace[],
    };
  });
}

export async function readSessionWindowSample(page: Page, sessionId: string): Promise<MessageListWindowSample> {
  return page.evaluate((targetSessionId: string) => {
    const sessionView = document.querySelector<HTMLElement>(
      `[data-testid="session-view"][data-session-id="${targetSessionId}"]`,
    );
    if (!sessionView) {
      return {
        atMs: performance.now(),
        sessionMounted: false,
        threadVisible: false,
        scrollerMounted: false,
        renderedItemCount: 0,
        blankTailPx: null,
        scrollTop: null,
        clientHeight: null,
        scrollHeight: null,
        distanceFromMaxScrollPx: null,
        firstItemId: null,
        lastItemId: null,
      };
    }

    const scroller = sessionView.querySelector<HTMLElement>(".wb-thread-scroller");
    if (!scroller) {
      return {
        atMs: performance.now(),
        sessionMounted: true,
        threadVisible: false,
        scrollerMounted: false,
        renderedItemCount: 0,
        blankTailPx: null,
        scrollTop: null,
        clientHeight: null,
        scrollHeight: null,
        distanceFromMaxScrollPx: null,
        firstItemId: null,
        lastItemId: null,
      };
    }

    const rows = Array.from(scroller.querySelectorAll<HTMLElement>("[role=\"listitem\"]"));
    const scrollerRect = scroller.getBoundingClientRect();
    const first = rows.at(0);
    const last = rows.at(-1);
    const firstRect = first?.getBoundingClientRect() ?? null;
    const lastRect = last?.getBoundingClientRect() ?? null;
    const scrollTop = scroller.scrollTop;
    const clientHeight = scroller.clientHeight;
    const scrollHeight = scroller.scrollHeight;
    const maxScrollTop = Math.max(0, scrollHeight - clientHeight);
    const blankTailPx =
      lastRect == null || Number.isNaN(lastRect.bottom) ? null : Math.max(0, scrollerRect.bottom - lastRect.bottom);

    return {
      atMs: performance.now(),
      sessionMounted: true,
      threadVisible: true,
      scrollerMounted: true,
      renderedItemCount: rows.length,
      blankTailPx,
      scrollTop,
      clientHeight,
      scrollHeight,
      distanceFromMaxScrollPx: Math.max(0, maxScrollTop - scrollTop),
      firstItemId: first?.getAttribute("data-thread-item-id"),
      lastItemId: last?.getAttribute("data-thread-item-id"),
    };
  }, sessionId);
}

export async function collectSessionTurnCoverageFromApi(
  request: APIRequestContext,
  sessionId: string,
  authToken?: string,
): Promise<SessionTurnCoverage> {
  const headers = authToken ? { authorization: `Bearer ${authToken}` } : undefined;
  const snapshotResp = await request.get(`/api/sessions/${sessionId}/snapshot`, { headers });
  if (!snapshotResp.ok()) {
    throw new Error(`snapshot request failed for ${sessionId}: ${snapshotResp.status()}`);
  }

  const snapshot = asRecord(await snapshotResp.json());
  const head = asRecord(snapshot.head);
  const headTurns = asArray(head.turns);
  const expectedTurnIds = new Set<string>(collectTurnIds(headTurns));
  const initialTurnIds = new Set<string>(collectTurnIds(headTurns));

  let hasMore = Boolean(head.has_more_turns);
  let beforeSeq = headTurns.length > 0 ? extractStartSeq(headTurns.at(0)) : null;
  let safety = 0;
  while (hasMore && Number.isFinite(beforeSeq)) {
    if (safety > 40) break;
    safety += 1;
    const historyResp = await request.get(
      `/api/sessions/${sessionId}/history?before_seq=${beforeSeq}`,
      { headers },
    );
    if (!historyResp.ok()) break;
    const page = asRecord(await historyResp.json());
    const pageTurns = asArray(page.turns);
    for (const id of collectTurnIds(pageTurns)) {
      expectedTurnIds.add(id);
    }

    hasMore = Boolean(page.has_more);
    const nextCursor = toFiniteNumber(page.next_cursor);
    if (!hasMore || !Number.isFinite(nextCursor) || nextCursor === beforeSeq) break;
    beforeSeq = nextCursor;
  }

  return {
    initialTurnIds: Array.from(initialTurnIds),
    expectedTurnIds: Array.from(expectedTurnIds),
  };
}

export async function collectSessionTurnCoverageFromPage(
  page: Page,
  sessionId: string,
  authToken?: string,
): Promise<SessionTurnCoverage> {
  return page.evaluate(async ({ targetSessionId, token }) => {
    const asRecord = (value: unknown): Record<string, unknown> =>
      value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
    const asArray = (value: unknown): unknown[] => (Array.isArray(value) ? value : []);
    const collectTurnIds = (turns: unknown[]): string[] =>
      turns
        .map((turn) => asRecord(turn).turn_id)
        .filter((value): value is string => typeof value === "string" && value.length > 0);
    const extractStartSeq = (turn: unknown): number | null => {
      const value = asRecord(turn).start_seq;
      return typeof value === "number" && Number.isFinite(value) ? value : null;
    };

    const withToken = (url: string) =>
      token
        ? `${url}${url.includes("?") ? "&" : "?"}token=${encodeURIComponent(token)}`
        : url;
    const readJson = async (url: string) => {
      const response = await fetch(withToken(url), { credentials: "same-origin" });
      if (!response.ok) {
        throw new Error(`fetch failed for ${withToken(url)}: ${response.status}`);
      }
      return asRecord(await response.json());
    };

    const snapshot = await readJson(`/api/sessions/${targetSessionId}/snapshot`);
    const head = asRecord(snapshot.head);
    const headTurns = asArray(head.turns);
    const expectedTurnIds = new Set<string>(collectTurnIds(headTurns));
    const initialTurnIds = new Set<string>(collectTurnIds(headTurns));

    let hasMore = Boolean(head.has_more_turns);
    let beforeSeq = headTurns.length > 0 ? extractStartSeq(headTurns.at(0)) : null;
    let safety = 0;
    while (hasMore && Number.isFinite(beforeSeq)) {
      if (safety > 40) break;
      safety += 1;
      const pageJson = await readJson(`/api/sessions/${targetSessionId}/history?before_seq=${beforeSeq}`);
      const pageTurns = asArray(pageJson.turns);
      for (const id of collectTurnIds(pageTurns)) {
        expectedTurnIds.add(id);
      }
      hasMore = Boolean(pageJson.has_more);
      const nextCursor = pageJson.next_cursor;
      if (typeof nextCursor !== "number" || !Number.isFinite(nextCursor) || nextCursor === beforeSeq) break;
      beforeSeq = nextCursor;
    }

    return {
      initialTurnIds: Array.from(initialTurnIds),
      expectedTurnIds: Array.from(expectedTurnIds),
    };
  }, { targetSessionId: sessionId, token: authToken ?? null });
}

export async function startSessionHistoryCapture(page: Page, sessionId: string) {
  const captures: SessionHistoryCaptureRecord[] = [];
  const sessionPath = `/api/sessions/${sessionId}/history`;
  const handler = async (response: Response) => {
    const request = response.request();
    const requestUrl = request.url();
    if (!requestUrl.includes(sessionPath)) return;
    const requestAtMs = Date.now();
    const parsed = new URL(requestUrl);
    const beforeSeq = toFiniteNumberFromString(parsed.searchParams.get("before_seq"));
    const authorizationHeader = request.headers()["authorization"] ?? null;
    const responseAtMs = Date.now();
    const status = response.status();
    const responseText = await response.text().catch(() => "");

    let payload: unknown = null;
    if (responseText) {
      try {
        payload = JSON.parse(responseText);
      } catch {
        payload = null;
      }
    }

    const responseRecord = asRecord(payload);
    const turns = asArray(responseRecord.turns);
    captures.push({
      beforeSeq,
      requestAtMs,
      responseAtMs,
      status,
      authorizationHeader,
      hasMore: Boolean(responseRecord.has_more),
      nextCursor: toFiniteNumber(responseRecord.next_cursor),
      turnCount: turns.length,
      turnIds: collectTurnIds(turns),
      url: requestUrl,
    });
  };

  page.on("response", handler);
  return {
    captures,
    stop: async () => {
      page.off("response", handler);
    },
  };
}
