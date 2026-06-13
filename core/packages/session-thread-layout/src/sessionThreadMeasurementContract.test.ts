import { describe, expect, it } from "vitest";
import {
  SESSION_MARKDOWN_MEASUREMENT_CONTRACT,
  SESSION_THREAD_MEASUREMENT_GEOMETRY_REVISION,
  SESSION_THREAD_ROW_MEASUREMENT_CONTRACT,
} from "./sessionThreadMeasurementContract";
import { resolveSessionMarkdownListMarkerColumnWidthPx } from "./sessionThreadLayoutTokens";

describe("sessionThreadMeasurementContract", () => {
  it("keeps inline code chrome derived from the edge geometry", () => {
    expect(SESSION_MARKDOWN_MEASUREMENT_CONTRACT.inlineCode.fragmentChromeWidthPx).toBe(
      SESSION_MARKDOWN_MEASUREMENT_CONTRACT.inlineCode.edgeInlinePx * 2,
    );
    expect(SESSION_MARKDOWN_MEASUREMENT_CONTRACT.inlineCode.fragmentChromeHeightPx).toBe(
      SESSION_MARKDOWN_MEASUREMENT_CONTRACT.inlineCode.edgeBlockPx * 2,
    );
  });

  it("keeps list marker widths above the minimum gutter and expands for wider ordered markers", () => {
    const bulletWidth = resolveSessionMarkdownListMarkerColumnWidthPx(["•"]);
    const orderedWidth = resolveSessionMarkdownListMarkerColumnWidthPx(["1.", "10.", "100."]);

    expect(bulletWidth).toBeGreaterThanOrEqual(
      SESSION_MARKDOWN_MEASUREMENT_CONTRACT.list.markerMinWidthPx,
    );
    expect(orderedWidth).toBeGreaterThan(bulletWidth);
  });

  it("keeps both measurement contracts on the same shared geometry revision", () => {
    expect(SESSION_MARKDOWN_MEASUREMENT_CONTRACT.geometryRevision).toBe(
      SESSION_THREAD_MEASUREMENT_GEOMETRY_REVISION,
    );
    expect(SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.geometryRevision).toBe(
      SESSION_THREAD_MEASUREMENT_GEOMETRY_REVISION,
    );
  });

  it("captures markdown block-gap overrides and checkbox gutter in the shared contract", () => {
    expect(SESSION_MARKDOWN_MEASUREMENT_CONTRACT.list.checkboxGutterPx).toBeGreaterThan(
      SESSION_MARKDOWN_MEASUREMENT_CONTRACT.list.markerGapPx,
    );
    expect(SESSION_MARKDOWN_MEASUREMENT_CONTRACT.blockSpacing.entryGapPxByContext.root.heading).toBe(16);
    expect(SESSION_MARKDOWN_MEASUREMENT_CONTRACT.blockSpacing.exitGapPxByContext.listItem.paragraph).toBe(0);
  });

  it("captures thought and tool row typography through the shared row contract", () => {
    expect(SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.thought.horizontalChromePx).toBe(
      SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.thought.paddingInlinePx * 2,
    );
    expect(SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.thought.verticalChromePx).toBe(
      SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.thought.paddingBlockPx * 2,
    );
    expect(SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.tools.summary.rowHeightPx).toBe(
      SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.tools.summary.paddingBlockPx * 2 +
        SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.tools.summary.typography.lineHeightPx,
    );
    expect(SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.tools.thoughtTitle.heightPx).toBe(
      SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.tools.thoughtTitle.typography.lineHeightPx +
        SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.tools.thoughtTitle.marginBottomPx,
    );
    expect(SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.tools.thoughtBody.chromeWidthPx).toBe(
      (SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.tools.thoughtBody.paddingPx +
        SESSION_THREAD_ROW_MEASUREMENT_CONTRACT.tools.thoughtBody.borderWidthPx) *
        2,
    );
  });
});
