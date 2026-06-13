import { describe, expect, it } from "vitest";
import type { WorkbenchListItem } from "./transcriptTypes";
import { getPretextVirtualizerRowLayout } from "./pretextVirtualizerRowLayout";
import {
  getTranscriptRowPlannedLayout,
  planTranscriptRowLayout,
} from "./transcriptLayoutPlanner";

describe("transcriptLayoutPlanner", () => {
  const item: WorkbenchListItem = {
    kind: "assistant",
    id: "assistant-1",
    turn_id: "turn-1",
    content: "Planner seam verification with `inline code` and plain text.",
    thought: "",
    is_complete: true,
    created_at: "2026-04-19T00:00:00Z",
  };

  it("preserves the existing row planner output", () => {
    const legacy = getPretextVirtualizerRowLayout(item, 640, {});
    const planned = getTranscriptRowPlannedLayout(item, 640, {});

    expect(planned).toEqual(legacy);
  });

  it("returns explicit row-plan metadata around the planned layout", () => {
    const plan = planTranscriptRowLayout(item, 640, {});

    expect(plan.itemId).toBe(item.id);
    expect(plan.rowKind).toBe(item.kind);
    expect(plan.totalHeight).toBe(plan.plannedLayout.height);
    expect(plan.geometryRevision.length).toBeGreaterThan(0);
    expect(plan.browserProfileId).toBe("chromium-like");
    expect(plan.trace).toEqual({
      planner: "pretext-row-layout",
      widthBucket: "w10",
    });
  });
});
