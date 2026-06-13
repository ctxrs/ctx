import { idToString, type Message } from "../api/client";
import { noteLateChunkAfterTerminal } from "./foregroundFreshnessTelemetry";

export type AssistantStreamingState = {
  content: string;
  providerMessageId: string | null;
  orderSeq: number | null;
};

export type AssistantStreamingStore = {
  assistantStreamingByTurnId: Record<string, AssistantStreamingState>;
  assistantStreamingRev: number;
  sealedAssistantTurnIds?: Set<string>;
};

function appendStreamingFragment(previous: string, fragment: string): string {
  if (!previous) return fragment;
  if (!fragment) return previous;
  if (fragment.startsWith(previous)) return fragment;
  if (previous.endsWith(fragment)) return previous;
  return `${previous}${fragment}`;
}

function normalizeTurnId(turnId: string | null | undefined): string {
  return idToString(turnId ?? "") || "";
}

function updateState(
  store: AssistantStreamingStore,
  turnId: string,
  next: AssistantStreamingState | null,
): boolean {
  const current = store.assistantStreamingByTurnId[turnId] ?? null;
  if (next === null) {
    if (!current) return false;
    const { [turnId]: _removed, ...rest } = store.assistantStreamingByTurnId;
    store.assistantStreamingByTurnId = rest;
    store.assistantStreamingRev += 1;
    return true;
  }
  if (
    current &&
    current.content === next.content &&
    current.providerMessageId === next.providerMessageId &&
    current.orderSeq === next.orderSeq
  ) {
    return false;
  }
  store.assistantStreamingByTurnId = {
    ...store.assistantStreamingByTurnId,
    [turnId]: next,
  };
  store.assistantStreamingRev += 1;
  return true;
}

function ensureSealedTurnSet(store: AssistantStreamingStore): Set<string> {
  if (store.sealedAssistantTurnIds) {
    return store.sealedAssistantTurnIds;
  }
  const next = new Set<string>();
  store.sealedAssistantTurnIds = next;
  return next;
}

export function clearAllAssistantStreaming(store: AssistantStreamingStore): boolean {
  const hadStreaming = Object.keys(store.assistantStreamingByTurnId).length > 0;
  const hadSealedTurns = (store.sealedAssistantTurnIds?.size ?? 0) > 0;
  if (!hadStreaming && !hadSealedTurns) return false;
  store.assistantStreamingByTurnId = {};
  if (store.sealedAssistantTurnIds) {
    store.sealedAssistantTurnIds = new Set<string>();
  }
  store.assistantStreamingRev += 1;
  return true;
}

export function clearAssistantStreaming(
  store: AssistantStreamingStore,
  turnId: string | null | undefined,
): boolean {
  const normalizedTurnId = normalizeTurnId(turnId);
  if (!normalizedTurnId) return false;
  return updateState(store, normalizedTurnId, null);
}

export function applyAssistantChunkToStreaming(
  store: AssistantStreamingStore,
  turnId: string | null | undefined,
  fragment: string,
  providerMessageId?: string | null,
  orderSeq?: number | null,
): boolean {
  const normalizedTurnId = normalizeTurnId(turnId);
  if (!normalizedTurnId || !fragment) return false;
  if (store.sealedAssistantTurnIds?.has(normalizedTurnId)) {
    noteLateChunkAfterTerminal(normalizedTurnId);
    return false;
  }
  const current = store.assistantStreamingByTurnId[normalizedTurnId] ?? null;
  const providerId = providerMessageId?.trim() || null;
  const normalizedOrderSeq = typeof orderSeq === "number" && Number.isFinite(orderSeq) ? orderSeq : null;
  const providerChanged = Boolean(providerId && current?.providerMessageId && providerId !== current.providerMessageId);
  const nextContent =
    providerChanged
      ? fragment
      : appendStreamingFragment(current?.content ?? "", fragment);
  return updateState(store, normalizedTurnId, {
    content: nextContent,
    providerMessageId: providerId ?? current?.providerMessageId ?? null,
    orderSeq: normalizedOrderSeq ?? (providerChanged ? null : current?.orderSeq ?? null),
  });
}

export function applyAssistantCompleteToStreaming(
  store: AssistantStreamingStore,
  turnId: string | null | undefined,
  fullContent: string,
  providerMessageId?: string | null,
  orderSeq?: number | null,
): boolean {
  const normalizedTurnId = normalizeTurnId(turnId);
  if (!normalizedTurnId) return false;
  const current = store.assistantStreamingByTurnId[normalizedTurnId] ?? null;
  const nextContent = String(fullContent || current?.content || "");
  if (!nextContent) return false;
  const sealedTurnIds = ensureSealedTurnSet(store);
  const addedSeal = !sealedTurnIds.has(normalizedTurnId);
  sealedTurnIds.add(normalizedTurnId);
  const changed = updateState(store, normalizedTurnId, {
    content: nextContent,
    providerMessageId: providerMessageId?.trim() || current?.providerMessageId || null,
    orderSeq: typeof orderSeq === "number" && Number.isFinite(orderSeq) ? orderSeq : current?.orderSeq ?? null,
  });
  if (addedSeal && !changed) {
    store.assistantStreamingRev += 1;
    return true;
  }
  return changed;
}

export function reconcileAssistantStreamingWithMessages(
  store: AssistantStreamingStore,
  incoming: Message[],
): boolean {
  let changed = false;
  for (const message of incoming) {
    if (message.role !== "assistant") continue;
    const normalizedTurnId = normalizeTurnId(message.turn_id);
    if (!normalizedTurnId) continue;
    const current = store.assistantStreamingByTurnId[normalizedTurnId];
    if (!current) continue;
    const pendingTrimmed = current.content.trim();
    const messageTrimmed = String(message.content ?? "").trim();
    if (!pendingTrimmed || pendingTrimmed !== messageTrimmed) continue;
    if (clearAssistantStreaming(store, normalizedTurnId)) {
      changed = true;
    }
  }
  return changed;
}
