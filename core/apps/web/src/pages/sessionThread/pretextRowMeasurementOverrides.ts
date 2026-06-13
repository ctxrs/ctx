import type { WorkbenchListItem } from "../SessionPage.types";
import { SESSION_TRANSCRIPT_LAYOUT_ENGINE_REVISION } from "./sessionMarkdownMeasurement";
import type { PretextVirtualizerMessageLayout } from "./transcriptLayoutPlanner.app";

const ROW_MEASUREMENT_OVERRIDE_CACHE_LIMIT = 4000;

const rowMeasurementOverrides = new Map<string, number>();

function normalizeHeight(value: number): number {
  return Number.isFinite(value) && value > 0 ? Math.max(1, Math.round(value * 16) / 16) : 0;
}

function fingerprintString(value: string): string {
  const normalized = String(value ?? "");
  let hash = 2166136261;
  for (let index = 0; index < normalized.length; index += 1) {
    hash ^= normalized.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return `${normalized.length}:${(hash >>> 0).toString(36)}`;
}

function pruneRowMeasurementOverrides(): void {
  while (rowMeasurementOverrides.size > ROW_MEASUREMENT_OVERRIDE_CACHE_LIMIT) {
    const oldestKey = rowMeasurementOverrides.keys().next().value;
    if (typeof oldestKey !== "string") {
      break;
    }
    rowMeasurementOverrides.delete(oldestKey);
  }
}

function fingerprintAttachments(
  attachments: ReadonlyArray<{
    kind?: string;
    name?: string | null;
    mime_type?: string | null;
  }>,
): string {
  return fingerprintString(
    JSON.stringify(
      attachments.map((attachment) => ({
        kind: attachment.kind ?? "",
        name: attachment.name ?? "",
        mimeType: attachment.mime_type ?? "",
      })),
    ) ?? "",
  );
}

function buildPretextRowMeasurementOverrideKey(params: {
  rowKind: "assistant" | "message";
  sessionId: string | null | undefined;
  rowId: string;
  viewportWidth: number;
  layoutParts: readonly string[];
  contentFingerprint: string;
}): string | null {
  const sessionId = String(params.sessionId ?? "").trim();
  if (!sessionId) {
    return null;
  }
  return [
    SESSION_TRANSCRIPT_LAYOUT_ENGINE_REVISION,
    params.rowKind,
    sessionId,
    params.rowId,
    Math.max(1, Math.round(params.viewportWidth)),
    ...params.layoutParts,
    params.contentFingerprint,
  ].join(":");
}

function readPretextRowMeasurementOverride(key: string | null): number | null {
  if (!key) {
    return null;
  }
  return rowMeasurementOverrides.get(key) ?? null;
}

function writePretextRowMeasurementOverride(params: {
  key: string | null;
  height: number;
}): boolean {
  const normalizedHeight = normalizeHeight(params.height);
  if (!params.key || normalizedHeight <= 0) {
    return false;
  }
  if (rowMeasurementOverrides.get(params.key) === normalizedHeight) {
    return false;
  }
  rowMeasurementOverrides.set(params.key, normalizedHeight);
  pruneRowMeasurementOverrides();
  return true;
}

export function clearPretextRowMeasurementOverrides(): void {
  rowMeasurementOverrides.clear();
}

export function buildPretextAssistantHeightOverrideKey(params: {
  sessionId: string | null | undefined;
  item: Extract<WorkbenchListItem, { kind: "assistant" }>;
  viewportWidth: number;
}): string | null {
  return buildPretextRowMeasurementOverrideKey({
    rowKind: "assistant",
    sessionId: params.sessionId,
    rowId: params.item.id,
    viewportWidth: params.viewportWidth,
    layoutParts: [params.item.is_complete ? "complete" : "partial"],
    contentFingerprint: fingerprintString(params.item.content),
  });
}

export function readPretextAssistantHeightOverride(params: {
  sessionId: string | null | undefined;
  item: Extract<WorkbenchListItem, { kind: "assistant" }>;
  viewportWidth: number;
}): number | null {
  return readPretextRowMeasurementOverride(buildPretextAssistantHeightOverrideKey(params));
}

export function writePretextAssistantHeightOverride(params: {
  sessionId: string | null | undefined;
  item: Extract<WorkbenchListItem, { kind: "assistant" }>;
  viewportWidth: number;
  height: number;
}): boolean {
  return writePretextRowMeasurementOverride({
    key: buildPretextAssistantHeightOverrideKey(params),
    height: params.height,
  });
}

export function buildPretextMessageHeightOverrideKey(params: {
  sessionId: string | null | undefined;
  item: Extract<WorkbenchListItem, { kind: "message" }>;
  viewportWidth: number;
  layout: PretextVirtualizerMessageLayout;
}): string | null {
  return buildPretextRowMeasurementOverrideKey({
    rowKind: "message",
    sessionId: params.sessionId,
    rowId: params.item.id,
    viewportWidth: params.viewportWidth,
    layoutParts: [
      params.layout.expanded ? "expanded" : "collapsed",
      params.layout.renderMode,
      fingerprintAttachments(params.item.attachments),
    ],
    contentFingerprint: fingerprintString(params.layout.shownContent),
  });
}

export function readPretextMessageHeightOverride(params: {
  sessionId: string | null | undefined;
  item: Extract<WorkbenchListItem, { kind: "message" }>;
  viewportWidth: number;
  layout: PretextVirtualizerMessageLayout;
}): number | null {
  return readPretextRowMeasurementOverride(buildPretextMessageHeightOverrideKey(params));
}

export function writePretextMessageHeightOverride(params: {
  sessionId: string | null | undefined;
  item: Extract<WorkbenchListItem, { kind: "message" }>;
  viewportWidth: number;
  layout: PretextVirtualizerMessageLayout;
  height: number;
}): boolean {
  return writePretextRowMeasurementOverride({
    key: buildPretextMessageHeightOverrideKey(params),
    height: params.height,
  });
}
