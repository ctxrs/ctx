import type { MutableRefObject } from "react";
import type { WorkbenchListItem } from "./SessionPage.types";
import { debugItemSummary, debugStableKey, findFirstRenderedItemContractViolation } from "./sessionMessageListDataDebug";

type Params = {
  sessionId: string;
  showDebug: boolean;
  nextRaw: WorkbenchListItem[];
  current: WorkbenchListItem[];
  next: WorkbenchListItem[];
  contractViolationLoggedRef: MutableRefObject<{ sessionId: string; violationKey: string } | null>;
};

export function runSessionMessageListDevValidation(params: Params): void {
  const { sessionId, showDebug, nextRaw, current, next, contractViolationLoggedRef } = params;
  if (!(import.meta.env.DEV && showDebug)) return;

  const violation = findFirstRenderedItemContractViolation(nextRaw);
  if (violation) {
    const violationKey = `${violation.kind}:${violation.reason}:${violation.id}`;
    const previous = contractViolationLoggedRef.current;
    if (!previous || previous.sessionId !== sessionId || previous.violationKey !== violationKey) {
      contractViolationLoggedRef.current = { sessionId, violationKey };
      // eslint-disable-next-line no-console
      console.error("[MessageList][contract-violation]", {
        sessionId,
        ...violation,
      });
    }
  }

  const seen = new Set<string>();
  const dupes: string[] = [];
  for (const item of next) {
    const itemId = String(item?.id ?? "");
    if (!itemId) continue;
    if (seen.has(itemId)) dupes.push(itemId);
    else seen.add(itemId);
  }
  if (dupes.length > 0) {
    // eslint-disable-next-line no-console
    console.error("[MessageList] duplicate WorkbenchListItem.id values detected", {
      count: dupes.length,
      sample: dupes.slice(0, 10),
    });
  }

  const currentByStable = new Map<string, string>();
  const stableKeyCollisions: Array<{ stableKey: string; ids: string[] }> = [];
  for (const item of current) {
    const stableKey = debugStableKey(item);
    const itemId = String(item.id ?? "");
    if (!stableKey || !itemId) continue;
    const previous = currentByStable.get(stableKey);
    if (previous && previous !== itemId) {
      stableKeyCollisions.push({ stableKey, ids: [previous, itemId] });
    } else {
      currentByStable.set(stableKey, itemId);
    }
  }

  const nextByStable = new Map<string, string>();
  const stableIdChanges: Array<{ stableKey: string; from: string; to: string }> = [];
  for (const item of next) {
    const stableKey = debugStableKey(item);
    const itemId = String(item.id ?? "");
    if (!stableKey || !itemId) continue;
    const previous = nextByStable.get(stableKey);
    if (previous && previous !== itemId) {
      stableKeyCollisions.push({ stableKey, ids: [previous, itemId] });
      continue;
    }
    nextByStable.set(stableKey, itemId);
    const currentId = currentByStable.get(stableKey);
    if (currentId && currentId !== itemId) {
      stableIdChanges.push({ stableKey, from: currentId, to: itemId });
    }
  }

  if (stableIdChanges.length > 0) {
    const currentById = new Map(current.map((item) => [item.id, item] as const));
    const nextById = new Map(next.map((item) => [item.id, item] as const));
    const sample = stableIdChanges.slice(0, 10).map((change) => ({
      ...change,
      fromItem: debugItemSummary(currentById.get(change.from) ?? { id: change.from }),
      toItem: debugItemSummary(nextById.get(change.to) ?? { id: change.to }),
    }));
    // eslint-disable-next-line no-console
    console.warn("[MessageList] possible unstable WorkbenchListItem.id detected (stableKey id changed)", {
      sessionId,
      count: stableIdChanges.length,
      sample,
    });
  }

  if (stableKeyCollisions.length > 0) {
    // eslint-disable-next-line no-console
    console.warn("[MessageList] stableKey collisions detected (diagnostic key too weak or duplicate items)", {
      sessionId,
      count: stableKeyCollisions.length,
      sample: stableKeyCollisions.slice(0, 5),
    });
  }
}
