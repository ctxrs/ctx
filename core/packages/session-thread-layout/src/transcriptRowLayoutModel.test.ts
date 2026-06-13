import { describe, expect, it } from "vitest";
import type { WorkbenchListItem } from "./transcriptTypes";
import {
  getWorkbenchMessageCollapseState,
  getWorkbenchMessageLayoutState,
  getWorkbenchTurnHeaderLayoutState,
} from "./transcriptRowLayoutModel";

describe("transcriptRowLayoutModel", () => {
  it("collapses giant multiline messages to the first 20 lines", () => {
    const lines = Array.from({ length: 2000 }, (_, index) => `line ${index + 1}`);
    const content = lines.join("\n");

    const collapseState = getWorkbenchMessageCollapseState(content);

    expect(collapseState.canCollapse).toBe(true);
    expect(collapseState.isExpandable).toBe(true);
    expect(collapseState.collapsedContent).toBe(lines.slice(0, 20).join("\n"));
  });

  it("keeps long single-line messages fixed-height even when they exceed the char threshold", () => {
    const content = `prefix ${"x".repeat(3000)}`;

    const collapseState = getWorkbenchMessageCollapseState(content);

    expect(collapseState.isExpandable).toBe(true);
    expect(collapseState.canCollapse).toBe(false);
    expect(collapseState.collapsedContent).toBe(content);
  });

  it("reuses cached collapse metadata for repeated giant-message lookups", () => {
    const content = Array.from({ length: 1000 }, (_, index) => `entry ${index + 1}`).join("\n");

    const first = getWorkbenchMessageCollapseState(content);
    const second = getWorkbenchMessageCollapseState(content);

    expect(second).toBe(first);
  });

  it("uses the cached collapsed preview in message layout state", () => {
    const item: Extract<WorkbenchListItem, { kind: "message" }> = {
      kind: "message",
      id: "message-1",
      role: "user",
      content: Array.from({ length: 40 }, (_, index) => `line ${index + 1}`).join("\n"),
      attachments: [],
      created_at: "2025-01-01T00:00:00.000Z",
    };

    const collapsed = getWorkbenchMessageLayoutState(item, {});
    const expanded = getWorkbenchMessageLayoutState(item, { "message-1": true });

    expect(collapsed.expandable).toBe(true);
    expect(collapsed.expanded).toBe(false);
    expect(collapsed.shownContent).toBe(item.content.split("\n").slice(0, 20).join("\n"));
    expect(expanded.expanded).toBe(true);
    expect(expanded.shownContent).toBe(item.content);
  });

  it("keeps giant turn headers collapsed by default and only materializes full text when expanded", () => {
    const content = Array.from({ length: 2000 }, (_, index) => `line ${index + 1}`).join("\n");
    const item: Extract<WorkbenchListItem, { kind: "turn_header" }> = {
      kind: "turn_header",
      id: "turn-header-1",
      header: {
        id: "turn-1",
        content,
        attachments: [],
        created_at: "2025-01-01T00:00:00.000Z",
      },
    };

    const collapsed = getWorkbenchTurnHeaderLayoutState(item, {});
    const expanded = getWorkbenchTurnHeaderLayoutState(item, { "turn-1": true });

    expect(collapsed.expandable).toBe(true);
    expect(collapsed.expanded).toBe(false);
    expect(collapsed.contentRevision).toBe("markdown:".concat(content));
    expect(collapsed.displayPlainText).toBe(["line 1", "line 2", "line 3", "line 4"].join("\n"));
    expect(expanded.expanded).toBe(true);
    expect(expanded.displayPlainText).toBe(content);
  });

  it("uses explicit turn-header content revisions as cache identity", () => {
    const item: Extract<WorkbenchListItem, { kind: "turn_header" }> = {
      kind: "turn_header",
      id: "turn-header-1",
      header: {
        id: "turn-1",
        content: Array.from({ length: 2000 }, (_, index) => `line ${index + 1}`).join("\n"),
        content_revision: "message-123",
        attachments: [],
        created_at: "2025-01-01T00:00:00.000Z",
      },
    };

    const collapsed = getWorkbenchTurnHeaderLayoutState(item, {});
    const expanded = getWorkbenchTurnHeaderLayoutState(item, { "turn-1": true });

    expect(collapsed.contentRevision).toBe("revision:message-123");
    expect(expanded.contentRevision).toBe("revision:message-123");
  });

  it("renders giant expanded user messages as deterministic plain text", () => {
    const content = ["# Reference", "", ...Array.from({ length: 2200 }, (_, index) => `reference line ${index + 1}`)].join(
      "\n",
    );
    const item: Extract<WorkbenchListItem, { kind: "message" }> = {
      kind: "message",
      id: "message-huge",
      role: "user",
      content,
      attachments: [],
      created_at: "2026-04-17T00:00:00.000Z",
    };

    const collapsed = getWorkbenchMessageLayoutState(item, {});
    expect(collapsed.expandable).toBe(true);
    expect(collapsed.renderMode).toBe("markdown");

    const expanded = getWorkbenchMessageLayoutState(item, { "message-huge": true });
    expect(expanded.expanded).toBe(true);
    expect(expanded.shownContent).toBe(content);
    expect(expanded.renderMode).toBe("plain_text");
  });
});
