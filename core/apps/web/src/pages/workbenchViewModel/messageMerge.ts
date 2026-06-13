import { idToString, type Message, type SessionTurn } from "../../api/client";

const compareMessageOrder = (a: Message, b: Message): number => {
  const c = String(a.created_at).localeCompare(String(b.created_at));
  if (c !== 0) return c;
  const sa = Number(a.turn_sequence ?? Number.NaN);
  const sb = Number(b.turn_sequence ?? Number.NaN);
  if (Number.isFinite(sa) && Number.isFinite(sb) && sa !== sb) return sa - sb;
  if (Number.isFinite(sa) && !Number.isFinite(sb)) return -1;
  if (!Number.isFinite(sa) && Number.isFinite(sb)) return 1;
  return String(idToString(a.id)).localeCompare(String(idToString(b.id)));
};

export function mergeMessagesForView(
  messages: Message[],
  pending: Message[],
  includeQueuedMessageIds: Set<string> = new Set(),
): Message[] {
  const shouldInclude = (message: Message) => {
    if (message.delivery !== "queued") return true;
    const mid = idToString(message.id);
    return !!mid && includeQueuedMessageIds.has(mid);
  };
  const filteredMessages = messages.filter(shouldInclude);
  const filteredPending = pending.filter((entry) => shouldInclude(entry));
  if (filteredPending.length === 0) return filteredMessages;
  const byId = new Map<string, Message>();
  for (const m of filteredMessages) {
    const id = idToString(m.id);
    if (id) byId.set(id, m);
  }
  for (const entry of filteredPending) {
    const id = idToString(entry.id);
    if (!id || byId.has(id)) continue;
    byId.set(id, entry);
  }
  return Array.from(byId.values()).sort(compareMessageOrder);
}

export function mergeQueuedMessagesForPanel(
  queue: Message[],
  pending: Message[],
): Message[] {
  if (queue.length === 0 && pending.length === 0) return [];
  if (pending.length === 0) return queue.slice();
  const byId = new Map<string, Message>();
  const pendingNoId: Message[] = [];
  for (const msg of queue) {
    const id = idToString(msg.id);
    if (id) byId.set(id, msg);
  }
  for (const msg of pending) {
    const id = idToString(msg.id);
    if (!id) {
      pendingNoId.push(msg);
      continue;
    }
    if (!byId.has(id)) byId.set(id, msg);
  }
  const merged = [...byId.values(), ...pendingNoId];
  merged.sort(compareMessageOrder);
  return merged;
}

export function filterQueuedMessagesForPanel(
  queue: Message[],
  turns: SessionTurn[],
): Message[] {
  if (queue.length === 0 || turns.length === 0) return queue;
  const statusByUserMessageId = new Map<string, string>();
  for (const turn of turns) {
    const mid = turn.user_message_id ? idToString(turn.user_message_id) : "";
    if (!mid) continue;
    statusByUserMessageId.set(mid, turn.status);
  }
  return queue.filter((message) => {
    const mid = idToString(message.id);
    if (!mid) return true;
    const status = statusByUserMessageId.get(mid);
    if (!status) return true;
    return status === "queued";
  });
}

export function filterTurnsForQueuedMessages(
  turns: SessionTurn[],
  queuedMessageIds: Set<string>,
): SessionTurn[] {
  if (queuedMessageIds.size === 0) return turns;
  return turns.filter((turn) => {
    const mid = turn.user_message_id ? idToString(turn.user_message_id) : "";
    if (!mid) return true;
    return !queuedMessageIds.has(mid);
  });
}

export function buildPendingTurns(turns: SessionTurn[], messages: Message[]): SessionTurn[] {
  if (messages.length === 0) return [];
  const turnIds = new Set<string>();
  const userMessageIds = new Set<string>();
  for (const turn of turns) {
    const tid = idToString(turn.turn_id);
    if (tid) turnIds.add(tid);
    const uid = turn.user_message_id ? idToString(turn.user_message_id) : "";
    if (uid) userMessageIds.add(uid);
  }
  const pending: SessionTurn[] = [];
  for (const message of messages) {
    if (message.role !== "user") continue;
    const mid = idToString(message.id);
    if (!mid || userMessageIds.has(mid)) continue;
    const turnId = idToString(message.turn_id);
    if (!turnId) continue;
    if (turnIds.has(turnId)) continue;
    pending.push({
      turn_id: turnId,
      session_id: message.session_id,
      run_id: null,
      user_message_id: message.id,
      status: message.delivery === "immediate" ? "running" : "queued",
      start_seq: null,
      end_seq: null,
      started_at: message.created_at,
      updated_at: message.created_at,
      assistant_partial: "",
      thought_partial: "",
      metrics_json: null,
      tool_total: 0,
      tool_pending: 0,
      tool_running: 0,
      tool_completed: 0,
      tool_failed: 0,
    });
    turnIds.add(turnId);
  }
  return pending;
}
