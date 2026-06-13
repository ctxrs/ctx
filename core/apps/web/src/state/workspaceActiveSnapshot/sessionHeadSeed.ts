import type {
  SessionHeadSnapshot,
  SessionSnapshotSummary,
} from "@ctx/types";
import { idToString } from "../../api/client";
import { emptySessionHeadWindow } from "../sessionHeadState";
import type { WorkspaceActiveSnapshotItem } from "./storeTypes";

const summaryToSeedHead = (summary: SessionSnapshotSummary): SessionHeadSnapshot => ({
  session: summary.session,
  turns: [],
  tool_summaries: [],
  events: [],
  messages: [],
  last_event_seq: summary.last_event_seq ?? 0,
  projection_rev: summary.projection_rev ?? 0,
  state_rev: summary.state_rev ?? 0,
  activity: undefined,
  has_more_turns: false,
  history_cursor: null,
  has_more_history: false,
  summary_checkpoint: undefined,
  head_window: emptySessionHeadWindow(),
});

export const createSeedHeadSnapshot = (
  tasks: Map<string, WorkspaceActiveSnapshotItem>,
  sessionId: string,
): SessionHeadSnapshot | null => {
  const id = idToString(sessionId);
  if (!id) return null;
  for (const item of tasks.values()) {
    for (const summary of item.sessions) {
      if (idToString(summary.session.id) === id) {
        return summaryToSeedHead(summary);
      }
    }
  }
  return null;
};
