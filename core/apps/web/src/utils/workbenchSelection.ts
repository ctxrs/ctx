import { idToString } from "../api/client";

type SessionCandidate = {
  id?: string | null;
  relationship?: string | null;
  status?: string | null;
};

export function pickPreferredSession(
  sessions: Array<SessionCandidate | null | undefined> | undefined | null,
  preferredSessionId?: string | null,
): SessionCandidate | null {
  const list = (sessions ?? []).filter((s): s is SessionCandidate => Boolean(s));
  if (!Array.isArray(list) || list.length === 0) return null;
  const isSubagent = (s: SessionCandidate) => s?.relationship === "sub_agent";
  const nonSubagents = list.filter((s) => !isSubagent(s));
  const candidates = nonSubagents.length > 0 ? nonSubagents : list;
  if (preferredSessionId) {
    const preferred = candidates.find((s) => idToString(s?.id ?? "") === preferredSessionId);
    if (preferred) return preferred;
  }
  return candidates[candidates.length - 1] ?? null;
}

export function pickPreferredSessionId(
  sessions: Array<SessionCandidate | null | undefined> | undefined | null,
  preferredSessionId?: string | null,
): string | null {
  const s = pickPreferredSession(sessions, preferredSessionId);
  const id = s ? idToString(s.id ?? "") : "";
  return id ? String(id) : null;
}
