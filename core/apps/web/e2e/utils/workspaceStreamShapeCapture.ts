import type { Page } from "playwright/test";

export type StreamDeltaShapeViolation = {
  source: string;
  reason: string;
  type?: string;
  deltaIndex?: number;
  sessionId?: string;
  keys?: string[];
  rev?: number;
  snapshotRev?: number;
  bytes?: number;
};

export type StreamDeltaShapeStats = {
  checkedDeltas: number;
  headsBatchMessages: number;
  eventMessages: number;
  maxFrameBytes: number;
  maxDeltaBytes: number;
  violations: StreamDeltaShapeViolation[];
};

const forbiddenDeltaKeys = [
  "head",
  "heads",
  "turns",
  "messages",
  "events",
  "active_snapshot",
  "activeSnapshot",
  "active_heads",
  "activeHeads",
];

const isRecord = (value: unknown): value is Record<string, unknown> =>
  Boolean(value) && typeof value === "object" && !Array.isArray(value);

const jsonBytes = (value: unknown): number => {
  try {
    return new Blob([JSON.stringify(value)]).size;
  } catch {
    return 0;
  }
};

const payloadToString = (payload: unknown): string => {
  if (typeof payload === "string") return payload;
  if (payload instanceof Uint8Array) {
    return new TextDecoder().decode(payload);
  }
  return String(payload);
};

export function createEmptyStreamDeltaShapeStats(): StreamDeltaShapeStats {
  return {
    checkedDeltas: 0,
    headsBatchMessages: 0,
    eventMessages: 0,
    maxFrameBytes: 0,
    maxDeltaBytes: 0,
    violations: [],
  };
}

function inspectSessionHeadDelta(params: {
  delta: unknown;
  stats: StreamDeltaShapeStats;
  source: string;
  deltaIndex?: number;
  rev?: number;
  snapshotRev?: number;
}) {
  const { delta, stats, source, deltaIndex, rev, snapshotRev } = params;
  stats.checkedDeltas += 1;
  const bytes = jsonBytes(delta);
  stats.maxDeltaBytes = Math.max(stats.maxDeltaBytes, bytes);
  if (!isRecord(delta)) {
    stats.violations.push({
      source,
      reason: "session_head_delta payload is not an object",
      deltaIndex,
      rev,
      snapshotRev,
      bytes,
    });
    return;
  }
  const presentForbiddenKeys = forbiddenDeltaKeys.filter((key) => key in delta);
  if (presentForbiddenKeys.length > 0) {
    stats.violations.push({
      source,
      reason: "session_head_delta must be delta-only and must not carry history-shaped keys",
      type: "session_head_delta",
      deltaIndex,
      sessionId: typeof delta.session_id === "string" ? delta.session_id : undefined,
      keys: presentForbiddenKeys,
      rev,
      snapshotRev,
      bytes,
    });
  }
}

function inspectWorkspaceEvent(params: {
  event: unknown;
  stats: StreamDeltaShapeStats;
  source: string;
  rev?: number;
  snapshotRev?: number;
}) {
  const { event, stats, source, rev, snapshotRev } = params;
  if (!isRecord(event) || event.type !== "session_head_delta") return;
  inspectSessionHeadDelta({
    delta: event.delta,
    stats,
    source,
    rev,
    snapshotRev:
      typeof event.snapshot_rev === "number" ? event.snapshot_rev : snapshotRev,
  });
}

export function inspectWorkspaceStreamFrame(
  frame: unknown,
  stats: StreamDeltaShapeStats,
  source = "workspace_stream",
): void {
  if (!isRecord(frame)) return;
  const frameBytes = jsonBytes(frame);
  stats.maxFrameBytes = Math.max(stats.maxFrameBytes, frameBytes);
  const rev = typeof frame.rev === "number" ? frame.rev : undefined;

  if (frame.type === "heads_batch") {
    stats.headsBatchMessages += 1;
    const snapshotRev =
      typeof frame.snapshot_rev === "number"
        ? frame.snapshot_rev
        : typeof frame.snapshotRev === "number"
          ? frame.snapshotRev
          : undefined;
    const deltas = Array.isArray(frame.deltas) ? frame.deltas : [];
    for (const [deltaIndex, delta] of deltas.entries()) {
      inspectSessionHeadDelta({
        delta,
        stats,
        source,
        deltaIndex,
        rev,
        snapshotRev,
      });
    }
    return;
  }

  if (frame.type === "event") {
    stats.eventMessages += 1;
    inspectWorkspaceEvent({
      event: frame.event,
      stats,
      source,
      rev,
    });
    return;
  }

  inspectWorkspaceEvent({
    event: frame,
    stats,
    source,
    rev,
  });
}

export function attachWorkspaceStreamShapeCapture(page: Page): StreamDeltaShapeStats {
  const stats = createEmptyStreamDeltaShapeStats();
  page.on("websocket", (ws) => {
    if (!/\/api\/workspaces\/[^/]+\/active_snapshot\/stream/.test(ws.url())) return;
    ws.on("framereceived", (frame) => {
      try {
        inspectWorkspaceStreamFrame(
          JSON.parse(payloadToString(frame.payload)),
          stats,
          "workspace_stream",
        );
      } catch {
        return;
      }
    });
  });
  return stats;
}
