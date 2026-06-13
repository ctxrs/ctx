import type { SessionEvent } from "../../api/client";
import type { AskUserQuestionAnswerState } from "../sessionView";

function normalizeAskUserQuestionAnswers(raw: unknown): Record<string, string> {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return {};
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(raw)) {
    if (typeof value === "string" && key.trim()) {
      out[key] = value;
    }
  }
  return out;
}

function extractAskUserQuestionAnswer(ev: SessionEvent): {
  toolCallId: string;
  outcome: "submitted" | "cancelled";
  answers: Record<string, string>;
} | null {
  if (ev.event_type !== "notice") return null;
  const payload = ev.payload_json ?? {};
  if (payload.kind !== "ask_user_question_answered") return null;
  const toolCallId = String(payload.tool_call_id ?? "").trim();
  if (!toolCallId) return null;
  const outcomeRaw = String(payload.outcome ?? "").trim();
  const outcome =
    outcomeRaw === "cancelled" ? "cancelled" : outcomeRaw === "submitted" ? "submitted" : "submitted";
  const answers = normalizeAskUserQuestionAnswers(payload.answers ?? payload.answer ?? {});
  return { toolCallId, outcome, answers };
}

export function collectAskUserQuestionAnswers(
  events: SessionEvent[],
  optimistic: Record<string, AskUserQuestionAnswerState>,
): Map<string, AskUserQuestionAnswerState> {
  const map = new Map<string, AskUserQuestionAnswerState>();
  for (const ev of events) {
    const parsed = extractAskUserQuestionAnswer(ev);
    if (!parsed) continue;
    map.set(parsed.toolCallId, { outcome: parsed.outcome, answers: parsed.answers });
  }
  for (const [toolCallId, state] of Object.entries(optimistic)) {
    if (!toolCallId) continue;
    const existing = map.get(toolCallId);
    if (!existing) {
      map.set(toolCallId, state);
      continue;
    }
    if (Object.keys(existing.answers ?? {}).length === 0 && Object.keys(state.answers ?? {}).length > 0) {
      map.set(toolCallId, { outcome: existing.outcome, answers: state.answers });
    }
  }
  return map;
}
