import type { WorkbenchListItem } from "./SessionPage.types";
import type { WorkbenchMessageListUiState } from "./sessionMessageListItemIdentity";

export type WorkbenchThreadProjectionOpKind =
  | "noop"
  | "replace_session"
  | "append_stream"
  | "prepend_history"
  | "hydrate_tools"
  | "terminalize_turn"
  | "toggle_expansion"
  | "reconcile";

export type WorkbenchThreadProjectionOp = {
  kind: WorkbenchThreadProjectionOpKind;
  projectionRevision: number;
  changedItemIds: string[];
  remeasureItemIds: string[];
};

function expandLocalizedRemeasureItemIds(
  listItems: readonly WorkbenchListItem[],
  changedItemIds: readonly string[],
): string[] {
  if (changedItemIds.length === 0 || listItems.length === 0) {
    return [];
  }

  const remeasureIds = new Set<string>();
  const changedSet = new Set(changedItemIds);
  for (let index = 0; index < listItems.length; index += 1) {
    const item = listItems[index];
    if (!item || !changedSet.has(item.id)) continue;
    remeasureIds.add(item.id);
    const previous = listItems[index - 1];
    const next = listItems[index + 1];
    if (previous) remeasureIds.add(previous.id);
    if (next) remeasureIds.add(next.id);
  }

  return Array.from(remeasureIds);
}

function dedupeIds(ids: readonly string[]): string[] {
  return Array.from(new Set(ids.filter((id) => id.trim().length > 0)));
}

export function createWorkbenchThreadProjectionOp(
  kind: WorkbenchThreadProjectionOpKind,
  projectionRevision: number,
  changedItemIds: readonly string[] = [],
  remeasureItemIds: readonly string[] = changedItemIds,
): WorkbenchThreadProjectionOp {
  const changed = dedupeIds(changedItemIds);
  const remeasure = dedupeIds(remeasureItemIds);
  if (kind !== "replace_session" && changed.length === 0 && remeasure.length === 0) {
    return {
      kind: "noop",
      projectionRevision,
      changedItemIds: [],
      remeasureItemIds: [],
    };
  }
  return {
    kind,
    projectionRevision,
    changedItemIds: changed,
    remeasureItemIds: remeasure,
  };
}

function startsWithIds(current: readonly WorkbenchListItem[], next: readonly WorkbenchListItem[]): boolean {
  if (next.length < current.length) return false;
  for (let index = 0; index < current.length; index += 1) {
    if (current[index]?.id !== next[index]?.id) return false;
  }
  return true;
}

function startsWithSameItems(current: readonly WorkbenchListItem[], next: readonly WorkbenchListItem[]): boolean {
  if (next.length < current.length) return false;
  for (let index = 0; index < current.length; index += 1) {
    if (current[index] !== next[index]) return false;
  }
  return true;
}

function endsWithIds(current: readonly WorkbenchListItem[], next: readonly WorkbenchListItem[]): boolean {
  if (next.length < current.length) return false;
  const offset = next.length - current.length;
  for (let index = 0; index < current.length; index += 1) {
    if (current[index]?.id !== next[offset + index]?.id) return false;
  }
  return true;
}

function endsWithSameItems(current: readonly WorkbenchListItem[], next: readonly WorkbenchListItem[]): boolean {
  if (next.length < current.length) return false;
  const offset = next.length - current.length;
  for (let index = 0; index < current.length; index += 1) {
    if (current[index] !== next[offset + index]) return false;
  }
  return true;
}

function hasSameIdSequence(current: readonly WorkbenchListItem[], next: readonly WorkbenchListItem[]): boolean {
  if (current.length !== next.length) return false;
  for (let index = 0; index < current.length; index += 1) {
    if (current[index]?.id !== next[index]?.id) return false;
  }
  return true;
}

function collectChangedItemIds(
  current: readonly WorkbenchListItem[],
  next: readonly WorkbenchListItem[],
): string[] {
  const changed = new Set<string>();
  const nextById = new Map(next.map((item) => [item.id, item] as const));

  for (const currentItem of current) {
    const nextItem = nextById.get(currentItem.id);
    if (!nextItem || nextItem !== currentItem) {
      changed.add(currentItem.id);
      if (nextItem) changed.add(nextItem.id);
    }
  }
  for (const nextItem of next) {
    if (!current.some((currentItem) => currentItem.id === nextItem.id)) {
      changed.add(nextItem.id);
    }
  }

  return Array.from(changed);
}

function turnTerminalized(current: WorkbenchListItem, next: WorkbenchListItem): boolean {
  if (current.kind === "turn_status" && next.kind === "turn_status") {
    const currentMutable =
      current.status === "running" || current.status === "starting" || current.status === "queued";
    const nextMutable = next.status === "running" || next.status === "starting" || next.status === "queued";
    return currentMutable && !nextMutable;
  }
  if (current.kind === "assistant" && next.kind === "assistant") {
    return !current.is_complete && next.is_complete;
  }
  if (current.kind === "tool" && next.kind === "tool") {
    const currentMutable = /running|pending|queued|starting/i.test(current.status ?? "");
    const nextMutable = /running|pending|queued|starting/i.test(next.status ?? "");
    return currentMutable && !nextMutable;
  }
  if (current.kind === "tool_group" && next.kind === "tool_group") {
    const currentMutable = current.tool_pending > 0 || current.tool_running > 0;
    const nextMutable = next.tool_pending > 0 || next.tool_running > 0;
    return currentMutable && !nextMutable;
  }
  return false;
}

function toolHydrated(current: WorkbenchListItem, next: WorkbenchListItem): boolean {
  return (
    (current.kind === "tool" && next.kind === "tool") ||
    (current.kind === "tool_group" && next.kind === "tool_group")
  );
}

export function classifyWorkbenchThreadProjectionOp(params: {
  current: readonly WorkbenchListItem[];
  next: readonly WorkbenchListItem[];
  projectionRevision: number;
  fallbackKind?: WorkbenchThreadProjectionOpKind;
}): WorkbenchThreadProjectionOp {
  const { current, next, projectionRevision, fallbackKind = "reconcile" } = params;

  if (current.length === 0) {
    return createWorkbenchThreadProjectionOp(
      "replace_session",
      projectionRevision,
      next.map((item) => item.id),
    );
  }

  if (next.length > current.length && startsWithIds(current, next) && startsWithSameItems(current, next)) {
    const changedItemIds = next.slice(current.length).map((item) => item.id);
    return createWorkbenchThreadProjectionOp(
      "append_stream",
      projectionRevision,
      changedItemIds,
      expandLocalizedRemeasureItemIds(next, changedItemIds),
    );
  }

  if (next.length > current.length && endsWithIds(current, next) && endsWithSameItems(current, next)) {
    const changedItemIds = next.slice(0, next.length - current.length).map((item) => item.id);
    return createWorkbenchThreadProjectionOp(
      "prepend_history",
      projectionRevision,
      changedItemIds,
      expandLocalizedRemeasureItemIds(next, changedItemIds),
    );
  }

  const changedItemIds = collectChangedItemIds(current, next);
  if (changedItemIds.length === 0) {
    return createWorkbenchThreadProjectionOp("noop", projectionRevision);
  }

  if (hasSameIdSequence(current, next)) {
    for (let index = 0; index < current.length; index += 1) {
      if (current[index]?.id !== next[index]?.id) continue;
      if (turnTerminalized(current[index]!, next[index]!)) {
        return createWorkbenchThreadProjectionOp(
          "terminalize_turn",
          projectionRevision,
          changedItemIds,
          changedItemIds,
        );
      }
    }
    for (let index = 0; index < current.length; index += 1) {
      if (current[index]?.id !== next[index]?.id) continue;
      if (toolHydrated(current[index]!, next[index]!)) {
        return createWorkbenchThreadProjectionOp(
          "hydrate_tools",
          projectionRevision,
          changedItemIds,
          changedItemIds,
        );
      }
    }
  }

  return createWorkbenchThreadProjectionOp(
    fallbackKind,
    projectionRevision,
    changedItemIds,
    expandLocalizedRemeasureItemIds(next, changedItemIds),
  );
}

function diffTrueKeys(
  previous: Readonly<Record<string, boolean>>,
  next: Readonly<Record<string, boolean>>,
): Set<string> {
  const changed = new Set<string>();
  for (const key of Object.keys(previous)) {
    if ((previous[key] ?? false) !== (next[key] ?? false)) changed.add(key);
  }
  for (const key of Object.keys(next)) {
    if ((previous[key] ?? false) !== (next[key] ?? false)) changed.add(key);
  }
  return changed;
}

function diffLoadingTurnIds(previous: readonly string[], next: readonly string[]): Set<string> {
  const changed = new Set<string>();
  const previousSet = new Set(previous);
  const nextSet = new Set(next);
  for (const turnId of previousSet) {
    if (!nextSet.has(turnId)) changed.add(turnId);
  }
  for (const turnId of nextSet) {
    if (!previousSet.has(turnId)) changed.add(turnId);
  }
  return changed;
}

export function createWorkbenchLayoutProjectionOp(params: {
  listItems: readonly WorkbenchListItem[];
  previousUiState: WorkbenchMessageListUiState | null;
  nextUiState: WorkbenchMessageListUiState;
  projectionRevision: number;
}): WorkbenchThreadProjectionOp {
  const { listItems, previousUiState, nextUiState, projectionRevision } = params;
  if (!previousUiState) {
    return createWorkbenchThreadProjectionOp("noop", projectionRevision);
  }

  const changedTurnHeaderIds = diffTrueKeys(previousUiState.expandedTurnHeaders, nextUiState.expandedTurnHeaders);
  const changedTurnDetailIds = diffTrueKeys(
    previousUiState.expandedTurnDetailsById,
    nextUiState.expandedTurnDetailsById,
  );
  const changedToolIds = diffTrueKeys(previousUiState.expandedToolById, nextUiState.expandedToolById);
  const changedMessageIds = diffTrueKeys(previousUiState.expandedMessageById, nextUiState.expandedMessageById);
  const changedLoadingTurnIds = diffLoadingTurnIds(
    previousUiState.turnToolsLoading,
    nextUiState.turnToolsLoading,
  );
  const verbosityChanged = (previousUiState.verbosity ?? null) !== (nextUiState.verbosity ?? null);

  if (
    changedTurnHeaderIds.size === 0 &&
    changedTurnDetailIds.size === 0 &&
    changedToolIds.size === 0 &&
    changedMessageIds.size === 0 &&
    changedLoadingTurnIds.size === 0 &&
    !verbosityChanged
  ) {
    return createWorkbenchThreadProjectionOp("noop", projectionRevision);
  }

  const changedItemIds = listItems
    .filter((item) => {
      switch (item.kind) {
        case "turn_header":
          return changedTurnHeaderIds.has(item.header.id);
        case "message":
          return changedMessageIds.has(item.id);
        case "tool":
          return verbosityChanged || changedToolIds.has(item.id);
        case "tool_group":
          return (
            verbosityChanged ||
            changedTurnDetailIds.has(item.turn_id) ||
            changedLoadingTurnIds.has(item.turn_id) ||
            item.tools.some((tool) => changedToolIds.has(tool.id))
          );
        default:
          return false;
      }
    })
    .map((item) => item.id);

  return createWorkbenchThreadProjectionOp(
    "toggle_expansion",
    projectionRevision,
    changedItemIds,
    expandLocalizedRemeasureItemIds(listItems, changedItemIds),
  );
}

export function mergeWorkbenchThreadProjectionOps(
  primary: WorkbenchThreadProjectionOp,
  overlay: WorkbenchThreadProjectionOp,
): WorkbenchThreadProjectionOp {
  if (overlay.kind === "noop") return primary;
  if (primary.kind === "noop") return overlay;
  if (primary.kind === "replace_session" || overlay.kind === "replace_session") {
    return createWorkbenchThreadProjectionOp(
      "replace_session",
      Math.max(primary.projectionRevision, overlay.projectionRevision),
      [...primary.changedItemIds, ...overlay.changedItemIds],
      [...primary.remeasureItemIds, ...overlay.remeasureItemIds],
    );
  }
  if (primary.kind === overlay.kind) {
    return createWorkbenchThreadProjectionOp(
      primary.kind,
      Math.max(primary.projectionRevision, overlay.projectionRevision),
      [...primary.changedItemIds, ...overlay.changedItemIds],
      [...primary.remeasureItemIds, ...overlay.remeasureItemIds],
    );
  }
  return createWorkbenchThreadProjectionOp(
    "reconcile",
    Math.max(primary.projectionRevision, overlay.projectionRevision),
    [...primary.changedItemIds, ...overlay.changedItemIds],
    [...primary.remeasureItemIds, ...overlay.remeasureItemIds],
  );
}
