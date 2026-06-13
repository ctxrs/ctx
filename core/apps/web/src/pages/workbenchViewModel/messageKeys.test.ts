import { describe, expect, it } from "vitest";
import type { Message, SessionTurn } from "../../api/client";
import { deriveAssistantStreamingKey, deriveMessagesKey, deriveTurnsKey } from "./messageKeys";

describe("messageKeys", () => {
  it("ignores assistant_partial when deriving turnsKey", () => {
    const turns = [
      {
        turn_id: "turn-1",
        start_seq: 1,
        updated_at: "2025-12-15T00:00:00.000Z",
        assistant_partial: "",
      },
      {
        turn_id: "turn-2",
        start_seq: 2,
        updated_at: "2025-12-15T00:00:01.000Z",
        assistant_partial: "",
      },
    ];

    const k1 = deriveTurnsKey(turns as unknown as SessionTurn[]);
    const k2 = deriveTurnsKey([
      {
        ...turns[0],
        assistant_partial: "stream",
      },
      turns[1],
    ] as unknown as SessionTurn[]);

    expect(k1).toBe(k2);
  });

  it("changes assistant streaming key when a non-tail pending assistant changes with the same timestamp", () => {
    const k1 = deriveAssistantStreamingKey({});
    const k2 = deriveAssistantStreamingKey({
      "turn-1": {
        content: "stream",
        providerMessageId: "provider-1",
        orderSeq: 2,
      },
    });

    expect(k1).not.toBe(k2);
  });

  it("changes messagesKey when queued delivery changes without changing array length", () => {
    const messages = [
      {
        id: "message-1",
        session_id: "session-1",
        role: "user",
        content: "hello",
        attachments: [],
        delivery: "immediate",
        created_at: "2025-12-15T00:00:00.000Z",
      },
    ];

    const k1 = deriveMessagesKey(messages as unknown as Message[]);
    const k2 = deriveMessagesKey([
      {
        ...messages[0],
        delivery: "queued",
      },
    ] as unknown as Message[]);

    expect(k1).not.toBe(k2);
  });
});
