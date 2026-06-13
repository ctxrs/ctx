import { idToString, type Message } from "../../api/client";
import type { ThreadItem, WorkbenchThreadView } from "../sessionView";

type SortableThreadGroup = {
  sort_seq: number;
  group: WorkbenchThreadView["groups"][number];
};

function buildSystemMessageGroups(messages: Message[]): SortableThreadGroup[] {
  const systemMessages = messages
    .filter((m) => m.role === "system")
    .map((m, idx) => ({
      message: m,
      orderSeq: Number(m.turn_sequence ?? Number.NaN),
      idx,
    }))
    .filter((entry) => Number.isFinite(entry.orderSeq))
    .sort((a, b) => {
      if (a.orderSeq !== b.orderSeq) return (a.orderSeq as number) - (b.orderSeq as number);
      return a.idx - b.idx;
    });
  return systemMessages.flatMap((entry) => {
    const m = entry.message;
    const id = idToString(m.id);
    if (!id) {
      if (import.meta.env.DEV) {
        // eslint-disable-next-line no-console
        console.error("[WorkbenchThreadViewModel] system message missing id", {
          created_at: m.created_at ?? null,
          turn_sequence: m.turn_sequence ?? null,
        });
      }
      return [];
    }
    const attachments = Array.isArray(m.attachments)
      ? m.attachments
      : [];
    return [{
      sort_seq: entry.orderSeq as number,
      group: {
        key: `system-${id}`,
        header: null,
        items: [
          {
            kind: "message",
            id,
            role: "system",
            content: m.content ?? "",
            attachments,
            created_at: m.created_at,
          } satisfies Extract<ThreadItem, { kind: "message" }>,
        ],
      },
    }];
  });
}

export function mergeGroupsWithSystemMessages(
  groups: SortableThreadGroup[],
  messages: Message[],
): WorkbenchThreadView["groups"] {
  const systemGroups = buildSystemMessageGroups(messages);
  if (systemGroups.length === 0) {
    return groups.map((g) => g.group);
  }
  const combined = [...groups, ...systemGroups];
  combined.sort((a, b) => {
    if (a.sort_seq !== b.sort_seq) return a.sort_seq - b.sort_seq;
    return String(a.group.key).localeCompare(String(b.group.key));
  });
  return combined.map((g) => g.group);
}
