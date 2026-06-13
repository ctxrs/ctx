import type { Message, SessionTurn } from "../../api/client";
import { describe, expect, it } from "vitest";
import {
  buildAssistantStreamingStamp,
  buildMessagesStamp,
  buildTurnsStamp,
} from "./stamps";

describe("session thread projection stamps", () => {
  it("tracks messages by revision and cheap structural markers instead of full content", () => {
    const messages = [
      { id: "message-1", content: "hello" },
      { id: "message-2", content: "world" },
    ] as Message[];

    expect(buildMessagesStamp(messages, 7)).toBe("7:2:message-1:message-2");
    expect(
      buildMessagesStamp(
        [
          { ...messages[0], content: "changed a lot" },
          { ...messages[1], content: "changed too" },
        ] as Message[],
        7,
      ),
    ).toBe("7:2:message-1:message-2");
  });

  it("changes turns stamps when the structural edge changes or the revision changes", () => {
    const turns = [
      { turn_id: "turn-1" },
      { turn_id: "turn-2" },
    ] as SessionTurn[];

    expect(buildTurnsStamp(turns, 5)).toBe("5:2:turn-1:turn-2");
    expect(buildTurnsStamp([{ turn_id: "older" }, ...turns] as SessionTurn[], 6)).toBe(
      "6:3:older:turn-2",
    );
  });

  it("keeps structural turn stamps stable when only assistant streaming changes", () => {
    const turns = [{ turn_id: "turn-1" }] as SessionTurn[];

    expect(buildTurnsStamp(turns, 5)).toBe(buildTurnsStamp(turns, 5));
    expect(buildAssistantStreamingStamp({}, 0)).not.toBe(
      buildAssistantStreamingStamp(
        {
          "turn-1": { content: "partial", providerMessageId: "provider-1", orderSeq: 2 },
        },
        1,
      ),
    );
  });

  it("tracks assistant streaming by explicit revision and entry count", () => {
    expect(buildAssistantStreamingStamp({}, 0)).toBe("0:0");
    expect(
      buildAssistantStreamingStamp(
        {
          "turn-1": { content: "partial", providerMessageId: "provider-1", orderSeq: 2 },
        },
        4,
      ),
    ).toBe("4:1");
  });
});
