import { describe, expect, it } from "vitest";
import { estimatePretextVirtualizerItemHeight } from "./estimateHeuristics";
import type { WorkbenchListItem } from "./SessionPage.types";

describe("estimatePretextVirtualizerItemHeight", () => {
  it("matches the compact rendered height contract for turn status rows", () => {
    const item: Extract<WorkbenchListItem, { kind: "turn_status" }> = {
      kind: "turn_status",
      id: "turn-status-1",
      turn_id: "turn-1",
      created_at: "",
      started_at: "",
      updated_at: "",
      status: "completed",
      assistant_messages_content: "done",
    };

    expect(estimatePretextVirtualizerItemHeight(item)).toBe(24);
  });

  it("matches the rendered spacer contract", () => {
    const item: Extract<WorkbenchListItem, { kind: "spacer" }> = {
      kind: "spacer",
      id: "spacer-1",
      created_at: "",
    };

    expect(estimatePretextVirtualizerItemHeight(item)).toBe(1);
  });

  it("uses a markdown-aware estimate for assistant rows with many short paragraphs", () => {
    const content = [
      "At 8:14 on a wet Thursday, someone stole the mayor's window.",
      "",
      "Not broke it.",
      "",
      "Not opened it.",
      "",
      "Stole it.",
    ].join("\n");
    const item: Extract<WorkbenchListItem, { kind: "assistant" }> = {
      kind: "assistant",
      id: "assistant-1",
      turn_id: "turn-1",
      created_at: "",
      content,
      thought: "",
      is_complete: true,
    };

    expect(estimatePretextVirtualizerItemHeight(item)).toBe(115);
  });
});
