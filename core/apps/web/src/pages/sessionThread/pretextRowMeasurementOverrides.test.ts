import { beforeEach, describe, expect, it } from "vitest";
import type { WorkbenchListItem } from "../SessionPage.types";
import {
  clearPretextRowMeasurementOverrides,
  readPretextAssistantHeightOverride,
  readPretextMessageHeightOverride,
  writePretextAssistantHeightOverride,
  writePretextMessageHeightOverride,
} from "./pretextRowMeasurementOverrides";

describe("pretextRowMeasurementOverrides", () => {
  beforeEach(() => {
    clearPretextRowMeasurementOverrides();
  });

  it("keys assistant overrides by session and width", () => {
    const item: Extract<WorkbenchListItem, { kind: "assistant" }> = {
      kind: "assistant",
      id: "assistant-1",
      turn_id: "turn-1",
      created_at: "2026-04-25T00:00:00Z",
      content: "Assistant row content",
      thought: "",
      is_complete: true,
    };

    expect(
      writePretextAssistantHeightOverride({
        sessionId: "session-1",
        item,
        viewportWidth: 788,
        height: 83,
      }),
    ).toBe(true);

    expect(
      readPretextAssistantHeightOverride({
        sessionId: "session-1",
        item,
        viewportWidth: 788,
      }),
    ).toBe(83);
    expect(
      readPretextAssistantHeightOverride({
        sessionId: "session-2",
        item,
        viewportWidth: 788,
      }),
    ).toBeNull();
    expect(
      readPretextAssistantHeightOverride({
        sessionId: "session-1",
        item,
        viewportWidth: 640,
      }),
    ).toBeNull();
  });

  it("keys message overrides by expansion state and shown content", () => {
    const item: Extract<WorkbenchListItem, { kind: "message" }> = {
      kind: "message",
      id: "message-1",
      role: "user",
      content: "Please continue with the full explanation.",
      attachments: [],
      created_at: "2026-04-25T00:00:00Z",
    };

    expect(
      writePretextMessageHeightOverride({
        sessionId: "session-1",
        item,
        viewportWidth: 788,
        layout: {
          expanded: true,
          expandable: true,
          renderMode: "plain_text",
          shownContent: item.content,
        },
        height: 101,
      }),
    ).toBe(true);

    expect(
      readPretextMessageHeightOverride({
        sessionId: "session-1",
        item,
        viewportWidth: 788,
        layout: {
          expanded: true,
          expandable: true,
          renderMode: "plain_text",
          shownContent: item.content,
        },
      }),
    ).toBe(101);
    expect(
      readPretextMessageHeightOverride({
        sessionId: "session-1",
        item,
        viewportWidth: 788,
        layout: {
          expanded: false,
          expandable: true,
          renderMode: "plain_text",
          shownContent: "Please continue",
        },
      }),
    ).toBeNull();
  });
});
