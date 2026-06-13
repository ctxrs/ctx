import { getDaemonIdentityScopeOrNull } from "../state/daemonTargetScopeIdentity";
import { uiStateBatch, uiStateDelete, uiStateGet, uiStateSet } from "../state/uiStateStore";
import { serializeDaemonTargetScope } from "../state/scopeIdentity";
import type {
  LayoutNode,
  PersistedWorkbenchDraftV1,
  PersistedWorkbenchTerminalLayoutV1,
  PersistedWorkbenchTerminalTitlesV1,
  PersistedWorkbenchTerminalOpenV1,
  PersistedWorkbenchWindowV1,
  SplitDirection,
  TerminalGroupState,
  TerminalLayoutNode,
  TerminalPanelScopeState,
  TerminalScope,
  WorkbenchTab,
  WorkbenchDraft,
} from "./types";

const WINDOW_DB_VERSION = 1 as const;
const DRAFT_DB_VERSION = 1 as const;
const DIFF_PANE_DB_VERSION = 1 as const;
const ARTIFACTS_PANE_DB_VERSION = 1 as const;
const SESSIONS_PANE_DB_VERSION = 1 as const;
const TERMINAL_PANEL_DB_VERSION = 1 as const;
const TERMINAL_LAYOUT_DB_VERSION = 1 as const;
const TERMINAL_TITLES_DB_VERSION = 1 as const;

export function workbenchDaemonKey(): string {
  const targetScope = getDaemonIdentityScopeOrNull();
  return targetScope ? serializeDaemonTargetScope(targetScope) : "unknown";
}

function safeKeyPart(v: string): string {
  return encodeURIComponent(v);
}

export function workbenchWindowKeyV1(workspaceId: string, windowId: string): string {
  return `wb.window.v${WINDOW_DB_VERSION}.${safeKeyPart(workbenchDaemonKey())}.${safeKeyPart(workspaceId)}.${safeKeyPart(windowId)}`;
}

export function workbenchDraftKeyV1(workspaceId: string, key: string): string {
  return `wb.draft.v${DRAFT_DB_VERSION}.${safeKeyPart(workbenchDaemonKey())}.${safeKeyPart(workspaceId)}.${safeKeyPart(key)}`;
}

export function workbenchDiffPaneKeyV1(workspaceId: string, scopeId: string): string {
  return `wb.diff_pane.v${DIFF_PANE_DB_VERSION}.${safeKeyPart(workbenchDaemonKey())}.${safeKeyPart(workspaceId)}.${safeKeyPart(scopeId)}`;
}

export function workbenchArtifactsPaneKeyV1(workspaceId: string, sessionId: string): string {
  return `wb.artifacts_pane.v${ARTIFACTS_PANE_DB_VERSION}.${safeKeyPart(workbenchDaemonKey())}.${safeKeyPart(workspaceId)}.${safeKeyPart(sessionId)}`;
}

export function workbenchSessionsPaneKeyV1(workspaceId: string, sessionId: string): string {
  return `wb.sessions_pane.v${SESSIONS_PANE_DB_VERSION}.${safeKeyPart(workbenchDaemonKey())}.${safeKeyPart(workspaceId)}.${safeKeyPart(sessionId)}`;
}

export function workbenchTerminalPanelKeyV1(workspaceId: string): string {
  return `wb.terminal_panel.v${TERMINAL_PANEL_DB_VERSION}.${safeKeyPart(workbenchDaemonKey())}.${safeKeyPart(workspaceId)}`;
}

export function workbenchTerminalLayoutKeyV1(workspaceId: string): string {
  return `wb.terminal_layout.v${TERMINAL_LAYOUT_DB_VERSION}.${safeKeyPart(workbenchDaemonKey())}.${safeKeyPart(workspaceId)}`;
}

export function workbenchTerminalTitlesKeyV1(workspaceId: string): string {
  return `wb.terminal_titles.v${TERMINAL_TITLES_DB_VERSION}.${safeKeyPart(workbenchDaemonKey())}.${safeKeyPart(workspaceId)}`;
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return !!v && typeof v === "object";
}

function isString(v: unknown): v is string {
  return typeof v === "string";
}

function isNumber(v: unknown): v is number {
  return typeof v === "number" && Number.isFinite(v);
}

function isBoolean(v: unknown): v is boolean {
  return typeof v === "boolean";
}

function decodeMessageAttachment(raw: unknown) {
  if (!isRecord(raw)) return null;
  if (raw.kind === "image") {
    if (!isString(raw.mime_type) || !isString(raw.data_base64)) return null;
    const name = raw.name == null ? null : isString(raw.name) ? raw.name : null;
    return {
      kind: "image" as const,
      mime_type: raw.mime_type,
      data_base64: raw.data_base64,
      name,
    };
  }
  if (raw.kind === "image_ref") {
    if (!isString(raw.blob_id) || !isString(raw.mime_type)) return null;
    const name = raw.name == null ? null : isString(raw.name) ? raw.name : null;
    return {
      kind: "image_ref" as const,
      blob_id: raw.blob_id,
      mime_type: raw.mime_type,
      name,
    };
  }
  return null;
}

function decodeSplitDirection(v: unknown): SplitDirection | null {
  if (v === "horizontal" || v === "vertical") return v;
  return null;
}

function decodeTerminalScope(v: unknown): TerminalScope | null {
  if (v === "task" || v === "workspace") return v;
  return null;
}

function decodeTerminalLayoutNode(raw: unknown, depth: number): TerminalLayoutNode | null {
  if (depth > 64) return null;
  if (!isRecord(raw)) return null;
  const kind = raw.kind;
  if (kind === "leaf") {
    if (!isString(raw.id) || !raw.id.trim()) return null;
    if (!isString(raw.terminalId) || !raw.terminalId.trim()) return null;
    return { kind: "leaf", id: raw.id, terminalId: raw.terminalId };
  }
  if (kind === "split") {
    if (!isString(raw.id) || !raw.id.trim()) return null;
    const direction = decodeSplitDirection(raw.direction);
    if (!direction) return null;
    const ratioRaw = raw.ratio;
    const ratio = isNumber(ratioRaw) ? ratioRaw : 0.5;
    const clampedRatio = Math.min(0.9, Math.max(0.1, ratio));
    const first = decodeTerminalLayoutNode(raw.first, depth + 1);
    const second = decodeTerminalLayoutNode(raw.second, depth + 1);
    if (!first || !second) return null;
    return { kind: "split", id: raw.id, direction, ratio: clampedRatio, first, second };
  }
  return null;
}

function terminalLayoutLeafIds(node: TerminalLayoutNode | null, out: string[] = []): string[] {
  if (!node) return out;
  if (node.kind === "leaf") {
    out.push(node.id);
    return out;
  }
  terminalLayoutLeafIds(node.first, out);
  terminalLayoutLeafIds(node.second, out);
  return out;
}

function decodeTerminalGroupState(raw: unknown): TerminalGroupState | null {
  if (!isRecord(raw)) return null;
  if (!isString(raw.id) || !raw.id.trim()) return null;
  const layout = decodeTerminalLayoutNode(raw.layout, 0);
  if (!layout) return null;
  const activeLeafId = raw.activeLeafId === null ? null : isString(raw.activeLeafId) ? raw.activeLeafId : null;
  const leafs = terminalLayoutLeafIds(layout);
  if (leafs.length === 0) return null;
  const resolvedActiveLeafId = activeLeafId && leafs.includes(activeLeafId) ? activeLeafId : leafs[0] ?? null;
  return { id: raw.id, layout, activeLeafId: resolvedActiveLeafId };
}

function decodeTerminalPanelScopeState(raw: unknown): TerminalPanelScopeState | null {
  if (!isRecord(raw)) return null;
  const tabOrderRaw = raw.tabOrder;
  if (!Array.isArray(tabOrderRaw)) return null;
  const tabOrder = tabOrderRaw.filter((v) => isString(v)).map((v) => String(v));

  const groupsRaw = raw.groups;
  const groups: TerminalGroupState[] = [];
  if (Array.isArray(groupsRaw)) {
    for (const g of groupsRaw) {
      const decoded = decodeTerminalGroupState(g);
      if (decoded) groups.push(decoded);
    }
  }

  let activeGroupId = raw.activeGroupId === null ? null : isString(raw.activeGroupId) ? raw.activeGroupId : null;

  if (groups.length === 0) {
    const layoutRaw = raw.layout;
    const layout = layoutRaw == null ? null : decodeTerminalLayoutNode(layoutRaw, 0);
    if (layoutRaw != null && !layout) return null;
    if (layout) {
      const leafs = terminalLayoutLeafIds(layout);
      if (leafs.length === 0) return null;
      const activeLeafId = raw.activeLeafId === null ? null : isString(raw.activeLeafId) ? raw.activeLeafId : null;
      const resolvedActiveLeafId = activeLeafId && leafs.includes(activeLeafId) ? activeLeafId : leafs[0] ?? null;
      groups.push({ id: "default", layout, activeLeafId: resolvedActiveLeafId });
      activeGroupId = "default";
    }
  }

  if (groups.length > 0 && (!activeGroupId || !groups.some((g) => g.id === activeGroupId))) {
    activeGroupId = groups[0]?.id ?? null;
  }

  return { groups, activeGroupId, tabOrder };
}

function decodeTerminalTitles(raw: unknown): PersistedWorkbenchTerminalTitlesV1 | null {
  if (!isRecord(raw)) return null;
  if (raw.v !== 1) return null;
  if (!isRecord(raw.titles)) return null;
  const titles: Record<string, string> = {};
  for (const [key, value] of Object.entries(raw.titles)) {
    if (!key || !isString(value)) continue;
    const trimmed = value.trim();
    if (!trimmed) continue;
    titles[key] = trimmed;
  }
  return { v: 1, titles };
}

function decodeTab(raw: unknown): WorkbenchTab | null {
  if (!isRecord(raw)) return null;
  if (!isString(raw.id) || !raw.id.trim()) return null;
  const kind = raw.kind;
  if (kind === "new_task") {
    const titleOverride = isString(raw.titleOverride) ? raw.titleOverride : undefined;
    const viewMode =
      raw.viewMode === "compact" || raw.viewMode === "normal" || raw.viewMode === "verbose" ? raw.viewMode : undefined;
    return { id: raw.id, kind: "new_task", titleOverride, viewMode };
  }
  if (kind === "task") {
    const ref = raw.ref;
    if (!isRecord(ref)) return null;
    if (!isString(ref.taskId) || !ref.taskId.trim()) return null;
    const sessionId = ref.sessionId === null ? null : isString(ref.sessionId) ? ref.sessionId : null;
    const titleOverride = isString(raw.titleOverride) ? raw.titleOverride : undefined;
    const viewMode =
      raw.viewMode === "compact" || raw.viewMode === "normal" || raw.viewMode === "verbose" ? raw.viewMode : undefined;
    return {
      id: raw.id,
      kind: "task",
      ref: { taskId: ref.taskId, sessionId },
      titleOverride,
      viewMode,
    };
  }
  return null;
}

function decodeLayoutNode(raw: unknown, depth: number): LayoutNode | null {
  if (depth > 64) return null;
  if (!isRecord(raw)) return null;
  const kind = raw.kind;
  if (kind === "split") {
    if (!isString(raw.id) || !raw.id.trim()) return null;
    const direction = decodeSplitDirection(raw.direction);
    if (!direction) return null;
    const ratioRaw = raw.ratio;
    const ratio = isNumber(ratioRaw) ? ratioRaw : 0.5;
    const clampedRatio = Math.min(0.9, Math.max(0.1, ratio));
    const first = decodeLayoutNode(raw.first, depth + 1);
    const second = decodeLayoutNode(raw.second, depth + 1);
    if (!first || !second) return null;
    return { kind: "split", id: raw.id, direction, ratio: clampedRatio, first, second };
  }
  if (kind === "leaf") {
    if (!isString(raw.id) || !raw.id.trim()) return null;
    const tabsRaw = raw.tabs;
    if (!Array.isArray(tabsRaw)) return null;
    const tabs: WorkbenchTab[] = [];
    for (const t of tabsRaw) {
      const decoded = decodeTab(t);
      if (decoded) tabs.push(decoded);
    }
    if (tabs.length === 0) return null;
    const activeTabIdRaw = raw.activeTabId;
    const activeTabId = isString(activeTabIdRaw) ? activeTabIdRaw : "";
    const activeExists = tabs.some((t) => t.id === activeTabId);
    return { kind: "leaf", id: raw.id, tabs, activeTabId: activeExists ? activeTabId : tabs[0].id };
  }
  return null;
}

export function decodePersistedWorkbenchWindowV1(raw: unknown): PersistedWorkbenchWindowV1 | null {
  if (!isRecord(raw)) return null;
  if (raw.v !== 1) return null;
  const layout = decodeLayoutNode(raw.layout, 0);
  if (!layout) return null;
  if (!isString(raw.focusedLeafId) || !raw.focusedLeafId.trim()) return null;
  return { v: 1, layout, focusedLeafId: raw.focusedLeafId };
}

export function decodePersistedWorkbenchTerminalLayoutV1(raw: unknown): PersistedWorkbenchTerminalLayoutV1 | null {
  if (!isRecord(raw)) return null;
  if (raw.v !== 1) return null;
  const scope = decodeTerminalScope(raw.scope);
  if (!scope) return null;
  const scopesRaw = raw.scopes;
  if (!isRecord(scopesRaw)) return null;
  const task = decodeTerminalPanelScopeState(scopesRaw.task);
  const workspace = decodeTerminalPanelScopeState(scopesRaw.workspace);
  if (!task || !workspace) return null;
  return { v: 1, scope, scopes: { task, workspace } };
}

export function decodePersistedWorkbenchTerminalOpenV1(raw: unknown): PersistedWorkbenchTerminalOpenV1 | null {
  if (!isRecord(raw)) return null;
  if (raw.v !== 1) return null;
  if (!isBoolean(raw.open)) return null;
  if (!isNumber(raw.height)) return null;
  return { v: 1, open: raw.open, height: raw.height };
}

export async function loadWorkbenchWindowV1(workspaceId: string, windowId: string): Promise<PersistedWorkbenchWindowV1 | null> {
  const raw = await uiStateGet(workbenchWindowKeyV1(workspaceId, windowId));
  return decodePersistedWorkbenchWindowV1(raw);
}

export async function saveWorkbenchWindowV1(workspaceId: string, windowId: string, win: PersistedWorkbenchWindowV1): Promise<void> {
  await uiStateSet(workbenchWindowKeyV1(workspaceId, windowId), win);
}

export async function saveWorkbenchWindowV1Immediate(
  workspaceId: string,
  windowId: string,
  win: PersistedWorkbenchWindowV1,
): Promise<void> {
  await uiStateBatch([{ kind: "set", key: workbenchWindowKeyV1(workspaceId, windowId), value: win }]);
}

export async function deleteWorkbenchWindowV1(workspaceId: string, windowId: string): Promise<void> {
  await uiStateDelete(workbenchWindowKeyV1(workspaceId, windowId));
}

export function decodePersistedWorkbenchDraftV1(raw: unknown): PersistedWorkbenchDraftV1 | null {
  if (!isRecord(raw)) return null;
  if (raw.v !== 1) return null;
  if (!isString(raw.key) || !raw.key.trim()) return null;
  const draftRaw = raw.draft;
  if (!isRecord(draftRaw)) return null;
  if (!isString(draftRaw.text)) return null;
  const modeId = isString(draftRaw.modeId) ? draftRaw.modeId : "default";
  if (modeId !== "default" && modeId !== "research" && modeId !== "plan" && modeId !== "review") return null;
  const attachmentsRaw = Array.isArray(draftRaw.attachments) ? draftRaw.attachments : [];
  const attachments = attachmentsRaw
    .map((attachment) => decodeMessageAttachment(attachment))
    .filter((attachment): attachment is NonNullable<typeof attachment> => attachment !== null);
  const updatedAtMs = isNumber(draftRaw.updatedAtMs) ? draftRaw.updatedAtMs : 0;
  const draft: WorkbenchDraft = { text: draftRaw.text, modeId, attachments, updatedAtMs };
  return { v: 1, key: raw.key, draft };
}

export async function loadWorkbenchDraftV1(workspaceId: string, key: string): Promise<WorkbenchDraft | null> {
  const raw = await uiStateGet(workbenchDraftKeyV1(workspaceId, key));
  const decoded = decodePersistedWorkbenchDraftV1(raw);
  return decoded?.draft ?? null;
}

export async function saveWorkbenchDraftV1(workspaceId: string, key: string, draft: WorkbenchDraft): Promise<void> {
  const payload: PersistedWorkbenchDraftV1 = { v: 1, key, draft };
  await uiStateSet(workbenchDraftKeyV1(workspaceId, key), payload);
}

export async function deleteWorkbenchDraftV1(workspaceId: string, key: string): Promise<void> {
  await uiStateDelete(workbenchDraftKeyV1(workspaceId, key));
}

export async function loadWorkbenchDiffPaneOpenV1(workspaceId: string, scopeId: string): Promise<boolean | null> {
  const raw = await uiStateGet(workbenchDiffPaneKeyV1(workspaceId, scopeId));
  return typeof raw === "boolean" ? raw : null;
}

export async function saveWorkbenchDiffPaneOpenV1(
  workspaceId: string,
  scopeId: string,
  open: boolean,
): Promise<void> {
  await uiStateSet(workbenchDiffPaneKeyV1(workspaceId, scopeId), open);
}

export async function loadWorkbenchArtifactsPaneOpenV1(
  workspaceId: string,
  sessionId: string,
): Promise<boolean | null> {
  const raw = await uiStateGet(workbenchArtifactsPaneKeyV1(workspaceId, sessionId));
  return typeof raw === "boolean" ? raw : null;
}

export async function saveWorkbenchArtifactsPaneOpenV1(
  workspaceId: string,
  sessionId: string,
  open: boolean,
): Promise<void> {
  await uiStateSet(workbenchArtifactsPaneKeyV1(workspaceId, sessionId), open);
}

export async function loadWorkbenchSessionsPaneOpenV1(
  workspaceId: string,
  sessionId: string,
): Promise<boolean | null> {
  const raw = await uiStateGet(workbenchSessionsPaneKeyV1(workspaceId, sessionId));
  return typeof raw === "boolean" ? raw : null;
}

export async function saveWorkbenchSessionsPaneOpenV1(
  workspaceId: string,
  sessionId: string,
  open: boolean,
): Promise<void> {
  await uiStateSet(workbenchSessionsPaneKeyV1(workspaceId, sessionId), open);
}

export async function loadWorkbenchTerminalPanelOpenV1(
  workspaceId: string,
): Promise<PersistedWorkbenchTerminalOpenV1 | null> {
  const raw = await uiStateGet(workbenchTerminalPanelKeyV1(workspaceId));
  return decodePersistedWorkbenchTerminalOpenV1(raw);
}

export async function saveWorkbenchTerminalPanelOpenV1(
  workspaceId: string,
  next: PersistedWorkbenchTerminalOpenV1,
): Promise<void> {
  await uiStateSet(workbenchTerminalPanelKeyV1(workspaceId), next);
}

export async function loadWorkbenchTerminalLayoutV1(
  workspaceId: string,
): Promise<PersistedWorkbenchTerminalLayoutV1 | null> {
  const raw = await uiStateGet(workbenchTerminalLayoutKeyV1(workspaceId));
  return decodePersistedWorkbenchTerminalLayoutV1(raw);
}

export async function loadWorkbenchTerminalTitlesV1(
  workspaceId: string,
): Promise<PersistedWorkbenchTerminalTitlesV1 | null> {
  const raw = await uiStateGet(workbenchTerminalTitlesKeyV1(workspaceId));
  return decodeTerminalTitles(raw);
}

export async function saveWorkbenchTerminalLayoutV1(
  workspaceId: string,
  next: PersistedWorkbenchTerminalLayoutV1,
): Promise<void> {
  await uiStateSet(workbenchTerminalLayoutKeyV1(workspaceId), next);
}

export async function saveWorkbenchTerminalTitlesV1(
  workspaceId: string,
  next: PersistedWorkbenchTerminalTitlesV1,
): Promise<void> {
  await uiStateSet(workbenchTerminalTitlesKeyV1(workspaceId), next);
}
export async function deleteWorkbenchDiffPaneOpenV1(workspaceId: string, scopeId: string): Promise<void> {
  await uiStateDelete(workbenchDiffPaneKeyV1(workspaceId, scopeId));
}
