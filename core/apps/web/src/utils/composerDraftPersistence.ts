import type { WorkbenchModeId } from "../components/WorkbenchComposer";

export type PersistedComposerDraftV1 = {
  v: 1;
  text: string;
  modeId?: WorkbenchModeId;
};

const WORKBENCH_MODES: WorkbenchModeId[] = ["default", "research", "plan", "review"];

function isWorkbenchModeId(v: unknown): v is WorkbenchModeId {
  return typeof v === "string" && (WORKBENCH_MODES as string[]).includes(v);
}

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

export function composerDraftKeyNewTaskV1(workspaceId: string) {
  return `wb.composerDraft.newTask.v1.${workspaceId}`;
}

export function composerDraftKeySessionV1(workspaceId: string, sessionId: string) {
  return `wb.composerDraft.session.v1.${workspaceId}.${sessionId}`;
}

export function loadComposerDraftV1(key: string): PersistedComposerDraftV1 | null {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return null;
    const parsed = asRecord(JSON.parse(raw));
    if (!parsed || Object.keys(parsed).length === 0) return null;
    if (parsed.v !== 1) return null;
    if (typeof parsed.text !== "string") return null;
    const modeId = isWorkbenchModeId(parsed.modeId) ? parsed.modeId : undefined;
    return { v: 1, text: parsed.text, modeId };
  } catch {
    return null;
  }
}

export function saveComposerDraftV1(key: string, draft: PersistedComposerDraftV1) {
  try {
    localStorage.setItem(key, JSON.stringify({ v: 1, text: draft.text, modeId: draft.modeId }));
  } catch {
    // ignore
  }
}

export function removeComposerDraft(key: string) {
  try {
    localStorage.removeItem(key);
  } catch {
    // ignore
  }
}
