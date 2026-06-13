import type { TerminalSession } from "@ctx/types";
import { idToString } from "../api/client";
import type {
  PersistedWorkbenchTerminalLayoutV1,
  SplitDirection,
  TerminalGroupState,
  TerminalLayoutNode,
  TerminalPanelScopeState,
} from "../workbench/types";
import { randomUuid } from "../utils/randomUuid";

const emptyScopeState = (): TerminalPanelScopeState => ({
  groups: [],
  activeGroupId: null,
  tabOrder: [],
});

export const defaultPanelState = (): PersistedWorkbenchTerminalLayoutV1 => ({
  v: 1,
  scope: "workspace",
  scopes: {
    task: emptyScopeState(),
    workspace: emptyScopeState(),
  },
});

function leafIds(node: TerminalLayoutNode | null, out: string[] = []): string[] {
  if (!node) return out;
  if (node.kind === "leaf") {
    out.push(node.id);
    return out;
  }
  leafIds(node.first, out);
  leafIds(node.second, out);
  return out;
}

export function firstLeafId(node: TerminalLayoutNode | null): string | null {
  if (!node) return null;
  if (node.kind === "leaf") return node.id;
  return firstLeafId(node.first) ?? firstLeafId(node.second);
}

export function findLeafIdForTerminal(node: TerminalLayoutNode | null, terminalId: string): string | null {
  if (!node) return null;
  if (node.kind === "leaf") return node.terminalId === terminalId ? node.id : null;
  return findLeafIdForTerminal(node.first, terminalId) ?? findLeafIdForTerminal(node.second, terminalId);
}

export function findTerminalIdForLeaf(node: TerminalLayoutNode | null, leafId: string | null): string | null {
  if (!node || !leafId) return null;
  if (node.kind === "leaf") return node.id === leafId ? node.terminalId : null;
  return findTerminalIdForLeaf(node.first, leafId) ?? findTerminalIdForLeaf(node.second, leafId);
}

export function terminalIdsInLayout(node: TerminalLayoutNode | null, out: string[] = []): string[] {
  if (!node) return out;
  if (node.kind === "leaf") {
    out.push(node.terminalId);
    return out;
  }
  terminalIdsInLayout(node.first, out);
  terminalIdsInLayout(node.second, out);
  return out;
}

export function resolveActiveLeafId(node: TerminalLayoutNode | null, activeLeafId: string | null): string | null {
  if (!node) return null;
  const list = leafIds(node);
  if (activeLeafId && list.includes(activeLeafId)) return activeLeafId;
  return list[0] ?? null;
}

export function createSingleTerminalGroup(terminalId: string): TerminalGroupState {
  const leafId = randomUuid();
  return {
    id: randomUuid(),
    layout: { kind: "leaf", id: leafId, terminalId },
    activeLeafId: leafId,
  };
}

export function findGroupForTerminal(
  groups: TerminalGroupState[],
  terminalId: string,
): { group: TerminalGroupState; leafId: string } | null {
  for (const group of groups) {
    const leafId = findLeafIdForTerminal(group.layout, terminalId);
    if (leafId) return { group, leafId };
  }
  return null;
}

function pruneLayout(node: TerminalLayoutNode | null, allowed: Set<string>): TerminalLayoutNode | null {
  if (!node) return null;
  if (node.kind === "leaf") {
    return allowed.has(node.terminalId) ? node : null;
  }
  const first = pruneLayout(node.first, allowed);
  const second = pruneLayout(node.second, allowed);
  if (!first && !second) return null;
  if (!first) return second;
  if (!second) return first;
  return { ...node, first, second };
}

export function updateSplitRatio(node: TerminalLayoutNode, splitId: string, ratio: number): TerminalLayoutNode {
  if (node.kind === "leaf") return node;
  if (node.id === splitId) return { ...node, ratio };
  return {
    ...node,
    first: updateSplitRatio(node.first, splitId, ratio),
    second: updateSplitRatio(node.second, splitId, ratio),
  };
}

export function splitLeaf(
  node: TerminalLayoutNode,
  leafId: string,
  newTerminalId: string,
  direction: SplitDirection,
): TerminalLayoutNode {
  if (node.kind === "leaf") {
    if (node.id !== leafId) return node;
    const existing = node.terminalId;
    return {
      kind: "split",
      id: randomUuid(),
      direction,
      ratio: 0.5,
      first: { kind: "leaf", id: randomUuid(), terminalId: existing },
      second: { kind: "leaf", id: randomUuid(), terminalId: newTerminalId },
    };
  }
  return {
    ...node,
    first: splitLeaf(node.first, leafId, newTerminalId, direction),
    second: splitLeaf(node.second, leafId, newTerminalId, direction),
  };
}

export function removeTerminalFromLayout(node: TerminalLayoutNode | null, terminalId: string): TerminalLayoutNode | null {
  if (!node) return null;
  if (node.kind === "leaf") {
    return node.terminalId === terminalId ? null : node;
  }
  const first = removeTerminalFromLayout(node.first, terminalId);
  const second = removeTerminalFromLayout(node.second, terminalId);
  if (!first && !second) return null;
  if (!first) return second;
  if (!second) return first;
  return { ...node, first, second };
}

export function reconcileScopeState(
  state: TerminalPanelScopeState,
  scopeTerminals: TerminalSession[],
): TerminalPanelScopeState {
  const ids = scopeTerminals.map((t) => idToString(t.id)).filter(Boolean) as string[];
  const allowed = new Set(ids);
  const tabOrder = state.tabOrder.filter((id) => allowed.has(id));
  const newIds = ids.filter((id) => !tabOrder.includes(id));
  const nextOrder = tabOrder.concat(newIds);
  let groups = state.groups
    .map((group) => {
      const layout = pruneLayout(group.layout, allowed);
      if (!layout) return null;
      const activeLeafId = resolveActiveLeafId(layout, group.activeLeafId);
      return { ...group, layout, activeLeafId };
    })
    .filter(Boolean) as TerminalGroupState[];

  if (groups.length === 0 && nextOrder.length > 0) {
    groups = [createSingleTerminalGroup(nextOrder[0])];
  }

  let activeGroupId = state.activeGroupId;
  if (activeGroupId && !groups.some((group) => group.id === activeGroupId)) {
    activeGroupId = groups[0]?.id ?? null;
  }
  if (!activeGroupId && groups.length > 0) {
    activeGroupId = groups[0].id;
  }
  return {
    groups,
    activeGroupId,
    tabOrder: nextOrder,
  };
}
