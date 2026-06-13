import { describe, expect, it } from "vitest";
import {
  applyAssistantChunkToStreaming,
  applyAssistantCompleteToStreaming,
  clearAllAssistantStreaming,
  clearAssistantStreaming,
  type AssistantStreamingStore,
} from "./assistantStreaming";

const mkStore = (): AssistantStreamingStore => ({
  assistantStreamingByTurnId: {},
  assistantStreamingRev: 0,
});

describe("assistantStreaming", () => {
  it("seals a turn after assistant_complete so late chunks cannot resurrect partials", () => {
    const store = mkStore();

    expect(applyAssistantCompleteToStreaming(store, "turn-1", "final answer", "provider-msg-1")).toBe(true);
    expect(store.assistantStreamingByTurnId["turn-1"]).toEqual({
      content: "final answer",
      providerMessageId: "provider-msg-1",
      orderSeq: null,
    });
    expect(store.sealedAssistantTurnIds?.has("turn-1")).toBe(true);

    expect(clearAssistantStreaming(store, "turn-1")).toBe(true);
    expect(store.assistantStreamingByTurnId["turn-1"]).toBeUndefined();
    expect(store.sealedAssistantTurnIds?.has("turn-1")).toBe(true);

    expect(applyAssistantChunkToStreaming(store, "turn-1", " late chunk", "provider-msg-1")).toBe(false);
    expect(store.assistantStreamingByTurnId["turn-1"]).toBeUndefined();
    expect(store.sealedAssistantTurnIds?.has("turn-1")).toBe(true);
  });

  it("clears sealed turn tracking on a full transcript reset", () => {
    const store = mkStore();

    applyAssistantCompleteToStreaming(store, "turn-1", "final answer", "provider-msg-1");
    expect(store.sealedAssistantTurnIds?.has("turn-1")).toBe(true);

    expect(clearAllAssistantStreaming(store)).toBe(true);
    expect(store.assistantStreamingByTurnId).toEqual({});
    expect(store.sealedAssistantTurnIds?.size ?? 0).toBe(0);

    expect(applyAssistantChunkToStreaming(store, "turn-1", "new stream", "provider-msg-2")).toBe(true);
    expect(store.assistantStreamingByTurnId["turn-1"]).toEqual({
      content: "new stream",
      providerMessageId: "provider-msg-2",
      orderSeq: null,
    });
  });
});
