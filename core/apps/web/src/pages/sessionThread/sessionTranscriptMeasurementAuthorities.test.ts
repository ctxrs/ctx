import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./sessionMarkdownMeasurement", () => ({
  SESSION_MARKDOWN_MEASUREMENT_CONTRACT: {
    typography: {
      bodyFontSizePx: 14,
      bodyFontFamily: "sans-serif",
      bodyLineHeightPx: 20,
    },
  },
  clearSessionMarkdownMeasurementCaches: vi.fn(),
  measureSessionMarkdownDocument: vi.fn(() => 10),
  shouldUseRenderedAssistantRowMeasurement: vi.fn(() => false),
  shouldUseRenderedMarkdownMeasurement: vi.fn(() => false),
}));

vi.mock("./sessionPlainTextMeasurement", () => ({
  clearSessionPlainTextMeasurementCaches: vi.fn(),
}));

vi.mock("./sessionTextMeasurement", () => ({
  measureSessionTextHeight: vi.fn(() => 10),
}));

vi.mock("./sessionMeasurementPolicy", () => ({
  shouldUseExactRenderedPlainTextMeasurement: vi.fn(() => true),
}));

vi.mock("./pretextRowMeasurementOverrides", () => ({
  clearPretextRowMeasurementOverrides: vi.fn(),
  readPretextAssistantHeightOverride: vi.fn(() => null),
  readPretextMessageHeightOverride: vi.fn(() => null),
}));

vi.mock("./sessionThreadDomMeasurement", () => ({
  clearSessionThreadExactMeasurementCaches: vi.fn(),
  measureRenderedSessionAssistantHeight: vi.fn(() => null),
  measureRenderedSessionMarkdownHeight: vi.fn(() => null),
  measureRenderedSessionPlainTextBlockHeight: vi.fn(() => 21),
  measureRenderedSessionTurnHeaderPreviewTextHeight: vi.fn(() => 57),
}));

import { buildSessionTranscriptMeasurementHooks } from "./sessionTranscriptMeasurementAuthorities";
import {
  measureRenderedSessionPlainTextBlockHeight,
  measureRenderedSessionTurnHeaderPreviewTextHeight,
} from "./sessionThreadDomMeasurement";

describe("sessionTranscriptMeasurementAuthorities", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("measures turn-header preview text with the turn-header DOM surface", () => {
    const hooks = buildSessionTranscriptMeasurementHooks({ sessionId: "session-1" });

    expect(
      hooks.measureTextHeight?.({
        kind: "turn-header-preview-text",
        cacheKey: "turn-header:1:collapsed",
        text: "Turn header preview text that should use turn-header styling.",
        width: 320,
        collapsedMaxHeightPx: 44,
        expanded: false,
      }),
    ).toEqual({
      status: "measured",
      height: 57,
    });

    expect(measureRenderedSessionTurnHeaderPreviewTextHeight).toHaveBeenCalledWith({
      text: "Turn header preview text that should use turn-header styling.",
      width: 320,
      collapsedMaxHeightPx: 44,
      expanded: false,
    });
    expect(measureRenderedSessionPlainTextBlockHeight).not.toHaveBeenCalled();
  });
});
