import { describe, expect, it } from "vitest";
import { allocateOptimisticOrderSeq, buildOptimisticUserMessage } from "./SessionPage.optimisticMessage";

describe("SessionPage optimistic message helpers", () => {
  it("builds optimistic user messages with strict-render anchors", () => {
    const message = buildOptimisticUserMessage({
      messageId: "msg-1",
      sessionId: "session-1",
      taskId: "task-1",
      turnId: "turn-1",
      content: "hello",
      attachments: [],
      delivery: "immediate",
      createdAt: "2026-01-01T00:00:00.000Z",
      orderSeqSeedMs: 1000,
    });

    expect(message.role).toBe("user");
    expect(message.turn_sequence).toBeTypeOf("number");
    expect(message.order_seq).toBe(message.turn_sequence);
    expect(Number.isFinite(Number(message.turn_sequence))).toBe(true);
  });

  it("assigns monotonic optimistic order sequence values", () => {
    const seed = 42;
    const first = allocateOptimisticOrderSeq(seed);
    const second = allocateOptimisticOrderSeq(seed);
    const third = allocateOptimisticOrderSeq(seed - 1000);

    expect(second).toBeGreaterThan(first);
    expect(third).toBeGreaterThan(second);
  });
});
