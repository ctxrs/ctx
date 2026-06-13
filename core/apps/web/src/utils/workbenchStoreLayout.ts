import { randomUuid } from "./randomUuid";
import type { LayoutNode, PersistedWorkbenchWindowV1, WorkbenchTab } from "../workbench/types";
import { decodePersistedWorkbenchWindowV1 } from "../workbench/persistence";

const WINDOW_ID_STORAGE_KEY = "contextUiWindowId.v1";
const WINDOW_SESSION_STORAGE_PREFIX = "wb.window.session.v1";

function sessionWindowKeyV1(workspaceId: string, windowId: string): string {
  return `${WINDOW_SESSION_STORAGE_PREFIX}.${workspaceId}.${windowId}`;
}

export function readSessionWindowV1(workspaceId: string, windowId: string): PersistedWorkbenchWindowV1 | null {
  try {
    const raw = sessionStorage.getItem(sessionWindowKeyV1(workspaceId, windowId));
    if (!raw) return null;
    return decodePersistedWorkbenchWindowV1(JSON.parse(raw));
  } catch {
    return null;
  }
}

export function writeSessionWindowV1(workspaceId: string, windowId: string, win: PersistedWorkbenchWindowV1) {
  try {
    sessionStorage.setItem(sessionWindowKeyV1(workspaceId, windowId), JSON.stringify(win));
  } catch {
    // ignore
  }
}

export function getOrCreateWindowId(): string {
  const windowNamePrefix = "ctx-ui-window-id:";
  const readWindowName = (): string | null => {
    try {
      if (typeof window === "undefined") return null;
      const name = String(window.name ?? "");
      if (!name.startsWith(windowNamePrefix)) return null;
      const id = name.slice(windowNamePrefix.length).trim();
      return id || null;
    } catch {
      return null;
    }
  };
  const writeWindowName = (id: string) => {
    try {
      if (typeof window === "undefined") return;
      const name = String(window.name ?? "");
      if (name && !name.startsWith(windowNamePrefix)) return;
      window.name = `${windowNamePrefix}${id}`;
    } catch {
      // ignore
    }
  };
  try {
    const existing = sessionStorage.getItem(WINDOW_ID_STORAGE_KEY);
    if (existing && existing.trim()) return existing;
    const fromName = readWindowName();
    if (fromName) {
      sessionStorage.setItem(WINDOW_ID_STORAGE_KEY, fromName);
      return fromName;
    }
    const created = randomUuid();
    sessionStorage.setItem(WINDOW_ID_STORAGE_KEY, created);
    writeWindowName(created);
    return created;
  } catch {
    const fromName = readWindowName();
    if (fromName) return fromName;
    const created = randomUuid();
    writeWindowName(created);
    return created;
  }
}

export function defaultWindowState(): PersistedWorkbenchWindowV1 {
  const leafId = randomUuid();
  const tabId = randomUuid();
  return {
    v: 1,
    layout: {
      kind: "leaf",
      id: leafId,
      tabs: [{ id: tabId, kind: "new_task" }],
      activeTabId: tabId,
    },
    focusedLeafId: leafId,
  };
}

export function findLeaf(node: LayoutNode, leafId: string): Extract<LayoutNode, { kind: "leaf" }> | null {
  if (node.kind === "leaf") return node.id === leafId ? node : null;
  return findLeaf(node.first, leafId) ?? findLeaf(node.second, leafId);
}

function mapLayout(node: LayoutNode, fn: (n: LayoutNode) => LayoutNode): LayoutNode {
  const next = fn(node);
  if (next.kind === "split") {
    return { ...next, first: mapLayout(next.first, fn), second: mapLayout(next.second, fn) };
  }
  return next;
}

export function updateLeaf(
  node: LayoutNode,
  leafId: string,
  fn: (leaf: Extract<LayoutNode, { kind: "leaf" }>) => LayoutNode,
): LayoutNode {
  return mapLayout(node, (n) => {
    if (n.kind !== "leaf") return n;
    if (n.id !== leafId) return n;
    return fn(n);
  });
}

export function ensureLeafActiveTab(leaf: Extract<LayoutNode, { kind: "leaf" }>): Extract<LayoutNode, { kind: "leaf" }> {
  const activeOk = leaf.tabs.some((t) => t.id === leaf.activeTabId);
  if (activeOk) return leaf;
  return { ...leaf, activeTabId: leaf.tabs[0]?.id ?? leaf.activeTabId };
}

export function getActiveTabFromLeaf(leaf: Extract<LayoutNode, { kind: "leaf" }>): WorkbenchTab | null {
  const found = leaf.tabs.find((t) => t.id === leaf.activeTabId);
  return found ?? leaf.tabs[0] ?? null;
}
