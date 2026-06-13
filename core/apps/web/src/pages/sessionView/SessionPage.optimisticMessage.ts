import type { Message, MessageAttachment } from "../../api/client";

let lastAssignedOrderSeq = 0;

function normalizeOrderSeqSeed(seedMs: number): number {
  const parsed = Number(seedMs);
  if (!Number.isFinite(parsed)) return Date.now();
  return Math.trunc(parsed);
}

export function allocateOptimisticOrderSeq(seedMs: number = Date.now()): number {
  const candidate = normalizeOrderSeqSeed(seedMs);
  if (candidate <= lastAssignedOrderSeq) {
    lastAssignedOrderSeq += 1;
  } else {
    lastAssignedOrderSeq = candidate;
  }
  return lastAssignedOrderSeq;
}

type BuildOptimisticUserMessageInput = {
  messageId: string;
  sessionId: string;
  taskId: string;
  turnId: string;
  content: string;
  attachments: MessageAttachment[];
  delivery: Message["delivery"];
  createdAt?: string;
  orderSeqSeedMs?: number;
};

export function buildOptimisticUserMessage(input: BuildOptimisticUserMessageInput): Message {
  const orderSeq = allocateOptimisticOrderSeq(input.orderSeqSeedMs);
  return {
    id: input.messageId,
    session_id: input.sessionId,
    task_id: input.taskId,
    turn_id: input.turnId,
    turn_sequence: orderSeq,
    order_seq: orderSeq,
    role: "user",
    content: input.content,
    attachments: input.attachments,
    delivery: input.delivery,
    created_at: input.createdAt ?? new Date().toISOString(),
  };
}
